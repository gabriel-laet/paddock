//! Replies and forwards: cites arrive as foreign ids and resolve on admit, early or late.

mod common;

use common::*;
use paddock::*;

#[test]
fn reply_shares_thread_and_sets_in_reply_to() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let parent = store
        .upsert(&NewItem {
            source_id: "incoming".into(),
            foreign_id: "p.md".into(),
            title: "parent".into(),
            body: "first".into(),
            ..Default::default()
        })
        .unwrap()
        .0;
    assert!(store.get(parent).unwrap().thread.is_none());
    let id = k
        .send(Draft {
            source_id: "incoming".into(),
            title: "re: parent".into(),
            body: "second".into(),
            reply_to: Some(parent),
            ..Default::default()
        })
        .unwrap()
        .id;
    let parent_item = store.get(parent).unwrap();
    let child = store.get(id).unwrap();
    let th = parent_item.thread.clone().expect("parent thread");
    assert_eq!(child.thread.as_deref(), Some(th.as_str()));
    assert_eq!(th, "incoming:p.md");
    assert_eq!(child.in_reply_to, Some(parent));
    let in_th = store.thread(&th).unwrap();
    let ids: Vec<i64> = in_th.iter().map(|i| i.id).collect();
    assert_eq!(in_th.len(), 2);
    assert!(ids.contains(&parent));
    assert!(ids.contains(&id));
}

#[test]
fn admit_reply_resolves_parent() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let parent = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "p.md".into(),
            title: "parent".into(),
            body: "first".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let child = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "c.md".into(),
            title: "re: parent".into(),
            body: "second".into(),
            in_reply_to: Some("p.md".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert_eq!(store.get(child).unwrap().in_reply_to, Some(parent));
}

#[test]
fn admit_reply_before_parent_stitches() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let child = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "c.md".into(),
            title: "re: parent".into(),
            body: "second".into(),
            in_reply_to: Some("p.md".into()),
            cite_excerpt: Some("first".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert!(store.get(child).unwrap().in_reply_to.is_none());
    assert_eq!(
        store.get(child).unwrap().cite_excerpt.as_deref(),
        Some("first")
    );
    let parent = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "p.md".into(),
            title: "parent".into(),
            body: "first".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert_eq!(store.get(child).unwrap().in_reply_to, Some(parent));
}

#[test]
fn admit_forward_resolves() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let src = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "orig.md".into(),
            title: "orig".into(),
            body: "hello".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let fwd = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "fwd.md".into(),
            title: "fwd".into(),
            body: "hello".into(),
            forward_of: Some("orig.md".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert_eq!(store.get(fwd).unwrap().forward_of, Some(src));
}
