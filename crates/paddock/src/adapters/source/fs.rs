//! A directory. Every non-dot file is an item; a draft becomes a new file.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use super::filename::{hidden, slug, unused};
use super::nonempty;
use crate::kernel::{Draft, NewItem, NewPart, PartKind, Source};

/// A directory. Every non-dot file is an item; a draft becomes a new file.
pub struct Fs {
    pub id: String,
    pub dir: PathBuf,
}

impl Source for Fs {
    fn pull(&self) -> Result<Vec<NewItem>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<_> = fs::read_dir(&self.dir)
            .with_context(|| format!("read {}", self.dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        entries
            .iter()
            .map(|e| e.path())
            .filter(|p| p.is_file() && !hidden(p))
            .map(|p| item_from_file(&self.id, &p))
            .collect()
    }

    fn send(&self, draft: &Draft, _reply_to_foreign: Option<&str>) -> Result<NewItem> {
        fs::create_dir_all(&self.dir)?;
        let dest = unused(&self.dir.join(format!("{}.md", slug(&draft.title))));
        fs::write(&dest, draft.body.as_bytes())?;
        let mut item = item_from_file(&self.id, &dest)?;
        item.title = draft.title.clone();
        if let Some(fid) = nonempty(draft.foreign_id.as_deref()) {
            item.foreign_id = fid.to_string();
        }
        Ok(item)
    }
}

/// Text files become a body; media files become one part.
pub fn item_from_file(source_id: &str, path: &Path) -> Result<NewItem> {
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.clone());
    let base = NewItem {
        source_id: source_id.to_string(),
        foreign_id: filename.clone(),
        title,
        href: Some(path.display().to_string()),
        ..Default::default()
    };
    if let Some((kind, mime)) = media_kind(path) {
        return Ok(NewItem {
            body: filename,
            parts: vec![NewPart {
                kind,
                mime: mime.to_string(),
                src: Some(path.display().to_string()),
                ..Default::default()
            }],
            ..base
        });
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(NewItem {
        body: String::from_utf8_lossy(&bytes).into_owned(),
        ..base
    })
}

fn media_kind(path: &Path) -> Option<(PartKind, &'static str)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => (PartKind::Image, "image/png"),
        "jpg" | "jpeg" => (PartKind::Image, "image/jpeg"),
        "gif" => (PartKind::Image, "image/gif"),
        "webp" => (PartKind::Image, "image/webp"),
        "svg" => (PartKind::Image, "image/svg+xml"),
        "mp3" => (PartKind::Audio, "audio/mpeg"),
        "wav" => (PartKind::Audio, "audio/wav"),
        "ogg" | "oga" => (PartKind::Audio, "audio/ogg"),
        "m4a" => (PartKind::Audio, "audio/mp4"),
        "flac" => (PartKind::Audio, "audio/flac"),
        "mp4" => (PartKind::Video, "video/mp4"),
        "webm" => (PartKind::Video, "video/webm"),
        "mov" => (PartKind::Video, "video/quicktime"),
        "mkv" => (PartKind::Video, "video/x-matroska"),
        _ => return None,
    })
}
