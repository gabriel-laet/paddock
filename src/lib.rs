//! paddock — an inbox kernel.
//!
//! Four nouns: item, source, label, inbox.
//! No UI lives here; `main.rs` is a thin CLI over this crate.

mod classify;
mod config;
mod engine;
mod source;
mod store;

pub use classify::{build_classifier, run_classifier, Classifier};
pub use config::{
    chain_matches, expand_path, inbox_matches, source_label, ClassifierConfig, Config, InboxConfig,
    Paths, SourceConfig, TreeNode,
};
pub use engine::{
    admit, admit_file, classify_item, filter_for_chain, forget, forget_stale, items_in_chain,
    label, pull_all, reply_title, send_draft, why,
};
pub use source::{pull_exec, pull_fs, pull_rss, send_exec, Draft, NewItem, SendResult};
pub use store::{Actor, ActorKind, Item, ItemFilter, NewPart, Part, PartKind, Store};

use anyhow::{Context, Result};
use std::fs;

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
    Store::open(&paths.db_path)?;
    Ok(())
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

pub fn load_or_init(paths: &Paths) -> Result<(Config, Store)> {
    if !paths.config_file.exists() {
        init(paths)?;
    }
    fs::create_dir_all(&paths.data_dir)?;
    fs::create_dir_all(&paths.incoming_dir)?;
    let config = Config::load(&paths.config_file)?;
    let store = Store::open(&paths.db_path)?;
    Ok((config, store))
}
