//! A feed. Read-only.

use anyhow::{bail, Context, Result};

use crate::kernel::{Draft, NewItem, Source};

/// A feed. Read-only.
pub struct Rss {
    pub id: String,
    pub url: String,
}

impl Source for Rss {
    fn pull(&self) -> Result<Vec<NewItem>> {
        let bytes = reqwest::blocking::Client::builder()
            .user_agent("paddock/0.1")
            .build()?
            .get(&self.url)
            .send()
            .with_context(|| format!("fetch {}", self.url))?
            .bytes()?;
        let channel = rss::Channel::read_from(&bytes[..]).context("parse rss")?;
        Ok(channel
            .items()
            .iter()
            .map(|it| {
                let link = it.link().map(str::to_string);
                let guid = it.guid().map(|g| g.value().to_string());
                let title = it.title().unwrap_or("untitled").to_string();
                NewItem {
                    source_id: self.id.clone(),
                    foreign_id: guid
                        .or_else(|| link.clone())
                        .unwrap_or_else(|| title.clone()),
                    title,
                    body: it.description().or(it.content()).unwrap_or("").to_string(),
                    href: link,
                    ..Default::default()
                }
            })
            .collect())
    }

    fn send(&self, _draft: &Draft, _reply_to_foreign: Option<&str>) -> Result<NewItem> {
        bail!("source cannot send")
    }
}
