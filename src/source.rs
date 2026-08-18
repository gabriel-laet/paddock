use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct NewItem {
    pub source_id: String,
    pub foreign_id: String,
    pub title: String,
    pub body: String,
    pub href: Option<String>,
}

/// Non-recursive. Skips dotfiles and directories.
pub fn pull_fs(source_id: &str, dir: &Path) -> Result<Vec<NewItem>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for ent in entries {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = ent.path();
        if !path.is_file() {
            continue;
        }
        out.push(item_from_file(source_id, &path)?);
    }
    Ok(out)
}

pub fn item_from_file(source_id: &str, path: &Path) -> Result<NewItem> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let body = String::from_utf8_lossy(&bytes).into_owned();
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.clone());
    Ok(NewItem {
        source_id: source_id.to_string(),
        foreign_id: filename,
        title,
        body,
        href: Some(path.display().to_string()),
    })
}

pub fn pull_rss(source_id: &str, url: &str) -> Result<Vec<NewItem>> {
    let bytes = reqwest::blocking::Client::builder()
        .user_agent("paddock/0.1")
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("fetch {url}"))?
        .bytes()
        .with_context(|| format!("read {url}"))?;
    let channel = rss::Channel::read_from(&bytes[..])
        .with_context(|| format!("parse rss {url}"))?;
    let mut out = Vec::new();
    for it in channel.items() {
        let href = it.link().map(|s| s.to_string());
        let foreign = it
            .guid()
            .map(|g| g.value().to_string())
            .or_else(|| href.clone())
            .or_else(|| it.title().map(|s| s.to_string()))
            .unwrap_or_else(|| "untitled".into());
        let title = it.title().unwrap_or("untitled").to_string();
        let body = it
            .content()
            .or_else(|| it.description())
            .unwrap_or("")
            .to_string();
        out.push(NewItem {
            source_id: source_id.to_string(),
            foreign_id: foreign,
            title,
            body,
            href,
        });
    }
    Ok(out)
}
