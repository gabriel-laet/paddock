//! Shared by every integration test: a throwaway host, and fixtures for
//! sources, embedders, models, and stores that need nothing but `sh`.
#![allow(dead_code)]

use paddock::*;
use std::fs;

pub fn temp_paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_dirs(dir.path().join("cfg"), dir.path().join("data"));
    (dir, paths)
}

pub fn items_match_chain(chain: &[&Inbox], item: &Item) -> bool {
    chain.iter().all(|ib| Question::of(&[&ib]).matches(&item))
}

/// A source that speaks the exec protocol with nothing but `sh` and `cat`.
pub fn write_exec_helper(dir: &std::path::Path) -> std::path::PathBuf {
    fs::write(
        dir.join("pull.json"),
        r#"[
  {"foreign_id": "note-1", "title": "note", "body": "hello"},
  {"foreign_id": "meet-1", "title": "meet", "body": "sync",
   "start": "2026-08-18T15:00:00Z", "end": "2026-08-18T16:00:00Z"}
]"#,
    )
    .unwrap();
    fs::write(
        dir.join("send.json"),
        r#"{"foreign_id": "sent-1", "start": "2026-08-19T10:00:00Z", "end": "2026-08-19T11:00:00Z"}"#,
    )
    .unwrap();
    let p = dir.join("exec_helper.sh");
    fs::write(
        &p,
        format!(
            r#"case "$1" in
  pull) cat "{dir}/pull.json" ;;
  send) cat >/dev/null; cat "{dir}/send.json" ;;
  *) echo "unknown verb" >&2; exit 1 ;;
esac
"#,
            dir = dir.display()
        ),
    )
    .unwrap();
    p
}

pub fn exec_source_toml(helper: &std::path::Path) -> String {
    let helper = helper
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "cal"
timed = true

[[source]]
id = "plug"
kind = "exec"
cmd = "sh"
args = ["{helper}"]
"#
    )
}

pub fn yesterday() -> String {
    (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

pub static PATH_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A host with a fake embedder and a fake model, both plain `sh`, so the
/// whole search stack runs without a network or a real model.
pub fn ai_toml(incoming: &std::path::Path) -> String {
    format!(
        r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "money"
labels = ["money"]

[[source]]
id = "incoming"
kind = "fs"
path = "{}"

[embedder]
cmd = "sh"
args = ["-c", "grep -qi money && echo '[1, 0]' || echo '[0, 1]'"]

[model]
cmd = "sh"
args = ["-c", "echo 'Pay the invoice, see #1 and #1. Not #999.'"]
"#,
        incoming.display()
    )
}

pub fn store_toml(incoming: &std::path::Path, store: &str) -> String {
    format!(
        r#"
[[inbox]]
name = "all"

[[source]]
id = "incoming"
kind = "fs"
path = "{}"

[store]
{store}
"#,
        incoming.display()
    )
}
