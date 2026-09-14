//! `paddock-wacli`: WhatsApp through the `wacli` CLI (github.com/openclaw/wacli).
//! `wacli` keeps its own synced store; this plugin lists what it has and
//! sends through it.
//!
//! ```toml
//! [[source]]
//! id = "wa"
//! kind = "wacli"                  # resolves to `paddock-wacli` on PATH
//! # cmd = "wacli"                 # the CLI, if not on PATH by that name
//! # account = "work"              # a named wacli account
//! # chat = "1234567890@s.whatsapp.net"   # one chat only
//! # limit = 200                   # newest messages per pull
//! # after = "2026-09-01"          # only messages after this date
//! # sync = true                   # run `wacli sync` before listing
//! # media = true                  # download media that is not on disk yet
//! ```
//!
//! A group chat is the room; a one-to-one chat is the thread and the
//! other party is `to`. A quoted message is a reply cite. Sending goes to
//! the draft's first `to`, else to the thread it replies in.

use anyhow::{Context, Result};
use paddock_protocol::{
    cache_dir, cannot_send, emit_items, emit_sent, now_rfc3339, run, run_json, verb, Actor, Draft,
    Item, Message, Request, Room, Sent,
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
        other => anyhow::bail!("usage: paddock-wacli pull|send (got `{other}`)"),
    }
}

fn cli(r: &Request) -> String {
    r.setting("cmd").unwrap_or_else(|| "wacli".into())
}

/// `--json`, and `--account` when set: wacli's global flags come first.
fn base(r: &Request) -> Vec<String> {
    let mut args = vec!["--json".to_string()];
    if let Some(a) = r.setting("account") {
        args.extend(["--account".into(), a]);
    }
    args
}

fn pull(r: &Request) -> Result<Vec<Item>> {
    let cmd = cli(r);
    if r.flag("sync") {
        let mut args = base(r);
        args.push("sync".into());
        run(&cmd, &args).context("wacli sync")?;
    }
    let mut args = base(r);
    args.extend([
        "messages".into(),
        "list".into(),
        "--limit".into(),
        r.number("limit").unwrap_or(200).to_string(),
    ]);
    if let Some(chat) = r.setting("chat") {
        args.extend(["--chat".into(), chat]);
    }
    if let Some(after) = r.setting("after") {
        args.extend(["--after".into(), after]);
    }
    let reply = run_json(&cmd, &args).context("wacli messages list")?;
    let cache = cache_dir(r)?;
    let download = r.flag("media");
    Ok(messages(&reply)
        .iter()
        .filter_map(|m| item(m, |chat, id| media(&cmd, r, chat, id, &cache, download)))
        .collect())
}

fn messages(reply: &Value) -> Vec<Value> {
    reply
        .get("messages")
        .or(Some(reply))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// One wacli message as an item. `fetch` finds the media file for a
/// message that has some, or none.
fn item(m: &Value, fetch: impl Fn(&str, &str) -> Option<(String, String)>) -> Option<Item> {
    if m.get("deleted_at").is_some() || m.get("Revoked").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let id = text(m, "MsgID")?;
    let chat = text(m, "ChatJID").unwrap_or_default();
    let chat_name = text(m, "ChatName");
    let from_me = m.get("FromMe").and_then(Value::as_bool).unwrap_or(false);
    let room_kind = if chat.ends_with("@g.us") {
        Some("group")
    } else if chat.ends_with("@newsletter") {
        Some("list")
    } else {
        None
    };
    let room = room_kind.map(|kind| Room {
        id: chat.clone(),
        name: chat_name.clone(),
        kind: kind.into(),
    });
    let from = match (text(m, "SenderJID"), room_kind.is_some(), from_me) {
        (Some(jid), _, _) => Actor {
            id: jid,
            name: text(m, "SenderName"),
            kind: None,
        },
        (None, false, false) => Actor {
            id: chat.clone(),
            name: chat_name.clone(),
            kind: None,
        },
        (None, _, _) => Actor {
            id: "me".into(),
            name: text(m, "SenderName"),
            kind: None,
        },
    };
    let to = if room_kind.is_none() && from_me && !chat.is_empty() {
        vec![Actor {
            id: chat.clone(),
            name: chat_name.clone(),
            kind: None,
        }]
    } else {
        Vec::new()
    };
    let body = text(m, "Text")
        .or_else(|| text(m, "MediaCaption"))
        .or_else(|| text(m, "DisplayText"))
        .unwrap_or_default();
    let media_type = text(m, "MediaType");
    let attachments = match (text(m, "LocalPath"), media_type.as_deref()) {
        (Some(path), _) => vec![(path, mime(m, media_type.as_deref()))],
        (None, Some(_)) => fetch(&chat, &id).into_iter().collect(),
        (None, None) => Vec::new(),
    };
    let msg = Message {
        id,
        room,
        from: Some(from),
        to,
        subject: None,
        text: body,
        href: None,
        reply_to: text(m, "quoted_msg_id"),
        mentions: Vec::new(),
        attachments,
        at: text(m, "Timestamp"),
        seen: from_me.then_some(true),
    };
    let mut it: Item = msg.into();
    if room_kind.is_none() && !chat.is_empty() {
        it.thread = Some(chat);
    }
    Some(it)
}

fn mime(m: &Value, media_type: Option<&str>) -> String {
    text(m, "MimeType").unwrap_or_else(|| {
        match media_type {
            Some("image") => "image/jpeg",
            Some("video") => "video/mp4",
            Some("audio") => "audio/ogg",
            _ => "application/octet-stream",
        }
        .into()
    })
}

/// `wacli media download` into the cache, when asked to.
fn media(
    cmd: &str,
    r: &Request,
    chat: &str,
    id: &str,
    cache: &std::path::Path,
    download: bool,
) -> Option<(String, String)> {
    if !download {
        return None;
    }
    let mut args = base(r);
    args.extend([
        "media".into(),
        "download".into(),
        "--chat".into(),
        chat.into(),
        "--id".into(),
        id.into(),
        "--output".into(),
        cache.display().to_string(),
    ]);
    let reply = run_json(cmd, &args).ok()?;
    let path = text(&reply, "path")?;
    let mime = text(&reply, "mime_type").unwrap_or_else(|| "application/octet-stream".into());
    Some((path, mime))
}

fn send(r: &Request, draft: &Draft) -> Result<Sent> {
    let to = draft
        .to
        .first()
        .map(|a| a.id.clone())
        .or_else(|| draft.thread.clone())
        .context("wacli send needs a recipient: --to, or a reply in a chat")?;
    let mut args = base(r);
    args.push("send".into());
    match draft.parts.iter().find_map(|p| p.path.clone()) {
        Some(file) => {
            args.extend(["file".into(), "--to".into(), to, "--file".into(), file]);
            if !draft.body.trim().is_empty() {
                args.extend(["--caption".into(), draft.body.clone()]);
            }
        }
        None => args.extend([
            "text".into(),
            "--to".into(),
            to,
            "--message".into(),
            draft.body.clone(),
        ]),
    }
    if let Some(id) = &draft.reply_to_foreign {
        args.extend(["--reply-to".into(), id.clone()]);
    }
    let reply = run_json(&cli(r), &args).context("wacli send")?;
    let id = text(&reply, "id").context("wacli send: no id in reply")?;
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
    fn a_group_message_is_a_room_and_a_quote_is_a_reply() {
        let m: Value = serde_json::json!({
            "ChatJID": "123@g.us", "ChatName": "Family", "MsgID": "ABC", "SenderJID": "55@s.whatsapp.net",
            "SenderName": "Ana", "Timestamp": "2026-09-14T10:00:00Z", "FromMe": false, "Text": "see this",
            "quoted_msg_id": "XYZ", "MediaType": "image", "LocalPath": "/tmp/a.jpg", "MimeType": "image/jpeg"
        });
        let it = item(&m, |_, _| None).unwrap();
        assert_eq!(it.foreign_id, "ABC");
        assert_eq!(it.thread.as_deref(), Some("123@g.us"));
        assert_eq!(it.to[0].kind.as_deref(), Some("group"));
        assert_eq!(it.from.as_ref().unwrap().name.as_deref(), Some("Ana"));
        assert_eq!(it.cites[0].kind, "reply");
        assert_eq!(it.cites[0].foreign_id.as_deref(), Some("XYZ"));
        assert_eq!(it.parts[0].path.as_deref(), Some("/tmp/a.jpg"));
        assert_eq!(it.read, None);
    }

    #[test]
    fn a_direct_chat_is_the_thread_and_my_messages_are_read() {
        let m: Value = serde_json::json!({
            "ChatJID": "55@s.whatsapp.net", "ChatName": "Bo", "MsgID": "M1", "FromMe": true,
            "Text": "hi", "Timestamp": "2026-09-14T10:00:00Z"
        });
        let it = item(&m, |_, _| None).unwrap();
        assert_eq!(it.thread.as_deref(), Some("55@s.whatsapp.net"));
        assert_eq!(it.to[0].id, "55@s.whatsapp.net");
        assert_eq!(it.from.as_ref().unwrap().id, "me");
        assert_eq!(it.read, Some(true));
        let theirs: Value = serde_json::json!({"ChatJID": "55@s.whatsapp.net", "ChatName": "Bo", "MsgID": "M2", "Text": "yo"});
        let it = item(&theirs, |_, _| None).unwrap();
        assert_eq!(it.from.as_ref().unwrap().id, "55@s.whatsapp.net");
        assert!(it.to.is_empty());
    }

    #[test]
    fn media_not_on_disk_is_fetched_when_asked() {
        let m: Value = serde_json::json!({"ChatJID": "1@g.us", "MsgID": "M3", "MediaType": "document", "Text": "[Document]"});
        let it = item(&m, |chat, id| {
            Some((format!("/cache/{chat}/{id}.pdf"), "application/pdf".into()))
        })
        .unwrap();
        assert_eq!(it.parts[0].path.as_deref(), Some("/cache/1@g.us/M3.pdf"));
        let deleted: Value = serde_json::json!({"ChatJID": "1@g.us", "MsgID": "M4", "deleted_at": "2026-01-01T00:00:00Z"});
        assert!(item(&deleted, |_, _| None).is_none());
    }

    /// A fake `wacli` on disk: the plugin runs the real thing the same way.
    #[test]
    fn pull_and_send_drive_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("wacli");
        std::fs::write(
            &fake,
            r#"#!/bin/sh
case "$*" in
  *"messages list"*) echo '{"messages":[{"ChatJID":"1@g.us","MsgID":"A","Text":"hello"}],"fts":true}' ;;
  *"send text"*) echo '{"sent":true,"to":"1@g.us","id":"SENT1"}' ;;
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
            id: "wa".into(),
            ..Default::default()
        };
        r.settings
            .insert("cmd".into(), Value::String(fake.display().to_string()));
        r.settings.insert(
            "cache".into(),
            Value::String(tmp.path().display().to_string()),
        );
        let items = pull(&r).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].body, "hello");
        let sent = send(
            &r,
            &Draft {
                body: "yo".into(),
                thread: Some("1@g.us".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(sent.foreign_id, "SENT1");
    }
}
