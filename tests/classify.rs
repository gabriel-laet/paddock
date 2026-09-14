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
    assert!(item.has("todo"), "root flag-todo regex");
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
    assert!(!item.has("todo"));
    assert!(!item.has("urgent"));

    k.label(id, &["todo".into()], &[]).unwrap();
    let item = store.get(id).unwrap();
    assert!(item.has("todo"));
    assert!(item.has("urgent"), "child classifier after enter");
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
    assert!(!store.get(id).unwrap().has("todo"));
    fs::write(&p, "hello todo").unwrap();
    let id2 = k.admit(item_from_file("incoming", &p).unwrap()).unwrap().id;
    assert_eq!(id, id2);
    assert!(store.get(id).unwrap().has("todo"));
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
    assert!(item.has("rfc"));
}

#[test]
fn a_hand_removal_is_denied_to_classifiers_until_a_hand_relents() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "TODO: call the bank".into(),
            body: "x".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let stamped = store.get(id).unwrap();
    assert!(stamped.has("todo"), "the regex stamped it on admit");
    assert_eq!(
        stamped.labels[0].by,
        By::Classifier("flag-todo".into()),
        "and the label says so"
    );

    k.label(id, &[], &["todo".into()]).unwrap();
    let after = store.get(id).unwrap();
    assert!(!after.has("todo"), "a hand took it off");
    assert!(after.denies("todo"), "and the removal is remembered");
    k.classify(id).unwrap();
    assert!(
        !store.get(id).unwrap().has("todo"),
        "reclassify does not put it back"
    );
    let todo = cfg.chain(&["all", "todo"]).unwrap();
    assert!(
        k.ask(&todo).unwrap().is_empty(),
        "so it left the todo inbox"
    );

    k.label(id, &["todo".into()], &[]).unwrap();
    let relented = store.get(id).unwrap();
    assert!(relented.has("todo"));
    assert!(
        !relented.denies("todo"),
        "a hand putting it back lifts the denial"
    );
    assert_eq!(relented.labels[0].by, By::Hand);
    assert!(
        !relented.labels[0].at.is_empty(),
        "stamped with the kernel's clock"
    );
}

#[test]
fn why_says_who_stamped_each_label_and_what_is_denied() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "an RFC, todo".into(),
            body: "x".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    k.label(id, &["later".into()], &["rfc".into()]).unwrap();
    let item = store.get(id).unwrap();
    let why = k.why(&item, &["all".into(), "later".into()]);
    assert_eq!(why.matched.len(), 1);
    assert_eq!(why.matched[0].name, "later");
    assert_eq!(why.matched[0].by, By::Hand);
    assert_eq!(why.denied.len(), 1);
    assert_eq!(why.denied[0].name, "rfc");
    let why = k.why(&item, &["all".into(), "todo".into()]);
    assert_eq!(why.matched[0].by, By::Classifier("flag-todo".into()));
}

#[test]
fn read_is_a_label_and_a_hands_unread_beats_the_source() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let seen = NewItem {
        source_id: "incoming".into(),
        foreign_id: "a".into(),
        title: "a".into(),
        body: "x".into(),
        read: Some(true),
        ..Default::default()
    };
    let id = k.admit(seen.clone()).unwrap().id;
    let item = store.get(id).unwrap();
    assert!(item.read(), "the source said read");
    assert_eq!(item.labels[0].name, READ);
    assert_eq!(item.labels[0].by, By::Source);

    k.read(id, false).unwrap();
    assert!(!store.get(id).unwrap().read(), "a hand said unread");
    k.admit(seen).unwrap();
    assert!(
        !store.get(id).unwrap().read(),
        "the source cannot override a hand"
    );
    k.read(id, true).unwrap();
    assert!(store.get(id).unwrap().read());
}

#[test]
fn without_is_the_negative_term_and_unread_is_just_without_read() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        format!(
            r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "fresh"
without = ["read", "later"]

[[source]]
id = "incoming"
kind = "fs"
path = "{}"
"#,
            paths.incoming_dir.display()
        ),
    )
    .unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let mut ids = Vec::new();
    for f in ["a", "b", "c"] {
        ids.push(
            k.admit(NewItem {
                source_id: "incoming".into(),
                foreign_id: f.into(),
                title: f.into(),
                body: "x".into(),
                ..Default::default()
            })
            .unwrap()
            .id,
        );
    }
    k.read(ids[0], true).unwrap();
    k.label(ids[1], &["later".into()], &[]).unwrap();
    let fresh = cfg.chain(&["all", "fresh"]).unwrap();
    let got: Vec<i64> = k.ask(&fresh).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(got, vec![ids[2]]);
    // the in-memory rule agrees with the store
    let q = k.question(&fresh);
    for it in store.ask(&Question::default()).unwrap() {
        assert_eq!(q.matches(&it), it.id == ids[2], "#{}", it.id);
    }
    let mut unread = k.question(&cfg.chain(&["all"]).unwrap());
    unread.without.push(READ.into());
    assert_eq!(store.count(&unread).unwrap(), 2);
}

#[test]
fn an_inbox_effect_sends_once_and_labels_on_enter() {
    let (tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let helper = write_exec_helper(tmp.path());
    fs::write(
        &paths.config_file,
        format!(
            r#"
[[inbox]]
name = "all"

[[inbox.inbox]]
name = "drafts"
sources = ["incoming"]

[[inbox.inbox.inbox]]
name = "approved"
labels = ["approved"]
then = ["send:plug", "label:done", "read"]

[[source]]
id = "incoming"
kind = "fs"
path = "{}"

[[source]]
id = "plug"
kind = "exec"
cmd = "sh"
args = ["{}"]
"#,
            paths.incoming_dir.display(),
            helper.display()
        ),
    )
    .unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let draft = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "d.md".into(),
            title: "hello".into(),
            body: "please ship".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert_eq!(
        store.count(&Question::default()).unwrap(),
        1,
        "nothing sent yet"
    );

    let warnings = k.label(draft, &["approved".into()], &[]).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let it = store.get(draft).unwrap();
    assert!(it.has(SENT) && it.has("done") && it.read());
    assert_eq!(
        it.labels.iter().find(|l| l.name == SENT).unwrap().by,
        By::Inbox("all/drafts/approved".into())
    );
    let sent: Vec<Item> = store
        .ask(&Question::default())
        .unwrap()
        .into_iter()
        .filter(|i| i.source_id == "plug")
        .collect();
    assert_eq!(sent.len(), 1, "the exec source delivered it once");
    assert_eq!(sent[0].foreign_id, "sent-1");
    assert_eq!(sent[0].body, "please ship");

    k.classify(draft).unwrap();
    k.classify(draft).unwrap();
    let again = store.ask(&Question::default()).unwrap();
    assert_eq!(
        again.iter().filter(|i| i.source_id == "plug").count(),
        1,
        "sent stays sent"
    );
}

#[test]
fn a_persona_is_an_inbox_with_sources_and_send_in_picks_its_source() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        format!(
            r#"
[[inbox]]
name = "work"
sources = ["work-mail"]

[[inbox.inbox]]
name = "todo"
labels = ["todo"]

[[inbox]]
name = "personal"
sources = ["home"]

[[source]]
id = "home"
kind = "fs"
path = "{0}/home"

[[source]]
id = "work-mail"
kind = "fs"
path = "{0}/work"
"#,
            paths.incoming_dir.display()
        ),
    )
    .unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let work_todo = cfg.chain(&["work", "todo"]).unwrap();
    assert_eq!(k.source_for(&work_todo).as_deref(), Some("work-mail"));
    assert_eq!(
        k.source_for(&cfg.chain(&["personal"]).unwrap()).as_deref(),
        Some("home")
    );
    let id = k
        .send(Draft {
            source_id: k.source_for(&work_todo).unwrap(),
            title: "standup".into(),
            body: "notes".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let it = store.get(id).unwrap();
    assert_eq!(it.source_id, "work-mail");
    assert!(k
        .ask(&cfg.chain(&["work"]).unwrap())
        .unwrap()
        .iter()
        .any(|i| i.id == id));
    assert!(
        k.ask(&cfg.chain(&["personal"]).unwrap())
            .unwrap()
            .is_empty(),
        "personas do not leak"
    );
    assert_eq!(ActorKind::parse("agent"), ActorKind::Agent);
}
