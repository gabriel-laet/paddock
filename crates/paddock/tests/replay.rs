//! A replay shows what a candidate config would change and writes nothing.

mod common;

use common::*;
use paddock::*;
use std::fs;

fn config(incoming: &std::path::Path, extra: &str) -> String {
    format!(
        r#"
[[inbox]]
name = "all"

[[inbox.classifier]]
id = "flag-todo"
kind = "regex"
pattern = "(?i)todo"
label = "todo"

[[inbox.inbox]]
name = "todo"
labels = ["todo"]
{extra}

[[source]]
id = "incoming"
kind = "fs"
path = "{}"
"#,
        incoming.display()
    )
}

#[test]
fn a_replay_diffs_labels_and_inboxes_under_a_candidate_and_leaves_the_store_alone() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(&paths.config_file, config(&paths.incoming_dir, "")).unwrap();
    fs::write(paths.incoming_dir.join("a.md"), "todo: pay the invoice").unwrap();
    fs::write(paths.incoming_dir.join("b.md"), "lunch?").unwrap();
    let (current, store) = load(&paths).unwrap();
    kernel(&current, &store).unwrap().pull().unwrap();
    let a = store.find("incoming", "a.md").unwrap().unwrap();
    let b = store.find("incoming", "b.md").unwrap().unwrap();
    assert!(store.get(a).unwrap().has("todo"));

    // The candidate: an invoice classifier and a paging inbox; todo is gone.
    let candidate_file = paths.config_dir.join("candidate.toml");
    fs::write(
        &candidate_file,
        config(
            &paths.incoming_dir,
            r#"
[[inbox.classifier]]
id = "money"
kind = "regex"
pattern = "(?i)invoice"
label = "money"

[[inbox.inbox]]
name = "money"
labels = ["money"]
then = ["notify", "label:seen-it"]
"#,
        )
        .replace("pattern = \"(?i)todo\"", "pattern = \"(?i)never-matches\""),
    )
    .unwrap();
    let candidate = load_config(&candidate_file).unwrap();

    let r = replay(&paths, &current, &store, &candidate, None, None).unwrap();
    assert_eq!(r.items, 2);
    assert_eq!(r.changes.len(), 1, "{:?}", r.changes);
    let c = &r.changes[0];
    assert_eq!(c.id, a);
    assert_eq!(
        c.added,
        ["money", "seen-it"],
        "the classifier and the label: effect, both"
    );
    assert!(
        c.removed.is_empty(),
        "a label already stamped stays: {:?}",
        c.removed
    );
    assert_eq!(c.entered, ["all/money"]);
    assert!(c.left.is_empty());
    assert_eq!(r.notices.len(), 1);
    assert_eq!(r.notices[0].inbox, "all/money");
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(
        c.line().contains("+money") && c.line().contains("→ all/money"),
        "{}",
        c.line()
    );

    // Nothing was written to the real store: no label, no effect, no run-once
    // mark, no leftover file.
    let after = store.get(a).unwrap();
    assert!(!after.has("money") && !after.has("seen-it"));
    assert!(!store.seen(a, "then:all/money:notify").unwrap());
    assert!(!paths.db_path.with_extension("db.replay").exists());
    assert!(!store.get(b).unwrap().has("money"));

    // Scoped to an inbox, only its items replay.
    let scoped = replay(&paths, &current, &store, &candidate, Some("all/todo"), None).unwrap();
    assert_eq!(scoped.items, 1);
    let capped = replay(&paths, &current, &store, &candidate, None, Some(1)).unwrap();
    assert_eq!(capped.items, 1);
}
