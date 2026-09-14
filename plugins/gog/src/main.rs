//! `paddock-gog`: Gmail through the `gog` CLI (github.com/openclaw/gogcli).
//! `gog` holds the Google login; this plugin searches with it and sends
//! through it.
//!
//! ```toml
//! [[source]]
//! id = "gmail"
//! kind = "gog"                    # resolves to `paddock-gog` on PATH
//! # cmd = "gog"                   # the CLI, if not on PATH by that name
//! # account = "me@gmail.com"      # which gog account
//! # query = "newer_than:7d"       # Gmail search syntax
//! # max = 50                      # messages per pull
//! # attachments = true            # download attachments into the cache
//! ```
//!
//! Gmail's thread is the thread. A message is read unless Gmail says
//! `UNREAD`. Sending replies through `gog gmail reply` when the draft
//! answers a message, else `gog gmail send` to the draft's `to`.

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
        other => anyhow::bail!("usage: paddock-gog pull|send (got `{other}`)"),
    }
}

fn cli(r: &Request) -> String {
    r.setting("cmd").unwrap_or_else(|| "gog".into())
}

/// `--json`, `--no-input`, and `--account` when set: gog's global flags.
fn base(r: &Request) -> Vec<String> {
    let mut args = vec!["--json".to_string(), "--no-input".to_string()];
    if let Some(a) = r.setting("account") {
        args.extend(["--account".into(), a]);
    }
    args
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn pull(r: &Request) -> Result<Vec<Item>> {
    let cmd = cli(r);
    let mut args = base(r);
    args.extend([
        "gmail".into(),
        "messages".into(),
        "search".into(),
        r.setting("query").unwrap_or_else(|| "newer_than:7d".into()),
        "--max".into(),
        r.number("max").unwrap_or(50).to_string(),
        "--include-body".into(),
        "--include-attachments".into(),
    ]);
    let reply = run_json(&cmd, &args).context("gog gmail messages search")?;
    let cache = cache_dir(r)?;
    let download = r.flag("attachments");
    let messages = reply
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(messages
        .iter()
        .filter_map(|m| item(m, |id, att| attachment(&cmd, r, id, att, &cache, download)))
        .collect())
}

/// One gog message as an item. `fetch` brings an attachment into the
/// cache, or does not.
fn item(m: &Value, fetch: impl Fn(&str, &Value) -> Option<String>) -> Option<Item> {
    let id = text(m, "id")?;
    let thread = text(m, "threadId");
    let labels: Vec<String> = m
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let attachments = m
        .get("attachments")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|att| {
                    let path = fetch(&id, att)?;
                    let mime =
                        text(att, "mimeType").unwrap_or_else(|| "application/octet-stream".into());
                    Some((path, mime))
                })
                .collect()
        })
        .unwrap_or_default();
    let msg = Message {
        id,
        room: None,
        from: text(m, "from").and_then(|f| Actor::mailbox(&f)),
        to: text(m, "to")
            .map(|t| Actor::mailboxes(&t))
            .unwrap_or_default(),
        subject: text(m, "subject"),
        text: text(m, "body")
            .or_else(|| text(m, "snippet"))
            .unwrap_or_default(),
        href: thread
            .as_ref()
            .map(|t| format!("https://mail.google.com/mail/#all/{t}")),
        reply_to: None,
        mentions: Vec::new(),
        attachments,
        at: text(m, "internalDateIso").or_else(|| text(m, "date")),
        seen: Some(!labels.iter().any(|l| l == "UNREAD")),
    };
    let mut it: Item = msg.into();
    it.thread = thread;
    Some(it)
}

/// `gog gmail attachment` into the cache, when asked to.
fn attachment(
    cmd: &str,
    r: &Request,
    message_id: &str,
    att: &Value,
    cache: &std::path::Path,
    download: bool,
) -> Option<String> {
    if !download {
        return None;
    }
    let att_id = text(att, "attachmentId")?;
    let dir = cache.join(paddock_protocol::safe_name(message_id));
    std::fs::create_dir_all(&dir).ok()?;
    let mut args = base(r);
    args.extend([
        "gmail".into(),
        "attachment".into(),
        message_id.into(),
        att_id,
        "--out".into(),
        dir.display().to_string(),
    ]);
    if let Some(name) = text(att, "filename") {
        args.extend(["--name".into(), paddock_protocol::safe_name(&name)]);
    }
    let reply = run_json(cmd, &args).ok()?;
    text(&reply, "path")
}

fn send(r: &Request, draft: &Draft) -> Result<Sent> {
    let mut args = base(r);
    args.push("gmail".into());
    match &draft.reply_to_foreign {
        Some(id) => {
            args.extend([
                "reply".into(),
                id.clone(),
                "--body".into(),
                draft.body.clone(),
            ]);
            for a in &draft.to {
                args.extend(["--to".into(), a.id.clone()]);
            }
        }
        None => {
            if draft.to.is_empty() {
                anyhow::bail!("gog send needs a recipient (--to)");
            }
            let to: Vec<String> = draft.to.iter().map(|a| a.id.clone()).collect();
            args.extend([
                "send".into(),
                "--to".into(),
                to.join(","),
                "--subject".into(),
                draft.title.clone(),
                "--body".into(),
                draft.body.clone(),
            ]);
        }
    }
    for path in draft.parts.iter().filter_map(|p| p.path.clone()) {
        args.extend(["--attach".into(), path]);
    }
    let reply = run_json(&cli(r), &args).context("gog gmail send")?;
    let id = text(&reply, "messageId")
        .or_else(|| text(&reply, "id"))
        .context("gog gmail send: no messageId in reply")?;
    Ok(Sent {
        foreign_id: id,
        start: Some(now_rfc3339()),
        end: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gmail_message_keeps_its_thread_and_unread_means_not_read() {
        let m: Value = serde_json::json!({
            "id": "18f", "threadId": "18a", "from": "Ana <ana@example.com>", "subject": "Invoice",
            "labels": ["INBOX", "UNREAD"], "body": "please pay", "internalDateIso": "2026-09-14T10:00:00+00:00",
            "attachments": [{"filename": "inv.pdf", "mimeType": "application/pdf", "attachmentId": "att1"}]
        });
        let it = item(&m, |id, att| {
            Some(format!("/cache/{id}/{}", att["filename"].as_str().unwrap()))
        })
        .unwrap();
        assert_eq!(it.foreign_id, "18f");
        assert_eq!(it.thread.as_deref(), Some("18a"));
        assert_eq!(it.title, "Invoice");
        assert_eq!(it.from.as_ref().unwrap().id, "ana@example.com");
        assert_eq!(it.read, Some(false));
        assert_eq!(it.parts[0].path.as_deref(), Some("/cache/18f/inv.pdf"));
        assert_eq!(it.parts[0].mime, "application/pdf");
        assert!(it.href.as_deref().unwrap().ends_with("18a"));
        let read: Value = serde_json::json!({"id": "x", "labels": ["INBOX"], "snippet": "hi"});
        let it = item(&read, |_, _| None).unwrap();
        assert_eq!(it.read, Some(true));
        assert_eq!(it.body, "hi");
        assert!(it.parts.is_empty());
    }

    #[test]
    fn pull_and_send_drive_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("gog");
        std::fs::write(
            &fake,
            r#"#!/bin/sh
case "$*" in
  *"messages search"*) echo '{"messages":[{"id":"m1","threadId":"t1","subject":"s","body":"b","labels":["INBOX"]}],"nextPageToken":""}' ;;
  *"gmail reply m1"*) echo '{"messageId":"m2","threadId":"t1"}' ;;
  *"gmail send"*) echo '{"messageId":"m3","threadId":"t3"}' ;;
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
        let mut r = Request {
            id: "g".into(),
            ..Default::default()
        };
        r.settings
            .insert("cmd".into(), Value::String(fake.display().to_string()));
        r.settings.insert(
            "cache".into(),
            Value::String(tmp.path().display().to_string()),
        );
        let items = pull(&r).unwrap();
        assert_eq!(items[0].foreign_id, "m1");
        let reply = send(
            &r,
            &Draft {
                body: "ok".into(),
                reply_to_foreign: Some("m1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.foreign_id, "m2");
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
        assert_eq!(fresh.foreign_id, "m3");
        assert!(send(&r, &Draft::default()).is_err(), "no recipient");
    }
}
