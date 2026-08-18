use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::classify::run_classifier;
use crate::config::{chain_matches, expand_path, Config, InboxConfig, Paths};
use crate::source::{item_from_file, pull_fs, pull_rss, NewItem};
use crate::store::{Item, Store};

/// Generation bump after store mutations from the watcher (TUI polls this).
pub static GEN: AtomicU64 = AtomicU64::new(0);

pub fn bump() {
    GEN.fetch_add(1, Ordering::Relaxed);
}

pub fn gen() -> u64 {
    GEN.load(Ordering::Relaxed)
}

pub fn admit(store: &Store, config: &Config, item: NewItem) -> Result<Option<i64>> {
    let Some(id) = store.insert_new(&item)? else {
        return Ok(None);
    };
    classify_item(store, config, id)?;
    bump();
    Ok(Some(id))
}

pub fn admit_file(store: &Store, config: &Config, source_id: &str, path: &Path) -> Result<Option<i64>> {
    if !path.is_file() {
        return Ok(None);
    }
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(true)
    {
        return Ok(None);
    }
    let new = item_from_file(source_id, path)?;
    if let Some(id) = store.insert_new(&new)? {
        classify_item(store, config, id)?;
        bump();
        return Ok(Some(id));
    }
    store.update_body(
        &new.source_id,
        &new.foreign_id,
        &new.title,
        &new.body,
        new.href.as_deref(),
    )?;
    if let Some(id) = store.id_by_foreign(&new.source_id, &new.foreign_id)? {
        classify_item(store, config, id)?;
        bump();
        return Ok(Some(id));
    }
    bump();
    Ok(None)
}

/// Toggle (or add/remove) a label, then classify so child inboxes can fire.
pub fn relabel(store: &Store, config: &Config, id: i64, label: &str) -> Result<bool> {
    let on = store.toggle_label(id, label)?;
    classify_item(store, config, id)?;
    bump();
    Ok(on)
}

/// Add a label if missing, then classify.
pub fn stamp(store: &Store, config: &Config, id: i64, label: &str) -> Result<()> {
    store.add_label(id, label)?;
    classify_item(store, config, id)?;
    bump();
    Ok(())
}

/// admit → enter root → classify → match children → classify → recurse.
pub fn classify_item(store: &Store, config: &Config, id: i64) -> Result<()> {
    let mut item = store.get(id)?;
    apply_classifiers(store, &config.classifier, &mut item)?;
    for inbox in &config.inbox {
        walk_inbox(store, inbox, &mut item)?;
    }
    Ok(())
}

fn walk_inbox(store: &Store, inbox: &InboxConfig, item: &mut Item) -> Result<()> {
    if !crate::config::inbox_matches(inbox, item) {
        return Ok(());
    }
    apply_classifiers(store, &inbox.classifier, item)?;
    for child in &inbox.inbox {
        walk_inbox(store, child, item)?;
    }
    Ok(())
}

fn apply_classifiers(store: &Store, classifiers: &[crate::config::ClassifierConfig], item: &mut Item) -> Result<()> {
    for cfg in classifiers {
        if let Some(label) = run_classifier(cfg, item)? {
            if !item.labels.iter().any(|l| l == &label) {
                store.add_label(item.id, &label)?;
                item.labels.push(label);
            }
        }
    }
    Ok(())
}

pub fn pull_all(store: &Store, config: &Config) -> Result<usize> {
    let mut n = 0;
    for src in &config.source {
        let batch = match src.kind.as_str() {
            "fs" => {
                let path = src
                    .path
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("source {} fs needs path", src.id))?;
                pull_fs(&src.id, &expand_path(path))?
            }
            "rss" => {
                let url = src
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("source {} rss needs url", src.id))?;
                pull_rss(&src.id, url)?
            }
            other => {
                anyhow::bail!("unknown source kind `{other}` on {}", src.id);
            }
        };
        for item in batch {
            if admit(store, config, item)?.is_some() {
                n += 1;
            }
        }
    }
    Ok(n)
}

pub fn items_in_chain(store: &Store, chain: &[&InboxConfig]) -> Result<Vec<Item>> {
    let all = store.list_all()?;
    Ok(all
        .into_iter()
        .filter(|it| chain_matches(chain, it))
        .collect())
}

pub struct WatchGuard {
    _watcher: RecommendedWatcher,
    _thread: thread::JoinHandle<()>,
}

/// Watch every fs source directory. Debounced admit on create/modify.
pub fn spawn_fs_watch(store: Store, paths: Paths) -> Result<WatchGuard> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    let config = Config::load(&paths.config_file).unwrap_or_default();
    let mut watched: HashSet<PathBuf> = HashSet::new();
    for src in &config.source {
        if src.kind != "fs" {
            continue;
        }
        if let Some(p) = src.path.as_deref() {
            let dir = expand_path(p);
            if dir.is_dir() {
                watcher.watch(&dir, RecursiveMode::NonRecursive)?;
                watched.insert(dir);
            }
        }
    }
    if watched.is_empty() && paths.incoming_dir.is_dir() {
        watcher.watch(&paths.incoming_dir, RecursiveMode::NonRecursive)?;
    }

    let thread = thread::spawn(move || {
        let debounce = Duration::from_millis(180);
        loop {
            let ev = match rx.recv() {
                Ok(v) => v,
                Err(_) => break,
            };
            let mut pending: HashSet<PathBuf> = HashSet::new();
            collect_paths(&ev, &mut pending);
            thread::sleep(debounce);
            while let Ok(more) = rx.try_recv() {
                collect_paths(&more, &mut pending);
            }
            let cfg = match Config::load(&paths.config_file) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for path in pending {
                if let Some(sid) = source_for_path(&cfg, &path, &paths) {
                    let _ = admit_file(&store, &cfg, &sid, &path);
                }
            }
        }
    });

    Ok(WatchGuard {
        _watcher: watcher,
        _thread: thread,
    })
}

fn collect_paths(ev: &notify::Result<notify::Event>, out: &mut HashSet<PathBuf>) {
    let Ok(event) = ev else { return };
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any => {}
        _ => return,
    }
    for p in &event.paths {
        out.insert(p.clone());
    }
}

fn source_for_path(cfg: &Config, path: &Path, paths: &Paths) -> Option<String> {
    let parent = path.parent()?;
    for src in &cfg.source {
        if src.kind != "fs" {
            continue;
        }
        if let Some(p) = src.path.as_deref() {
            let dir = expand_path(p);
            if parent == dir.as_path() || path.starts_with(&dir) && path.parent() == Some(dir.as_path()) {
                return Some(src.id.clone());
            }
        }
    }
    if parent == paths.incoming_dir.as_path() {
        return Some("incoming".into());
    }
    None
}
