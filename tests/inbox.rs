//! Inboxes are questions. Matching, nesting, time windows, and ordering.

mod common;

use common::*;
use paddock::*;
use std::fs;

#[test]
fn inbox_match_empty_is_everything() {
    let ib = Inbox {
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
    assert!(Question::of(&[&ib]).matches(&item));
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

    let recent_ib = Inbox {
        name: "recent".into(),
        newer_than: Some("14d".into()),
        ..Default::default()
    };
    assert!(Question::of(&[&recent_ib]).matches(&recent));
    assert!(!Question::of(&[&recent_ib]).matches(&old));

    let dormant_ib = Inbox {
        name: "dormant".into(),
        older_than: Some("14d".into()),
        ..Default::default()
    };
    assert!(!Question::of(&[&dormant_ib]).matches(&recent));
    assert!(Question::of(&[&dormant_ib]).matches(&old));

    // No start at all: falls back to created_at, still matches "recent".
    recent.start = None;
    assert!(Question::of(&[&recent_ib]).matches(&recent));
}

#[test]
fn child_requires_all_listed_labels() {
    let parent = Inbox {
        name: "all".into(),
        ..Default::default()
    };
    let child = Inbox {
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
    assert!(Question::of(&[&parent]).matches(&item));
    assert!(!Question::of(&[&child]).matches(&item));
    item.labels.push("later".into());
    assert!(Question::of(&[&child]).matches(&item));
    assert!(items_match_chain(&[&parent, &child], &item));
}

#[test]
fn source_filter_and_label_and() {
    let ib = Inbox {
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
    assert!(!Question::of(&[&ib]).matches(&item));
    item.source_id = "a".into();
    assert!(Question::of(&[&ib]).matches(&item));
    item.labels = vec!["x".into()];
    assert!(!Question::of(&[&ib]).matches(&item));
}

#[test]
fn timed_inbox_requires_start() {
    let ib = Inbox {
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
    assert!(!Question::of(&[&ib]).matches(&item));
    item.start = Some("2026-08-18T15:00:00Z".into());
    assert!(Question::of(&[&ib]).matches(&item));
}

#[test]
fn timed_inbox_sorts_by_start() {
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
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "late".into(),
        title: "late".into(),
        body: "b".into(),
        start: Some("2026-08-19T18:00:00Z".into()),
        ..Default::default()
    })
    .unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "early".into(),
        title: "early".into(),
        body: "a".into(),
        start: Some("2026-08-19T09:00:00Z".into()),
        ..Default::default()
    })
    .unwrap();
    let cal = cfg.chain(&["all", "cal"]).unwrap();
    let listed = k.ask(&cal).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].foreign_id, "early");
    assert_eq!(listed[1].foreign_id, "late");
}

#[test]
fn items_in_chain_cal_still_only_timed() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "note".into(),
        title: "note".into(),
        body: "plain".into(),
        ..Default::default()
    })
    .unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "meet".into(),
        title: "meet".into(),
        body: "sync".into(),
        start: Some("2026-08-19T12:00:00Z".into()),
        ..Default::default()
    })
    .unwrap();
    let all = cfg.chain(&["all"]).unwrap();
    assert_eq!(k.ask(&all).unwrap().len(), 2);
    let cal = cfg.chain(&["all", "cal"]).unwrap();
    let timed = k.ask(&cal).unwrap();
    assert_eq!(timed.len(), 1);
    assert_eq!(timed[0].foreign_id, "meet");
    assert_eq!(
        store
            .count(&Question {
                timed: true,
                ..Default::default()
            })
            .unwrap(),
        1
    );
    assert_eq!(store.count(&Question::default()).unwrap(), 2);
}
