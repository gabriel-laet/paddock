//! A host on disk: where the config, store, and incoming directory live, how
//! the TOML config is read, and the standard set of adapters wired together.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use super::llm::LlmClassifier;
use super::script::CelClassifier;
use super::sources::{Exec, Fs, Rss};
use super::sqlite::Sqlite;
use crate::kernel::{
    Adapters, Classifier, ClassifierSpec, Config, Kernel, Source, SourceSpec, Store,
};

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

    /// `PADDOCK_DIR`, then walk up from `start` for `.paddock/`, else XDG.
    pub fn discover(start: &Path) -> Self {
        if let Some(v) = std::env::var("PADDOCK_DIR").ok().filter(|v| !v.is_empty()) {
            return Self::from_root(expand_path(&v));
        }
        if let Some(root) = find_dot_paddock(start) {
            return Self::from_root(root);
        }
        let config_dir = xdg_dir("XDG_CONFIG_HOME", ".config").join("paddock");
        let data_dir = xdg_dir("XDG_DATA_HOME", ".local/share").join("paddock");
        Self::from_dirs(config_dir, data_dir)
    }

    /// One directory holds config, store, and incoming.
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

fn find_dot_paddock(start: &Path) -> Option<PathBuf> {
    let mut cur = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
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
    std::env::var(env)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(fallback_under_home))
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_path(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if p == "~" => home(),
        None => PathBuf::from(p),
    }
}

/// Create config, data dir, incoming dir, and an empty store. Idempotent.
pub fn init(paths: &Paths) -> Result<()> {
    for dir in [&paths.config_dir, &paths.data_dir, &paths.incoming_dir] {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    if !paths.config_file.exists() {
        let text = default_config_toml(&paths.incoming_dir.display().to_string());
        fs::write(&paths.config_file, text)
            .with_context(|| format!("create {}", paths.config_file.display()))?;
    }
    Sqlite::open(&paths.db_path)?;
    Ok(())
}

/// Read the config and open the store, initializing the host first if needed.
pub fn load(paths: &Paths) -> Result<(Config, Sqlite)> {
    if !paths.config_file.exists() {
        init(paths)?;
    }
    fs::create_dir_all(&paths.data_dir)?;
    fs::create_dir_all(&paths.incoming_dir)?;
    Ok((
        load_config(&paths.config_file)?,
        Sqlite::open(&paths.db_path)?,
    ))
}

pub fn load_config(path: &Path) -> Result<Config> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parse config {}", path.display()))?;
    Ok(config.with_root())
}

/// A kernel over the standard adapters.
pub fn kernel<'a>(config: &'a Config, store: &'a dyn Store) -> Kernel<'a> {
    Kernel::new(config, store, &STD)
}

/// fs, rss, and exec sources; the script (CEL) and llm classifiers.
pub struct Std;

pub static STD: Std = Std;

impl Adapters for Std {
    fn source(&self, spec: &SourceSpec) -> Result<Box<dyn Source>> {
        let id = spec.id.clone();
        let need = |field: Option<&str>, what: &str| {
            field
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("source {id} {} needs {what}", spec.kind))
        };
        Ok(match spec.kind.as_str() {
            "fs" => Box::new(Fs {
                id: spec.id.clone(),
                dir: expand_path(&need(spec.path.as_deref(), "path")?),
            }),
            "rss" => Box::new(Rss {
                id: spec.id.clone(),
                url: need(spec.url.as_deref(), "url")?,
            }),
            "exec" => Box::new(Exec {
                id: spec.id.clone(),
                cmd: expand_path(&need(spec.cmd.as_deref(), "cmd")?),
                args: spec.args.clone(),
                dir: spec
                    .dir
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(expand_path),
            }),
            other => bail!("unknown source kind `{other}` on {}", spec.id),
        })
    }

    fn classifier(&self, spec: &ClassifierSpec) -> Result<Box<dyn Classifier>> {
        match spec.kind.as_str() {
            "script" => Ok(Box::new(CelClassifier::new(spec)?)),
            "llm" => Ok(Box::new(LlmClassifier::new(spec))),
            other => bail!("unknown classifier kind `{other}` (regex, script, llm)"),
        }
    }
}

pub fn default_config_toml(incoming: &str) -> String {
    format!(
        r#"# paddock — inboxes nest. a child is a tighter question over its parent.
# classifiers belong to an inbox and run when an item enters it.
# a label change re-runs classify so children can fire (classify-on-enter).

keep = ["todo", "later"]
# forget_after = "30d"   # optional host default for untimed

[[inbox]]
name = "all"

[[inbox.classifier]]
id = "flag-rfc"
kind = "regex"
pattern = "(?i)rfc"
label = "rfc"

[[inbox.classifier]]
id = "flag-todo"
kind = "regex"
pattern = "(?i)todo"
label = "todo"

[[inbox.inbox]]
name = "later"
labels = ["later"]

[[inbox.inbox]]
name = "todo"
labels = ["todo"]

[[inbox.inbox]]
name = "cal"
timed = true

[[source]]
id = "incoming"
kind = "fs"
path = {incoming}
"#,
        incoming = toml_string(incoming)
    )
}

fn toml_string(s: &str) -> String {
    let escaped: String = s
        .chars()
        .flat_map(|c| match c {
            '\\' | '"' => vec!['\\', c],
            _ => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}
