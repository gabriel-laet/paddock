//! `paddock-hey`: HEY mail through the `hey` CLI (github.com/basecamp/hey-cli).
//! `hey` holds the login; this plugin reads a box with it and replies
//! through it.
//!
//! ```toml
//! [[source]]
//! id = "hey"
//! kind = "hey"                    # resolves to `paddock-hey` on PATH
//! # cmd = "hey"                   # the CLI, if not on PATH by that name
//! # account = "12345"             # a linked account
//! # box = "imbox"                 # imbox, feed, "paper trail", or an id
//! # limit = 50                    # threads per pull
//! # bodies = true                 # read each thread; false lists postings only
//! # attachments = true            # save attachments into the cache
//! ```
//!
//! A HEY thread (its `topic_id`) is the thread; every entry in it is an
//! item, and the posting's seen state is theirs. Replying goes through
//! `hey reply` on the thread and the reply's entry is its foreign id. A
//! fresh message goes through `hey compose`, which answers no id, so the
//! sent item is named by the time it was sent and HEY's own copy arrives
//! as its own entry.

use anyhow::{Context, Result};
use paddock_protocol::{
    cache_dir, cannot_send, emit_items, emit_sent, now_rfc3339, run_json, verb, Actor, Draft, Item,
    Message, Request, Sent,
};
use serde_json::Value;

fn main() -> Result<()> {
    let request = Request::read().context("read request")?;
    match verb().as_str() {
        "pull" => {
            emit_items(&pull(&request)?)?;
            Ok(())
        }
        "send" => match request.draft.clone() {
            Some(draft) => {
                emit_sent(&send(&request, &draft)?)?;
                Ok(())
            }
            None => cannot_send(),
        },
        other => anyhow::bail!("usage: paddock-hey pull|send (got `{other}`)"),
    }
}

fn cli(r: &Request) -> String {
    r.setting("cmd").unwrap_or_else(|| "hey".into())
}

/// `--json`, and `--account` when set.
fn base(r: &Request) -> Vec<String> {
    let mut args = vec!["--json".to_string()];
    if let Some(a) = r.setting("account") {
        args.extend(["--account".into(), a]);
    }
    args
}

fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `hey` wraps every answer in `{ok, data, ...}`.
fn data(reply: &Value) -> Value {
    reply.get("data").cloned().unwrap_or(Value::Null)
}

fn contact(v: &Value) -> Option<Actor> {
    let id = text(v, "email_address")?;
    Some(Actor {
        id: id.clone(),
        name: text(v, "name").filter(|n| *n != id),
        kind: None,
    })
}

struct Posting {
    topic: String,
    subject: Option<String>,
    summary: String,
    from: Option<Actor>,
    at: Option<String>,
    seen: Option<bool>,
    href: Option<String>,
    has_attachments: bool,
}

fn posting(p: &Value) -> Option<Posting> {
    Some(Posting {
        topic: text(p, "topic_id")?,
        subject: text(p, "name"),
        summary: text(p, "summary").unwrap_or_default(),
        from: p.get("creator").and_then(contact),
        at: text(p, "active_at").or_else(|| text(p, "created_at")),
        seen: p.get("seen").and_then(Value::as_bool),
        href: text(p, "app_url"),
        has_attachments: p
            .get("includes_attachments")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn pull(r: &Request) -> Result<Vec<Item>> {
    let cmd = cli(r);
    let mut args = base(r);
    args.extend([
        "box".into(),
        "view".into(),
        r.setting("box").unwrap_or_else(|| "imbox".into()),
        "--limit".into(),
        r.number("limit").unwrap_or(50).to_string(),
    ]);
    let reply = run_json(&cmd, &args).context("hey box view")?;
    let postings: Vec<Posting> = data(&reply)
        .get("postings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(posting).collect())
        .unwrap_or_default();
    let bodies = r.setting("bodies").as_deref() != Some("false");
    let cache = cache_dir(r)?;
    let mut items = Vec::new();
    for p in postings {
        if !bodies {
            items.push(thread_item(&p));
            continue;
        }
        let entries =
            thread(&cmd, r, &p.topic).with_context(|| format!("hey thread read {}", p.topic))?;
        let files = if r.flag("attachments") && p.has_attachments {
            attachments(&cmd, r, &p.topic, &cache)
        } else {
            Vec::new()
        };
        items.extend(entries.iter().filter_map(|e| entry_item(&p, e, &files)));
    }
    Ok(items)
}

/// The thread's entries, oldest first.
fn thread(cmd: &str, r: &Request, topic: &str) -> Result<Vec<Value>> {
    let mut args = base(r);
    args.extend([
        "thread".into(),
        "read".into(),
        topic.into(),
        "--allow-partial".into(),
    ]);
    Ok(data(&run_json(cmd, &args)?)
        .as_array()
        .cloned()
        .unwrap_or_default())
}

/// A posting alone, when bodies are not read: the thread as one item.
fn thread_item(p: &Posting) -> Item {
    let mut it: Item = Message {
        id: format!("topic-{}", p.topic),
        room: None,
        from: p.from.clone(),
        to: Vec::new(),
        subject: p.subject.clone(),
        text: p.summary.clone(),
        href: p.href.clone(),
        reply_to: None,
        mentions: Vec::new(),
        attachments: Vec::new(),
        at: p.at.clone(),
        seen: p.seen,
    }
    .into();
    it.thread = Some(p.topic.clone());
    it
}

/// One entry of a thread as an item, with the files saved for it.
fn entry_item(p: &Posting, e: &Value, files: &[(String, String, String)]) -> Option<Item> {
    let id = text(e, "id")?;
    let body = match e.get("body") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => text(e, "summary").unwrap_or_default(),
    };
    let mut it: Item = Message {
        id: id.clone(),
        room: None,
        from: e
            .get("creator")
            .and_then(contact)
            .or_else(|| p.from.clone()),
        to: Vec::new(),
        subject: p.subject.clone(),
        text: body,
        href: text(e, "app_url").or_else(|| p.href.clone()),
        reply_to: None,
        mentions: Vec::new(),
        attachments: files
            .iter()
            .filter(|(entry, _, _)| *entry == id)
            .map(|(_, path, mime)| (path.clone(), mime.clone()))
            .collect(),
        at: text(e, "created_at").or_else(|| p.at.clone()),
        seen: p.seen,
    }
    .into();
    it.thread = Some(p.topic.clone());
    Some(it)
}

/// Every attachment of a thread saved into the cache: (entry id, path, mime).
fn attachments(
    cmd: &str,
    r: &Request,
    topic: &str,
    cache: &std::path::Path,
) -> Vec<(String, String, String)> {
    let mut args = base(r);
    args.extend(["attachment".into(), "list".into(), topic.into()]);
    let Ok(reply) = run_json(cmd, &args) else {
        return Vec::new();
    };
    let dir = cache.join(paddock_protocol::safe_name(topic));
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    data(&reply)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|a| {
            let id = text(a, "id")?;
            let entry = text(a, "message_id")?;
            let mut args = base(r);
            args.extend([
                "attachment".into(),
                "save".into(),
                id,
                "--output".into(),
                dir.display().to_string(),
                "--force".into(),
            ]);
            let saved = data(&run_json(cmd, &args).ok()?);
            let path = text(&saved, "path")?;
            let mime = text(a, "content_type").unwrap_or_else(|| "application/octet-stream".into());
            Some((entry, path, mime))
        })
        .collect()
}

fn send(r: &Request, draft: &Draft) -> Result<Sent> {
    let cmd = cli(r);
    let mut args = base(r);
    let topic = draft.thread.clone().filter(|t| !t.is_empty());
    match &topic {
        Some(topic) => args.extend([
            "reply".into(),
            topic.clone(),
            "-m".into(),
            draft.body.clone(),
        ]),
        None => {
            if draft.to.is_empty() {
                anyhow::bail!("hey send needs a thread to reply in, or a recipient (--to)");
            }
            args.extend([
                "compose".into(),
                "--subject".into(),
                draft.title.clone(),
                "-m".into(),
                draft.body.clone(),
            ]);
            for a in &draft.to {
                args.extend(["--to".into(), a.id.clone()]);
            }
        }
    }
    for path in draft.parts.iter().filter_map(|p| p.path.clone()) {
        args.extend(["--attach".into(), path]);
    }
    run_json(&cmd, &args).context("hey send")?;
    let foreign_id = match &topic {
        Some(topic) => thread(&cmd, r, topic)
            .ok()
            .and_then(|entries| entries.last().and_then(|e| text(e, "id")))
            .unwrap_or_else(|| format!("sent-{}", now_rfc3339())),
        None => format!("sent-{}", now_rfc3339()),
    };
    Ok(Sent {
        foreign_id,
        start: Some(now_rfc3339()),
        end: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_hey(dir: &std::path::Path) -> std::path::PathBuf {
        let fake = dir.join("hey");
        std::fs::write(
            &fake,
            r#"#!/bin/sh
case "$*" in
  *"box view"*) echo '{"ok":true,"data":{"id":1,"name":"Imbox","postings":[{"id":10,"topic_id":500,"name":"Lunch?","summary":"Thursday?","seen":false,"creator":{"name":"Ana","email_address":"ana@example.com"},"created_at":"2026-09-14T10:00:00Z","app_url":"https://app.hey.com/topics/500","includes_attachments":true}],"total_count":1}}' ;;
  *"thread read 500"*) echo '{"ok":true,"data":[{"id":900,"created_at":"2026-09-14T10:00:00Z","creator":{"name":"Ana","email_address":"ana@example.com"},"summary":"Thursday?","kind":"email","app_url":"https://app.hey.com/entries/900","body":"Thursday at noon?"},{"id":901,"created_at":"2026-09-14T10:05:00Z","creator":{"name":"Me","email_address":"me@hey.com"},"body":"works"}]}' ;;
  *"attachment list 500"*) echo '{"ok":true,"data":[{"id":"900:1","message_id":900,"filename":"menu.pdf","content_type":"application/pdf"}]}' ;;
  *"attachment save 900:1"*) echo '{"ok":true,"data":{"id":"900:1","filename":"menu.pdf","path":"/cache/500/menu.pdf","byte_size":3}}' ;;
  *"reply 500"*) echo '{"ok":true,"summary":"Reply sent"}' ;;
  *"compose"*) echo '{"ok":true,"summary":"Message sent"}' ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Another test's spawn may still hold the file open across its fork;
        // exec fails with "text file busy" until it has exec'd. Wait it out.
        for _ in 0..100 {
            match std::process::Command::new(&fake).arg("--probe").output() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                _ => break,
            }
        }
        fake
    }

    fn request(tmp: &std::path::Path, fake: &std::path::Path) -> Request {
        let mut r = Request {
            id: "hey".into(),
            ..Default::default()
        };
        r.settings
            .insert("cmd".into(), Value::String(fake.display().to_string()));
        r.settings
            .insert("cache".into(), Value::String(tmp.display().to_string()));
        r
    }

    #[test]
    fn every_entry_of_a_thread_is_an_item_on_that_thread() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake_hey(tmp.path());
        let mut r = request(tmp.path(), &fake);
        r.settings.insert("attachments".into(), Value::Bool(true));
        let items = pull(&r).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].foreign_id, "900");
        assert_eq!(items[0].thread.as_deref(), Some("500"));
        assert_eq!(items[0].title, "Lunch?");
        assert_eq!(items[0].body, "Thursday at noon?");
        assert_eq!(items[0].from.as_ref().unwrap().id, "ana@example.com");
        assert_eq!(items[0].read, Some(false));
        assert_eq!(
            items[0].parts[0].path.as_deref(),
            Some("/cache/500/menu.pdf")
        );
        assert_eq!(items[1].foreign_id, "901");
        assert!(items[1].parts.is_empty());
        assert_eq!(
            items[1].href.as_deref(),
            Some("https://app.hey.com/topics/500")
        );
    }

    #[test]
    fn without_bodies_a_thread_is_one_item() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake_hey(tmp.path());
        let mut r = request(tmp.path(), &fake);
        r.settings.insert("bodies".into(), Value::Bool(false));
        let items = pull(&r).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].foreign_id, "topic-500");
        assert_eq!(items[0].body, "Thursday?");
    }

    #[test]
    fn a_reply_is_named_by_its_entry_and_a_compose_by_its_time() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = fake_hey(tmp.path());
        let r = request(tmp.path(), &fake);
        let reply = send(
            &r,
            &Draft {
                body: "ok".into(),
                thread: Some("500".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.foreign_id, "901");
        let fresh = send(
            &r,
            &Draft {
                title: "hi".into(),
                body: "there".into(),
                to: vec![Actor {
                    id: "bo@example.com".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(fresh.foreign_id.starts_with("sent-"));
        assert!(send(&r, &Draft::default()).is_err());
    }
}
