//! `paddock-rss`: a feed as a source. Read-only.
//!
//! ```toml
//! [[source]]
//! id = "feed"
//! kind = "rss"                  # resolves to `paddock-rss` on PATH
//! url = "https://example.com/feed.xml"
//! ```

use anyhow::{Context, Result};
use paddock_protocol::{cannot_send, emit_items, verb, Item, Request};

fn main() -> Result<()> {
    match verb().as_str() {
        "pull" => {
            let request = Request::read().context("read request")?;
            let url = request.setting("url").context("rss needs url")?;
            let bytes = reqwest::blocking::Client::builder()
                .user_agent("paddock-rss/0.1")
                .build()?
                .get(&url)
                .send()
                .with_context(|| format!("fetch {url}"))?
                .bytes()?;
            emit_items(&items(&bytes)?)?;
            Ok(())
        }
        "send" => cannot_send(),
        other => anyhow::bail!("usage: paddock-rss pull|send (got `{other}`)"),
    }
}

/// Every entry in a feed, guid (else link, else title) as the foreign id.
fn items(feed: &[u8]) -> Result<Vec<Item>> {
    let channel = rss::Channel::read_from(feed).context("parse rss")?;
    Ok(channel
        .items()
        .iter()
        .map(|it| {
            let link = it.link().map(str::to_string);
            let guid = it.guid().map(|g| g.value().to_string());
            let title = it.title().unwrap_or("untitled").to_string();
            Item {
                foreign_id: guid
                    .or_else(|| link.clone())
                    .unwrap_or_else(|| title.clone()),
                title,
                body: it.description().or(it.content()).unwrap_or("").to_string(),
                href: link,
                start: it.pub_date().and_then(rfc3339),
                ..Default::default()
            }
        })
        .collect())
}

/// Feeds date entries RFC 2822; the protocol wants RFC 3339.
fn rfc3339(rfc2822: &str) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc2822(rfc2822).ok()?;
    Some(t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>t</title><link>https://x</link><description>d</description>
<item><title>First</title><link>https://x/1</link><guid>g1</guid><description>hello</description>
<pubDate>Mon, 14 Sep 2026 10:00:00 GMT</pubDate></item>
<item><title>Second</title><link>https://x/2</link><description>world</description></item>
</channel></rss>"#;

    #[test]
    fn entries_become_items_with_stable_ids() {
        let got = items(FEED.as_bytes()).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].foreign_id, "g1");
        assert_eq!(got[0].title, "First");
        assert_eq!(got[0].body, "hello");
        assert_eq!(got[0].href.as_deref(), Some("https://x/1"));
        assert_eq!(got[0].start.as_deref(), Some("2026-09-14T10:00:00Z"));
        assert_eq!(got[1].foreign_id, "https://x/2", "no guid: the link");
        assert!(got[1].start.is_none());
    }
}
