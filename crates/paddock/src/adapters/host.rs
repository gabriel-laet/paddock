//! A host on disk: where the config, store, and incoming directory live, how
//! the TOML config is read, and how a config becomes a resolved kernel.
//! This is the only place adapters are chosen.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::source::{Exec, Fs};
use super::store::Sqlite;
use super::transport::{resolve_cmds, secret};
use super::{classifier, embedder, model};
use crate::kernel::{setting, setting_list, Config, Kernel, Source, SourceSpec, Store};

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
    open_store(paths, &load_config(&paths.config_file)?)?;
    Ok(())
}

/// The store the config names: sqlite, encrypted when `[store]` has a
/// `key` or a `key_cmd`.
pub fn open_store(paths: &Paths, config: &Config) -> Result<Sqlite> {
    let spec = config.store.clone().unwrap_or_default();
    match spec.kind.trim() {
        "" | "sqlite" => {}
        other => bail!("unknown store kind `{other}` (sqlite)"),
    }
    let key = store_key(&spec.settings)?;
    Sqlite::open(&paths.db_path, key.as_deref())
}

/// The store's encryption key from `[store]`: `key`, or `key_cmd` run.
pub fn store_key(settings: &crate::kernel::Settings) -> Result<Option<String>> {
    secret(settings, "key")
}

/// Read the config and open the store, initializing the host first if needed.
pub fn load(paths: &Paths) -> Result<(Config, Sqlite)> {
    if !paths.config_file.exists() {
        init(paths)?;
    }
    fs::create_dir_all(&paths.data_dir)?;
    fs::create_dir_all(&paths.incoming_dir)?;
    let config = load_config(&paths.config_file)?;
    let store = open_store(paths, &config)?;
    Ok((config, store))
}

/// Read a config; its skills come from the `skills/` beside it.
pub fn load_config(path: &Path) -> Result<Config> {
    load_config_in(path, path.parent().unwrap_or(Path::new(".")))
}

/// Read a config file that may live anywhere (a candidate for `replay`),
/// with skills resolved against `config_dir`, the host's.
pub fn load_config_in(path: &Path, config_dir: &Path) -> Result<Config> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parse config {}", path.display()))?;
    Ok(super::skills::graft(config, config_dir)?.with_root())
}

/// Resolve every spec in the config into a live adapter and hand the kernel
/// the lot, as of now. A bad spec fails here, before any verb runs.
pub fn kernel<'a>(config: &'a Config, store: &'a dyn Store) -> Result<Kernel<'a>> {
    kernel_at(config, store, chrono::Utc::now())
}

pub fn kernel_at<'a>(
    config: &'a Config,
    store: &'a dyn Store,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Kernel<'a>> {
    let sources = config
        .source
        .iter()
        .map(|spec| Ok((spec.id.clone(), source(spec)?)))
        .collect::<Result<Vec<_>>>()?;
    let classifiers = config
        .classifiers()
        .into_iter()
        .map(|spec| Ok((spec.id.clone(), classifier::build(spec)?)))
        .collect::<Result<HashMap<_, _>>>()?;
    let embedder = config.embedder.as_ref().map(embedder::build).transpose()?;
    let model = config.model.as_ref().map(model::build).transpose()?;
    Ok(Kernel {
        config,
        store,
        sources,
        classifiers,
        embedder,
        model,
        now,
    })
}

/// fs, exec, or a plugin: any other kind is `paddock-<kind>` on PATH,
/// spoken to over the exec protocol with the spec's settings. A program
/// gets every `NAME_cmd` setting already run, as `NAME`.
fn source(spec: &SourceSpec) -> Result<Box<dyn Source>> {
    let s = &spec.settings;
    let need = |key: &str| {
        setting(s, key)
            .ok_or_else(|| anyhow::anyhow!("source {} {} needs {key}", spec.id, spec.kind))
    };
    let settings = || resolve_cmds(s).with_context(|| format!("source {}", spec.id));
    Ok(match spec.kind.as_str() {
        "fs" => Box::new(Fs {
            id: spec.id.clone(),
            dir: expand_path(&need("path")?),
        }),
        "exec" => Box::new(Exec {
            id: spec.id.clone(),
            cmd: expand_path(&need("cmd")?),
            args: setting_list(s, "args"),
            dir: setting(s, "dir").map(|d| expand_path(&d)),
            settings: settings()?,
        }),
        "" => bail!("source {} has no kind", spec.id),
        plugin => Box::new(Exec {
            id: spec.id.clone(),
            cmd: PathBuf::from(format!("paddock-{plugin}")),
            args: Vec::new(),
            dir: None,
            settings: settings()?,
        }),
    })
}

/// The store's mirror, if `[store.mirror]` names one.
pub fn mirror(config: &Config) -> Result<Option<super::mirror::Mirror>> {
    let spec = config.store.clone().unwrap_or_default();
    super::mirror::build(&spec.settings)
}

/// Whether `pull` should push the store to its mirror when done.
pub fn mirror_after_pull(config: &Config) -> bool {
    config
        .store
        .as_ref()
        .is_some_and(|s| super::mirror::after_pull(&s.settings))
}

pub fn default_config_toml(incoming: &str) -> String {
    format!(
        r#"# paddock — inboxes nest. a child is a tighter question over its parent.
# classifiers belong to an inbox and run when an item enters it.
# a label change re-runs classify so children can fire (classify-on-enter).

# skills (`paddock skills`) are fragments of classifiers and inboxes; `use` grafts them.

keep = ["todo", "later"]
# forget_after = "30d"   # optional host default for untimed
me = []                  # who you are across sources: "you@example.com", a Slack id, a WhatsApp jid
use = ["codes", "mentions"]   # page on one-time codes and on being mentioned
# notify_cmd = "notify-send 'paddock {{inbox}}' {{title}}"

[[inbox]]
name = "all"

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

/// Every `NAME_cmd` anywhere in the config run once, so a long-lived
/// caller (an app) does not run `pass` on every verb. The result lives in
/// memory only; the file on disk keeps its commands.
pub fn resolve_secrets(mut config: Config) -> Result<Config> {
    fn walk(inboxes: &mut [crate::kernel::Inbox]) -> Result<()> {
        for ib in inboxes {
            for c in &mut ib.classifier {
                c.settings = resolve_cmds(&c.settings)?;
            }
            walk(&mut ib.inbox)?;
        }
        Ok(())
    }
    for s in &mut config.source {
        s.settings = resolve_cmds(&s.settings).with_context(|| format!("source {}", s.id))?;
    }
    for c in &mut config.classifier {
        c.settings = resolve_cmds(&c.settings)?;
    }
    walk(&mut config.inbox)?;
    for spec in [
        &mut config.embedder,
        &mut config.model,
        &mut config.store,
        &mut config.agent,
    ]
    .into_iter()
    .flatten()
    {
        spec.settings = resolve_cmds(&spec.settings)?;
    }
    Ok(config)
}

/// Snapshot the store beside itself and hand the snapshot to the mirror.
/// Returns where it went.
pub fn push_mirror(paths: &Paths, config: &Config, store: &Sqlite) -> Result<String> {
    let m = mirror(config)?.ok_or_else(|| anyhow::anyhow!("no [store.mirror] in config"))?;
    let snapshot = paths.db_path.with_extension("db.snapshot");
    store.snapshot(&snapshot)?;
    let pushed = m.push(&snapshot);
    let _ = fs::remove_file(&snapshot);
    pushed?;
    Ok(m.describe())
}
