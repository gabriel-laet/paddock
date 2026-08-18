//! paddock — an inbox host.
//!
//! Four nouns: item, source, label, inbox.

mod classify;
pub mod cmd;
mod config;
pub mod engine;
pub mod keys;
mod source;
mod store;
pub mod theme;

pub use classify::{build_classifier, Classifier, RegexClassifier, StubClassifier};
pub use cmd::{run_verb, Outcome, VerbCtx};
pub use config::{expand_path, inbox_matches, ClassifierConfig, Config, InboxConfig, Paths, SourceConfig, TreeNode};
pub use engine::{admit, admit_file, classify_item, items_in_chain, pull_all, relabel, spawn_fs_watch, stamp, WatchGuard};
pub use keys::{parse_colon, Verb, HELP};
pub use source::{pull_fs, pull_rss, NewItem};
pub use store::{Item, Store};
pub use theme::{load_theme, Theme};

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;

pub const BIND: &str = "127.0.0.1:4736";

/// Create config, data dir, incoming dir, and empty store. Idempotent.
pub fn init(paths: &Paths) -> Result<()> {
    fs::create_dir_all(&paths.config_dir)
        .with_context(|| format!("create {}", paths.config_dir.display()))?;
    fs::create_dir_all(&paths.data_dir)
        .with_context(|| format!("create {}", paths.data_dir.display()))?;
    fs::create_dir_all(&paths.incoming_dir)
        .with_context(|| format!("create {}", paths.incoming_dir.display()))?;

    if !paths.config_file.exists() {
        let incoming = paths.incoming_dir.display().to_string();
        let text = default_config_toml(&incoming);
        let mut f = fs::File::create(&paths.config_file)
            .with_context(|| format!("create {}", paths.config_file.display()))?;
        f.write_all(text.as_bytes())?;
    }

    crate::theme::install_bundled(&paths.config_dir.join("themes"))?;

    let _store = Store::open(&paths.db_path)?;
    Ok(())
}

pub fn default_config_toml(incoming: &str) -> String {
    format!(
        r#"# paddock — inboxes nest. a child is a tighter question over its parent.
# classifiers belong to an inbox and run when an item enters it.
# a label change re-runs classify so children can fire (classify-on-enter).

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

[[source]]
id = "incoming"
kind = "fs"
path = {incoming}
"#,
        incoming = toml_string(incoming)
    )
}

fn toml_string(s: &str) -> String {
    let mut out = String::from('"');
    for ch in s.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::run_classifier;
    use crate::config::ClassifierConfig;
    use std::fs;
    use std::path::PathBuf;

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_dirs(dir.path().join("cfg"), dir.path().join("data"));
        (dir, paths)
    }

    #[test]
    fn init_is_idempotent() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        init(&paths).unwrap();
        assert!(paths.config_file.exists());
        assert!(paths.incoming_dir.exists());
        assert!(paths.db_path.exists());
        assert!(paths.config_dir.join("themes/phosphor.toml").exists());
        assert!(paths.config_dir.join("themes/carbon.toml").exists());
    }

    #[test]
    fn nested_config_parses() {
        let toml = default_config_toml("/tmp/incoming");
        let cfg: Config = toml::from_str(&toml).unwrap();
        assert_eq!(cfg.inbox.len(), 1);
        assert_eq!(cfg.inbox[0].name, "all");
        assert_eq!(cfg.inbox[0].classifier.len(), 2);
        assert_eq!(cfg.inbox[0].classifier[0].id, "flag-rfc");
        assert_eq!(cfg.inbox[0].classifier[1].id, "flag-todo");
        assert_eq!(cfg.inbox[0].inbox.len(), 2);
        assert_eq!(cfg.inbox[0].inbox[0].name, "later");
        assert_eq!(cfg.inbox[0].inbox[0].labels, vec!["later"]);
        assert!(cfg.inbox[0].inbox[0].classifier.is_empty());
        assert_eq!(cfg.inbox[0].inbox[1].name, "todo");
        assert_eq!(cfg.inbox[0].inbox[1].labels, vec!["todo"]);
        assert_eq!(cfg.source[0].kind, "fs");
    }

    #[test]
    fn inbox_match_empty_is_everything() {
        let ib = InboxConfig {
            name: "all".into(),
            ..Default::default()
        };
        let item = Item {
            id: 1,
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "a".into(),
            body: String::new(),
            href: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
        };
        assert!(inbox_matches(&ib, &item));
    }

    #[test]
    fn child_requires_all_listed_labels() {
        let parent = InboxConfig {
            name: "all".into(),
            ..Default::default()
        };
        let child = InboxConfig {
            name: "later".into(),
            labels: vec!["later".into()],
            ..Default::default()
        };
        let mut item = Item {
            id: 1,
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "a".into(),
            body: String::new(),
            href: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
        };
        assert!(inbox_matches(&parent, &item));
        assert!(!inbox_matches(&child, &item));
        item.labels.push("later".into());
        assert!(inbox_matches(&child, &item));
        assert!(items_match_chain(&[&parent, &child], &item));
    }

    fn items_match_chain(chain: &[&InboxConfig], item: &Item) -> bool {
        chain.iter().all(|ib| inbox_matches(ib, item))
    }

    #[test]
    fn unique_on_source_and_foreign_id() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let n = NewItem {
            source_id: "incoming".into(),
            foreign_id: "note.md".into(),
            title: "note".into(),
            body: "hi".into(),
            href: Some("/tmp/note.md".into()),
        };
        let a = store.insert_new(&n).unwrap();
        let b = store.insert_new(&n).unwrap();
        assert!(a.is_some());
        assert!(b.is_none());
        assert_eq!(store.list_all().unwrap().len(), 1);
    }

    #[test]
    fn regex_classifier_case_insensitive() {
        let cfg = ClassifierConfig {
            id: "flag-rfc".into(),
            kind: "regex".into(),
            pattern: Some("(?i)rfc".into()),
            label: Some("rfc".into()),
        };
        let item = Item {
            id: 1,
            source_id: "incoming".into(),
            foreign_id: "x".into(),
            title: "Please review RFC 9110".into(),
            body: String::new(),
            href: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
        };
        assert_eq!(run_classifier(&cfg, &item).unwrap(), Some("rfc".into()));
        let miss = Item {
            title: "hello".into(),
            ..item.clone()
        };
        assert_eq!(run_classifier(&cfg, &miss).unwrap(), None);
    }

    #[test]
    fn script_and_llm_are_stubs() {
        let item = Item {
            id: 1,
            source_id: "s".into(),
            foreign_id: "f".into(),
            title: "todo".into(),
            body: "todo".into(),
            href: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
        };
        for kind in ["script", "llm"] {
            let cfg = ClassifierConfig {
                id: kind.into(),
                kind: kind.into(),
                pattern: None,
                label: Some("x".into()),
            };
            assert_eq!(run_classifier(&cfg, &item).unwrap(), None);
        }
    }

    #[test]
    fn classify_todo_regex_enters_todo_inbox() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "t.md".into(),
                title: "note".into(),
                body: "contains todo in the body".into(),
                href: None,
            })
            .unwrap()
            .unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        classify_item(&store, &cfg, id).unwrap();
        let item = store.get(id).unwrap();
        assert!(item.labels.contains(&"todo".into()), "root flag-todo regex");
        let chain = cfg.find_chain(&["all", "todo"]).unwrap();
        let listed = items_in_chain(&store, &chain).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
    }

    #[test]
    fn relabel_enters_todo_and_runs_child_classifier() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        fs::write(
            &paths.config_file,
            r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "todo"
labels = ["todo"]

[[inbox.inbox.classifier]]
id = "flag-child"
kind = "regex"
pattern = "(?i)urgent"
label = "urgent"

[[source]]
id = "incoming"
kind = "fs"
path = "/tmp"
"#,
        )
        .unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "x.md".into(),
                title: "note".into(),
                body: "this is urgent".into(),
                href: None,
            })
            .unwrap()
            .unwrap();
        classify_item(&store, &cfg, id).unwrap();
        let item = store.get(id).unwrap();
        assert!(!item.labels.contains(&"todo".into()));
        assert!(!item.labels.contains(&"urgent".into()));

        relabel(&store, &cfg, id, "todo").unwrap();
        let item = store.get(id).unwrap();
        assert!(item.labels.contains(&"todo".into()));
        assert!(
            item.labels.contains(&"urgent".into()),
            "child classifier after enter"
        );
        let chain = cfg.find_chain(&["all", "todo"]).unwrap();
        assert!(items_in_chain(&store, &chain)
            .unwrap()
            .iter()
            .any(|i| i.id == id));
    }

    #[test]
    fn admit_file_reclassifies_on_update() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let p = paths.incoming_dir.join("note.md");
        fs::write(&p, "hello").unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = admit_file(&store, &cfg, "incoming", &p).unwrap().unwrap();
        assert!(!store.get(id).unwrap().labels.contains(&"todo".into()));
        fs::write(&p, "hello todo").unwrap();
        let id2 = admit_file(&store, &cfg, "incoming", &p).unwrap().unwrap();
        assert_eq!(id, id2);
        assert!(store.get(id).unwrap().labels.contains(&"todo".into()));
    }

    #[test]
    fn fs_pull_and_chain_query() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        fs::write(paths.incoming_dir.join("hello.md"), "hello body").unwrap();
        fs::write(paths.incoming_dir.join(".hidden"), "no").unwrap();
        fs::create_dir_all(paths.incoming_dir.join("subdir")).unwrap();
        fs::write(paths.incoming_dir.join("subdir").join("nested.md"), "no").unwrap();

        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let n = pull_all(&store, &cfg).unwrap();
        assert_eq!(n, 1);
        let items = store.list_all().unwrap();
        assert_eq!(items[0].foreign_id, "hello.md");
        assert_eq!(items[0].title, "hello");
        assert_eq!(items[0].body, "hello body");

        let all = cfg.find_chain(&["all"]).unwrap();
        let listed = items_in_chain(&store, &all).unwrap();
        assert_eq!(listed.len(), 1);
        let later = cfg.find_chain(&["all", "later"]).unwrap();
        assert!(items_in_chain(&store, &later).unwrap().is_empty());
    }

    #[test]
    fn admit_file_classifies() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let p = paths.incoming_dir.join("rfc-note.md");
        fs::write(&p, "see the rfc please").unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = admit_file(&store, &cfg, "incoming", &p).unwrap().unwrap();
        let item = store.get(id).unwrap();
        assert!(item.labels.contains(&"rfc".into()));
    }

    #[test]
    fn source_filter_and_label_and() {
        let ib = InboxConfig {
            name: "mail".into(),
            sources: vec!["a".into()],
            labels: vec!["x".into(), "y".into()],
            ..Default::default()
        };
        let mut item = Item {
            id: 1,
            source_id: "b".into(),
            foreign_id: "1".into(),
            title: "t".into(),
            body: String::new(),
            href: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec!["x".into(), "y".into()],
        };
        assert!(!inbox_matches(&ib, &item));
        item.source_id = "a".into();
        assert!(inbox_matches(&ib, &item));
        item.labels = vec!["x".into()];
        assert!(!inbox_matches(&ib, &item));
    }

    #[test]
    fn expand_tilde() {
        let p = expand_path("~/incoming");
        assert!(p.is_absolute() || !p.starts_with("~"));
        assert!(p.ends_with(PathBuf::from("incoming")));
    }
}
