use anyhow::Result;
use std::collections::BTreeMap;
use std::io::Write;

use crate::config::{Config, InboxConfig, Paths};
use crate::engine::filter_for_chain;
use crate::keys::HELP;
use crate::store::Store;

/// Agent-ready dump of this host. No secrets. Safe to pipe.
pub fn write_context(paths: &Paths, config: &Config, store: &Store, mut w: impl Write) -> Result<()> {
    writeln!(w, "# paddock")?;
    writeln!(w)?;
    writeln!(
        w,
        "Inbox host. Four nouns: item, source, label, inbox. No other product nouns."
    )?;
    writeln!(
        w,
        "An item is source-shaped data stripped: foreign_id, title, body, href, start, end, thread, parts, from, to[], cites."
    )?;
    writeln!(
        w,
        "A cite arrives as a foreign id and resolves on admit (late parent still stitches)."
    )?;
    writeln!(
        w,
        "A source admits items and may send. kinds: fs, rss, exec. rss cannot send. exec runs `{{cmd}} {{args}} pull|send`."
    )?;
    writeln!(
        w,
        "Inboxes nest. A child is a tighter question over the parent. Match: sources AND labels (all) AND timed (start set)."
    )?;
    writeln!(
        w,
        "view is not a type: list | calendar | board. timed=true means item.start is set. Board columns are labels."
    )?;
    writeln!(
        w,
        "Classifiers are per-inbox, ordered, kinds regex | script | llm. They stamp labels. They are not sources."
    )?;
    writeln!(
        w,
        "Actor kind is person | group | list. Do not add Calendar or Conversation types."
    )?;
    writeln!(
        w,
        "Admit upserts on (source_id, foreign_id). Re-admit refreshes the item and keeps read + labels."
    )?;
    writeln!(w)?;
    writeln!(w, "## this host")?;
    writeln!(w, "config {}", paths.config_file.display())?;
    writeln!(w, "db     {}", paths.db_path.display())?;
    writeln!(w, "data   {}", paths.data_dir.display())?;
    writeln!(w, "incoming {}", paths.incoming_dir.display())?;
    if let Some(t) = config.theme.as_deref() {
        writeln!(w, "theme  {t}")?;
    }
    writeln!(w)?;
    writeln!(w, "## sources")?;
    let by_src: BTreeMap<String, i64> = store.counts_by_source()?.into_iter().collect();
    let timed = store.count_timed()?;
    let total = store.count_all()?;
    for src in &config.source {
        let n = by_src.get(&src.id).copied().unwrap_or(0);
        let extra = match src.kind.as_str() {
            "fs" => src.path.clone().unwrap_or_default(),
            "rss" => src.url.clone().unwrap_or_default(),
            "exec" => src.cmd.clone().unwrap_or_default(),
            _ => String::new(),
        };
        writeln!(w, "{}  kind={}  items={n}  {extra}", src.id, src.kind)?;
    }
    for (id, n) in &by_src {
        if !config.source.iter().any(|s| s.id == *id) {
            writeln!(w, "{id}  items={n}  (not in config)")?;
        }
    }
    writeln!(w, "total {total}  timed {timed}")?;
    writeln!(w)?;
    writeln!(w, "## inboxes")?;
    for node in config.flatten() {
        let refs: Vec<&str> = node.path.iter().map(|s| s.as_str()).collect();
        let n = config
            .find_chain(&refs)
            .and_then(|chain| store.count_filtered(&filter_for_chain(&chain)).ok())
            .unwrap_or(0);
        write_inbox_line(&mut w, &node.path.join("/"), &node.inbox, n)?;
    }
    writeln!(w)?;
    writeln!(w, "## tune")?;
    writeln!(
        w,
        "Edit config.toml, then `paddock pull`. Do not invent nouns. Exec plugins live next to the binary as `plugins/*` and speak JSON items."
    )?;
    writeln!(
        w,
        "Send is a verb. Draft may carry to[] and a foreign id the source returns."
    )?;
    writeln!(w)?;
    writeln!(w, "## keys")?;
    writeln!(w, "{HELP}")?;
    Ok(())
}

fn write_inbox_line(w: &mut impl Write, path: &str, ib: &InboxConfig, n: usize) -> Result<()> {
    write!(w, "{path}  view={}  items={n}", ib.view_kind())?;
    if ib.timed {
        write!(w, "  timed")?;
    }
    if !ib.labels.is_empty() {
        write!(w, "  labels={}", ib.labels.join(","))?;
    }
    if !ib.sources.is_empty() {
        write!(w, "  sources={}", ib.sources.join(","))?;
    }
    if !ib.columns.is_empty() {
        write!(w, "  columns={}", ib.columns.join(","))?;
    }
    if !ib.classifier.is_empty() {
        let ids: Vec<&str> = ib.classifier.iter().map(|c| c.id.as_str()).collect();
        write!(w, "  classifiers={}", ids.join(","))?;
    }
    writeln!(w)?;
    Ok(())
}
