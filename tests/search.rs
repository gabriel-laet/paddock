//! Search: words through the text index, meaning through the embedder, answers through the model.

mod common;

use common::*;
use paddock::*;
use std::fs;

#[test]
fn text_term_searches_title_and_text_by_prefix() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let a = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "Invoice from Ana".into(),
            body: "please pay by friday".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let b = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "b".into(),
            title: "lunch".into(),
            body: "ana says friday works".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let ask = |text: &str| -> Vec<i64> {
        let q = Question {
            text: Some(text.into()),
            ..Default::default()
        };
        let ids: Vec<i64> = store.ask(&q).unwrap().iter().map(|i| i.id).collect();
        // the in-memory rule agrees with the store on every item
        for it in store.ask(&Question::default()).unwrap() {
            assert_eq!(q.matches(&it), ids.contains(&it.id), "{text} on #{}", it.id);
        }
        ids
    };
    assert_eq!(ask("invoice"), vec![a]);
    assert_eq!(ask("ana friday"), vec![b, a]);
    assert_eq!(ask("lunch ana"), vec![b]);
    assert_eq!(ask("nothing"), Vec::<i64>::new());
    assert_eq!(
        store
            .count(&Question {
                text: Some("inv".into()),
                ..Default::default()
            })
            .unwrap(),
        1
    );
    assert!(k.forget(a).unwrap());
    assert_eq!(ask("invoice"), Vec::<i64>::new());
}

#[test]
fn items_are_embedded_on_admit_and_ranked_by_meaning() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(&paths.config_file, ai_toml(&paths.incoming_dir)).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let money = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "send money".into(),
            body: "x".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let lunch = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "b".into(),
            title: "lunch".into(),
            body: "y".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert!(store.unembedded().unwrap().is_empty(), "admit embeds");
    let q = Question {
        near: Some(k.near("money please").unwrap()),
        limit: Some(1),
        ..Default::default()
    };
    let ids: Vec<i64> = store.ask(&q).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![money]);
    let q = Question {
        near: Some(k.near("food").unwrap()),
        ..Default::default()
    };
    let ids: Vec<i64> = store.ask(&q).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![lunch, money]);
    // a question still composes: an inbox chain narrows what gets ranked
    let chain = cfg.chain(&["all", "money"]).unwrap();
    k.label(lunch, &["money".into()], &[]).unwrap();
    let mut q = Question::of(&chain);
    q.near = Some(k.near("food").unwrap());
    let ids: Vec<i64> = store.ask(&q).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![lunch]);
}

#[test]
fn embed_missing_backfills_and_answer_cites_only_shown_items() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    // admit with no embedder, then turn one on
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let id = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "invoice".into(),
            body: "send money".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert_eq!(store.unembedded().unwrap(), vec![id]);
    fs::write(&paths.config_file, ai_toml(&paths.incoming_dir)).unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    assert_eq!(k.embed_missing().unwrap().count, 1);
    assert_eq!(k.embed_missing().unwrap().count, 0);
    let all = cfg.chain(&["all"]).unwrap();
    let a = k.answer(&all, "what should I pay?").unwrap();
    assert!(a.text.contains("invoice"));
    assert_eq!(a.cites, vec![id], "cited once, and #999 was never shown");
    assert_eq!(a.considered, vec![id]);
}

#[test]
fn without_a_model_or_embedder_the_verbs_say_so() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let k = kernel(&cfg, &store).unwrap();
    let all = cfg.chain(&["all"]).unwrap();
    assert!(k
        .answer(&all, "?")
        .unwrap_err()
        .to_string()
        .contains("no model"));
    assert!(k.near("x").unwrap_err().to_string().contains("no embedder"));
    assert_eq!(k.embed_missing().unwrap().count, 0);
}
