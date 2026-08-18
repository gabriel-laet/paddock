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

pub use classify::{build_classifier, Classifier, LlmClassifier, RegexClassifier, ScriptClassifier};
pub use cmd::{run_verb, Outcome, VerbCtx};
pub use config::{expand_path, inbox_matches, ClassifierConfig, Config, InboxConfig, Paths, SourceConfig, TreeNode};
pub use engine::{
    admit, admit_file, classify_item, items_in_chain, pull_all, relabel, reply_title, send_draft,
    spawn_fs_watch, stamp, WatchGuard,
};
pub use keys::{parse_colon, Verb, HELP};
pub use source::{pull_fs, pull_rss, Draft, NewItem};
pub use store::{Actor, ActorKind, Item, NewPart, Part, PartKind, Store};
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
            start: None,
            end: None,
            thread: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
            parts: vec![],
            ..Default::default()
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
            start: None,
            end: None,
            thread: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
            parts: vec![],
            ..Default::default()
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
            start: None,
            end: None,
            thread: None,
            parts: vec![],
            ..Default::default()
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
            ..Default::default()
        };
        let item = Item {
            id: 1,
            source_id: "incoming".into(),
            foreign_id: "x".into(),
            title: "Please review RFC 9110".into(),
            body: String::new(),
            href: None,
            start: None,
            end: None,
            thread: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
            parts: vec![],
            ..Default::default()
        };
        assert_eq!(run_classifier(&cfg, &item).unwrap(), Some("rfc".into()));
        let miss = Item {
            title: "hello".into(),
            ..item.clone()
        };
        assert_eq!(run_classifier(&cfg, &miss).unwrap(), None);
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
                start: None,
                end: None,
                thread: None,
                parts: vec![],
                ..Default::default()
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
                start: None,
                end: None,
                thread: None,
                parts: vec![],
                ..Default::default()
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
            start: None,
            end: None,
            thread: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec!["x".into(), "y".into()],
            parts: vec![],
            ..Default::default()
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

    #[test]
    fn opens_legacy_db_without_thread_column() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("old.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE items (
                    id INTEGER PRIMARY KEY,
                    source_id TEXT NOT NULL,
                    foreign_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    body TEXT NOT NULL,
                    href TEXT,
                    created_at TEXT NOT NULL,
                    read INTEGER NOT NULL DEFAULT 0,
                    UNIQUE(source_id, foreign_id)
                );
                INSERT INTO items (source_id, foreign_id, title, body, created_at, read)
                VALUES ('incoming', 'a.md', 'a', 'hello', '2026-01-01T00:00:00Z', 0);
                "#,
            )
            .unwrap();
        }
        let store = Store::open(&db).unwrap();
        let items = store.list_all().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].thread.is_none());
        assert_eq!(items[0].body, "hello");
    }

    #[test]
    fn store_roundtrip_start_end() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "meet.md".into(),
                title: "meet".into(),
                body: "sync".into(),
                href: None,
                start: Some("2026-08-18T15:00:00Z".into()),
                end: Some("2026-08-18T16:00:00Z".into()),
                thread: None,
                parts: vec![],
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let item = store.get(id).unwrap();
        assert_eq!(item.start.as_deref(), Some("2026-08-18T15:00:00Z"));
        assert_eq!(item.end.as_deref(), Some("2026-08-18T16:00:00Z"));
    }

    #[test]
    fn inbox_view_defaults_list_and_parses_board() {
        let list: InboxConfig = toml::from_str(r#"name = "all""#).unwrap();
        assert_eq!(list.view_kind(), "list");
        assert!(list.columns.is_empty());
        let board: InboxConfig = toml::from_str(
            r#"
name = "work"
view = "board"
columns = ["todo", "doing", "done"]
"#,
        )
        .unwrap();
        assert_eq!(board.view_kind(), "board");
        assert_eq!(board.columns, vec!["todo", "doing", "done"]);
        let item = Item {
            id: 1,
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "a".into(),
            body: String::new(),
            href: None,
            start: None,
            end: None,
            thread: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec!["doing".into()],
            parts: vec![],
            ..Default::default()
        };
        assert_eq!(board.board_column(&item), Some("doing"));
    }

    #[test]
    fn default_init_stays_regex_list() {
        let toml = default_config_toml("/tmp/incoming");
        assert!(!toml.contains("kind = \"script\""));
        assert!(!toml.contains("kind = \"llm\""));
        assert!(!toml.contains("view ="));
        let cfg: Config = toml::from_str(&toml).unwrap();
        assert_eq!(cfg.inbox[0].view_kind(), "list");
    }

    #[test]
    fn discover_walks_up_to_dot_paddock() {
        let _g = PATH_ENV.lock().unwrap();
        std::env::remove_var("PADDOCK_DIR");
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        let child = proj.join("a").join("b");
        fs::create_dir_all(&child).unwrap();
        let root = proj.join(".paddock");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("config.toml"), "\n").unwrap();
        let paths = Paths::discover(&child);
        assert_eq!(paths.config_dir, root);
        assert_eq!(paths.config_file, root.join("config.toml"));
        assert_eq!(paths.db_path, root.join("paddock.db"));
        assert_eq!(paths.incoming_dir, root.join("incoming"));
        assert_eq!(paths.data_dir, root);
    }

    #[test]
    fn discover_paddock_dir_env_wins() {
        let _g = PATH_ENV.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let env_root = dir.path().join("host");
        fs::create_dir_all(&env_root).unwrap();
        let other = dir.path().join("proj");
        fs::create_dir_all(other.join(".paddock")).unwrap();
        std::env::set_var("PADDOCK_DIR", &env_root);
        let paths = Paths::discover(&other);
        std::env::remove_var("PADDOCK_DIR");
        assert_eq!(paths.config_dir, env_root);
        assert_eq!(paths.db_path, env_root.join("paddock.db"));
    }

    #[test]
    fn discover_falls_back_to_xdg() {
        let _g = PATH_ENV.lock().unwrap();
        std::env::remove_var("PADDOCK_DIR");
        let dir = tempfile::tempdir().unwrap();
        let start = dir.path().join("empty");
        fs::create_dir_all(&start).unwrap();
        let xdg_cfg = dir.path().join("xdg-cfg");
        let xdg_data = dir.path().join("xdg-data");
        std::env::set_var("XDG_CONFIG_HOME", &xdg_cfg);
        std::env::set_var("XDG_DATA_HOME", &xdg_data);
        let paths = Paths::discover(&start);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_DATA_HOME");
        assert_eq!(paths.config_dir, xdg_cfg.join("paddock"));
        assert_eq!(paths.data_dir, xdg_data.join("paddock"));
        assert_eq!(paths.incoming_dir, xdg_data.join("paddock").join("incoming"));
    }

    #[test]
    fn init_here_creates_dot_paddock() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::here(dir.path());
        init(&paths).unwrap();
        assert!(dir.path().join(".paddock/config.toml").exists());
        assert!(dir.path().join(".paddock/incoming").is_dir());
        assert!(dir.path().join(".paddock/paddock.db").exists());
        assert!(dir.path().join(".paddock/themes/carbon.toml").exists());
        assert_eq!(paths.db_path, dir.path().join(".paddock/paddock.db"));
    }

    #[test]
    fn insert_body_only_synthesizes_text_part() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "n.md".into(),
                title: "note".into(),
                body: "hello body".into(),
                href: None,
                start: None,
                end: None,
                thread: None,
                parts: vec![],
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let item = store.get(id).unwrap();
        assert_eq!(item.body, "hello body");
        assert_eq!(item.parts.len(), 1);
        assert_eq!(item.parts[0].kind, PartKind::Text);
        assert_eq!(item.parts[0].mime, "text/plain");
        assert_eq!(item.parts[0].text.as_deref(), Some("hello body"));
        assert!(item.parts[0].path.is_none());
        assert!(item.thread.is_none());
    }

    #[test]
    fn insert_image_and_text_parts() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "pic.md".into(),
                title: "pic".into(),
                body: "ignored".into(),
                href: None,
                start: None,
                end: None,
                thread: None,
                parts: vec![
                    NewPart {
                        kind: PartKind::Image,
                        mime: "image/png".into(),
                        text: None,
                        bytes: Some(vec![0x89, 0x50, 0x4e, 0x47]),
                        src: None,
                    },
                    NewPart {
                        kind: PartKind::Text,
                        mime: "text/plain".into(),
                        text: Some("caption".into()),
                        bytes: None,
                        src: None,
                    },
                ],
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let item = store.get(id).unwrap();
        assert_eq!(item.body, "caption");
        assert_eq!(item.parts.len(), 2);
        assert_eq!(item.parts[0].kind, PartKind::Image);
        assert_eq!(item.parts[0].mime, "image/png");
        assert!(item.parts[0]
            .path
            .as_deref()
            .unwrap_or("")
            .starts_with("parts/"));
        assert_eq!(item.parts[1].kind, PartKind::Text);
        assert_eq!(item.parts[1].text.as_deref(), Some("caption"));
        let listed = store.list_all().unwrap();
        assert_eq!(listed[0].parts.len(), 2);
        assert_eq!(listed[0].body, "caption");
        let extra = store
            .add_part(
                id,
                &NewPart {
                    kind: PartKind::File,
                    mime: "application/pdf".into(),
                    text: None,
                    bytes: Some(b"%PDF".to_vec()),
                    src: None,
                },
            )
            .unwrap();
        assert!(extra > 0);
        let item = store.get(id).unwrap();
        assert_eq!(item.parts.len(), 3);
        assert_eq!(item.parts[2].kind, PartKind::File);
        assert_eq!(item.body, "caption");
    }

    #[test]
    fn set_thread_and_items_in_thread() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let a = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "a.md".into(),
                title: "a".into(),
                body: "one".into(),
                href: None,
                start: None,
                end: None,
                thread: Some("conv-1".into()),
                parts: vec![],
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let b = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "b.md".into(),
                title: "b".into(),
                body: "two".into(),
                href: None,
                start: None,
                end: None,
                thread: None,
                parts: vec![],
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        store.set_thread(b, Some("conv-1")).unwrap();
        let c = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "c.md".into(),
                title: "c".into(),
                body: "other".into(),
                href: None,
                start: None,
                end: None,
                thread: Some("other".into()),
                parts: vec![],
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let got = store.items_in_thread("conv-1").unwrap();
        let ids: Vec<i64> = got.iter().map(|i| i.id).collect();
        assert_eq!(got.len(), 2);
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
        assert!(!ids.contains(&c));
        assert_eq!(store.get(b).unwrap().thread.as_deref(), Some("conv-1"));
        store.set_thread(b, None).unwrap();
        assert!(store.get(b).unwrap().thread.is_none());
        assert_eq!(store.items_in_thread("conv-1").unwrap().len(), 1);
    }

    #[test]
    fn backfill_parts_from_body() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let _ = Store::open(&paths.db_path).unwrap();
        {
            let conn = rusqlite::Connection::open(&paths.db_path).unwrap();
            conn.execute(
                "INSERT INTO items (source_id, foreign_id, title, body, created_at, read)
                 VALUES ('incoming', 'old.md', 'old', 'legacy body', '2026-01-01T00:00:00Z', 0)",
                [],
            )
            .unwrap();
        }
        let store = Store::open(&paths.db_path).unwrap();
        let items = store.list_all().unwrap();
        let item = items.iter().find(|i| i.foreign_id == "old.md").unwrap();
        assert_eq!(item.body, "legacy body");
        assert_eq!(item.parts.len(), 1);
        assert_eq!(item.parts[0].kind, PartKind::Text);
        assert_eq!(item.parts[0].text.as_deref(), Some("legacy body"));
        let again = store.get(item.id).unwrap();
        assert_eq!(again.parts.len(), 1);
    }

    #[test]
    fn send_draft_fs_writes_file_and_text_part() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = send_draft(
            &store,
            &cfg,
            &paths,
            Draft {
                source_id: "incoming".into(),
                title: "Hello World".into(),
                body: "the body".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let dest = paths.incoming_dir.join("Hello-World.md");
        assert!(dest.exists(), "{}", dest.display());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "the body");
        let item = store.get(id).unwrap();
        assert_eq!(item.body, "the body");
        assert_eq!(item.parts.len(), 1);
        assert_eq!(item.parts[0].kind, PartKind::Text);
        assert_eq!(item.parts[0].text.as_deref(), Some("the body"));
        assert!(item.thread.is_none());
        assert!(item.in_reply_to.is_none());
    }

    #[test]
    fn reply_shares_thread_and_sets_in_reply_to() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let parent = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "p.md".into(),
                title: "parent".into(),
                body: "first".into(),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        assert!(store.get(parent).unwrap().thread.is_none());
        let id = send_draft(
            &store,
            &cfg,
            &paths,
            Draft {
                source_id: "incoming".into(),
                title: "re: parent".into(),
                body: "second".into(),
                reply_to: Some(parent),
                ..Default::default()
            },
        )
        .unwrap();
        let parent_item = store.get(parent).unwrap();
        let child = store.get(id).unwrap();
        let th = parent_item.thread.clone().expect("parent thread");
        assert_eq!(child.thread.as_deref(), Some(th.as_str()));
        assert_eq!(th, "incoming:p.md");
        assert_eq!(child.in_reply_to, Some(parent));
        let in_th = store.items_in_thread(&th).unwrap();
        let ids: Vec<i64> = in_th.iter().map(|i| i.id).collect();
        assert_eq!(in_th.len(), 2);
        assert!(ids.contains(&parent));
        assert!(ids.contains(&id));
    }

    #[test]
    fn rss_source_cannot_send() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        std::fs::write(
            &paths.config_file,
            r#"
[[inbox]]
name = "all"

[[source]]
id = "feed"
kind = "rss"
url = "https://example.com/feed.xml"
"#,
        )
        .unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let err = send_draft(
            &store,
            &cfg,
            &paths,
            Draft {
                source_id: "feed".into(),
                title: "nope".into(),
                body: "x".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("source cannot send"),
            "{err}"
        );
    }

    #[test]
    fn insert_from_to_actors() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = store
            .insert_new(&NewItem {
                source_id: "incoming".into(),
                foreign_id: "m.md".into(),
                title: "note".into(),
                body: "hi".into(),
                from: Some(Actor {
                    id: "ann@x".into(),
                    name: Some("Ann".into()),
                    kind: ActorKind::Person,
                }),
                to: vec![
                    Actor {
                        id: "eng".into(),
                        name: Some("eng".into()),
                        kind: ActorKind::List,
                    },
                    Actor {
                        id: "g1".into(),
                        name: None,
                        kind: ActorKind::Group,
                    },
                ],
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let item = store.get(id).unwrap();
        let from = item.from.expect("from");
        assert_eq!(from.id, "ann@x");
        assert_eq!(from.name.as_deref(), Some("Ann"));
        assert_eq!(from.kind, ActorKind::Person);
        assert_eq!(item.to.len(), 2);
        assert_eq!(item.to[0].id, "eng");
        assert_eq!(item.to[0].kind, ActorKind::List);
        assert_eq!(item.to[1].id, "g1");
        assert_eq!(item.to[1].kind, ActorKind::Group);
        let listed = store.list_all().unwrap();
        assert_eq!(listed[0].to.len(), 2);
        assert!(listed[0].from.is_some());
    }

    #[test]
    fn fs_video_part() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let p = paths.incoming_dir.join("clip.mp4");
        std::fs::write(&p, b"ftyp").unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = admit_file(&store, &cfg, "incoming", &p).unwrap().unwrap();
        let item = store.get(id).unwrap();
        assert_eq!(item.body, "clip.mp4");
        assert_eq!(item.parts.len(), 1);
        assert_eq!(item.parts[0].kind, PartKind::Video);
        assert_eq!(item.parts[0].mime, "video/mp4");
        assert!(item.parts[0]
            .path
            .as_deref()
            .unwrap_or("")
            .starts_with("parts/"));
        let part = store.get_part(item.parts[0].id).unwrap();
        assert_eq!(part.kind, PartKind::Video);
        let abs = store.part_abs_path(&part).unwrap();
        assert!(abs.exists());
    }


    #[test]
    fn admit_reply_resolves_parent() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let parent = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "p.md".into(),
                title: "parent".into(),
                body: "first".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let child = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "c.md".into(),
                title: "re: parent".into(),
                body: "second".into(),
                in_reply_to: Some("p.md".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(store.get(child).unwrap().in_reply_to, Some(parent));
    }

    #[test]
    fn admit_reply_before_parent_stitches() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let child = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "c.md".into(),
                title: "re: parent".into(),
                body: "second".into(),
                in_reply_to: Some("p.md".into()),
                cite_excerpt: Some("first".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(store.get(child).unwrap().in_reply_to.is_none());
        assert_eq!(
            store.get(child).unwrap().cite_excerpt.as_deref(),
            Some("first")
        );
        let parent = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "p.md".into(),
                title: "parent".into(),
                body: "first".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(store.get(child).unwrap().in_reply_to, Some(parent));
    }

    #[test]
    fn admit_forward_resolves() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let src = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "orig.md".into(),
                title: "orig".into(),
                body: "hello".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let fwd = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "fwd.md".into(),
                title: "fwd".into(),
                body: "hello".into(),
                forward_of: Some("orig.md".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(store.get(fwd).unwrap().forward_of, Some(src));
    }

    #[test]
    fn readmit_updates_item_keeps_labels_and_read() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "n.md".into(),
                title: "old".into(),
                body: "hello".into(),
                ..Default::default()
            },
        )
        .unwrap();
        store.add_label(id, "keep").unwrap();
        store.set_read(id, true).unwrap();
        let id2 = admit(
            &store,
            &cfg,
            NewItem {
                source_id: "incoming".into(),
                foreign_id: "n.md".into(),
                title: "new".into(),
                body: "hello todo".into(),
                thread: Some("incoming:n.md".into()),
                start: Some("2026-08-18T12:00:00Z".into()),
                end: Some("2026-08-18T13:00:00Z".into()),
                from: Some(Actor {
                    id: "ann@x".into(),
                    name: Some("Ann".into()),
                    kind: ActorKind::Person,
                }),
                to: vec![Actor {
                    id: "eng".into(),
                    name: Some("eng".into()),
                    kind: ActorKind::List,
                }],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(id, id2);
        let item = store.get(id).unwrap();
        assert_eq!(item.title, "new");
        assert_eq!(item.body, "hello todo");
        assert_eq!(item.thread.as_deref(), Some("incoming:n.md"));
        assert_eq!(item.start.as_deref(), Some("2026-08-18T12:00:00Z"));
        assert_eq!(item.end.as_deref(), Some("2026-08-18T13:00:00Z"));
        assert_eq!(item.from.as_ref().map(|a| a.id.as_str()), Some("ann@x"));
        assert_eq!(item.to.len(), 1);
        assert_eq!(item.to[0].id, "eng");
        assert!(item.read);
        assert!(item.labels.contains(&"keep".into()));
        assert!(item.labels.contains(&"todo".into()));
    }

    #[test]
    fn send_draft_keeps_source_foreign_id() {
        let (_tmp, paths) = temp_paths();
        init(&paths).unwrap();
        let cfg = Config::load(&paths.config_file).unwrap();
        let store = Store::open(&paths.db_path).unwrap();
        let id = send_draft(
            &store,
            &cfg,
            &paths,
            Draft {
                source_id: String::new(),
                title: "note".into(),
                body: "x".into(),
                foreign_id: Some("mid-1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(store.get(id).unwrap().foreign_id, "mid-1");
    }

    static PATH_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
}


