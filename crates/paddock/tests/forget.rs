//! Stale cleanup: passed deadlines, forget_after windows, and labels that keep an item.

mod common;

use common::*;
use paddock::*;
use std::fs;

#[test]
fn forget_stale_drops_past_timed() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let y = yesterday();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "old-meet".into(),
        title: "old meet".into(),
        body: "done".into(),
        start: Some(y.clone()),
        end: Some(y),
        ..Default::default()
    })
    .unwrap();
    let n = k.forget_stale().unwrap();
    assert_eq!(n, 1);
    assert!(store.ask(&Question::default()).unwrap().is_empty());
}

#[test]
fn forget_stale_keeps_start_only_past_item() {
    // A `start` with no `end` is a timestamp (e.g. a chat message's send
    // time), not a deadline — it must not trip the "past timed" forget path.
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "old-msg".into(),
        title: "old msg".into(),
        body: "hi".into(),
        start: Some(yesterday()),
        ..Default::default()
    })
    .unwrap();
    let n = k.forget_stale().unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.ask(&Question::default()).unwrap().len(), 1);
}

#[test]
fn forget_stale_keeps_past_timed_todo() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let y = yesterday();
    let id = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "keep-meet".into(),
            title: "keep meet".into(),
            body: "still".into(),
            start: Some(y.clone()),
            end: Some(y),
            ..Default::default()
        })
        .unwrap()
        .id;
    store.note(id, Fact::Label(Label::hand("todo"))).unwrap();
    let n = k.forget_stale().unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.ask(&Question::default()).unwrap().len(), 1);
}

#[test]
fn forget_stale_keeps_untimed_when_forget_after_unset() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    k.admit(NewItem {
        source_id: "incoming".into(),
        foreign_id: "note.md".into(),
        title: "note".into(),
        body: "stay".into(),
        ..Default::default()
    })
    .unwrap();
    let n = k.forget_stale().unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.ask(&Question::default()).unwrap().len(), 1);
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
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = store
        .upsert(&NewItem {
            source_id: "incoming".into(),
            foreign_id: "old.md".into(),
            title: "old".into(),
            body: "aged".into(),
            ..Default::default()
        })
        .unwrap()
        .0;
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
    let n = k.forget_stale().unwrap();
    assert_eq!(n, 1);
    assert!(store.ask(&Question::default()).unwrap().is_empty());
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
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = store
        .upsert(&NewItem {
            source_id: "incoming".into(),
            foreign_id: "mid.md".into(),
            title: "mid".into(),
            body: "source window".into(),
            ..Default::default()
        })
        .unwrap()
        .0;
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
    let n = k.forget_stale().unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.ask(&Question::default()).unwrap().len(), 1);
}
