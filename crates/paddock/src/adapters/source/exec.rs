//! Any program that speaks the exec protocol (`paddock-protocol`). The host
//! runs `{cmd} {args...} pull|send` with a JSON request on stdin: the
//! source's id and settings, and for `send` the draft.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{nonempty, opt};
use crate::kernel::{
    Actor, ActorKind, Cite, CiteKind, Draft, NewItem, NewPart, PartKind, Settings, Source,
};
use crate::protocol as wire;

pub struct Exec {
    pub id: String,
    pub cmd: PathBuf,
    pub args: Vec<String>,
    pub dir: Option<PathBuf>,
    /// Handed to the program on stdin with every verb.
    pub settings: Settings,
}

impl Source for Exec {
    fn pull(&self) -> Result<Vec<NewItem>> {
        let request = wire::Request {
            id: self.id.clone(),
            settings: self.settings.clone(),
            draft: None,
        };
        let output = self.run("pull", &request)?;
        if !output.status.success() {
            return Err(self.failed("pull", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        self.parse_items(&stdout)
    }

    fn send(&self, draft: &Draft, reply_to_foreign: Option<&str>) -> Result<NewItem> {
        let request = wire::Request {
            id: self.id.clone(),
            settings: self.settings.clone(),
            draft: Some(wire::Draft {
                title: draft.title.clone(),
                body: draft.body.clone(),
                thread: draft.thread.clone(),
                reply_to_foreign: nonempty(reply_to_foreign).map(str::to_string),
                to: draft.to.iter().map(actor_out).collect(),
                parts: draft.parts.iter().map(part_out).collect(),
            }),
        };
        let output = self.run("send", &request)?;
        if !output.status.success() {
            return Err(self.failed("send", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let sent: wire::Sent = serde_json::from_str(stdout.trim())
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
    fn run(&self, verb: &str, request: &wire::Request) -> Result<std::process::Output> {
        let mut c = Command::new(&self.cmd);
        c.args(&self.args).arg(verb);
        if let Some(d) = &self.dir {
            c.current_dir(d);
        }
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let cannot = || anyhow::anyhow!("source {}: cannot run `{}`", self.id, self.cmd.display());
        let mut child = c.spawn().map_err(|_| cannot())?;
        if let Some(mut pipe) = child.stdin.take() {
            // A program may not read its request; that is its business.
            match pipe.write_all(&serde_json::to_vec(request)?) {
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                other => {
                    other.with_context(|| format!("source {}: write {verb} stdin", self.id))?
                }
            }
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
        let items = parse_wire_items(stdout).with_context(|| format!("source {} pull", self.id))?;
        items
            .into_iter()
            .map(|it| {
                if it.foreign_id.trim().is_empty() {
                    bail!("source {} pull: item missing foreign_id", self.id);
                }
                Ok(item_in(&self.id, it))
            })
            .collect()
    }
}

/// A JSON array, or one item per line.
pub fn parse_wire_items(stdout: &str) -> Result<Vec<wire::Item>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).context("invalid JSON array");
    }
    stdout
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l.trim())
                .with_context(|| format!("invalid NDJSON on line {}", i + 1))
        })
        .collect()
}

/// A wire item as the kernel sees it, from `source_id`.
pub fn item_in(source_id: &str, it: wire::Item) -> NewItem {
    NewItem {
        source_id: source_id.to_string(),
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

fn cite_in(c: wire::Cite) -> Cite {
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

fn actor_in(a: wire::Actor) -> Actor {
    Actor {
        id: a.id,
        name: a.name.filter(|s| !s.is_empty()),
        kind: ActorKind::parse(a.kind.as_deref().unwrap_or("")),
    }
}

fn actor_out(a: &Actor) -> wire::Actor {
    wire::Actor {
        id: a.id.clone(),
        name: a.name.clone(),
        kind: Some(a.kind.as_str().to_string()),
    }
}

fn part_in(p: wire::Part) -> NewPart {
    NewPart {
        kind: PartKind::parse(&p.kind),
        mime: p.mime,
        text: p.text,
        bytes: None,
        src: p.path.filter(|s| !s.is_empty()),
    }
}

fn part_out(p: &NewPart) -> wire::Part {
    wire::Part {
        kind: p.kind.as_str().to_string(),
        mime: p.mime.clone(),
        text: p.text.clone(),
        path: p.src.clone(),
    }
}
