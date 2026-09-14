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

    /// A string setting, trimmed; missing or empty is `None`.
    pub fn setting(&self, key: &str) -> Option<String> {
        self.settings
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
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
    std::env::args().last().unwrap_or_default()
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
    fn a_request_reads_settings_and_carries_a_draft_only_for_send() {
        let r: Request =
            serde_json::from_str(r#"{"id":"feed","settings":{"url":" https://x "}}"#).unwrap();
        assert_eq!(r.setting("url").as_deref(), Some("https://x"));
        assert!(r.draft.is_none());
        let s = serde_json::to_string(&Request::default()).unwrap();
        assert!(!s.contains("draft"));
    }
}
