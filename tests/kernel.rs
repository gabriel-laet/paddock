//! Kernel tests: item, source, label, inbox. No UI, no network, no third-party CLIs.

use paddock::*;
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
    assert_eq!(cfg.inbox[0].inbox.len(), 3);
    assert_eq!(cfg.inbox[0].inbox[0].name, "later");
    assert_eq!(cfg.inbox[0].inbox[0].labels, vec!["later"]);
    assert!(cfg.inbox[0].inbox[0].classifier.is_empty());
    assert_eq!(cfg.inbox[0].inbox[1].name, "todo");
    assert_eq!(cfg.inbox[0].inbox[1].labels, vec!["todo"]);
    assert_eq!(cfg.inbox[0].inbox[2].name, "cal");
    assert!(cfg.inbox[0].inbox[2].timed);
    assert!(cfg.inbox[0].inbox[2].labels.is_empty());
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
fn newer_than_and_older_than_partition_by_effective_date() {
    let now = chrono::Utc::now();
    let mut recent = Item {
        id: 1,
        source_id: "wacli".into(),
        foreign_id: "a".into(),
        title: "a".into(),
        body: String::new(),
        href: None,
        start: Some((now - chrono::Duration::days(1)).to_rfc3339()),
        end: None,
        thread: None,
        created_at: now.to_rfc3339(),
        read: false,
        labels: vec![],
        parts: vec![],
        ..Default::default()
    };
    let mut old = recent.clone();
    old.id = 2;
    old.start = Some((now - chrono::Duration::days(30)).to_rfc3339());

    let recent_ib = InboxConfig {
        name: "recent".into(),
        newer_than: Some("14d".into()),
        ..Default::default()
    };
    assert!(inbox_matches(&recent_ib, &recent));
    assert!(!inbox_matches(&recent_ib, &old));

    let dormant_ib = InboxConfig {
        name: "dormant".into(),
        older_than: Some("14d".into()),
        ..Default::default()
    };
    assert!(!inbox_matches(&dormant_ib, &recent));
    assert!(inbox_matches(&dormant_ib, &old));

    // No start at all: falls back to created_at, still matches "recent".
    recent.start = None;
    assert!(inbox_matches(&recent_ib, &recent));
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
fn llm_classified_marks_and_persists() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let id = store
        .insert_new(&NewItem {
            source_id: "incoming".into(),
            foreign_id: "x.md".into(),
            title: "note".into(),
            body: "hi".into(),
            href: None,
            start: None,
            end: None,
            thread: None,
            parts: vec![],
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    assert!(!store.llm_classified(id, "important").unwrap());
    store.mark_llm_classified(id, "important").unwrap();
    assert!(store.llm_classified(id, "important").unwrap());
    // Distinct classifier id on the same item is tracked separately.
    assert!(!store.llm_classified(id, "other").unwrap());
    // Marking twice does not error (INSERT OR IGNORE).
    store.mark_llm_classified(id, "important").unwrap();
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

    label(&store, &cfg, id, &["todo".into()], &[]).unwrap();
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
fn default_init_stays_regex_list() {
    let toml = default_config_toml("/tmp/incoming");
    assert!(!toml.contains("kind = \"script\""));
    assert!(!toml.contains("kind = \"llm\""));
    let cfg: Config = toml::from_str(&toml).unwrap();
    let cal = cfg.inbox[0]
        .inbox
        .iter()
        .find(|i| i.name == "cal")
        .expect("cal child");
    assert!(cal.timed);
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
    assert_eq!(
        paths.incoming_dir,
        xdg_data.join("paddock").join("incoming")
    );
}

#[test]
fn init_here_creates_dot_paddock() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::here(dir.path());
    init(&paths).unwrap();
    assert!(dir.path().join(".paddock/config.toml").exists());
    assert!(dir.path().join(".paddock/incoming").is_dir());
    assert!(dir.path().join(".paddock/paddock.db").exists());
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
    assert!(err.to_string().contains("source cannot send"), "{err}");
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
    let part = item.parts[0].clone();
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

/// A source that speaks the exec protocol with nothing but `sh` and `cat`.
fn write_exec_helper(dir: &std::path::Path) -> std::path::PathBuf {
    fs::write(
        dir.join("pull.json"),
        r#"[
  {"foreign_id": "note-1", "title": "note", "body": "hello"},
  {"foreign_id": "meet-1", "title": "meet", "body": "sync",
   "start": "2026-08-18T15:00:00Z", "end": "2026-08-18T16:00:00Z"}
]"#,
    )
    .unwrap();
    fs::write(
        dir.join("send.json"),
        r#"{"foreign_id": "sent-1", "start": "2026-08-19T10:00:00Z", "end": "2026-08-19T11:00:00Z"}"#,
    )
    .unwrap();
    let p = dir.join("exec_helper.sh");
    fs::write(
        &p,
        format!(
            r#"case "$1" in
  pull) cat "{dir}/pull.json" ;;
  send) cat >/dev/null; cat "{dir}/send.json" ;;
  *) echo "unknown verb" >&2; exit 1 ;;
esac
"#,
            dir = dir.display()
        ),
    )
    .unwrap();
    p
}

fn exec_source_toml(helper: &std::path::Path) -> String {
    let helper = helper
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "cal"
timed = true

[[source]]
id = "plug"
kind = "exec"
cmd = "sh"
args = ["{helper}"]
"#
    )
}

#[test]
fn exec_pull_admits_items_including_timed() {
    let (tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let helper = write_exec_helper(tmp.path());
    fs::write(&paths.config_file, exec_source_toml(&helper)).unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let n = pull_all(&store, &cfg).unwrap();
    assert_eq!(n, 2);
    let items = store.list_all().unwrap();
    assert_eq!(items.len(), 2);
    let note = items.iter().find(|i| i.foreign_id == "note-1").unwrap();
    assert_eq!(note.source_id, "plug");
    assert_eq!(note.title, "note");
    assert_eq!(note.body, "hello");
    assert!(note.start.is_none());
    let meet = items.iter().find(|i| i.foreign_id == "meet-1").unwrap();
    assert_eq!(meet.title, "meet");
    assert_eq!(meet.start.as_deref(), Some("2026-08-18T15:00:00Z"));
    assert_eq!(meet.end.as_deref(), Some("2026-08-18T16:00:00Z"));

    let all = cfg.find_chain(&["all"]).unwrap();
    assert_eq!(items_in_chain(&store, &all).unwrap().len(), 2);
    let cal = cfg.find_chain(&["all", "cal"]).unwrap();
    let timed = items_in_chain(&store, &cal).unwrap();
    assert_eq!(timed.len(), 1);
    assert_eq!(timed[0].foreign_id, "meet-1");
}

#[test]
fn exec_send_uses_returned_foreign_id() {
    let (tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let helper = write_exec_helper(tmp.path());
    fs::write(&paths.config_file, exec_source_toml(&helper)).unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let id = send_draft(
        &store,
        &cfg,
        &paths,
        Draft {
            source_id: "plug".into(),
            title: "hello".into(),
            body: "out".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let item = store.get(id).unwrap();
    assert_eq!(item.foreign_id, "sent-1");
    assert_eq!(item.title, "hello");
    assert_eq!(item.body, "out");
    assert_eq!(item.source_id, "plug");
    assert_eq!(item.start.as_deref(), Some("2026-08-19T10:00:00Z"));
    assert_eq!(item.end.as_deref(), Some("2026-08-19T11:00:00Z"));
}

#[test]
fn unknown_exec_cmd_fails_cleanly() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        r#"
[[inbox]]
name = "all"

[[source]]
id = "gone"
kind = "exec"
cmd = "paddock-no-such-exec-cmd"
"#,
    )
    .unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let err = pull_all(&store, &cfg).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot run")
            || msg.contains("gone")
            || msg.contains("paddock-no-such-exec-cmd"),
        "{msg}"
    );
    let err = send_draft(
        &store,
        &cfg,
        &paths,
        Draft {
            source_id: "gone".into(),
            title: "x".into(),
            body: "y".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot run")
            || msg.contains("gone")
            || msg.contains("paddock-no-such-exec-cmd"),
        "{msg}"
    );
    assert!(!msg.to_lowercase().contains("panic"));
}

#[test]
fn timed_inbox_requires_start() {
    let ib = InboxConfig {
        name: "cal".into(),
        timed: true,
        ..Default::default()
    };
    let mut item = Item {
        id: 1,
        source_id: "plug".into(),
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
    assert!(!inbox_matches(&ib, &item));
    item.start = Some("2026-08-18T15:00:00Z".into());
    assert!(inbox_matches(&ib, &item));
}

#[test]
fn calendar_view_sorts_by_start() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "cal"
timed = true

[[source]]
id = "incoming"
kind = "fs"
path = "/tmp"
"#,
    )
    .unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "late".into(),
            title: "late".into(),
            body: "b".into(),
            start: Some("2026-08-19T18:00:00Z".into()),
            ..Default::default()
        },
    )
    .unwrap();
    admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "early".into(),
            title: "early".into(),
            body: "a".into(),
            start: Some("2026-08-19T09:00:00Z".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let cal = cfg.find_chain(&["all", "cal"]).unwrap();
    let listed = items_in_chain(&store, &cal).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].foreign_id, "early");
    assert_eq!(listed[1].foreign_id, "late");
}

#[test]
fn default_init_has_no_brand_exec_sources() {
    let toml = default_config_toml("/tmp/incoming");
    assert!(!toml.contains("gog"));
    assert!(!toml.contains("hey"));
    assert!(!toml.contains("wacli"));
    let cfg: Config = toml::from_str(&toml).unwrap();
    assert!(cfg.source.iter().all(|s| s.kind == "fs"));
    assert_eq!(cfg.source.len(), 1);
    let cal = cfg.inbox[0]
        .inbox
        .iter()
        .find(|i| i.name == "cal")
        .expect("cal child");
    assert!(cal.timed);
}

#[test]
fn forget_deletes_the_row() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let id = store
        .insert_new(&NewItem {
            source_id: "incoming".into(),
            foreign_id: "gone.md".into(),
            title: "gone".into(),
            body: "bye".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    assert!(forget(&store, id).unwrap());
    assert!(store.list_all().unwrap().is_empty());
    assert!(!forget(&store, id).unwrap());
}

fn yesterday() -> String {
    (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

#[test]
fn forget_stale_drops_past_timed() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let y = yesterday();
    admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "old-meet".into(),
            title: "old meet".into(),
            body: "done".into(),
            start: Some(y.clone()),
            end: Some(y),
            ..Default::default()
        },
    )
    .unwrap();
    let n = forget_stale(&store, &cfg).unwrap();
    assert_eq!(n, 1);
    assert!(store.list_all().unwrap().is_empty());
}

#[test]
fn forget_stale_keeps_start_only_past_item() {
    // A `start` with no `end` is a timestamp (e.g. a chat message's send
    // time), not a deadline — it must not trip the "past timed" forget path.
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "old-msg".into(),
            title: "old msg".into(),
            body: "hi".into(),
            start: Some(yesterday()),
            ..Default::default()
        },
    )
    .unwrap();
    let n = forget_stale(&store, &cfg).unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.list_all().unwrap().len(), 1);
}

#[test]
fn forget_stale_keeps_past_timed_todo() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let y = yesterday();
    let id = admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "keep-meet".into(),
            title: "keep meet".into(),
            body: "still".into(),
            start: Some(y.clone()),
            end: Some(y),
            ..Default::default()
        },
    )
    .unwrap();
    store.add_label(id, "todo").unwrap();
    let n = forget_stale(&store, &cfg).unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.list_all().unwrap().len(), 1);
}

#[test]
fn forget_stale_keeps_untimed_when_forget_after_unset() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "note.md".into(),
            title: "note".into(),
            body: "stay".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let n = forget_stale(&store, &cfg).unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.list_all().unwrap().len(), 1);
}

#[test]
fn forget_stale_drops_untimed_when_forget_after_1d() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        r#"
keep = ["todo", "later"]
forget_after = "1d"

[[inbox]]
name = "all"

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
            foreign_id: "old.md".into(),
            title: "old".into(),
            body: "aged".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    let old = (chrono::Utc::now() - chrono::Duration::days(3))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    {
        let conn = rusqlite::Connection::open(&paths.db_path).unwrap();
        conn.execute(
            "UPDATE items SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![old, id],
        )
        .unwrap();
    }
    let n = forget_stale(&store, &cfg).unwrap();
    assert_eq!(n, 1);
    assert!(store.list_all().unwrap().is_empty());
}

#[test]
fn forget_stale_source_forget_after_overrides_host() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        r#"
keep = ["todo", "later"]
forget_after = "1d"

[[inbox]]
name = "all"

[[source]]
id = "incoming"
kind = "fs"
path = "/tmp"
forget_after = "30d"
"#,
    )
    .unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    let id = store
        .insert_new(&NewItem {
            source_id: "incoming".into(),
            foreign_id: "mid.md".into(),
            title: "mid".into(),
            body: "source window".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    let old = (chrono::Utc::now() - chrono::Duration::days(3))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    {
        let conn = rusqlite::Connection::open(&paths.db_path).unwrap();
        conn.execute(
            "UPDATE items SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![old, id],
        )
        .unwrap();
    }
    let n = forget_stale(&store, &cfg).unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.list_all().unwrap().len(), 1);
}

#[test]
fn items_in_chain_cal_still_only_timed() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = Config::load(&paths.config_file).unwrap();
    let store = Store::open(&paths.db_path).unwrap();
    admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "note".into(),
            title: "note".into(),
            body: "plain".into(),
            ..Default::default()
        },
    )
    .unwrap();
    admit(
        &store,
        &cfg,
        NewItem {
            source_id: "incoming".into(),
            foreign_id: "meet".into(),
            title: "meet".into(),
            body: "sync".into(),
            start: Some("2026-08-19T12:00:00Z".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let all = cfg.find_chain(&["all"]).unwrap();
    assert_eq!(items_in_chain(&store, &all).unwrap().len(), 2);
    let cal = cfg.find_chain(&["all", "cal"]).unwrap();
    let timed = items_in_chain(&store, &cal).unwrap();
    assert_eq!(timed.len(), 1);
    assert_eq!(timed[0].foreign_id, "meet");
    assert_eq!(store.count_timed().unwrap(), 1);
    assert_eq!(store.count_all().unwrap(), 2);
}

#[test]
fn source_label_falls_back_to_id() {
    let cfg = Config {
        source: vec![
            SourceConfig {
                id: "chat".into(),
                kind: "exec".into(),
                name: Some("Messages".into()),
                ..Default::default()
            },
            SourceConfig {
                id: "incoming".into(),
                kind: "fs".into(),
                name: None,
                ..Default::default()
            },
            SourceConfig {
                id: "blank".into(),
                kind: "fs".into(),
                name: Some("   ".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(source_label(&cfg, "chat"), "Messages");
    assert_eq!(source_label(&cfg, "incoming"), "incoming");
    assert_eq!(source_label(&cfg, "blank"), "blank");
    assert_eq!(source_label(&cfg, "missing"), "missing");
}

static PATH_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
