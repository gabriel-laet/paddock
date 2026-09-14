use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::classify::{run_classifier, LlmClassifier};
use crate::config::{
    expand_path, parse_duration, parse_when, Config, InboxConfig, Paths, SourceConfig,
};
use crate::source::{item_from_file, pull_exec, pull_fs, pull_rss, send_exec, Draft, NewItem};
use crate::store::{Item, ItemFilter, StaleHint, Store};

pub fn admit(store: &Store, config: &Config, item: NewItem) -> Result<i64> {
    let (id, _) = store.upsert(&item)?;
    classify_item(store, config, id)?;
    Ok(id)
}

pub fn admit_file(
    store: &Store,
    config: &Config,
    source_id: &str,
    path: &Path,
) -> Result<Option<i64>> {
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

/// Add and remove labels, then classify so a newly matching child can fire.
pub fn label(
    store: &Store,
    config: &Config,
    id: i64,
    add: &[String],
    remove: &[String],
) -> Result<()> {
    for l in add {
        store.add_label(id, l)?;
    }
    for l in remove {
        store.remove_label(id, l)?;
    }
    classify_item(store, config, id)
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

fn apply_classifiers(
    store: &Store,
    classifiers: &[crate::config::ClassifierConfig],
    item: &mut Item,
) -> Result<()> {
    for cfg in classifiers {
        let label = if cfg.kind == "llm" {
            if store.llm_classified(item.id, &cfg.id)? {
                continue;
            }
            match LlmClassifier::new(cfg)?.classify_result(item) {
                Ok(label) => {
                    store.mark_llm_classified(item.id, &cfg.id)?;
                    label
                }
                Err(e) => {
                    // Transient (key missing, rate limit, network): don't cache
                    // the miss, so the next pull tries again instead of giving up.
                    eprintln!("classifier {}: {e:#}", cfg.id);
                    None
                }
            }
        } else {
            run_classifier(cfg, item)?
        };
        if let Some(label) = label {
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
    let now = chrono::Utc::now();
    // Most restrictive wins when more than one inbox in the chain sets a bound:
    // newer_than -> latest cutoff (item must be at least this fresh); older_than -> earliest.
    let mut newer_than: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut older_than: Option<chrono::DateTime<chrono::Utc>> = None;
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
        if let Some(cutoff) = ib
            .newer_than
            .as_deref()
            .and_then(parse_duration)
            .map(|d| now - d)
        {
            newer_than = Some(newer_than.map_or(cutoff, |c| c.max(cutoff)));
        }
        if let Some(cutoff) = ib
            .older_than
            .as_deref()
            .and_then(parse_duration)
            .map(|d| now - d)
        {
            older_than = Some(older_than.map_or(cutoff, |c| c.min(cutoff)));
        }
    }
    ItemFilter {
        sources,
        labels,
        timed,
        unread_only: false,
        newer_than: newer_than.map(|c| c.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        older_than: older_than.map(|c| c.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        order_by_start: timed,
    }
}

/// Always deletes the item (user asked to forget it).
pub fn forget(store: &Store, id: i64) -> Result<bool> {
    let gone = store.delete(id)?;
    if gone {}
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

fn should_forget_stale(
    item: &StaleHint,
    config: &Config,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    // `end` (not `start` alone) means "this has a deadline that passed" — a
    // calendar event. A start-only item (e.g. a chat message's send time) is
    // not a deadline, so it stays on the forget_after path like before.
    let timed = nonempty(item.end.as_deref()).is_some();
    if timed {
        return match nonempty(item.end.as_deref()).and_then(parse_when) {
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

/// First `kind=fs` source, else "incoming".
pub fn default_send_source(config: &Config) -> String {
    config
        .source
        .iter()
        .find(|s| s.kind == "fs")
        .map(|s| s.id.clone())
        .unwrap_or_else(|| "incoming".into())
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
            if !draft.title.trim().is_empty() {
                new.title = draft.title.clone();
            }
            new.thread = thread;
            new.in_reply_to = reply_foreign;
            new.to = draft.to.clone();
            if let Some(fid) = draft
                .foreign_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
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
                read: None,
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
                read: None,
            };
            Ok(admit(store, config, new)?)
        }
    }
}

/// Which of the inbox chain's labels the item carries, and which classifiers
/// on the way down could have stamped them. Best effort: a label can also
/// come from a source or a hand.
pub fn why(config: &Config, item: &Item, inbox_path: &[String]) -> String {
    let refs: Vec<&str> = inbox_path.iter().map(|s| s.as_str()).collect();
    let chain = config.find_chain(&refs).unwrap_or_default();
    let has = |l: &str| item.labels.iter().any(|x| x == l);
    let matched: Vec<&str> = chain
        .iter()
        .flat_map(|ib| ib.labels.iter())
        .map(String::as_str)
        .filter(|l| has(l))
        .collect();
    let fired: Vec<&str> = config
        .classifier
        .iter()
        .chain(chain.iter().flat_map(|ib| ib.classifier.iter()))
        .filter(|c| c.label.as_deref().is_some_and(has))
        .map(|c| c.id.as_str())
        .collect();
    let dash = |v: Vec<&str>| {
        if v.is_empty() {
            "-".to_string()
        } else {
            v.join(" ")
        }
    };
    format!("labels: {}  classifiers: {}", dash(matched), dash(fired))
}
