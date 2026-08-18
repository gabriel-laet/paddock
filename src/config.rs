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
        let config_dir = xdg_dir("XDG_CONFIG_HOME", ".config").join("paddock");
        let data_dir = xdg_dir("XDG_DATA_HOME", ".local/share").join("paddock");
        Self::from_dirs(config_dir, data_dir)
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
    #[serde(default)]
    pub classifier: Vec<ClassifierConfig>,
    #[serde(default)]
    pub inbox: Vec<InboxConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ClassifierConfig {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SourceConfig {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
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
/// AND (labels empty OR item has ALL listed labels).
pub fn inbox_matches(inbox: &InboxConfig, item: &Item) -> bool {
    let source_ok =
        inbox.sources.is_empty() || inbox.sources.iter().any(|s| s == &item.source_id);
    let labels_ok = inbox.labels.is_empty()
        || inbox.labels.iter().all(|l| item.labels.iter().any(|x| x == l));
    source_ok && labels_ok
}

pub fn chain_matches(chain: &[&InboxConfig], item: &Item) -> bool {
    chain.iter().all(|ib| inbox_matches(ib, item))
}
