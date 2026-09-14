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
        labels: vec![],
        parts: vec![],
        ..Default::default()
    };
    assert!(Question::of(&[&parent]).matches(&item));
    assert!(!Question::of(&[&child]).matches(&item));
    item.labels.push(Label::hand("later"));
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
        labels: vec![Label::hand("x"), Label::hand("y")],
        parts: vec![],
        ..Default::default()
    };
    assert!(!Question::of(&[&ib]).matches(&item));
    item.source_id = "a".into();
    assert!(Question::of(&[&ib]).matches(&item));
    item.labels = vec![Label::hand("x")];
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

#[test]
fn from_and_to_ask_by_actor_and_a_child_can_only_narrow() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        format!(
            r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "family"
to = ["fam@g.us"]

[[inbox.inbox.inbox]]
name = "ana"
from = ["ana", "bo"]

[[inbox.inbox.inbox.inbox]]
name = "only-ana"
from = ["ana", "cy"]

[[source]]
id = "chat"
kind = "fs"
path = "{}"
"#,
            paths.incoming_dir.display()
        ),
    )
    .unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let say = |id: &str, from: &str, to: &str| {
        k.admit(NewItem {
            source_id: "chat".into(),
            foreign_id: id.into(),
            title: id.into(),
            body: "x".into(),
            from: Some(Actor {
                id: from.into(),
                ..Default::default()
            }),
            to: vec![Actor {
                id: to.into(),
                kind: ActorKind::Group,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap()
        .id
    };
    let ana_fam = say("1", "ana", "fam@g.us");
    let bo_fam = say("2", "bo", "fam@g.us");
    let ana_work = say("3", "ana", "work@g.us");
    let ids = |path: &[&str]| -> Vec<i64> {
        let chain = cfg.chain(path).unwrap();
        let got: Vec<i64> = k.ask(&chain).unwrap().iter().map(|i| i.id).collect();
        // the in-memory rule agrees with the store on every item
        let q = k.question(&chain);
        for it in store.ask(&Question::default()).unwrap() {
            assert_eq!(
                q.matches(&it),
                got.contains(&it.id),
                "{path:?} on #{}",
                it.id
            );
        }
        got
    };
    assert_eq!(ids(&["all", "family"]), vec![bo_fam, ana_fam]);
    assert_eq!(ids(&["all", "family", "ana"]), vec![bo_fam, ana_fam]);
    assert_eq!(
        ids(&["all", "family", "ana", "only-ana"]),
        vec![ana_fam],
        "intersection: ana, not cy"
    );
    let mut from_ana = k.question(&cfg.chain(&["all"]).unwrap());
    from_ana.from = Some(vec!["ana".into()]);
    let got: Vec<i64> = store.ask(&from_ana).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(got, vec![ana_work, ana_fam]);
}
