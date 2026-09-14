//! The three built-in sources: a directory of files, an RSS feed, and any
//! program that speaks the exec protocol.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::kernel::{Actor, ActorKind, Draft, NewItem, NewPart, PartKind, Source};

// ---------------------------------------------------------------- fs

/// A directory. Every non-dot file is an item; a draft becomes a new file.
pub struct Fs {
    pub id: String,
    pub dir: PathBuf,
}

impl Source for Fs {
    fn pull(&self) -> Result<Vec<NewItem>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<_> = fs::read_dir(&self.dir)
            .with_context(|| format!("read {}", self.dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        entries
            .iter()
            .map(|e| e.path())
            .filter(|p| p.is_file() && !is_dotfile(p))
            .map(|p| item_from_file(&self.id, &p))
            .collect()
    }

    fn send(&self, draft: &Draft, _reply_to_foreign: Option<&str>) -> Result<NewItem> {
        fs::create_dir_all(&self.dir)?;
        let dest = unique_path(&self.dir.join(format!("{}.md", filename(&draft.title))));
        fs::write(&dest, draft.body.as_bytes())?;
        let mut item = item_from_file(&self.id, &dest)?;
        item.title = draft.title.clone();
        if let Some(fid) = nonempty(draft.foreign_id.as_deref()) {
            item.foreign_id = fid.to_string();
        }
        Ok(item)
    }
}

fn is_dotfile(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_none_or(|n| n.starts_with('.'))
}

/// Text files become a body; media files become one part.
pub fn item_from_file(source_id: &str, path: &Path) -> Result<NewItem> {
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.clone());
    let base = NewItem {
        source_id: source_id.to_string(),
        foreign_id: filename.clone(),
        title,
        href: Some(path.display().to_string()),
        ..Default::default()
    };
    if let Some((kind, mime)) = media_kind(path) {
        return Ok(NewItem {
            body: filename,
            parts: vec![NewPart {
                kind,
                mime: mime.to_string(),
                src: Some(path.display().to_string()),
                ..Default::default()
            }],
            ..base
        });
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(NewItem {
        body: String::from_utf8_lossy(&bytes).into_owned(),
        ..base
    })
}

fn media_kind(path: &Path) -> Option<(PartKind, &'static str)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => (PartKind::Image, "image/png"),
        "jpg" | "jpeg" => (PartKind::Image, "image/jpeg"),
        "gif" => (PartKind::Image, "image/gif"),
        "webp" => (PartKind::Image, "image/webp"),
        "svg" => (PartKind::Image, "image/svg+xml"),
        "mp3" => (PartKind::Audio, "audio/mpeg"),
        "wav" => (PartKind::Audio, "audio/wav"),
        "ogg" | "oga" => (PartKind::Audio, "audio/ogg"),
        "m4a" => (PartKind::Audio, "audio/mp4"),
        "flac" => (PartKind::Audio, "audio/flac"),
        "mp4" => (PartKind::Video, "video/mp4"),
        "webm" => (PartKind::Video, "video/webm"),
        "mov" => (PartKind::Video, "video/quicktime"),
        "mkv" => (PartKind::Video, "video/x-matroska"),
        _ => return None,
    })
}

/// Ascii letters, digits, `-` and `_`; runs of anything else collapse to one `-`.
pub fn filename(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_matches('-');
    if s.is_empty() {
        "untitled".into()
    } else {
        s.to_string()
    }
}

/// `name.md`, else `name-2.md`, `name-3.md`, ...
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("md");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    (2..1000)
        .map(|n| parent.join(format!("{stem}-{n}.{ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

// ---------------------------------------------------------------- rss

/// A feed. Read-only.
pub struct Rss {
    pub id: String,
    pub url: String,
}

impl Source for Rss {
    fn pull(&self) -> Result<Vec<NewItem>> {
        let bytes = reqwest::blocking::Client::builder()
            .user_agent("paddock/0.1")
            .build()?
            .get(&self.url)
            .send()
            .with_context(|| format!("fetch {}", self.url))?
            .bytes()?;
        let channel = rss::Channel::read_from(&bytes[..]).context("parse rss")?;
        Ok(channel
            .items()
            .iter()
            .map(|it| {
                let link = it.link().map(str::to_string);
                let guid = it.guid().map(|g| g.value().to_string());
                let title = it.title().unwrap_or("untitled").to_string();
                NewItem {
                    source_id: self.id.clone(),
                    foreign_id: guid
                        .or_else(|| link.clone())
                        .unwrap_or_else(|| title.clone()),
                    title,
                    body: it.description().or(it.content()).unwrap_or("").to_string(),
                    href: link,
                    ..Default::default()
                }
            })
            .collect())
    }

    fn send(&self, _draft: &Draft, _reply_to_foreign: Option<&str>) -> Result<NewItem> {
        bail!("source cannot send")
    }
}

// ---------------------------------------------------------------- exec

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
    in_reply_to: Option<String>,
    #[serde(default)]
    forward_of: Option<String>,
    #[serde(default)]
    cite_excerpt: Option<String>,
    #[serde(default)]
    cite_actor: Option<ExecActor>,
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
            in_reply_to: opt(it.in_reply_to),
            forward_of: opt(it.forward_of),
            cite_excerpt: opt(it.cite_excerpt),
            cite_actor: it.cite_actor.map(actor_in),
            read: it.read,
        }
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

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

fn opt(s: Option<String>) -> Option<String> {
    nonempty(s.as_deref()).map(str::to_string)
}
