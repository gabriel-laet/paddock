//! `paddock-maildir`: a Maildir, or a folder of `.eml` files, as a source.
//! Read-only; keep the Maildir synced with mbsync, offlineimap, or whatever
//! you already use.
//!
//! ```toml
//! [[source]]
//! id = "mail"
//! kind = "maildir"                # resolves to `paddock-maildir` on PATH
//! path = "~/Mail/work"            # a Maildir (cur/ new/), or any folder of .eml files
//! # cache = "~/.cache/paddock"    # where attachments are written for the host to read
//! ```

use anyhow::{Context, Result};
use paddock_protocol::{cache_dir, cannot_send, emit_items, expand_home, verb, Item, Request};
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    match verb().as_str() {
        "pull" => {
            let request = Request::read().context("read request")?;
            let path = expand_home(&request.setting("path").context("maildir needs path")?);
            let cache = cache_dir(&request)?;
            emit_items(&items(&path, &cache)?)?;
            Ok(())
        }
        "send" => cannot_send(),
        other => anyhow::bail!("usage: paddock-maildir pull|send (got `{other}`)"),
    }
}

/// A Maildir when `cur/` or `new/` is there; else every `.eml` in the folder.
fn items(dir: &Path, cache: &Path) -> Result<Vec<Item>> {
    let is_maildir = dir.join("cur").is_dir() || dir.join("new").is_dir();
    let mut out = Vec::new();
    if is_maildir {
        for (sub, seen) in [("new", Some(false)), ("cur", None)] {
            for file in files(&dir.join(sub))? {
                let name = file_name(&file);
                let seen = seen.or_else(|| Some(flags(&name).contains('S')));
                out.extend(item(&file, &unique(&name), seen, cache)?);
            }
        }
    } else {
        for file in files(dir)? {
            if file
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("eml"))
            {
                out.extend(item(&file, &file_name(&file), None, cache)?);
            }
        }
    }
    Ok(out)
}

fn item(file: &Path, fallback_id: &str, seen: Option<bool>, cache: &Path) -> Result<Option<Item>> {
    let raw = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Ok(
        paddock_mail::message(&raw, fallback_id, seen, cache).map(|m| {
            let mut it: Item = m.into();
            it.href = Some(file.display().to_string());
            it
        }),
    )
}

/// Regular, non-hidden files, in name order. A missing directory is empty.
fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && !file_name(p).starts_with('.'))
        .collect();
    files.sort();
    Ok(files)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The Maildir flags after `:2,`: `S` seen, `R` replied, `T` trashed...
fn flags(name: &str) -> &str {
    name.rsplit_once(":2,").map(|(_, f)| f).unwrap_or("")
}

/// A Maildir file's name without its flags, which change as it is read.
fn unique(name: &str) -> String {
    name.split_once(':')
        .map(|(u, _)| u)
        .unwrap_or(name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(id: &str, subject: &str) -> String {
        format!(
            "From: a@x\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\n\r\nbody of {subject}\r\n"
        )
    }

    #[test]
    fn a_maildir_reads_new_as_unseen_and_cur_flags_as_seen() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("box");
        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::create_dir_all(dir.join("cur")).unwrap();
        std::fs::write(dir.join("new/1.host"), mail("n1@x", "fresh")).unwrap();
        std::fs::write(dir.join("cur/2.host:2,S"), mail("c2@x", "seen")).unwrap();
        std::fs::write(dir.join("cur/3.host:2,"), "not a message id\r\n\r\nbody").unwrap();
        let got = items(&dir, tmp.path()).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].foreign_id, "n1@x");
        assert_eq!(got[0].read, Some(false));
        assert_eq!(got[1].foreign_id, "c2@x");
        assert_eq!(got[1].read, Some(true));
        assert_eq!(got[1].title, "seen");
        assert_eq!(
            got[2].foreign_id, "3.host",
            "no Message-ID: the file's unique name"
        );
        assert_eq!(got[2].read, Some(false));
        assert!(got[0].href.as_deref().unwrap().ends_with("new/1.host"));
    }

    #[test]
    fn a_plain_folder_reads_only_eml_files_and_has_no_opinion_on_read() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.eml"), mail("a@x", "one")).unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "skip me").unwrap();
        let got = items(tmp.path(), tmp.path()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].foreign_id, "a@x");
        assert_eq!(got[0].read, None);
    }
}
