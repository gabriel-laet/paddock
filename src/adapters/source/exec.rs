//! Any program that speaks the exec protocol. An item's `cites` are
//! `{kind, foreign_id?, source_id?, href?, excerpt?, actor?}`; kind is
//! reply | forward | quote | mention | attach.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{nonempty, opt};
use crate::kernel::{Actor, ActorKind, Cite, CiteKind, Draft, NewItem, NewPart, PartKind, Source};

/// Any program. `{cmd} {args...} pull` prints items as JSON (array or NDJSON);
/// `{cmd} {args...} send` reads a JSON draft on stdin and prints
/// `{foreign_id, start?, end?}`. Exit 2 means the source cannot send.
pub struct Exec {
    pub id: String,
    pub cmd: PathBuf,
    pub args: Vec<String>,
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct ExecActor {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct ExecPart {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    mime: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ExecCite {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    foreign_id: Option<String>,
    #[serde(default)]
    href: Option<String>,
    #[serde(default)]
    excerpt: Option<String>,
    #[serde(default)]
    actor: Option<ExecActor>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ExecItem {
    #[serde(default)]
    foreign_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    href: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    from: Option<ExecActor>,
    #[serde(default)]
    to: Vec<ExecActor>,
    #[serde(default)]
    cites: Vec<ExecCite>,
    #[serde(default)]
    parts: Vec<ExecPart>,
    #[serde(default)]
    read: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct ExecDraft {
    title: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_foreign: Option<String>,
    to: Vec<ExecActor>,
    parts: Vec<ExecPart>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ExecSent {
    #[serde(default)]
    foreign_id: String,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

impl Source for Exec {
    fn pull(&self) -> Result<Vec<NewItem>> {
        let output = self.run("pull", None)?;
        if !output.status.success() {
            return Err(self.failed("pull", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        self.parse_items(&stdout)
    }

    fn send(&self, draft: &Draft, reply_to_foreign: Option<&str>) -> Result<NewItem> {
        let payload = ExecDraft {
            title: draft.title.clone(),
            body: draft.body.clone(),
            thread: draft.thread.clone(),
            reply_to_foreign: nonempty(reply_to_foreign).map(str::to_string),
            to: draft.to.iter().map(actor_out).collect(),
            parts: draft.parts.iter().map(part_out).collect(),
        };
        let bytes = serde_json::to_vec(&payload)?;
        let output = self.run("send", Some(&bytes))?;
        if !output.status.success() {
            return Err(self.failed("send", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let sent: ExecSent = serde_json::from_str(stdout.trim())
            .with_context(|| format!("source {} send: invalid JSON", self.id))?;
        if sent.foreign_id.trim().is_empty() {
            bail!("source {} send: missing foreign_id", self.id);
        }
        Ok(NewItem {
            source_id: self.id.clone(),
            foreign_id: sent.foreign_id,
            title: draft.title.clone(),
            body: draft.body.clone(),
            start: opt(sent.start),
            end: opt(sent.end),
            parts: draft.parts.clone(),
            ..Default::default()
        })
    }
}

impl Exec {
    fn run(&self, verb: &str, stdin: Option<&[u8]>) -> Result<std::process::Output> {
        let mut c = Command::new(&self.cmd);
        c.args(&self.args).arg(verb);
        if let Some(d) = &self.dir {
            c.current_dir(d);
        }
        c.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let cannot = || anyhow::anyhow!("source {}: cannot run `{}`", self.id, self.cmd.display());
        let Some(data) = stdin else {
            return c.output().map_err(|_| cannot());
        };
        let mut child = c.spawn().map_err(|_| cannot())?;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(data)
                .with_context(|| format!("source {}: write {verb} stdin", self.id))?;
        }
        child.wait_with_output().map_err(|_| cannot())
    }

    fn failed(&self, verb: &str, output: &std::process::Output) -> anyhow::Error {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let refused = output.status.code() == Some(2)
            || stderr.contains("source cannot send")
            || stdout.contains("source cannot send");
        if verb == "send" && refused {
            return anyhow::anyhow!("source cannot send");
        }
        match stderr.trim() {
            "" => anyhow::anyhow!(
                "source {} {verb} failed (exit {})",
                self.id,
                output.status.code().unwrap_or(-1)
            ),
            detail => anyhow::anyhow!("source {} {verb} failed: {detail}", self.id),
        }
    }

    fn parse_items(&self, stdout: &str) -> Result<Vec<NewItem>> {
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let raw: Vec<ExecItem> = if trimmed.starts_with('[') {
            serde_json::from_str(trimmed)
                .with_context(|| format!("source {} pull: invalid JSON array", self.id))?
        } else {
            stdout
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
                .map(|(i, l)| {
                    serde_json::from_str(l.trim()).with_context(|| {
                        format!("source {} pull: invalid NDJSON on line {}", self.id, i + 1)
                    })
                })
                .collect::<Result<_>>()?
        };
        raw.into_iter()
            .map(|it| {
                if it.foreign_id.trim().is_empty() {
                    bail!("source {} pull: item missing foreign_id", self.id);
                }
                Ok(self.item_in(it))
            })
            .collect()
    }

    fn item_in(&self, it: ExecItem) -> NewItem {
        NewItem {
            source_id: self.id.clone(),
            foreign_id: it.foreign_id,
            title: it.title,
            body: it.body,
            href: it.href,
            start: opt(it.start),
            end: opt(it.end),
            thread: opt(it.thread),
            parts: it.parts.into_iter().map(part_in).collect(),
            from: it.from.map(actor_in),
            to: it.to.into_iter().map(actor_in).collect(),
            cites: it.cites.into_iter().map(cite_in).collect(),
            read: it.read,
        }
    }
}

fn cite_in(c: ExecCite) -> Cite {
    Cite {
        kind: CiteKind::parse(&c.kind),
        source_id: opt(c.source_id),
        foreign_id: opt(c.foreign_id),
        id: None,
        href: opt(c.href),
        excerpt: opt(c.excerpt),
        actor: c.actor.map(actor_in),
    }
}

fn actor_in(a: ExecActor) -> Actor {
    Actor {
        id: a.id,
        name: a.name.filter(|s| !s.is_empty()),
        kind: ActorKind::parse(a.kind.as_deref().unwrap_or("")),
    }
}

fn actor_out(a: &Actor) -> ExecActor {
    ExecActor {
        id: a.id.clone(),
        name: a.name.clone(),
        kind: Some(a.kind.as_str().to_string()),
    }
}

fn part_in(p: ExecPart) -> NewPart {
    NewPart {
        kind: PartKind::parse(&p.kind),
        mime: p.mime,
        text: p.text,
        bytes: None,
        src: p.path.filter(|s| !s.is_empty()),
    }
}

fn part_out(p: &NewPart) -> ExecPart {
    ExecPart {
        kind: p.kind.as_str().to_string(),
        mime: p.mime.clone(),
        text: p.text.clone(),
        path: p.src.clone(),
    }
}
