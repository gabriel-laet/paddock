use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::Item;

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub incoming_dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Self {
        let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::discover(&start)
    }

    /// PADDOCK_DIR, then walk up from `start` for `.paddock/`, else XDG.
    pub fn discover(start: &Path) -> Self {
        if let Ok(v) = std::env::var("PADDOCK_DIR") {
            if !v.is_empty() {
                return Self::from_root(expand_path(&v));
            }
        }
        if let Some(root) = find_paddock_dir(start) {
            return Self::from_root(root);
        }
        let config_dir = xdg_dir("XDG_CONFIG_HOME", ".config").join("paddock");
        let data_dir = xdg_dir("XDG_DATA_HOME", ".local/share").join("paddock");
        Self::from_dirs(config_dir, data_dir)
    }

    /// Host root is `.paddock/` or `$PADDOCK_DIR`: config, db, incoming, themes live together.
    pub fn from_root(root: PathBuf) -> Self {
        Self {
            config_file: root.join("config.toml"),
            incoming_dir: root.join("incoming"),
            db_path: root.join("paddock.db"),
            config_dir: root.clone(),
            data_dir: root,
        }
    }

    pub fn here(cwd: &Path) -> Self {
        Self::from_root(cwd.join(".paddock"))
    }

    pub fn from_dirs(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_file: config_dir.join("config.toml"),
            incoming_dir: data_dir.join("incoming"),
            db_path: data_dir.join("paddock.db"),
            config_dir,
            data_dir,
        }
    }
}

fn find_paddock_dir(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    if let Ok(c) = cur.canonicalize() {
        cur = c;
    }
    loop {
        if cur.file_name().is_some_and(|n| n == ".paddock") && cur.is_dir() {
            return Some(cur);
        }
        let candidate = cur.join(".paddock");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !cur.pop() {
            return None;
        }
    }
}

fn xdg_dir(env: &str, fallback_under_home: &str) -> PathBuf {
    if let Ok(v) = std::env::var(env) {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(fallback_under_home)
}

pub fn expand_path(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest);
    }
    PathBuf::from(p)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub inbox: Vec<InboxConfig>,
    #[serde(default)]
    pub source: Vec<SourceConfig>,
    /// Classifiers on the implicit root (the whole pile).
    #[serde(default)]
    pub classifier: Vec<ClassifierConfig>,
    /// Theme name. File lives at `$config_dir/themes/<name>.toml`.
    #[serde(default)]
    pub theme: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct InboxConfig {
    pub name: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    /// "list" | "calendar" | "board". Missing/unknown → list.
    #[serde(default)]
    pub view: Option<String>,
    /// Board columns (label names). Ignored unless view = "board".
    #[serde(default)]
    pub columns: Vec<String>,
    /// If true, match only items that have `start` set.
    #[serde(default)]
    pub timed: bool,
    #[serde(default)]
    pub classifier: Vec<ClassifierConfig>,
    #[serde(default)]
    pub inbox: Vec<InboxConfig>,
}

impl InboxConfig {
    /// Kernel view: list, calendar, or board.
    pub fn view_kind(&self) -> &str {
        match self.view.as_deref().map(str::trim) {
            Some("calendar") => "calendar",
            Some("board") => "board",
            _ => "list",
        }
    }

    /// First configured board column whose label the item has.
    pub fn board_column<'a>(&'a self, item: &Item) -> Option<&'a str> {
        if self.view_kind() != "board" {
            return None;
        }
        self.columns
            .iter()
            .find(|c| item.labels.iter().any(|l| l == *c))
            .map(|s| s.as_str())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ClassifierConfig {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Rhai script. Required for kind = "script".
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// "ollama" | "openai". Default: openai if a key is set, else ollama.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    /// Allow-list for kind = "llm". Model must pick one or NONE.
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SourceConfig {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Command for kind = "exec".
    #[serde(default)]
    pub cmd: Option<String>,
    /// Extra args before the verb (`pull` / `send`).
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory for kind = "exec".
    #[serde(default)]
    pub dir: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parse config {}", path.display()))?;
        if cfg.inbox.is_empty() {
            let mut cfg = cfg;
            cfg.inbox.push(InboxConfig {
                name: "all".into(),
                ..Default::default()
            });
            return Ok(cfg);
        }
        Ok(cfg)
    }

    /// Ancestor chain from top-level inboxes for a path like ["all","later"].
    pub fn find_chain(&self, path: &[&str]) -> Option<Vec<&InboxConfig>> {
        find_chain(&self.inbox, path)
    }

    pub fn flatten(&self) -> Vec<TreeNode> {
        let mut out = Vec::new();
        flatten(&self.inbox, &[], 0, &mut out);
        out
    }
}

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub depth: usize,
    pub path: Vec<String>,
    pub inbox: InboxConfig,
}

fn flatten(inboxes: &[InboxConfig], prefix: &[String], depth: usize, out: &mut Vec<TreeNode>) {
    for ib in inboxes {
        let mut path = prefix.to_vec();
        path.push(ib.name.clone());
        out.push(TreeNode {
            depth,
            path: path.clone(),
            inbox: ib.clone(),
        });
        flatten(&ib.inbox, &path, depth + 1, out);
    }
}

fn find_chain<'a>(inboxes: &'a [InboxConfig], path: &[&str]) -> Option<Vec<&'a InboxConfig>> {
    if path.is_empty() {
        return Some(Vec::new());
    }
    let (head, tail) = path.split_first()?;
    let ib = inboxes.iter().find(|i| i.name == *head)?;
    let mut chain = vec![ib];
    if !tail.is_empty() {
        chain.extend(find_chain(&ib.inbox, tail)?);
    }
    Some(chain)
}

/// Item matches an inbox if (sources empty OR source in list)
/// AND (labels empty OR item has ALL listed labels)
/// AND (not timed OR item.start is set).
pub fn inbox_matches(inbox: &InboxConfig, item: &Item) -> bool {
    let source_ok =
        inbox.sources.is_empty() || inbox.sources.iter().any(|s| s == &item.source_id);
    let labels_ok = inbox.labels.is_empty()
        || inbox.labels.iter().all(|l| item.labels.iter().any(|x| x == l));
    let timed_ok = !inbox.timed || item.start.is_some();
    source_ok && labels_ok && timed_ok
}

pub fn chain_matches(chain: &[&InboxConfig], item: &Item) -> bool {
    chain.iter().all(|ib| inbox_matches(ib, item))
}
