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
use crate::source::{item_from_file, pull_fs, pull_rss, Draft, NewItem};
use crate::store::{Item, Store};

/// Generation bump after store mutations from the watcher (TUI polls this).
pub static GEN: AtomicU64 = AtomicU64::new(0);

pub fn bump() {
    GEN.fetch_add(1, Ordering::Relaxed);
}

pub fn gen() -> u64 {
    GEN.load(Ordering::Relaxed)
}

pub fn admit(store: &Store, config: &Config, item: NewItem) -> Result<i64> {
    let (id, _) = store.upsert(&item)?;
    classify_item(store, config, id)?;
    bump();
    Ok(id)
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
    Ok(Some(admit(store, config, new)?))
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
            let existed = store
                .id_by_foreign(&item.source_id, &item.foreign_id)?
                .is_some();
            admit(store, config, item)?;
            if !existed {
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


/// First `kind=fs` source, else "incoming".
pub fn default_send_source(config: &Config) -> String {
    config
        .source
        .iter()
        .find(|s| s.kind == "fs")
        .map(|s| s.id.clone())
        .unwrap_or_else(|| "incoming".into())
}

pub fn source_can_send(config: &Config, source_id: &str) -> bool {
    match config
        .source
        .iter()
        .find(|s| s.id == source_id)
        .map(|s| s.kind.as_str())
    {
        Some("rss") => false,
        _ => true,
    }
}

pub fn reply_title(parent: &Item) -> String {
    let t = parent.title.trim();
    if t.is_empty() {
        "re:".into()
    } else if t.to_ascii_lowercase().starts_with("re:") {
        t.to_string()
    } else {
        format!("re: {t}")
    }
}

pub fn sanitize_filename(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("md");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for n in 2..1000 {
        let p = parent.join(format!("{stem}-{n}.{ext}"));
        if !p.exists() {
            return p;
        }
    }
    path.to_path_buf()
}

/// Persist a draft. The source sends if it can (`fs` writes a file; unknown admits locally).
pub fn send_draft(store: &Store, config: &Config, paths: &Paths, draft: Draft) -> Result<i64> {
    let source_id = if draft.source_id.is_empty() {
        default_send_source(config)
    } else {
        draft.source_id.clone()
    };
    let kind = config
        .source
        .iter()
        .find(|s| s.id == source_id)
        .map(|s| s.kind.as_str());
    if kind == Some("rss") {
        anyhow::bail!("source cannot send");
    }

    let mut thread = draft.thread.clone();
    let mut reply_foreign = None;
    if let Some(pid) = draft.reply_to {
        let parent = store.get(pid)?;
        reply_foreign = Some(parent.foreign_id.clone());
        let th = thread
            .clone()
            .or(parent.thread.clone())
            .unwrap_or_else(|| format!("{}:{}", parent.source_id, parent.foreign_id));
        if parent.thread.is_none() {
            store.set_thread(pid, Some(&th))?;
        }
        thread = Some(th);
    }

    match kind {
        Some("fs") => {
            std::fs::create_dir_all(&paths.incoming_dir)?;
            let stem = sanitize_filename(&draft.title);
            let dest = unique_path(&paths.incoming_dir.join(format!("{stem}.md")));
            std::fs::write(&dest, draft.body.as_bytes())?;
            let mut new = item_from_file(&source_id, &dest)?;
            new.thread = thread;
            new.in_reply_to = reply_foreign;
            new.to = draft.to.clone();
            if let Some(fid) = draft.foreign_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                new.foreign_id = fid.to_string();
            }
            Ok(admit(store, config, new)?)
        }
        _ => {
            let stamp = chrono::Utc::now().timestamp_millis();
            let foreign = draft
                .foreign_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}-{stamp}", sanitize_filename(&draft.title)));
            let new = NewItem {
                source_id,
                foreign_id: foreign,
                title: if draft.title.trim().is_empty() {
                    "untitled".into()
                } else {
                    draft.title.clone()
                },
                body: draft.body.clone(),
                href: None,
                start: None,
                end: None,
                thread,
                parts: draft.parts.clone(),
                from: None,
                to: draft.to.clone(),
                in_reply_to: reply_foreign,
                forward_of: None,
                cite_excerpt: None,
                cite_actor: None,
            };
            Ok(admit(store, config, new)?)
        }
    }
}
