use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::classify::run_classifier;
use crate::config::{expand_path, Config, InboxConfig, Paths, SourceConfig};
use crate::source::{item_from_file, pull_exec, pull_fs, pull_rss, send_exec, Draft, NewItem};
use crate::store::{Item, ItemFilter, StaleHint, Store};

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
            "exec" => {
                let (cmd, dir) = exec_cmd_dir(src)?;
                pull_exec(&src.id, &cmd, &src.args, dir.as_deref())?
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
    store.list_filtered(&filter_for_chain(chain))
}

/// AND each inbox in the chain into one SQL filter.
pub fn filter_for_chain(chain: &[&InboxConfig]) -> ItemFilter {
    let mut sources: Option<Vec<String>> = None;
    let mut labels = Vec::new();
    let mut timed = false;
    for ib in chain {
        if !ib.sources.is_empty() {
            sources = Some(match sources.take() {
                None => ib.sources.clone(),
                Some(existing) => existing
                    .into_iter()
                    .filter(|s| ib.sources.iter().any(|x| x == s))
                    .collect(),
            });
        }
        labels.extend(ib.labels.iter().cloned());
        if ib.timed {
            timed = true;
        }
    }
    ItemFilter {
        sources,
        labels,
        timed,
        unread_only: false,
        order_by_start: chain.last().is_some_and(|ib| ib.view_kind() == "calendar"),
    }
}

/// Always deletes the item (user asked to forget it).
pub fn forget(store: &Store, id: i64) -> Result<bool> {
    let gone = store.delete(id)?;
    if gone {
        bump();
    }
    Ok(gone)
}

/// Drop stale items. Keep-labels never auto-forget.
pub fn forget_stale(store: &Store, config: &Config) -> Result<usize> {
    let keep = keep_labels(config);
    let hints = store.list_stale_hints()?;
    let mut n = 0usize;
    let now = chrono::Utc::now();
    for item in hints {
        if item.labels.iter().any(|l| keep.iter().any(|k| k == l)) {
            continue;
        }
        if should_forget_stale(&item, config, now) && forget(store, item.id)? {
            n += 1;
        }
    }
    Ok(n)
}

fn keep_labels(config: &Config) -> Vec<String> {
    if config.keep.is_empty() {
        vec!["todo".into(), "later".into()]
    } else {
        config.keep.clone()
    }
}

fn should_forget_stale(item: &StaleHint, config: &Config, now: chrono::DateTime<chrono::Utc>) -> bool {
    let timed = nonempty(item.start.as_deref()).is_some() || nonempty(item.end.as_deref()).is_some();
    if timed {
        let raw = nonempty(item.end.as_deref()).or_else(|| nonempty(item.start.as_deref()));
        return match raw.and_then(parse_when) {
            Some(dt) => dt < now,
            None => false,
        };
    }
    let after = config
        .source
        .iter()
        .find(|s| s.id == item.source_id)
        .and_then(|s| s.forget_after.as_deref())
        .or(config.forget_after.as_deref());
    let Some(after) = after.and_then(parse_duration) else {
        return false;
    };
    match parse_when(&item.created_at) {
        Some(created) => now.signed_duration_since(created) > after,
        None => false,
    }
}

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

fn parse_when(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|n| n.and_utc())
}

fn parse_duration(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix(['d', 'D']) {
        return n.trim().parse::<i64>().ok().map(chrono::Duration::days);
    }
    if let Some(n) = s.strip_suffix(['h', 'H']) {
        return n.trim().parse::<i64>().ok().map(chrono::Duration::hours);
    }
    None
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

fn exec_cmd_dir(src: &SourceConfig) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    let cmd = src
        .cmd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("source {} exec needs cmd", src.id))?;
    let dir = src
        .dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(expand_path);
    Ok((expand_path(cmd), dir))
}

/// Persist a draft. fs writes a file; exec runs the source command; rss cannot send; unknown admits locally.
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
        Some("exec") => {
            let src = config
                .source
                .iter()
                .find(|s| s.id == source_id)
                .ok_or_else(|| anyhow::anyhow!("source {source_id} not found"))?;
            let (cmd, dir) = exec_cmd_dir(src)?;
            let result = send_exec(
                &source_id,
                &cmd,
                &src.args,
                dir.as_deref(),
                &draft,
                reply_foreign.as_deref(),
            )?;
            let new = NewItem {
                source_id,
                foreign_id: result.foreign_id,
                title: if draft.title.trim().is_empty() {
                    "untitled".into()
                } else {
                    draft.title.clone()
                },
                body: draft.body.clone(),
                href: None,
                start: result.start,
                end: result.end,
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


/// One list row after thread collapse. `item` is the latest in the thread.
#[derive(Debug, Clone)]
pub struct ListRow {
    pub item: Item,
    pub count: usize,
}

/// Collapse items that share a non-empty `thread` into one row.
/// Empty/missing thread stays a singleton. Same thread keeps the newest
/// `created_at` (then highest id). Surviving heads stay in input order
/// (store lists are already newest-first, or start order for calendar).
pub fn collapse_threads(items: Vec<Item>) -> Vec<ListRow> {
    let mut rows: Vec<ListRow> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for item in items {
        let key = item
            .thread
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        match key {
            None => rows.push(ListRow { item, count: 1 }),
            Some(th) => {
                if let Some(&i) = at.get(&th) {
                    rows[i].count += 1;
                    if is_newer_item(&item, &rows[i].item) {
                        rows[i].item = item;
                    }
                } else {
                    at.insert(th, rows.len());
                    rows.push(ListRow { item, count: 1 });
                }
            }
        }
    }
    rows
}

fn is_newer_item(a: &Item, b: &Item) -> bool {
    match a.created_at.cmp(&b.created_at) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => a.id > b.id,
        std::cmp::Ordering::Less => false,
    }
}

/// First non-empty line of body, whitespace collapsed.
pub fn body_snippet(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    collapse_ws(line)
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in s.chars() {
        if c.is_whitespace() {
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        }
    }
    out
}

fn actor_label(actor: Option<&crate::store::Actor>) -> Option<String> {
    let a = actor?;
    let name = a
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(n) = name {
        return Some(n.to_string());
    }
    let id = a.id.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Who + text for a list row (chat: title is the thread name; mail: title is the subject).
pub fn row_who_text(item: &Item) -> (String, String) {
    let from = actor_label(item.from.as_ref());
    let title = item.title.trim();
    let snippet = body_snippet(&item.body);
    if let Some(ref from) = from {
        if !title.is_empty() && title != from.as_str() {
            let text = if snippet.is_empty() {
                String::new()
            } else {
                format!("{from}: {snippet}")
            };
            return (title.to_string(), text);
        }
    }
    let who = from.unwrap_or_else(|| title.to_string());
    let text = if snippet.is_empty() {
        title.to_string()
    } else {
        snippet
    };
    (who, text)
}

/// Board / calendar prefix in front of the who/snippet.
pub fn view_prefix(inbox: Option<&InboxConfig>, item: &Item) -> String {
    match inbox.map(|ib| ib.view_kind()) {
        Some("board") => format!(
            "[{}] ",
            inbox.and_then(|ib| ib.board_column(item)).unwrap_or("—")
        ),
        Some("calendar") => item
            .start
            .as_deref()
            .map(|s| format!("{} ", s.chars().take(10).collect::<String>()))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Display width: wide glyphs (CJK, emoji) count as 2.
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_display_width).sum()
}

fn char_display_width(c: char) -> usize {
    match c {
        '\u{00AD}' => 0,
        '\u{0300}'..='\u{036F}' | '\u{20D0}'..='\u{20FF}' => 0,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' => 0,
        '\u{FE00}'..='\u{FE0F}' | '\u{FE20}'..='\u{FE2F}' => 0,
        c if c.is_control() => 0,
        '\u{1100}'..='\u{115F}'
        | '\u{2329}'..='\u{232A}'
        | '\u{2E80}'..='\u{A4CF}'
        | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FE10}'..='\u{FE19}'
        | '\u{FE30}'..='\u{FE6F}'
        | '\u{FF01}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}'
        | '\u{2190}'..='\u{21FF}'
        | '\u{2300}'..='\u{23FF}'
        | '\u{2460}'..='\u{24FF}'
        | '\u{25A0}'..='\u{27BF}'
        | '\u{2900}'..='\u{297F}'
        | '\u{2B00}'..='\u{2BFF}'
        | '\u{1F000}'..='\u{1FAFF}' => 2,
        _ => 1,
    }
}

pub fn trunc_width(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) <= max {
        return s.to_string();
    }
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = char_display_width(c);
        if w + cw > budget {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.push('…');
    out
}

pub fn pad_width(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width {
        trunc_width(s, width)
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

/// Show an inbox in the tree: always `all`, always the selected path (and ancestors),
/// hide empty nodes, and hide children of a hidden parent.
pub fn inbox_visible(
    name: &str,
    path: &[String],
    total: usize,
    selected: &[String],
    parent_hidden: bool,
) -> bool {
    if name == "all" {
        return true;
    }
    if !selected.is_empty() && selected.starts_with(path) {
        return true;
    }
    if parent_hidden {
        return false;
    }
    total > 0
}

pub fn filter_visible_inboxes(
    nodes: &[crate::config::TreeNode],
    selected: &[String],
    total_of: impl Fn(&[String]) -> usize,
) -> Vec<crate::config::TreeNode> {
    let mut hidden: Vec<Vec<String>> = Vec::new();
    let mut out = Vec::new();
    for n in nodes {
        let parent_hidden = hidden
            .iter()
            .any(|p| n.path.starts_with(p) && n.path.len() > p.len());
        if inbox_visible(&n.inbox.name, &n.path, total_of(&n.path), selected, parent_hidden)
        {
            out.push(n.clone());
        } else {
            hidden.push(n.path.clone());
        }
    }
    out
}

/// Open `todo` when that path exists and has items; else the first inbox (`all`).
pub fn open_inbox_path(
    tree: &[crate::config::TreeNode],
    total_of: impl Fn(&[String]) -> usize,
) -> Vec<String> {
    tree.iter()
        .find(|n| n.inbox.name == "todo" && total_of(&n.path) > 0)
        .or_else(|| tree.first())
        .map(|n| n.path.clone())
        .unwrap_or_else(|| vec!["all".into()])
}

pub const IDLE_HINT: &str = ":cmd  /search  ?help  q";
