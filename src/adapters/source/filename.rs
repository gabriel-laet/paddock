//! Naming files on disk: a safe stem from a title, a free path next to a
//! taken one, and the dotfile rule.

use std::path::{Path, PathBuf};

/// `Hello, world!` becomes `Hello-world`. Only ascii letters, digits, `-` and
/// `_` survive; each run of anything else becomes one `-`. Never empty.
pub fn slug(title: &str) -> String {
    let keep = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut out = String::new();
    for c in title.chars() {
        match (keep(c), out.ends_with('-')) {
            (true, _) => out.push(c),
            (false, false) => out.push('-'),
            (false, true) => {}
        }
    }
    match out.trim_matches('-') {
        "" => "untitled".to_string(),
        s => s.to_string(),
    }
}

/// `note.md` if free, else `note-2.md`, `note-3.md`, and so on.
pub fn unused(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("md");
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.{ext}")))
        .find(|p| !p.exists())
        .expect("some suffix is free")
}

/// A name starting with `.`, or one that is not valid unicode.
pub fn hidden(path: &Path) -> bool {
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name.starts_with('.'),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_words_and_drops_the_rest() {
        assert_eq!(slug("Hello, world!"), "Hello-world");
        assert_eq!(slug("../x"), "x");
        assert_eq!(slug("***"), "untitled");
        assert_eq!(slug("re: RFC 12"), "re-RFC-12");
    }

    #[test]
    fn unused_counts_up() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("note.md");
        assert_eq!(unused(&p), p);
        std::fs::write(&p, "").unwrap();
        assert_eq!(unused(&p), dir.path().join("note-2.md"));
        std::fs::write(dir.path().join("note-2.md"), "").unwrap();
        assert_eq!(unused(&p), dir.path().join("note-3.md"));
    }

    #[test]
    fn hidden_is_the_dot_rule() {
        assert!(hidden(Path::new("/x/.env")));
        assert!(!hidden(Path::new("/x/env")));
    }
}
