//! `paddock-slack`: Slack through the `slackcli` CLI (slackcli.dev). It
//! signs in with your browser session, so there is no Slack app to register
//! and no admin to ask; this plugin reads and sends through it.
//!
//! ```toml
//! [[source]]
//! id = "slack"
//! kind = "slack"                  # resolves to `paddock-slack` on PATH
//! # cmd = "slackcli"              # the CLI, if not on PATH by that name
//! # workspace = "acme"            # one of the enrolled workspaces
//! # channels = ["incidents", "C0123"]   # by name or id; default: everything you are in
//! # types = "public_channel,private_channel,mpim,im"
//! # limit = 100                   # newest messages per conversation
//! # since = "7d"                  # how far back a pull looks
//! # unread = true                 # ask which conversations have unreads; the rest count as read
//! # files = true                  # download attached files into the cache
//! # url = "https://acme.slack.com"   # makes every item's href a permalink
//! ```
//!
//! A channel or group DM is the room; a DM is a thread with the other
//! person as `to`. A Slack thread is the thread, and a reply cites its
//! parent. `<@U…>` in the text becomes the person's name and a mention
//! cite. Sending goes to the draft's first `to` (a channel or user id),
//! else into the thread it replies in.

use anyhow::{Context, Result};
use paddock_protocol::{
    cache_dir, cannot_send, emit_items, emit_sent, now_rfc3339, run_json, safe_name, verb, Actor,
    Draft, Item, Message, Request, Room, Sent,
};
use serde_json::Value;
use std::collections::HashMap;

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
        other => anyhow::bail!("usage: paddock-slack pull|send (got `{other}`)"),
    }
}

fn cli(r: &Request) -> String {
    r.setting("cmd").unwrap_or_else(|| "slackcli".into())
}

/// `--json`, and `--workspace` when set: slackcli takes them on any command.
fn base(r: &Request) -> Vec<String> {
    let mut args = vec!["--json".to_string()];
    if let Some(w) = r.setting("workspace") {
        args.push(format!("--workspace={w}"));
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

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Who is who: user id to a display name, from the `users` arrays every
/// slackcli answer carries.
#[derive(Default)]
struct People(HashMap<String, String>);

impl People {
    fn learn(&mut self, reply: &Value) {
        for u in reply
            .get("users")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = text(u, "id") {
                let name = text(u, "real_name").or_else(|| text(u, "name"));
                if let Some(name) = name {
                    self.0.insert(id, name);
                }
            }
        }
    }

    fn actor(&self, id: &str) -> Actor {
        Actor {
            id: id.to_string(),
            name: self.0.get(id).cloned(),
            kind: None,
        }
    }
}

/// One conversation as slackcli lists it.
struct Conversation {
    id: String,
    name: Option<String>,
    /// The other person, for a DM.
    user: Option<String>,
    im: bool,
}

fn conversation(v: &Value) -> Option<Conversation> {
    Some(Conversation {
        id: text(v, "id")?,
        name: text(v, "name"),
        user: text(v, "user"),
        im: flag(v, "is_im"),
    })
}

fn pull(r: &Request) -> Result<Vec<Item>> {
    let cmd = cli(r);
    let mut people = People::default();

    let mut args = base(r);
    args.extend([
        "conversations".into(),
        "list".into(),
        "--limit=500".into(),
        "--exclude-archived".into(),
    ]);
    if let Some(types) = r.setting("types") {
        args.push(format!("--types={types}"));
    }
    let listed = run_json(&cmd, &args).context("slackcli conversations list")?;
    people.learn(&listed);
    let wanted: Vec<String> = r
        .settings
        .get("channels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim_start_matches('#').to_string())
                .collect()
        })
        .unwrap_or_default();
    let conversations: Vec<Conversation> = listed
        .get("conversations")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(conversation).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            wanted.is_empty()
                || wanted
                    .iter()
                    .any(|w| *w == c.id || c.name.as_deref() == Some(w.as_str()))
        })
        .collect();

    // Slack has no per-message read state, but it knows which conversations
    // have unreads. Everything elsewhere counts as read.
    let unread: Option<Vec<String>> = if r.setting("unread").as_deref() != Some("false") {
        let mut args = base(r);
        args.extend(["conversations".into(), "unread".into()]);
        run_json(&cmd, &args).ok().map(|v| {
            v.get("unread_channels")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| text(c, "id")).collect())
                .unwrap_or_default()
        })
    } else {
        None
    };

    let oldest = since(r.setting("since").as_deref().unwrap_or("7d"));
    let limit = r.number("limit").unwrap_or(100);
    let cache = cache_dir(r)?;
    let download = r.flag("files");
    let url = r.setting("url");
    let mut items = Vec::new();
    for c in conversations {
        let mut args = base(r);
        args.extend([
            "conversations".into(),
            "read".into(),
            c.id.clone(),
            format!("--limit={limit}"),
        ]);
        if let Some(oldest) = oldest {
            args.push(format!("--oldest={oldest}"));
        }
        let read = match run_json(&cmd, &args) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("slack: {}: {e}", c.id);
                continue;
            }
        };
        people.learn(&read);
        let seen = unread.as_ref().map(|u| !u.contains(&c.id));
        for m in read
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let fetch = |file: &Value| files(&cmd, r, &c.id, file, &cache, download);
            if let Some(it) = item(&c, m, &people, seen, url.as_deref(), fetch) {
                items.push(it);
            }
        }
    }
    Ok(items)
}

/// `7d`, `12h`, `2w`, or a `YYYY-MM-DD`, as a unix timestamp for `--oldest`.
fn since(text: &str) -> Option<u64> {
    let text = text.trim();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if let Some((y, rest)) = text.split_once('-') {
        let (mo, d) = rest.split_once('-')?;
        let (y, mo, d): (i64, i64, i64) = (y.parse().ok()?, mo.parse().ok()?, d.parse().ok()?);
        return Some(days_from_civil(y, mo, d).checked_mul(86_400)? as u64);
    }
    let (n, unit) = text.split_at(text.len().checked_sub(1)?);
    let n: u64 = n.parse().ok()?;
    let secs = match unit {
        "h" => n * 3600,
        "d" => n * 86_400,
        "w" => n * 7 * 86_400,
        _ => return None,
    };
    Some(now.saturating_sub(secs))
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `1234567890.123456` as RFC3339, to the second.
fn when(ts: &str) -> Option<String> {
    let secs: u64 = ts.split('.').next()?.parse().ok()?;
    let days = (secs / 86_400) as i64;
    let (h, mi, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    Some(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"))
}

/// A message's stable name: channel and timestamp, since a `ts` repeats
/// across channels.
fn foreign(channel: &str, ts: &str) -> String {
    format!("{channel}:{ts}")
}

/// `https://team.slack.com/archives/C…/p1234567890123456`.
fn permalink(url: &str, channel: &str, ts: &str) -> String {
    format!(
        "{}/archives/{channel}/p{}",
        url.trim_end_matches('/'),
        ts.replace('.', "")
    )
}

/// One slackcli message as an item. `fetch` brings a file into the cache,
/// or does not.
fn item(
    c: &Conversation,
    m: &Value,
    people: &People,
    seen: Option<bool>,
    url: Option<&str>,
    fetch: impl Fn(&Value) -> Option<String>,
) -> Option<Item> {
    let ts = text(m, "ts")?;
    let (body, mentions) = unescape(&text(m, "text").unwrap_or_default(), people);
    let files: Vec<&Value> = m
        .get("files")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if body.trim().is_empty() && files.is_empty() {
        return None;
    }
    let from = text(m, "user").map(|u| people.actor(&u)).or_else(|| {
        text(m, "bot_id").map(|b| Actor {
            id: b,
            name: None,
            kind: Some("agent".into()),
        })
    });
    let room = (!c.im).then(|| Room {
        id: c.id.clone(),
        name: c.name.as_ref().map(|n| format!("#{n}")),
        kind: "group".into(),
    });
    let to = if c.im {
        let other = c.user.clone().unwrap_or_else(|| c.id.clone());
        vec![Actor {
            id: c.id.clone(),
            name: people.0.get(&other).cloned().or_else(|| c.name.clone()),
            kind: None,
        }]
    } else {
        Vec::new()
    };
    let thread_ts = text(m, "thread_ts");
    let reply_to = thread_ts
        .as_ref()
        .filter(|t| **t != ts)
        .map(|t| foreign(&c.id, t));
    let attachments = files
        .iter()
        .filter_map(|f| {
            let path = fetch(f)?;
            let mime = text(f, "mimetype").unwrap_or_else(|| "application/octet-stream".into());
            Some((path, mime))
        })
        .collect();
    let mut it: Item = Message {
        id: foreign(&c.id, &ts),
        room,
        from,
        to,
        subject: None,
        text: body,
        href: url.map(|u| permalink(u, &c.id, &ts)),
        reply_to,
        mentions,
        attachments,
        at: when(&ts),
        seen,
    }
    .into();
    it.thread = match thread_ts {
        Some(t) => Some(foreign(&c.id, &t)),
        None if c.im => Some(c.id.clone()),
        None => None,
    };
    // A file with no comment still reads as something.
    if it.title == "untitled" {
        if let Some(name) = files
            .first()
            .and_then(|f| text(f, "name").or_else(|| text(f, "title")))
        {
            it.title = name;
            if it.body.is_empty() {
                it.body = it.title.clone();
            }
        }
    }
    Some(it)
}

/// Slack's `<@U…>`, `<#C…|name>`, `<url|label>`, and HTML escapes, as text;
/// every `<@U…>` is also a mention.
fn unescape(text: &str, people: &People) -> (String, Vec<Actor>) {
    let mut out = String::with_capacity(text.len());
    let mut mentions: Vec<Actor> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = &rest[start + 1..start + end];
        let (target, label) = inner.split_once('|').unwrap_or((inner, ""));
        match target.chars().next() {
            Some('@') => {
                let id = &target[1..];
                let actor = people.actor(id);
                let name = actor.name.clone().unwrap_or_else(|| id.to_string());
                out.push('@');
                out.push_str(if label.is_empty() { &name } else { label });
                if !mentions.iter().any(|a| a.id == actor.id) {
                    mentions.push(actor);
                }
            }
            Some('#') => {
                out.push('#');
                out.push_str(if label.is_empty() {
                    &target[1..]
                } else {
                    label
                });
            }
            Some('!') => out.push_str(&format!("@{}", &target[1..])),
            _ => {
                out.push_str(if label.is_empty() { target } else { label });
                if !label.is_empty() {
                    out.push_str(&format!(" ({target})"));
                }
            }
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    (out, mentions)
}

/// `slackcli files download` into the cache, when asked to. A file already
/// there is reused; slackcli refuses to overwrite and so do we.
fn files(
    cmd: &str,
    r: &Request,
    channel: &str,
    file: &Value,
    cache: &std::path::Path,
    download: bool,
) -> Option<String> {
    if !download {
        return None;
    }
    let id = text(file, "id")?;
    let name = text(file, "name")
        .or_else(|| text(file, "title"))
        .unwrap_or_else(|| "file".into());
    let dir = cache.join(safe_name(channel));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}-{}", safe_name(&id), safe_name(&name)));
    if !path.exists() {
        let mut args = base(r);
        args.extend([
            "files".into(),
            "download".into(),
            id,
            "--output".into(),
            path.display().to_string(),
        ]);
        paddock_protocol::run(cmd, &args).ok()?;
    }
    Some(path.display().to_string())
}

fn send(r: &Request, draft: &Draft) -> Result<Sent> {
    // A thread key is `channel:thread_ts`; a recipient is a channel or user id.
    let in_thread = draft
        .thread
        .as_deref()
        .and_then(|t| t.split_once(':'))
        .map(|(c, ts)| (c.to_string(), ts.to_string()));
    let recipient = draft
        .to
        .first()
        .map(|a| a.id.clone())
        .or_else(|| in_thread.as_ref().map(|(c, _)| c.clone()))
        .or_else(|| draft.thread.clone())
        .context("slack send needs a recipient (--to a channel or user id), or a reply")?;
    let mut args = base(r);
    args.extend([
        "messages".into(),
        "send".into(),
        format!("--recipient-id={recipient}"),
    ]);
    let body = if draft.body.trim().is_empty() {
        draft.title.clone()
    } else {
        draft.body.clone()
    };
    args.push(format!("--message={body}"));
    if let Some((_, ts)) = &in_thread {
        args.push(format!("--thread-ts={ts}"));
    }
    if let Some(path) = draft.parts.iter().find_map(|p| p.path.clone()) {
        args.push(format!("--file={path}"));
    }
    let reply = run_json(&cli(r), &args).context("slackcli messages send")?;
    let channel = text(&reply, "channel_id").unwrap_or(recipient);
    let foreign_id = match (text(&reply, "ts"), text(&reply, "file_id")) {
        (Some(ts), _) => foreign(&channel, &ts),
        (None, Some(f)) => format!("{channel}:file:{f}"),
        (None, None) => anyhow::bail!("slackcli messages send: no ts in reply"),
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

    fn people() -> People {
        let mut p = People::default();
        p.learn(&serde_json::json!({"users": [
            {"id": "U1", "name": "ada", "real_name": "Ada Lovelace"},
            {"id": "U2", "name": "bo"}
        ]}));
        p
    }

    #[test]
    fn slack_markup_becomes_text_and_mentions() {
        let (t, m) = unescape(
            "hi <@U1> and <@U2|bob>, see <#C9|dev> and <https://x.y|the doc> &amp; <!here>",
            &people(),
        );
        assert_eq!(
            t,
            "hi @Ada Lovelace and @bob, see #dev and the doc (https://x.y) & @here"
        );
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(m[1].id, "U2");
        assert_eq!(
            when("1741356241.004500").as_deref(),
            Some("2025-03-07T14:04:01Z")
        );
        assert_eq!(since("2025-03-07"), Some(1_741_305_600));
        assert!(since("soon").is_none());
    }

    #[test]
    fn a_channel_message_is_in_a_room_and_a_reply_cites_its_parent() {
        let c = Conversation {
            id: "C1".into(),
            name: Some("dev".into()),
            user: None,
            im: false,
        };
        let parent = serde_json::json!({"ts": "100.1", "user": "U1", "text": "deploy failed"});
        let reply = serde_json::json!({"ts": "100.2", "thread_ts": "100.1", "user": "U2", "text": "<@U1> on it",
            "files": [{"id": "F1", "name": "log.txt", "mimetype": "text/plain"}]});
        let p = people();
        let it = item(
            &c,
            &parent,
            &p,
            Some(false),
            Some("https://acme.slack.com/"),
            |_| None,
        )
        .unwrap();
        assert_eq!(it.foreign_id, "C1:100.1");
        assert_eq!(it.title, "deploy failed");
        assert_eq!(it.thread, None, "a top-level channel message stands alone");
        assert_eq!(it.to[0].kind.as_deref(), Some("group"));
        assert_eq!(it.to[0].name.as_deref(), Some("#dev"));
        assert_eq!(
            it.from.as_ref().unwrap().name.as_deref(),
            Some("Ada Lovelace")
        );
        assert_eq!(
            it.href.as_deref(),
            Some("https://acme.slack.com/archives/C1/p1001")
        );
        assert_eq!(it.read, Some(false));
        let it = item(&c, &reply, &p, None, None, |f| {
            Some(format!("/cache/{}", f["name"].as_str().unwrap()))
        })
        .unwrap();
        assert_eq!(it.thread.as_deref(), Some("C1:100.1"));
        assert_eq!(it.cites[0].kind, "reply");
        assert_eq!(it.cites[0].foreign_id.as_deref(), Some("C1:100.1"));
        assert_eq!(it.cites[1].kind, "mention");
        assert_eq!(it.body, "@Ada Lovelace on it");
        assert_eq!(it.parts[0].path.as_deref(), Some("/cache/log.txt"));
        assert_eq!(it.parts[0].mime, "text/plain");
        assert_eq!(it.read, None);
    }

    #[test]
    fn a_dm_is_one_thread_with_the_other_person_as_to() {
        let c = Conversation {
            id: "D1".into(),
            name: None,
            user: Some("U1".into()),
            im: true,
        };
        let m = serde_json::json!({"ts": "5.5", "user": "U1", "text": "lunch?"});
        let it = item(&c, &m, &people(), Some(true), None, |_| None).unwrap();
        assert_eq!(it.thread.as_deref(), Some("D1"));
        assert!(it.to.iter().all(|a| a.kind.is_none()));
        assert_eq!(it.to[0].name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(it.read, Some(true));
        let empty = serde_json::json!({"ts": "5.6", "user": "U1", "text": ""});
        assert!(item(&c, &empty, &people(), None, None, |_| None).is_none());
        let only_file = serde_json::json!({"ts": "5.7", "user": "U1", "text": "", "files": [{"id": "F2", "name": "pic.png", "mimetype": "image/png"}]});
        let it = item(&c, &only_file, &people(), None, None, |_| None).unwrap();
        assert_eq!(it.title, "pic.png");
    }

    #[test]
    fn pull_and_send_drive_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("slackcli");
        std::fs::write(
            &fake,
            r#"#!/bin/sh
case "$*" in
  *"conversations list"*) echo '{"conversations":[{"id":"C1","name":"dev","is_channel":true},{"id":"C2","name":"random","is_channel":true},{"id":"D1","user":"U1","is_im":true}],"users":[{"id":"U1","name":"ada","real_name":"Ada"}],"next_cursor":null}' ;;
  *"conversations unread"*) echo '{"unread_channels":[{"id":"C1","name":"dev","unread_count":1,"mention_count":0}]}' ;;
  *"conversations read C1"*) echo '{"channel_id":"C1","messages":[{"ts":"1.1","user":"U1","text":"hello <@U1>"}],"users":[]}' ;;
  *"conversations read D1"*) echo '{"channel_id":"D1","messages":[{"ts":"2.2","user":"U1","text":"psst"}],"users":[]}' ;;
  *"messages send"*) case "$*" in *"--thread-ts=1.1"*) echo '{"channel_id":"C1","ts":"1.9","permalink":"https://acme.slack.com/archives/C1/p19"}' ;; *) echo '{"channel_id":"D1","ts":"3.3"}' ;; esac ;;
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
            id: "slack".into(),
            ..Default::default()
        };
        r.settings
            .insert("cmd".into(), Value::String(fake.display().to_string()));
        r.settings.insert(
            "cache".into(),
            Value::String(tmp.path().display().to_string()),
        );
        r.settings
            .insert("channels".into(), serde_json::json!(["#dev", "D1"]));
        let items = pull(&r).unwrap();
        assert_eq!(items.len(), 2, "random was not asked for");
        assert_eq!(items[0].foreign_id, "C1:1.1");
        assert_eq!(items[0].read, Some(false), "dev has unreads");
        assert_eq!(items[0].body, "hello @Ada");
        assert_eq!(items[1].foreign_id, "D1:2.2");
        assert_eq!(items[1].read, Some(true), "the DM has none");

        let reply = send(
            &r,
            &Draft {
                body: "on it".into(),
                thread: Some("C1:1.1".into()),
                reply_to_foreign: Some("C1:1.1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.foreign_id, "C1:1.9");
        let dm = send(
            &r,
            &Draft {
                title: "hi".into(),
                to: vec![Actor {
                    id: "U1".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(dm.foreign_id, "D1:3.3");
        assert!(send(&r, &Draft::default()).is_err(), "no recipient");
    }
}
