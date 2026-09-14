//! The exec protocol: what paddock and a plugin say to each other.
//!
//! The host runs `{cmd} {args...} VERB` and writes a JSON [`Request`] on
//! stdin. For `pull`, stdout is a JSON array (or NDJSON) of [`Item`]s. For
//! `send`, the request carries a [`Draft`] and stdout is a [`Sent`]. A
//! source that cannot send exits 2 or prints `source cannot send`.
//!
//! Every field except `foreign_id` is optional; the host defaults what is
//! missing. Unknown fields are ignored, so a plugin may emit more than the
//! host reads. See `PROTOCOL.md` at the repository root.
//!
//! [`Message`] is the shape most sources actually have, mail and chat
//! alike: someone sent something to someone, maybe in reply, maybe with
//! attachments. It lowers to an [`Item`] one way, so every plugin makes
//! the same choices about threads, cites, and actors.

use serde::{Deserialize, Serialize};

/// What the host writes on stdin for every verb.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Request {
    /// The source's id in the host's config.
    #[serde(default)]
    pub id: String,
    /// The source's settings from the config, beyond what the host reads
    /// (`id`, `kind`, `name`, `forget_after`). A `url`, a `path`, an account.
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Present for `send`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Draft>,
}

impl Request {
    /// Read the request from stdin. An empty stdin is an empty request.
    pub fn read() -> Result<Self, serde_json::Error> {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text);
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&text)
    }

    /// A string setting, trimmed; missing or empty is `None`. A number or a
    /// bool reads as its text.
    pub fn setting(&self, key: &str) -> Option<String> {
        let v = self.settings.get(key)?;
        let text = match v {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => return None,
        };
        (!text.is_empty()).then_some(text)
    }

    /// A yes-or-no setting: `true`, `"true"`, `"yes"`, `1`. Missing is no.
    pub fn flag(&self, key: &str) -> bool {
        matches!(
            self.setting(key).as_deref(),
            Some("true") | Some("yes") | Some("1") | Some("on")
        )
    }

    /// A whole-number setting.
    pub fn number(&self, key: &str) -> Option<u64> {
        self.setting(key)?.parse().ok()
    }
}

impl Actor {
    /// One mailbox: `Ana <ana@example.com>`, `"Ana" <ana@example.com>`, or
    /// a bare address. A name that is the address is no name.
    pub fn mailbox(text: &str) -> Option<Actor> {
        let text = text.trim();
        let (name, id) = match (text.rfind('<'), text.ends_with('>')) {
            (Some(i), true) => (
                text[..i].trim().trim_matches('"').trim(),
                text[i + 1..text.len() - 1].trim(),
            ),
            _ => ("", text),
        };
        let id = if id.is_empty() { name } else { id };
        if id.is_empty() {
            return None;
        }
        Some(Actor {
            id: id.to_string(),
            name: (!name.is_empty() && name != id).then(|| name.to_string()),
            kind: None,
        })
    }

    /// A header's worth of mailboxes, comma-separated, commas inside quotes
    /// and angle brackets left alone.
    pub fn mailboxes(text: &str) -> Vec<Actor> {
        let mut out = Vec::new();
        let (mut quoted, mut depth, mut start) = (false, 0usize, 0usize);
        for (i, c) in text.char_indices() {
            match c {
                '"' => quoted = !quoted,
                '<' if !quoted => depth += 1,
                '>' if !quoted => depth = depth.saturating_sub(1),
                ',' if !quoted && depth == 0 => {
                    out.extend(Actor::mailbox(&text[start..i]));
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.extend(Actor::mailbox(&text[start..]));
        out
    }
}

/// Run `{cmd} {args...}` with no stdin; stdout as text. A non-zero exit is
/// an error carrying stderr. For plugins over another program.
pub fn run(cmd: &str, args: &[String]) -> std::io::Result<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| std::io::Error::new(e.kind(), format!("cannot run `{cmd}`: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(std::io::Error::other(format!(
            "`{cmd} {}` exited {}: {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            err.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// [`run`], then parse stdout as JSON.
pub fn run_json(cmd: &str, args: &[String]) -> std::io::Result<serde_json::Value> {
    let text = run(cmd, args)?;
    serde_json::from_str(text.trim())
        .map_err(|e| std::io::Error::other(format!("`{cmd}` printed invalid JSON: {e}")))
}

/// Where a plugin puts files it fetched (attachments, media) for the host to
/// read into its store on admit: the `cache` setting, else a directory per
/// source under the system temp dir.
pub fn cache_dir(request: &Request) -> std::io::Result<std::path::PathBuf> {
    let dir = match request.setting("cache") {
        Some(c) => expand_home(&c),
        None => std::env::temp_dir()
            .join("paddock")
            .join(safe_name(&request.id)),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `~/x` under the home directory.
pub fn expand_home(path: &str) -> std::path::PathBuf {
    let home = || {
        std::env::var("HOME")
            .ok()
            .filter(|h| !h.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    };
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => std::path::PathBuf::from(path),
    }
}

/// A file name from anything: letters, digits, `-`, `_`, `.`; else `_`.
pub fn safe_name(text: &str) -> String {
    let name: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = name.trim_matches('.').to_string();
    if name.is_empty() {
        "item".into()
    } else {
        name
    }
}

/// The RFC3339 time now, to the second, in UTC.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil-from-days (Howard Hinnant), enough for a timestamp.
    let days = (secs / 86_400) as i64;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
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
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// `person`, `group`, `list`, or `agent`. Anything else reads as person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Actor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// `text`, `file`, `image`, `audio`, or `video`. Text goes inline; anything
/// else names a file the host reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Part {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `reply`, `forward`, `quote`, `mention`, or `attach`. Names an item by
/// its source's id (the citing item's source unless `source_id` says
/// otherwise), or something outside the pile by `href`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Cite {
    #[serde(default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
}

/// One thing that arrived. Only `foreign_id` is required.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Item {
    pub foreign_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// RFC3339, or a bare date. A message's own time, or an event's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// The source's own grouping key: a conversation, a mail thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Actor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<Actor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cites: Vec<Cite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// The source's read state, when it tracks one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
}

/// What the host asks a source to deliver.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Draft {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// The foreign id of the item this replies to, on the same source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_foreign: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<Actor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
}

/// What a source answers after `send`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Sent {
    pub foreign_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
}

/// Where a message was said: a chat, a channel, a mailing list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Room {
    pub id: String,
    pub name: Option<String>,
    /// `group` or `list`. A one-to-one chat has no room.
    pub kind: String,
}

/// Someone sent something to someone. Mail and chat both fit, and both
/// lower to an [`Item`] the same way:
///
/// - the room, if any, is the `thread` and a `to` actor of its kind
/// - `reply_to` is a reply cite by foreign id
/// - `mentions` are mention cites carrying the actor
/// - `attachments` are file parts the host reads
/// - `at` is `start`; `seen` is `read`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    pub id: String,
    pub room: Option<Room>,
    pub from: Option<Actor>,
    pub to: Vec<Actor>,
    pub subject: Option<String>,
    pub text: String,
    pub href: Option<String>,
    pub reply_to: Option<String>,
    pub mentions: Vec<Actor>,
    /// Paths of files the host should read in as parts, with their mime.
    pub attachments: Vec<(String, String)>,
    pub at: Option<String>,
    pub seen: Option<bool>,
}

impl From<Message> for Item {
    fn from(m: Message) -> Self {
        let mut to = m.to;
        let thread = m.room.as_ref().map(|r| r.id.clone());
        if let Some(room) = m.room {
            to.push(Actor {
                id: room.id,
                name: room.name,
                kind: Some(room.kind),
            });
        }
        let mut cites: Vec<Cite> = m
            .reply_to
            .into_iter()
            .map(|f| Cite {
                kind: "reply".into(),
                foreign_id: Some(f),
                ..Default::default()
            })
            .collect();
        cites.extend(m.mentions.into_iter().map(|a| Cite {
            kind: "mention".into(),
            actor: Some(a),
            ..Default::default()
        }));
        let title = m.subject.unwrap_or_else(|| first_line(&m.text));
        Item {
            foreign_id: m.id,
            title,
            body: m.text,
            href: m.href,
            start: m.at,
            thread,
            from: m.from,
            to,
            cites,
            parts: m
                .attachments
                .into_iter()
                .map(|(path, mime)| Part {
                    kind: "file".into(),
                    mime,
                    path: Some(path),
                    ..Default::default()
                })
                .collect(),
            read: m.seen,
            ..Default::default()
        }
    }
}

/// A title for something that has none: the first line, at most 80 chars.
pub fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("untitled");
    line.chars().take(80).collect()
}

/// The verb the host asked for: the last argument.
pub fn verb() -> String {
    std::env::args().next_back().unwrap_or_default()
}

/// Print items for `pull`.
pub fn emit_items(items: &[Item]) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string(items)?);
    Ok(())
}

/// Print the answer for `send`.
pub fn emit_sent(sent: &Sent) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string(sent)?);
    Ok(())
}

/// Refuse `send` the way the host expects: exit 2.
pub fn cannot_send() -> ! {
    eprintln!("source cannot send");
    std::process::exit(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_in_a_group_lowers_to_thread_room_actor_and_cites() {
        let m = Message {
            id: "m2".into(),
            room: Some(Room {
                id: "fam@g.us".into(),
                name: Some("Family".into()),
                kind: "group".into(),
            }),
            from: Some(Actor {
                id: "ana".into(),
                ..Default::default()
            }),
            text: "see this\nand that".into(),
            reply_to: Some("m1".into()),
            mentions: vec![Actor {
                id: "bo".into(),
                ..Default::default()
            }],
            attachments: vec![("/tmp/a.png".into(), "image/png".into())],
            at: Some("2026-09-14T10:00:00Z".into()),
            seen: Some(false),
            ..Default::default()
        };
        let it: Item = m.into();
        assert_eq!(it.foreign_id, "m2");
        assert_eq!(it.title, "see this");
        assert_eq!(it.thread.as_deref(), Some("fam@g.us"));
        assert_eq!(it.to.len(), 1);
        assert_eq!(it.to[0].kind.as_deref(), Some("group"));
        assert_eq!(it.cites.len(), 2);
        assert_eq!(it.cites[0].kind, "reply");
        assert_eq!(it.cites[0].foreign_id.as_deref(), Some("m1"));
        assert_eq!(it.cites[1].kind, "mention");
        assert_eq!(
            it.cites[1].actor.as_ref().map(|a| a.id.as_str()),
            Some("bo")
        );
        assert_eq!(it.parts[0].path.as_deref(), Some("/tmp/a.png"));
        assert_eq!(it.read, Some(false));
    }

    #[test]
    fn an_item_needs_only_a_foreign_id_and_ignores_extras() {
        let it: Item = serde_json::from_str(r#"{"foreign_id":"x","weird":1}"#).unwrap();
        assert_eq!(it.foreign_id, "x");
        assert!(it.cites.is_empty());
        assert!(serde_json::from_str::<Item>(r#"{"title":"no id"}"#).is_err());
    }

    #[test]
    fn a_mailbox_is_an_actor_and_a_header_is_many() {
        let a = Actor::mailbox("Ana <ana@example.com>").unwrap();
        assert_eq!(
            (a.id.as_str(), a.name.as_deref()),
            ("ana@example.com", Some("Ana"))
        );
        let bare = Actor::mailbox("  bo@example.com ").unwrap();
        assert_eq!((bare.id.as_str(), bare.name), ("bo@example.com", None));
        let quoted = Actor::mailbox(r#""Cy, Jr." <cy@example.com>"#).unwrap();
        assert_eq!(quoted.name.as_deref(), Some("Cy, Jr."));
        assert!(Actor::mailbox("  ").is_none());
        let many = Actor::mailboxes(r#""Cy, Jr." <cy@example.com>, di@example.com,, Ed <ed@x>"#);
        assert_eq!(
            many.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["cy@example.com", "di@example.com", "ed@x"]
        );
    }

    #[test]
    fn settings_read_as_text_flags_and_numbers() {
        let r: Request = serde_json::from_str(
            r#"{"settings":{"limit":20,"media":true,"sync":"yes","name":" x ","list":[1]}}"#,
        )
        .unwrap();
        assert_eq!(r.number("limit"), Some(20));
        assert!(r.flag("media") && r.flag("sync") && !r.flag("name") && !r.flag("gone"));
        assert_eq!(r.setting("name").as_deref(), Some("x"));
        assert_eq!(r.setting("list"), None);
    }

    #[test]
    fn helpers_run_a_program_name_files_safely_and_tell_the_time() {
        assert_eq!(
            run("sh", &["-c".into(), "echo hi".into()]).unwrap().trim(),
            "hi"
        );
        let err = run("sh", &["-c".into(), "echo bad >&2; exit 3".into()]).unwrap_err();
        assert!(err.to_string().contains("exited 3") && err.to_string().contains("bad"));
        assert_eq!(
            run_json("sh", &["-c".into(), "echo '{\"a\":1}'".into()]).unwrap()["a"],
            1
        );
        assert_eq!(safe_name("a b/c<d>.pdf"), "a_b_c_d_.pdf");
        assert_eq!(safe_name("..."), "item");
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.starts_with("20") && now.ends_with('Z'));
    }

    #[test]
    fn a_request_reads_settings_and_carries_a_draft_only_for_send() {
        let r: Request =
            serde_json::from_str(r#"{"id":"feed","settings":{"url":" https://x "}}"#).unwrap();
        assert_eq!(r.setting("url").as_deref(), Some("https://x"));
        assert!(r.draft.is_none());
        let s = serde_json::to_string(&Request::default()).unwrap();
        assert!(!s.contains("draft"));
    }
}
