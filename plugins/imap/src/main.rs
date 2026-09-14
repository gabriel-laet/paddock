//! `paddock-imap`: any mailbox over IMAP, sending over SMTP. Fastmail,
//! Gmail with an app password, iCloud, a self-hosted box: anything that
//! speaks the two protocols.
//!
//! ```toml
//! [[source]]
//! id = "mail"
//! kind = "imap"                   # resolves to `paddock-imap` on PATH
//! host = "imap.fastmail.com"
//! user = "me@example.com"
//! password_cmd = "pass show fastmail"   # the host runs it; the plugin sees `password`
//! # port = 993                    # 993 is TLS; anything else tries STARTTLS
//! # folder = "INBOX"
//! # since = "30d"                 # how far back a pull looks
//! # limit = 200                   # newest messages per pull
//! # smtp_host = "smtp.fastmail.com"   # default: host
//! # smtp_port = 465               # 465 is TLS; 587 is STARTTLS
//! # from = "me@example.com"       # default: user
//! ```
//!
//! Pulling never marks anything read on the server. A sent message gets a
//! `Message-ID` of its own, which is its foreign id; replies carry
//! `In-Reply-To`.

use anyhow::{anyhow, Context, Result};
use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{SmtpTransport, Transport};
use paddock_mail::strip_brackets;
use paddock_protocol::{
    cache_dir, cannot_send, emit_items, emit_sent, now_rfc3339, safe_name, verb, Draft, Item,
    Request, Sent,
};

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
        other => anyhow::bail!("usage: paddock-imap pull|send (got `{other}`)"),
    }
}

struct Account {
    host: String,
    user: String,
    password: String,
}

fn account(r: &Request) -> Result<Account> {
    let need = |k: &str| r.setting(k).ok_or_else(|| anyhow!("imap needs {k}"));
    Ok(Account {
        host: need("host")?,
        user: need("user")?,
        password: need("password")?,
    })
}

/// The newest `limit` messages since `since`, read without touching flags.
fn pull(r: &Request) -> Result<Vec<Item>> {
    let acct = account(r)?;
    let port = r.number("port").unwrap_or(993) as u16;
    let folder = r.setting("folder").unwrap_or_else(|| "INBOX".into());
    let limit = r.number("limit").unwrap_or(200) as usize;
    let cache = cache_dir(r)?;

    let client = imap::ClientBuilder::new(&acct.host, port)
        .connect()
        .with_context(|| format!("connect {}:{port}", acct.host))?;
    let mut session = client
        .login(&acct.user, &acct.password)
        .map_err(|e| anyhow!("login {} at {}: {}", acct.user, acct.host, e.0))?;
    session
        .examine(&folder)
        .with_context(|| format!("open folder {folder}"))?;

    let query = match since(r.setting("since").as_deref().unwrap_or("30d")) {
        Some(day) => format!("SINCE {}", day.format("%d-%b-%Y")),
        None => "ALL".into(),
    };
    let mut uids: Vec<u32> = session.uid_search(&query)?.into_iter().collect();
    uids.sort_unstable();
    let uids: Vec<String> = uids
        .iter()
        .rev()
        .take(limit)
        .rev()
        .map(u32::to_string)
        .collect();
    let mut items = Vec::new();
    if !uids.is_empty() {
        let fetched = session.uid_fetch(uids.join(","), "(UID FLAGS BODY.PEEK[])")?;
        for f in fetched.iter() {
            let uid = f.uid.unwrap_or(0);
            let seen = f
                .flags()
                .iter()
                .any(|fl| matches!(fl, imap::types::Flag::Seen));
            let Some(raw) = f.body() else { continue };
            if let Some(m) =
                paddock_mail::message(raw, &format!("{folder}-uid-{uid}"), Some(seen), &cache)
            {
                items.push(Item::from(m));
            }
        }
    }
    session.logout().ok();
    Ok(items)
}

/// `30d`, `12h`, `2w`, or a `YYYY-MM-DD`, as the day a pull looks back to.
fn since(text: &str) -> Option<chrono::NaiveDate> {
    let text = text.trim();
    if let Ok(day) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(day);
    }
    let (n, unit) = text.split_at(text.len().checked_sub(1)?);
    let n: i64 = n.parse().ok()?;
    let hours = match unit {
        "h" => n,
        "d" => n * 24,
        "w" => n * 24 * 7,
        _ => return None,
    };
    Some((chrono::Utc::now() - chrono::Duration::hours(hours)).date_naive())
}

fn send(r: &Request, draft: &Draft) -> Result<Sent> {
    let acct = account(r)?;
    let from = r.setting("from").unwrap_or_else(|| acct.user.clone());
    let smtp_host = r.setting("smtp_host").unwrap_or_else(|| acct.host.clone());
    let smtp_port = r.number("smtp_port").unwrap_or(465) as u16;
    if draft.to.is_empty() {
        anyhow::bail!("imap send needs a recipient (--to)");
    }

    let domain = from.rsplit('@').next().unwrap_or("paddock").to_string();
    let message_id = format!(
        "{}.{}@{domain}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    );
    let mut b = lettre::Message::builder()
        .from(from.parse().with_context(|| format!("from `{from}`"))?)
        .subject(draft.title.clone())
        .date_now()
        .message_id(Some(format!("<{message_id}>")));
    for a in &draft.to {
        let mbox = match &a.name {
            Some(n) => format!("{n} <{}>", a.id),
            None => a.id.clone(),
        };
        b = b.to(mbox.parse().with_context(|| format!("to `{mbox}`"))?);
    }
    if let Some(id) = draft.reply_to_foreign.as_deref().map(strip_brackets) {
        b = b
            .in_reply_to(format!("<{id}>"))
            .references(format!("<{id}>"));
    }

    let text = SinglePart::plain(draft.body.clone());
    let files: Vec<&paddock_protocol::Part> =
        draft.parts.iter().filter(|p| p.path.is_some()).collect();
    let message = if files.is_empty() {
        b.singlepart(text)?
    } else {
        let mut mp = MultiPart::mixed().singlepart(text);
        for p in files {
            let path = p.path.clone().unwrap_or_default();
            let bytes = std::fs::read(&path).with_context(|| format!("read {path}"))?;
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| safe_name(&n.to_string_lossy()))
                .unwrap_or_else(|| "file".into());
            let mime = ContentType::parse(&p.mime)
                .or_else(|_| ContentType::parse("application/octet-stream"))?;
            mp = mp.singlepart(Attachment::new(name).body(bytes, mime));
        }
        b.multipart(mp)?
    };

    let builder = if smtp_port == 587 {
        SmtpTransport::starttls_relay(&smtp_host)
    } else {
        SmtpTransport::relay(&smtp_host)
    }
    .with_context(|| format!("smtp {smtp_host}"))?;
    let transport = builder
        .port(smtp_port)
        .credentials(Credentials::new(acct.user, acct.password))
        .build();
    transport
        .send(&message)
        .with_context(|| format!("send through {smtp_host}:{smtp_port}"))?;
    Ok(Sent {
        foreign_id: message_id,
        start: Some(now_rfc3339()),
        end: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_reads_a_span_or_a_day() {
        assert_eq!(
            since("2026-09-01"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap())
        );
        let week = since("7d").unwrap();
        let today = chrono::Utc::now().date_naive();
        assert_eq!((today - week).num_days(), 7);
        assert_eq!(since("2w").map(|d| (today - d).num_days()), Some(14));
        assert!(since("soon").is_none());
    }
}
