//! RFC 822 bytes as a protocol [`Message`]: what a Maildir file and an IMAP
//! fetch have in common. The mail plugins share this so a message id, a
//! list, a reply, and an attachment mean the same thing whichever way the
//! mail arrived.
//!
//! - `Message-ID` (without its brackets) is the id; a mailbox's own name for
//!   the message is the fallback
//! - `List-Id` is a room of kind `list`
//! - `From` is `from`; `To` and `Cc` are `to`
//! - `In-Reply-To`, else the last of `References`, is `reply_to`
//! - the text body, else the HTML body stripped, is `text`
//! - attachments are written under `cache/<id>/` for the host to read

use mail_parser::{HeaderValue, MessageParser, MimeHeaders};
use paddock_protocol::{safe_name, Actor, Message, Room};
use std::path::Path;

/// Parse one message. `fallback_id` names it when it has no `Message-ID`;
/// `seen` is what the mailbox says, if anything; attachments land in
/// `cache`. Bytes that are not a message are `None`.
pub fn message(raw: &[u8], fallback_id: &str, seen: Option<bool>, cache: &Path) -> Option<Message> {
    let m = MessageParser::default().parse(raw)?;
    let id = m
        .message_id()
        .map(strip_brackets)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_id.to_string());
    let mut to: Vec<Actor> = m.to().map(actors).unwrap_or_default();
    to.extend(m.cc().map(actors).unwrap_or_default());
    let room = m
        .list_id()
        .as_address()
        .and_then(|a| a.first())
        .and_then(|a| a.address().map(|id| (id.trim().to_string(), a.name())))
        .filter(|(id, _)| !id.is_empty())
        .map(|(id, name)| Room {
            name: name
                .map(str::trim)
                .filter(|n| !n.is_empty() && *n != id)
                .map(str::to_string),
            id,
            kind: "list".into(),
        });
    let text = m
        .body_text(0)
        .map(|c| c.into_owned())
        .or_else(|| m.body_html(0).map(|h| strip_html(&h)))
        .unwrap_or_default()
        .trim()
        .to_string();
    let reply_to = last_id(m.in_reply_to()).or_else(|| last_id(m.references()));
    let attachments = write_attachments(&m, &id, cache);
    Some(Message {
        id,
        room,
        from: m.from().and_then(|a| a.first()).and_then(actor),
        to,
        subject: m.subject().map(str::to_string),
        text,
        href: None,
        reply_to,
        mentions: Vec::new(),
        attachments,
        at: m.date().map(|d| d.to_rfc3339()),
        seen,
    })
}

fn actors(a: &mail_parser::Address) -> Vec<Actor> {
    a.iter().filter_map(actor).collect()
}

fn actor(a: &mail_parser::Addr) -> Option<Actor> {
    let id = a.address()?.trim();
    if id.is_empty() {
        return None;
    }
    Some(Actor {
        id: id.to_string(),
        name: a
            .name()
            .map(str::trim)
            .filter(|n| !n.is_empty() && *n != id)
            .map(str::to_string),
        kind: None,
    })
}

/// The last message id in a header that holds one or many.
fn last_id(v: &HeaderValue) -> Option<String> {
    let id = match v.as_text_list() {
        Some(list) => list.last().map(|s| s.to_string()),
        None => v.as_text().map(str::to_string),
    }?;
    let id = strip_brackets(&id);
    (!id.is_empty()).then_some(id)
}

/// `<id>` is `id`.
pub fn strip_brackets(id: &str) -> String {
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_string()
}

/// Every attachment written to `cache/<id>/<name>`, with its mime.
fn write_attachments(m: &mail_parser::Message, id: &str, cache: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut dir = None;
    for (i, part) in m.attachments().enumerate() {
        let dir = match &dir {
            Some(d) => d,
            None => {
                let d = cache.join(safe_name(id));
                if std::fs::create_dir_all(&d).is_err() {
                    return out;
                }
                dir.insert(d)
            }
        };
        let name = part
            .attachment_name()
            .map(safe_name)
            .unwrap_or_else(|| format!("part-{}", i + 1));
        let path = dir.join(name);
        if std::fs::write(&path, part.contents()).is_err() {
            continue;
        }
        let mime = part
            .content_type()
            .map(|ct| format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or("octet-stream")))
            .unwrap_or_else(|| "application/octet-stream".into());
        out.push((path.display().to_string(), mime));
    }
    out
}

/// Tags out, a few entities back, whitespace folded. Enough for a preview
/// and for search; the HTML itself is not kept.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if let Some(end) = skip_until {
            match lower[i..].find(end) {
                Some(j) => {
                    i += j + end.len();
                    skip_until = None;
                }
                None => break,
            }
            continue;
        }
        let c = bytes[i] as char;
        if in_tag {
            if c == '>' {
                in_tag = false;
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if c == '<' {
            for (open, close) in [("<style", "</style>"), ("<script", "</script>")] {
                if lower[i..].starts_with(open) {
                    skip_until = Some(close);
                }
            }
            if skip_until.is_none() {
                in_tag = true;
            }
            i += 1;
            continue;
        }
        if c == '&' {
            if let Some(j) = html[i..].find(';').filter(|j| *j <= 8) {
                let entity = &html[i + 1..i + j];
                let decoded = match entity {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" | "#39" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => entity
                        .strip_prefix('#')
                        .and_then(|n| n.parse::<u32>().ok())
                        .and_then(char::from_u32),
                };
                if let Some(d) = decoded {
                    out.push(d);
                    i += j + 1;
                    continue;
                }
            }
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "From: Ana <ana@example.com>\r\nTo: Bo <bo@example.com>, cy@example.com\r\nCc: Di <di@example.com>\r\nSubject: Invoice\r\nMessage-ID: <m2@example.com>\r\nIn-Reply-To: <m1@example.com>\r\nList-Id: Team <team.example.com>\r\nDate: Mon, 14 Sep 2026 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplease pay\r\n--b\r\nContent-Type: application/pdf; name=\"inv.pdf\"\r\nContent-Disposition: attachment; filename=\"inv.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--b--\r\n";

    #[test]
    fn headers_become_the_message_and_attachments_land_in_the_cache() {
        let cache = std::env::temp_dir().join(format!("paddock-mail-test-{}", std::process::id()));
        let m = message(RAW.as_bytes(), "fallback", Some(false), &cache).unwrap();
        assert_eq!(m.id, "m2@example.com");
        assert_eq!(m.from.as_ref().unwrap().id, "ana@example.com");
        assert_eq!(m.from.as_ref().unwrap().name.as_deref(), Some("Ana"));
        assert_eq!(m.to.len(), 3, "to and cc");
        assert_eq!(m.subject.as_deref(), Some("Invoice"));
        assert_eq!(m.text.trim(), "please pay");
        assert_eq!(m.reply_to.as_deref(), Some("m1@example.com"));
        let room = m.room.as_ref().unwrap();
        assert_eq!(room.id, "team.example.com");
        assert_eq!(room.name.as_deref(), Some("Team"));
        assert_eq!(room.kind, "list");
        assert_eq!(m.at.as_deref(), Some("2026-09-14T10:00:00Z"));
        assert_eq!(m.seen, Some(false));
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].1, "application/pdf");
        assert_eq!(std::fs::read(&m.attachments[0].0).unwrap(), b"%PDF-");
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn no_message_id_means_the_fallback_and_html_is_stripped() {
        let raw = "From: a@x\r\nSubject: s\r\nContent-Type: text/html\r\n\r\n<p>Hi &amp; <b>bye</b><style>p{}</style></p>";
        let m = message(raw.as_bytes(), "file-1", None, Path::new("/nonexistent")).unwrap();
        assert_eq!(m.id, "file-1");
        assert_eq!(m.text, "Hi & bye");
        assert!(m.reply_to.is_none());
        assert!(m.room.is_none());
    }
}
