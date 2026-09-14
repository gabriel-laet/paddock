//! Sources: fs, the exec protocol, and plugins found on PATH, for pull and for send.

mod common;

use common::*;
use paddock::*;
use std::fs;

#[test]
fn fs_pull_and_chain_query() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(paths.incoming_dir.join("hello.md"), "hello body").unwrap();
    fs::write(paths.incoming_dir.join(".hidden"), "no").unwrap();
    fs::create_dir_all(paths.incoming_dir.join("subdir")).unwrap();
    fs::write(paths.incoming_dir.join("subdir").join("nested.md"), "no").unwrap();

    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let n = k.pull().unwrap().count;
    assert_eq!(n, 1);
    let items = store.ask(&Question::default()).unwrap();
    assert_eq!(items[0].foreign_id, "hello.md");
    assert_eq!(items[0].title, "hello");
    assert_eq!(items[0].body, "hello body");

    let all = cfg.chain(&["all"]).unwrap();
    let listed = k.ask(&all).unwrap();
    assert_eq!(listed.len(), 1);
    let later = cfg.chain(&["all", "later"]).unwrap();
    assert!(k.ask(&later).unwrap().is_empty());
}

#[test]
fn send_draft_fs_writes_file_and_text_part() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k
        .send(Draft {
            source_id: "incoming".into(),
            title: "Hello World".into(),
            body: "the body".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let dest = paths.incoming_dir.join("Hello-World.md");
    assert!(dest.exists(), "{}", dest.display());
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "the body");
    let item = store.get(id).unwrap();
    assert_eq!(item.body, "the body");
    assert_eq!(item.parts.len(), 1);
    assert_eq!(item.parts[0].kind, PartKind::Text);
    assert_eq!(item.parts[0].text.as_deref(), Some("the body"));
    assert!(item.thread.is_none());
    assert!(item.cites.is_empty());
}

#[test]
fn an_unknown_kind_is_a_plugin_named_paddock_kind_on_path() {
    let _g = PATH_ENV.lock().unwrap();
    let (tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    // A plugin that reports whether its settings reached it on stdin.
    let plugin = bin.join("paddock-echo");
    fs::write(
        &plugin,
        r#"#!/bin/sh
case "$1" in
  pull)
    req=$(cat)
    case "$req" in *'"greeting":"hi there"'*) t=got-it ;; *) t=missed ;; esac
    printf '[{"foreign_id":"from-plugin","title":"%s"}]\n' "$t" ;;
  send) echo "source cannot send" >&2; exit 2 ;;
esac
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
    fs::write(
        &paths.config_file,
        r#"
[[inbox]]
name = "all"

[[source]]
id = "e"
kind = "echo"
greeting = "hi there"
"#,
    )
    .unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let pulled = k.pull().unwrap();
    assert_eq!(pulled.count, 1);
    let items = store.ask(&Question::default()).unwrap();
    assert_eq!(items[0].foreign_id, "from-plugin");
    assert_eq!(
        items[0].title, "got-it",
        "settings reached the plugin on stdin"
    );
    let err = k
        .send(Draft {
            source_id: "e".into(),
            title: "x".into(),
            body: "y".into(),
            ..Default::default()
        })
        .unwrap_err();
    std::env::set_var("PATH", old_path);
    assert!(err.to_string().contains("cannot send"), "{err}");
}

#[test]
fn fs_video_part() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let p = paths.incoming_dir.join("clip.mp4");
    std::fs::write(&p, b"ftyp").unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k.admit(item_from_file("incoming", &p).unwrap()).unwrap().id;
    let item = store.get(id).unwrap();
    assert_eq!(item.body, "clip.mp4");
    assert_eq!(item.parts.len(), 1);
    assert_eq!(item.parts[0].kind, PartKind::Video);
    assert_eq!(item.parts[0].mime, "video/mp4");
    let part = item.parts[0].clone();
    assert_eq!(part.kind, PartKind::Video);
    assert_eq!(part.size, Some(4));
    assert_eq!(store.blob(part.id).unwrap(), b"ftyp");
}

#[test]
fn send_draft_keeps_source_foreign_id() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k
        .send(Draft {
            source_id: String::new(),
            title: "note".into(),
            body: "x".into(),
            foreign_id: Some("mid-1".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert_eq!(store.get(id).unwrap().foreign_id, "mid-1");
}

#[test]
fn exec_pull_admits_items_including_timed() {
    let (tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let helper = write_exec_helper(tmp.path());
    fs::write(&paths.config_file, exec_source_toml(&helper)).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let n = k.pull().unwrap().count;
    assert_eq!(n, 2);
    let items = store.ask(&Question::default()).unwrap();
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

    let all = cfg.chain(&["all"]).unwrap();
    assert_eq!(k.ask(&all).unwrap().len(), 2);
    let cal = cfg.chain(&["all", "cal"]).unwrap();
    let timed = k.ask(&cal).unwrap();
    assert_eq!(timed.len(), 1);
    assert_eq!(timed[0].foreign_id, "meet-1");
}

#[test]
fn exec_send_uses_returned_foreign_id() {
    let (tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let helper = write_exec_helper(tmp.path());
    fs::write(&paths.config_file, exec_source_toml(&helper)).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k
        .send(Draft {
            source_id: "plug".into(),
            title: "hello".into(),
            body: "out".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
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
    let cfg = load_config(&paths.config_file).unwrap();
    let store = Sqlite::open(&paths.db_path, None).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let err = k.pull().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot run")
            || msg.contains("gone")
            || msg.contains("paddock-no-such-exec-cmd"),
        "{msg}"
    );
    let err = k
        .send(Draft {
            source_id: "gone".into(),
            title: "x".into(),
            body: "y".into(),
            ..Default::default()
        })
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
