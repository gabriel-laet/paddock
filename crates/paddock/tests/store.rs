//! The SQLite store: upsert, parts, threads, old databases, and encryption at rest.

mod common;

use common::*;
use paddock::*;
use std::fs;

#[test]
fn unique_on_source_and_foreign_id() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
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
    let a = store.upsert(&n).unwrap();
    let b = store.upsert(&n).unwrap();
    assert!(a.1);
    assert!(!b.1);
    assert_eq!(a.0, b.0);
    assert_eq!(store.ask(&Question::default()).unwrap().len(), 1);
}

#[test]
fn store_roundtrip_start_end() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let id = store
        .upsert(&NewItem {
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
        .0;
    let item = store.get(id).unwrap();
    assert_eq!(item.start.as_deref(), Some("2026-08-18T15:00:00Z"));
    assert_eq!(item.end.as_deref(), Some("2026-08-18T16:00:00Z"));
}

#[test]
fn insert_body_only_synthesizes_text_part() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let id = store
        .upsert(&NewItem {
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
        .0;
    let item = store.get(id).unwrap();
    assert_eq!(item.body, "hello body");
    assert_eq!(item.parts.len(), 1);
    assert_eq!(item.parts[0].kind, PartKind::Text);
    assert_eq!(item.parts[0].mime, "text/plain");
    assert_eq!(item.parts[0].text.as_deref(), Some("hello body"));
    assert!(item.parts[0].size.is_none());
    assert!(item.thread.is_none());
}

#[test]
fn insert_image_and_text_parts() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let id = store
        .upsert(&NewItem {
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
        .0;
    let item = store.get(id).unwrap();
    assert_eq!(item.body, "caption");
    assert_eq!(item.parts.len(), 2);
    assert_eq!(item.parts[0].kind, PartKind::Image);
    assert_eq!(item.parts[0].mime, "image/png");
    assert_eq!(item.parts[0].size, Some(4), "png bytes are kept");
    assert_eq!(store.blob(item.parts[0].id).unwrap(), b"\x89PNG");
    assert_eq!(item.parts[1].kind, PartKind::Text);
    assert_eq!(item.parts[1].text.as_deref(), Some("caption"));
    let listed = store.ask(&Question::default()).unwrap();
    assert_eq!(listed[0].parts.len(), 2);
    assert_eq!(listed[0].body, "caption");
}

#[test]
fn set_thread_and_items_in_thread() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let a = store
        .upsert(&NewItem {
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
        .0;
    let b = store
        .upsert(&NewItem {
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
        .0;
    store.note(b, Fact::Thread(Some("conv-1".into()))).unwrap();
    let c = store
        .upsert(&NewItem {
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
        .0;
    let got = store.thread("conv-1").unwrap();
    let ids: Vec<i64> = got.iter().map(|i| i.id).collect();
    assert_eq!(got.len(), 2);
    assert!(ids.contains(&a));
    assert!(ids.contains(&b));
    assert!(!ids.contains(&c));
    assert_eq!(store.get(b).unwrap().thread.as_deref(), Some("conv-1"));
    store.note(b, Fact::Thread(None)).unwrap();
    assert!(store.get(b).unwrap().thread.is_none());
    assert_eq!(store.thread("conv-1").unwrap().len(), 1);
}

#[test]
fn insert_from_to_actors() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let id = store
        .upsert(&NewItem {
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
        .0;
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
    let listed = store.ask(&Question::default()).unwrap();
    assert_eq!(listed[0].to.len(), 2);
    assert!(listed[0].from.is_some());
}

#[test]
fn readmit_updates_item_keeps_labels_and_read() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "n.md".into(),
            title: "old".into(),
            body: "hello".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    store.note(id, Fact::Label(Label::hand("keep"))).unwrap();
    k.read(id, true).unwrap();
    let id2 = k
        .admit(NewItem {
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
        })
        .unwrap()
        .id;
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
    assert!(item.read());
    assert!(item.has("keep"));
    assert!(item.has("todo"));
}

#[test]
fn forget_deletes_the_row() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let id = store
        .upsert(&NewItem {
            source_id: "incoming".into(),
            foreign_id: "gone.md".into(),
            title: "gone".into(),
            body: "bye".into(),
            ..Default::default()
        })
        .unwrap()
        .0;
    assert!(store.delete(id).unwrap());
    assert!(store.ask(&Question::default()).unwrap().is_empty());
    assert!(!store.delete(id).unwrap());
}

#[test]
fn an_encrypted_store_is_unreadable_without_its_key() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        store_toml(&paths.incoming_dir, r#"key = "hunter2""#),
    )
    .unwrap();
    fs::remove_file(&paths.db_path).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "a".into(),
        title: "secret plans".into(),
        body: "x".into(),
        parts: vec![NewPart {
            kind: PartKind::File,
            mime: "application/octet-stream".into(),
            bytes: Some(b"\x00\x01\x02".to_vec()),
            ..Default::default()
        }],
        ..Default::default()
    })
    .unwrap();
    drop(store);

    let raw = fs::read(&paths.db_path).unwrap();
    assert!(!raw.starts_with(b"SQLite format 3"), "no plaintext header");
    assert!(
        !raw.windows(12).any(|w| w == b"secret plans"),
        "the title is not on disk in the clear"
    );
    assert!(
        Sqlite::open(&paths.db_path, None).is_err(),
        "no key, no store"
    );
    assert!(Sqlite::open(&paths.db_path, Some("wrong")).is_err());

    let again = Sqlite::open(&paths.db_path, Some("hunter2")).unwrap();
    let items = again.ask(&Question::default()).unwrap();
    assert_eq!(items[0].title, "secret plans");
    assert_eq!(again.blob(items[0].parts[0].id).unwrap(), b"\x00\x01\x02");
}

#[test]
fn the_store_key_can_come_from_a_command() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        store_toml(&paths.incoming_dir, r#"key_cmd = "echo from-a-command""#),
    )
    .unwrap();
    fs::remove_file(&paths.db_path).unwrap();
    let (_cfg, store) = load(&paths).unwrap();
    drop(store);
    assert!(Sqlite::open(&paths.db_path, None).is_err());
    assert!(Sqlite::open(&paths.db_path, Some("from-a-command")).is_ok());
}

#[test]
fn a_plaintext_store_opened_with_a_key_says_so() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let err = match Sqlite::open(&paths.db_path, Some("k")) {
        Ok(_) => panic!("a plaintext store must not open with a key"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("wrong key"), "{err}");
}

#[test]
fn a_snapshot_is_a_whole_store_in_one_file_with_the_same_key() {
    let (tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        store_toml(&paths.incoming_dir, r#"key = "hunter2""#),
    )
    .unwrap();
    fs::remove_file(&paths.db_path).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "a".into(),
        title: "kept".into(),
        body: "x".into(),
        ..Default::default()
    })
    .unwrap();
    let snap = tmp.path().join("snap.db");
    store.snapshot(&snap).unwrap();
    assert!(Sqlite::open(&snap, None).is_err(), "still encrypted");
    let copy = Sqlite::open(&snap, Some("hunter2")).unwrap();
    assert_eq!(copy.ask(&Question::default()).unwrap()[0].title, "kept");
    store.snapshot(&snap).unwrap();
    assert!(snap.exists(), "a second snapshot replaces the first");
}
