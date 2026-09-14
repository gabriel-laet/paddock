//! Classify-on-enter: classifiers stamp labels as an item enters an inbox, and children re-evaluate.

mod common;

use common::*;
use paddock::*;
use std::fs;

#[test]
fn llm_classified_marks_and_persists() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let id = store
        .upsert(&NewItem {
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
        .0;
    assert!(!store.classified(id, "important").unwrap());
    store
        .note(id, Fact::Classified("important".into()))
        .unwrap();
    assert!(store.classified(id, "important").unwrap());
    // Distinct classifier id on the same item is tracked separately.
    assert!(!store.classified(id, "other").unwrap());
    // Marking twice does not error (INSERT OR IGNORE).
    store
        .note(id, Fact::Classified("important".into()))
        .unwrap();
}

#[test]
fn regex_classifier_case_insensitive() {
    let cfg = ClassifierSpec {
        id: "flag-rfc".into(),
        kind: "regex".into(),
        settings: [("pattern".to_string(), serde_json::json!("(?i)rfc"))]
            .into_iter()
            .collect(),
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
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let id = store
        .upsert(&NewItem {
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
        .0;
    let cfg = load_config(&paths.config_file).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    k.classify(id).unwrap();
    let item = store.get(id).unwrap();
    assert!(item.labels.contains(&"todo".into()), "root flag-todo regex");
    let chain = cfg.chain(&["all", "todo"]).unwrap();
    let listed = k.ask(&chain).unwrap();
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
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = store
        .upsert(&NewItem {
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
        .0;
    k.classify(id).unwrap();
    let item = store.get(id).unwrap();
    assert!(!item.labels.contains(&"todo".into()));
    assert!(!item.labels.contains(&"urgent".into()));

    k.label(id, &["todo".into()], &[]).unwrap();
    let item = store.get(id).unwrap();
    assert!(item.labels.contains(&"todo".into()));
    assert!(
        item.labels.contains(&"urgent".into()),
        "child classifier after enter"
    );
    let chain = cfg.chain(&["all", "todo"]).unwrap();
    assert!(k.ask(&chain).unwrap().iter().any(|i| i.id == id));
}

#[test]
fn admit_file_reclassifies_on_update() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let p = paths.incoming_dir.join("note.md");
    fs::write(&p, "hello").unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k.admit(item_from_file("incoming", &p).unwrap()).unwrap().id;
    assert!(!store.get(id).unwrap().labels.contains(&"todo".into()));
    fs::write(&p, "hello todo").unwrap();
    let id2 = k.admit(item_from_file("incoming", &p).unwrap()).unwrap().id;
    assert_eq!(id, id2);
    assert!(store.get(id).unwrap().labels.contains(&"todo".into()));
}

#[test]
fn admit_file_classifies() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let p = paths.incoming_dir.join("rfc-note.md");
    fs::write(&p, "see the rfc please").unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k.admit(item_from_file("incoming", &p).unwrap()).unwrap().id;
    let item = store.get(id).unwrap();
    assert!(item.labels.contains(&"rfc".into()));
}
