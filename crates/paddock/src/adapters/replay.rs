//! What a candidate config would change, shown on your own items and
//! written nowhere: the store is snapshotted, the copy forgets its run-once
//! marks, every item in scope is classified again under the candidate with
//! every effect but `send:`, and the labels and inboxes before and after are
//! compared.
//! This is the eval: edit a skill or a prompt, replay, read the diff.
//!
//! Run-once classifiers run again, so an `llm` classifier costs one call
//! per item; scope a replay with an inbox path or a limit.

use anyhow::{Context, Result};
use std::collections::BTreeSet;

use super::host::{kernel, store_key, Paths};
use super::store::Sqlite;
use crate::kernel::{Config, Item, Notice, Question, Store};

/// One item whose labels or inboxes would differ.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Change {
    pub id: i64,
    pub title: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Inbox paths the item would newly answer to.
    pub entered: Vec<String>,
    /// Inbox paths it would no longer answer to.
    pub left: Vec<String>,
}

/// A replay's findings.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Replay {
    /// How many items were replayed.
    pub items: usize,
    pub changes: Vec<Change>,
    /// Notices the candidate would have raised.
    pub notices: Vec<Notice>,
    pub warnings: Vec<String>,
}

/// Replay the items an inbox path answers to (default `all`), at most
/// `limit`, under `candidate`. `current` is the config the store lives
/// under, for the "before" side.
pub fn replay(
    paths: &Paths,
    current: &Config,
    store: &Sqlite,
    candidate: &Config,
    inbox: Option<&str>,
    limit: Option<usize>,
) -> Result<Replay> {
    let copy = paths.db_path.with_extension("db.replay");
    store.snapshot(&copy)?;
    let result = replay_on(&copy, current, store, candidate, inbox, limit);
    for suffix in ["db.replay", "db.replay-wal", "db.replay-shm"] {
        let _ = std::fs::remove_file(paths.db_path.with_extension(suffix));
    }
    result
}

fn replay_on(
    copy: &std::path::Path,
    current: &Config,
    store: &Sqlite,
    candidate: &Config,
    inbox: Option<&str>,
    limit: Option<usize>,
) -> Result<Replay> {
    let key = store_key(&candidate.store.clone().unwrap_or_default().settings)?;
    let scratch = Sqlite::open(copy, key.as_deref()).context("open the replay copy")?;
    scratch.clear_seen()?;
    let before = kernel(current, store)?;
    let after = kernel(candidate, &scratch)?;

    let path: Vec<&str> = inbox
        .unwrap_or("all")
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    let chain = current
        .chain(&path)
        .with_context(|| format!("no inbox {}", path.join("/")))?;
    let mut question = before.question(&chain);
    question.limit = limit;
    let items = store.ask(&question)?;

    let mut out = Replay {
        items: items.len(),
        ..Default::default()
    };
    for was in items {
        let told = after.rehearse(was.id)?;
        out.warnings.extend(told.warnings);
        out.notices.extend(told.notices);
        let now = scratch.get(was.id)?;
        let (added, removed) = diff(&was.label_names(), &now.label_names());
        let (entered, left) = diff(
            &inboxes(current, &before, &was),
            &inboxes(candidate, &after, &now),
        );
        if !(added.is_empty() && removed.is_empty() && entered.is_empty() && left.is_empty()) {
            out.changes.push(Change {
                id: was.id,
                title: was.title.clone(),
                added,
                removed,
                entered,
                left,
            });
        }
    }
    Ok(out)
}

/// Every inbox path the item answers to under this config.
fn inboxes(config: &Config, k: &crate::kernel::Kernel, item: &Item) -> Vec<String> {
    config
        .nodes()
        .iter()
        .filter(|node| {
            let refs: Vec<&str> = node.path.iter().map(String::as_str).collect();
            config
                .chain(&refs)
                .is_some_and(|chain| Question::of_at(&chain, k.now).matches(item))
        })
        .map(|node| node.path.join("/"))
        .collect()
}

/// (in `after` only, in `before` only), each sorted.
fn diff(before: &[String], after: &[String]) -> (Vec<String>, Vec<String>) {
    let b: BTreeSet<&String> = before.iter().collect();
    let a: BTreeSet<&String> = after.iter().collect();
    (
        a.difference(&b).map(|s| s.to_string()).collect(),
        b.difference(&a).map(|s| s.to_string()).collect(),
    )
}

impl Change {
    /// `#12  title  +code -newsletter  → all/codes  ← all/newsletters`
    pub fn line(&self) -> String {
        let mut parts = vec![
            format!("#{}", self.id),
            self.title.chars().take(48).collect(),
        ];
        let labels: Vec<String> = self
            .added
            .iter()
            .map(|l| format!("+{l}"))
            .chain(self.removed.iter().map(|l| format!("-{l}")))
            .collect();
        if !labels.is_empty() {
            parts.push(labels.join(" "));
        }
        if !self.entered.is_empty() {
            parts.push(format!("→ {}", self.entered.join(", ")));
        }
        if !self.left.is_empty() {
            parts.push(format!("← {}", self.left.join(", ")));
        }
        parts.join("  ")
    }
}
