use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::store::{Actor, ActorKind, NewPart, PartKind};

#[derive(Debug, Clone, Default)]
pub struct NewItem {
    pub source_id: String,
    pub foreign_id: String,
    pub title: String,
    pub body: String,
    pub href: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub thread: Option<String>,
    pub parts: Vec<NewPart>,
    pub from: Option<Actor>,
    pub to: Vec<Actor>,
    /// Foreign id on the same source. Resolved to a local id on admit.
    pub in_reply_to: Option<String>,
    /// Foreign id on the same source. Resolved to a local id on admit.
    pub forward_of: Option<String>,
    pub cite_excerpt: Option<String>,
    pub cite_actor: Option<Actor>,
    /// Read state reported by the source, if it tracks one (e.g. a chat's
    /// unread tail). `None` means the source has no opinion.
    pub read: Option<bool>,
}

/// A compose or reply waiting to become an item.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub thread: Option<String>,
    pub reply_to: Option<i64>,
    pub foreign_id: Option<String>,
    pub parts: Vec<NewPart>,
    pub to: Vec<Actor>,
}

/// Non-recursive. Skips dotfiles and directories.
pub fn pull_fs(source_id: &str, dir: &Path) -> Result<Vec<NewItem>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for ent in entries {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        out.push(item_from_file(source_id, &path)?);
    }
    Ok(out)
}

pub fn item_from_file(source_id: &str, path: &Path) -> Result<NewItem> {
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.clone());
    if let Some((kind, mime)) = media_kind(path) {
        return Ok(NewItem {
            source_id: source_id.to_string(),
            foreign_id: filename.clone(),
            title,
            body: filename,
            href: Some(path.display().to_string()),
            start: None,
            end: None,
            thread: None,
            parts: vec![NewPart {
                kind,
                mime: mime.to_string(),
                text: None,
                bytes: None,
                src: Some(path.display().to_string()),
            }],
            ..Default::default()
        });
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let body = String::from_utf8_lossy(&bytes).into_owned();
    Ok(NewItem {
        source_id: source_id.to_string(),
        foreign_id: filename,
        title,
        body,
        href: Some(path.display().to_string()),
        start: None,
        end: None,
        thread: None,
        parts: Vec::new(),
        ..Default::default()
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

pub fn pull_rss(source_id: &str, url: &str) -> Result<Vec<NewItem>> {
    let bytes = reqwest::blocking::Client::builder()
        .user_agent("paddock/0.1")
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("fetch {url}"))?
        .bytes()
        .with_context(|| format!("read {url}"))?;
    let channel = rss::Channel::read_from(&bytes[..])
        .with_context(|| format!("parse rss {url}"))?;
    let mut out = Vec::new();
    for it in channel.items() {
        let href = it.link().map(|s| s.to_string());
        let foreign = it
            .guid()
            .map(|g| g.value().to_string())
            .or_else(|| href.clone())
            .or_else(|| it.title().map(|s| s.to_string()))
            .unwrap_or_else(|| "untitled".into());
        let title = it.title().unwrap_or("untitled").to_string();
        let body = it
            .content()
            .or_else(|| it.description())
            .unwrap_or("")
            .to_string();
        out.push(NewItem {
            source_id: source_id.to_string(),
            foreign_id: foreign,
            title,
            body,
            href,
            start: None,
            end: None,
            thread: None,
            parts: Vec::new(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// Result of an exec `send`: foreign id plus optional times from the plugin.
#[derive(Debug, Clone, Default)]
pub struct SendResult {
    pub foreign_id: String,
    pub start: Option<String>,
    pub end: Option<String>,
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
    source_id: Option<String>,
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
struct ExecDraftJson {
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
struct ExecSendJson {
    #[serde(default)]
    foreign_id: String,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

fn actor_from_exec(a: ExecActor) -> Actor {
    Actor {
        id: a.id,
        name: a.name.filter(|s| !s.is_empty()),
        kind: ActorKind::parse(a.kind.as_deref().unwrap_or("")),
    }
}

fn actor_to_exec(a: &Actor) -> ExecActor {
    ExecActor {
        id: a.id.clone(),
        name: a.name.clone(),
        kind: Some(a.kind.as_str().to_string()),
    }
}

fn part_from_exec(p: ExecPart) -> NewPart {
    NewPart {
        kind: PartKind::parse(&p.kind),
        mime: p.mime,
        text: p.text,
        bytes: None,
        src: p.path.filter(|s| !s.is_empty()),
    }
}

fn part_to_exec(p: &NewPart) -> ExecPart {
    ExecPart {
        kind: p.kind.as_str().to_string(),
        mime: p.mime.clone(),
        text: p.text.clone(),
        path: p.src.clone(),
    }
}

fn empty_to_none(s: Option<String>) -> Option<String> {
    s.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

fn exec_item_to_new(source_id: &str, it: ExecItem) -> NewItem {
    let sid = it
        .source_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(source_id)
        .to_string();
    NewItem {
        source_id: sid,
        foreign_id: it.foreign_id,
        title: it.title,
        body: it.body,
        href: it.href,
        start: empty_to_none(it.start),
        end: empty_to_none(it.end),
        thread: empty_to_none(it.thread),
        parts: it.parts.into_iter().map(part_from_exec).collect(),
        from: it.from.map(actor_from_exec),
        to: it.to.into_iter().map(actor_from_exec).collect(),
        in_reply_to: empty_to_none(it.in_reply_to),
        forward_of: empty_to_none(it.forward_of),
        cite_excerpt: empty_to_none(it.cite_excerpt),
        cite_actor: it.cite_actor.map(actor_from_exec),
        read: it.read,
    }
}

fn parse_pull_items(source_id: &str, stdout: &str) -> Result<Vec<NewItem>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<ExecItem> = if trimmed.starts_with('[') {
        serde_json::from_str(trimmed)
            .with_context(|| format!("source {source_id} pull: invalid JSON array"))?
    } else {
        let mut items = Vec::new();
        for (i, line) in stdout.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            items.push(serde_json::from_str(line).with_context(|| {
                format!("source {source_id} pull: invalid NDJSON on line {}", i + 1)
            })?);
        }
        items
    };
    let mut out = Vec::new();
    for it in raw {
        if it.foreign_id.trim().is_empty() {
            anyhow::bail!("source {source_id} pull: item missing foreign_id");
        }
        out.push(exec_item_to_new(source_id, it));
    }
    Ok(out)
}

fn run_exec(
    source_id: &str,
    cmd: &Path,
    args: &[String],
    dir: Option<&Path>,
    verb: &str,
    stdin_data: Option<&[u8]>,
) -> Result<std::process::Output> {
    let mut c = Command::new(cmd);
    c.args(args);
    c.arg(verb);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    c.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    let spawn_err = || anyhow::anyhow!("source {source_id}: cannot run `{}`", cmd.display());
    if let Some(data) = stdin_data {
        let mut child = c.spawn().map_err(|_| spawn_err())?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(data)
                .with_context(|| format!("source {source_id}: write {verb} stdin"))?;
        }
        child.wait_with_output().map_err(|_| spawn_err())
    } else {
        c.output().map_err(|_| spawn_err())
    }
}

fn exec_failed(source_id: &str, verb: &str, output: &std::process::Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if verb == "send"
        && (output.status.code() == Some(2)
            || stderr.contains("source cannot send")
            || stdout.contains("source cannot send"))
    {
        return anyhow::anyhow!("source cannot send");
    }
    let detail = stderr.trim();
    if detail.is_empty() {
        anyhow::anyhow!(
            "source {source_id} {verb} failed (exit {})",
            output.status.code().unwrap_or(-1)
        )
    } else {
        anyhow::anyhow!("source {source_id} {verb} failed: {detail}")
    }
}

/// Run `{cmd} {args...} pull`. Stdout is a JSON array of items or NDJSON.
pub fn pull_exec(
    source_id: &str,
    cmd: &Path,
    args: &[String],
    dir: Option<&Path>,
) -> Result<Vec<NewItem>> {
    let output = run_exec(source_id, cmd, args, dir, "pull", None)?;
    if !output.status.success() {
        return Err(exec_failed(source_id, "pull", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_pull_items(source_id, &stdout)
}

/// Run `{cmd} {args...} send` with a JSON draft on stdin.
pub fn send_exec(
    source_id: &str,
    cmd: &Path,
    args: &[String],
    dir: Option<&Path>,
    draft: &Draft,
    reply_to_foreign: Option<&str>,
) -> Result<SendResult> {
    let payload = ExecDraftJson {
        title: draft.title.clone(),
        body: draft.body.clone(),
        thread: draft.thread.clone(),
        reply_to_foreign: reply_to_foreign
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        to: draft.to.iter().map(actor_to_exec).collect(),
        parts: draft.parts.iter().map(part_to_exec).collect(),
    };
    let bytes = serde_json::to_vec(&payload)
        .with_context(|| format!("source {source_id} send: encode draft"))?;
    let output = run_exec(source_id, cmd, args, dir, "send", Some(&bytes))?;
    if !output.status.success() {
        return Err(exec_failed(source_id, "send", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        anyhow::bail!("source {source_id} send: missing foreign_id");
    }
    let got: ExecSendJson = serde_json::from_str(trimmed)
        .with_context(|| format!("source {source_id} send: invalid JSON"))?;
    if got.foreign_id.trim().is_empty() {
        anyhow::bail!("source {source_id} send: missing foreign_id");
    }
    Ok(SendResult {
        foreign_id: got.foreign_id,
        start: empty_to_none(got.start),
        end: empty_to_none(got.end),
    })
}
