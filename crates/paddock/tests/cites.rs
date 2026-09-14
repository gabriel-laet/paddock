//! Cites: one shape for replies, forwards, quotes, mentions, and attachments.
//! A cite names an item by its source's id and resolves when that item is
//! here, early or late; a thread is the source's key, else what cites join.

mod common;

use common::*;
use paddock::*;

fn note(source: &str, foreign: &str, title: &str) -> NewItem {
    NewItem {
        source_id: source.into(),
        foreign_id: foreign.into(),
        title: title.into(),
        body: "x".into(),
        ..Default::default()
    }
}

#[test]
fn a_reply_by_send_joins_the_parent_thread_and_cites_it() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let parent = store.upsert(&note("incoming", "p.md", "parent")).unwrap().0;
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
    let child = store.get(id).unwrap();
    assert_eq!(
        store.get(parent).unwrap().thread.as_deref(),
        Some("incoming:p.md")
    );
    assert_eq!(child.thread.as_deref(), Some("incoming:p.md"));
    assert_eq!(child.reply_to(), Some(parent));
    assert_eq!(child.cites[0].kind, CiteKind::Reply);
    let ids: Vec<i64> = k.thread(id).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![id, parent], "newest first");
}

#[test]
fn a_cite_resolves_when_the_parent_is_already_here() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let parent = k.admit(note("incoming", "p.md", "parent")).unwrap().id;
    let mut child = note("incoming", "c.md", "re: parent");
    child.cites = vec![Cite::reply("p.md")];
    let child = k.admit(child).unwrap().id;
    assert_eq!(store.get(child).unwrap().reply_to(), Some(parent));
    let citing: Vec<i64> = store.citing(parent).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(citing, vec![child]);
}

#[test]
fn a_cite_to_an_item_not_here_yet_resolves_when_it_arrives() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let mut child = note("incoming", "c.md", "re: parent");
    child.cites = vec![Cite {
        excerpt: Some("first".into()),
        ..Cite::reply("p.md")
    }];
    let child = k.admit(child).unwrap().id;
    let early = store.get(child).unwrap();
    assert_eq!(early.reply_to(), None, "not here yet");
    assert_eq!(early.cites[0].foreign_id.as_deref(), Some("p.md"));
    assert_eq!(early.cites[0].excerpt.as_deref(), Some("first"));

    let parent = k.admit(note("incoming", "p.md", "parent")).unwrap().id;
    let late = store.get(child).unwrap();
    assert_eq!(late.reply_to(), Some(parent), "stitched on arrival");
    assert_eq!(
        late.cites[0].excerpt.as_deref(),
        Some("first"),
        "and nothing else changed"
    );
}

#[test]
fn forwards_mentions_and_attachments_are_the_same_shape() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let orig = k.admit(note("incoming", "orig.md", "orig")).unwrap().id;
    let mut fwd = note("incoming", "fwd.md", "fwd");
    fwd.cites = vec![
        Cite::forward("orig.md"),
        Cite {
            href: Some("https://example.com/spec".into()),
            ..Cite::to(CiteKind::Mention, "")
        },
        Cite {
            kind: CiteKind::Attach,
            href: Some("/tmp/deck.pdf".into()),
            actor: Some(Actor {
                id: "ana@example.com".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];
    let fwd = k.admit(fwd).unwrap().id;
    let it = store.get(fwd).unwrap();
    let kinds: Vec<CiteKind> = it.cites.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        vec![CiteKind::Forward, CiteKind::Mention, CiteKind::Attach]
    );
    assert_eq!(it.cites[0].id, Some(orig));
    assert_eq!(
        it.cites[1].href.as_deref(),
        Some("https://example.com/spec")
    );
    assert_eq!(
        it.cites[1].foreign_id, None,
        "an empty foreign id is no foreign id"
    );
    assert_eq!(
        it.cites[2].actor.as_ref().map(|a| a.id.as_str()),
        Some("ana@example.com")
    );
    assert_eq!(it.reply_to(), None, "a forward is not a reply");
}

#[test]
fn a_thread_without_a_key_is_what_replies_join_in_both_directions() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let root = k.admit(note("incoming", "a", "a")).unwrap().id;
    let mut b = note("incoming", "b", "b");
    b.cites = vec![Cite::reply("a")];
    let b = k.admit(b).unwrap().id;
    let mut c = note("incoming", "c", "c");
    c.cites = vec![Cite::reply("b")];
    let c = k.admit(c).unwrap().id;
    let mut d = note("incoming", "d", "d");
    d.cites = vec![Cite::to(CiteKind::Mention, "a")];
    let d = k.admit(d).unwrap().id;
    let lone = k.admit(note("incoming", "e", "e")).unwrap().id;

    let from_middle: Vec<i64> = k.thread(b).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(
        from_middle,
        vec![c, b, root],
        "up to the root and down to the leaves"
    );
    assert!(
        !from_middle.contains(&d),
        "a mention does not join a thread"
    );
    assert_eq!(k.thread(lone).unwrap().len(), 1);
}
