//! Inboxes and the questions they ask. Also the host config they live in.
//!
//! An inbox is a named question over the pile: which sources, which labels,
//! which window of time. Children nest and tighten the parent's question.

use serde::{Deserialize, Serialize};

use super::item::Item;

/// Everything a host declares. Parsed by an adapter; the kernel only reads it.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub inbox: Vec<Inbox>,
    #[serde(default)]
    pub source: Vec<SourceSpec>,
    /// Classifiers on the implicit root (the whole pile).
    #[serde(default)]
    pub classifier: Vec<ClassifierSpec>,
    /// SSH host for `paddock --remote`. Not a source.
    #[serde(default)]
    pub remote: Option<String>,
    /// Labels that never auto-forget. Empty means ["todo", "later"].
    #[serde(default)]
    pub keep: Vec<String>,
    /// Host default for untimed stale cleanup (`"14d"`, `"24h"`).
    #[serde(default)]
    pub forget_after: Option<String>,
    /// Text to vector, for `near` questions. Items are embedded on admit.
    #[serde(default)]
    pub embedder: Option<ModelSpec>,
    /// The chat model `answer` talks to.
    #[serde(default)]
    pub model: Option<ModelSpec>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Inbox {
    pub name: String,
    /// Item must carry ALL of these.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Item must come from ONE of these. Empty means any.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Only items with `start`; listed in `start` order.
    #[serde(default)]
    pub timed: bool,
    /// Only items whose moment is within this duration of now (`"14d"`, `"24h"`).
    #[serde(default)]
    pub newer_than: Option<String>,
    /// Only items whose moment is older than this duration.
    #[serde(default)]
    pub older_than: Option<String>,
    #[serde(default)]
    pub classifier: Vec<ClassifierSpec>,
    #[serde(default)]
    pub inbox: Vec<Inbox>,
}

/// Whatever an adapter needs beyond what the kernel reads: `cmd`, `url`,
/// `key`, `pattern`, and so on. The kernel never looks inside.
pub type Settings = serde_json::Map<String, serde_json::Value>;

/// A string setting, trimmed; missing or empty is `None`.
pub fn setting(settings: &Settings, key: &str) -> Option<String> {
    settings
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A list-of-strings setting; missing is empty.
pub fn setting_list(settings: &Settings, key: &str) -> Vec<String> {
    settings
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A classifier as declared. The kernel reads `id`, `kind`, `label`,
/// `labels`, and `once`; the rest is the adapter's.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ClassifierSpec {
    pub id: String,
    pub kind: String,
    /// The label a yes/no classifier stamps.
    #[serde(default)]
    pub label: Option<String>,
    /// Allow-list: the classifier must pick one of these or nothing.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Run once per item and remember the verdict.
    #[serde(default)]
    pub once: bool,
    #[serde(flatten)]
    pub settings: Settings,
}

/// A model or embedder as declared. `kind` picks the adapter.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ModelSpec {
    #[serde(default)]
    pub kind: String,
    #[serde(flatten)]
    pub settings: Settings,
}

/// A source as declared. The kernel reads `id`, `kind`, `name`, and
/// `forget_after`; the rest is the adapter's.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SourceSpec {
    pub id: String,
    pub kind: String,
    /// Display name. Empty means `id`.
    #[serde(default)]
    pub name: Option<String>,
    /// Per-source stale window for untimed items. Wins over the host default.
    #[serde(default)]
    pub forget_after: Option<String>,
    #[serde(flatten)]
    pub settings: Settings,
}

/// One inbox in the flattened tree, with its path from the root.
#[derive(Debug, Clone)]
pub struct Node {
    pub depth: usize,
    pub path: Vec<String>,
    pub inbox: Inbox,
}

impl Config {
    /// The `all` inbox exists even when the config names none.
    pub fn with_root(mut self) -> Self {
        if self.inbox.is_empty() {
            self.inbox.push(Inbox {
                name: "all".into(),
                ..Default::default()
            });
        }
        self
    }

    /// Ancestor chain for a path like ["all", "later"].
    pub fn chain(&self, path: &[&str]) -> Option<Vec<&Inbox>> {
        chain(&self.inbox, path)
    }

    /// Every inbox, depth first, with its path.
    pub fn nodes(&self) -> Vec<Node> {
        let mut out = Vec::new();
        walk(&self.inbox, &[], 0, &mut out);
        out
    }

    /// Every classifier spec: the root's, then each inbox's, depth first.
    pub fn classifiers(&self) -> Vec<&ClassifierSpec> {
        fn walk<'a>(inboxes: &'a [Inbox], out: &mut Vec<&'a ClassifierSpec>) {
            for ib in inboxes {
                out.extend(ib.classifier.iter());
                walk(&ib.inbox, out);
            }
        }
        let mut out: Vec<&ClassifierSpec> = self.classifier.iter().collect();
        walk(&self.inbox, &mut out);
        out
    }

    pub fn source(&self, id: &str) -> Option<&SourceSpec> {
        self.source.iter().find(|s| s.id == id)
    }

    /// Display name for a source: its `name`, else its id.
    pub fn source_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.source(id)
            .and_then(|s| s.name.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(id)
    }

    /// Labels that survive stale cleanup.
    pub fn keep(&self) -> Vec<String> {
        if self.keep.is_empty() {
            vec!["todo".into(), "later".into()]
        } else {
            self.keep.clone()
        }
    }
}

fn walk(inboxes: &[Inbox], prefix: &[String], depth: usize, out: &mut Vec<Node>) {
    for ib in inboxes {
        let mut path = prefix.to_vec();
        path.push(ib.name.clone());
        out.push(Node {
            depth,
            path: path.clone(),
            inbox: ib.clone(),
        });
        walk(&ib.inbox, &path, depth + 1, out);
    }
}

fn chain<'a>(inboxes: &'a [Inbox], path: &[&str]) -> Option<Vec<&'a Inbox>> {
    let Some((head, tail)) = path.split_first() else {
        return Some(Vec::new());
    };
    let ib = inboxes.iter().find(|i| i.name == *head)?;
    let mut out = vec![ib];
    out.extend(chain(&ib.inbox, tail)?);
    Some(out)
}

/// A question over the pile. Built from an inbox chain, answered by a store
/// or by `matches` on a single item. The two must agree; this is the one place
/// the matching rule is written down.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Question {
    /// None = any source; Some(empty) = no source, so nothing.
    pub sources: Option<Vec<String>>,
    /// Item must carry ALL of these.
    pub labels: Vec<String>,
    /// `start` must be set.
    pub timed: bool,
    pub unread: bool,
    /// RFC3339 cutoff: the item's moment must be >= this.
    pub newer_than: Option<String>,
    /// RFC3339 cutoff: the item's moment must be < this.
    pub older_than: Option<String>,
    /// Words that must all appear in the title or text. A store may match
    /// by prefix; `matches` is a plain case-insensitive contains.
    pub text: Option<String>,
    /// Rank by closeness to this vector instead of by date. A ranking, not a
    /// test: `matches` ignores it.
    pub near: Option<Vec<f32>>,
    /// At most this many. Default 20 when `near` is set, else all.
    pub limit: Option<usize>,
    /// List in `start` order instead of newest first.
    pub by_start: bool,
}

impl Question {
    /// AND every inbox in the chain into one question, as of `now`.
    pub fn of_at(chain: &[&Inbox], now: chrono::DateTime<chrono::Utc>) -> Self {
        let mut q = Question::default();
        for ib in chain {
            if !ib.sources.is_empty() {
                q.sources = Some(match q.sources.take() {
                    None => ib.sources.clone(),
                    Some(had) => had.into_iter().filter(|s| ib.sources.contains(s)).collect(),
                });
            }
            q.labels.extend(ib.labels.iter().cloned());
            q.timed |= ib.timed;
            // Most restrictive wins: latest lower bound, earliest upper bound.
            if let Some(c) = ib
                .newer_than
                .as_deref()
                .and_then(parse_duration)
                .map(|d| now - d)
            {
                q.newer_than = Some(later(q.newer_than.take(), c));
            }
            if let Some(c) = ib
                .older_than
                .as_deref()
                .and_then(parse_duration)
                .map(|d| now - d)
            {
                q.older_than = Some(earlier(q.older_than.take(), c));
            }
        }
        q.by_start = q.timed;
        q
    }

    /// As of the wall clock. For the edges; the kernel passes its own `now`.
    pub fn of(chain: &[&Inbox]) -> Self {
        Self::of_at(chain, chrono::Utc::now())
    }

    pub fn matches(&self, item: &Item) -> bool {
        let source_ok = match &self.sources {
            None => true,
            Some(s) => s.iter().any(|s| s == &item.source_id),
        };
        let labels_ok = self.labels.iter().all(|l| item.labels.contains(l));
        let timed_ok = !self.timed || item.when() != item.created_at;
        let read_ok = !self.unread || !item.read;
        let when = parse_when(item.when());
        // No parsable moment: never hide an item behind a window it cannot be placed in.
        let newer_ok = match (&self.newer_than, when) {
            (Some(c), Some(w)) => parse_when(c).is_none_or(|c| w >= c),
            _ => true,
        };
        let older_ok = match (&self.older_than, when) {
            (Some(c), Some(w)) => parse_when(c).is_none_or(|c| w < c),
            _ => true,
        };
        let text_ok = match self
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            None => true,
            Some(t) => {
                let hay = item.text().to_lowercase();
                t.split_whitespace()
                    .all(|w| hay.contains(&w.to_lowercase()))
            }
        };
        source_ok && labels_ok && timed_ok && read_ok && newer_ok && older_ok && text_ok
    }
}

fn later(had: Option<String>, c: chrono::DateTime<chrono::Utc>) -> String {
    match had.as_deref().and_then(parse_when) {
        Some(h) if h > c => had.unwrap_or_default(),
        _ => rfc3339(c),
    }
}

fn earlier(had: Option<String>, c: chrono::DateTime<chrono::Utc>) -> String {
    match had.as_deref().and_then(parse_when) {
        Some(h) if h < c => had.unwrap_or_default(),
        _ => rfc3339(c),
    }
}

pub fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// RFC3339, or a bare date as midnight UTC.
pub fn parse_when(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|n| n.and_utc())
}

/// `"14d"` or `"24h"`.
pub fn parse_duration(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix(['d', 'D']) {
        return n.trim().parse::<i64>().ok().map(chrono::Duration::days);
    }
    if let Some(n) = s.strip_suffix(['h', 'H']) {
        return n.trim().parse::<i64>().ok().map(chrono::Duration::hours);
    }
    None
}
