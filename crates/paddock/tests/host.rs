//! A host on disk: paths, init, the default config, and the clock the kernel is given.

mod common;

use common::*;
use paddock::*;
use std::fs;
use std::path::PathBuf;

#[test]
fn init_is_idempotent() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    init(&paths).unwrap();
    assert!(paths.config_file.exists());
    assert!(paths.incoming_dir.exists());
    assert!(paths.db_path.exists());
}

#[test]
fn nested_config_parses() {
    let toml = default_config_toml("/tmp/incoming");
    let cfg: Config = toml::from_str(&toml).unwrap();
    assert_eq!(cfg.inbox.len(), 1);
    assert_eq!(cfg.inbox[0].name, "all");
    assert_eq!(cfg.inbox[0].classifier.len(), 1);
    assert_eq!(cfg.inbox[0].classifier[0].id, "flag-todo");
    assert_eq!(cfg.use_, ["codes", "mentions"], "skills, grafted at load");
    assert_eq!(cfg.inbox[0].inbox.len(), 3);
    assert_eq!(cfg.inbox[0].inbox[0].name, "later");
    assert_eq!(cfg.inbox[0].inbox[0].labels, vec!["later"]);
    assert!(cfg.inbox[0].inbox[0].classifier.is_empty());
    assert_eq!(cfg.inbox[0].inbox[1].name, "todo");
    assert_eq!(cfg.inbox[0].inbox[1].labels, vec!["todo"]);
    assert_eq!(cfg.inbox[0].inbox[2].name, "cal");
    assert!(cfg.inbox[0].inbox[2].timed);
    assert!(cfg.inbox[0].inbox[2].labels.is_empty());
    assert_eq!(cfg.source[0].kind, "fs");
}

#[test]
fn expand_tilde() {
    let p = expand_path("~/incoming");
    assert!(p.is_absolute() || !p.starts_with("~"));
    assert!(p.ends_with(PathBuf::from("incoming")));
}

#[test]
fn default_init_stays_regex_list() {
    let toml = default_config_toml("/tmp/incoming");
    assert!(!toml.contains("kind = \"script\""));
    assert!(!toml.contains("kind = \"llm\""));
    let cfg: Config = toml::from_str(&toml).unwrap();
    let cal = cfg.inbox[0]
        .inbox
        .iter()
        .find(|i| i.name == "cal")
        .expect("cal child");
    assert!(cal.timed);
}

#[test]
fn discover_walks_up_to_dot_paddock() {
    let _g = PATH_ENV.lock().unwrap();
    std::env::remove_var("PADDOCK_DIR");
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("proj");
    let child = proj.join("a").join("b");
    fs::create_dir_all(&child).unwrap();
    let root = proj.join(".paddock");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("config.toml"), "\n").unwrap();
    let paths = Paths::discover(&child);
    assert_eq!(paths.config_dir, root);
    assert_eq!(paths.config_file, root.join("config.toml"));
    assert_eq!(paths.db_path, root.join("paddock.db"));
    assert_eq!(paths.incoming_dir, root.join("incoming"));
    assert_eq!(paths.data_dir, root);
}

#[test]
fn discover_paddock_dir_env_wins() {
    let _g = PATH_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let env_root = dir.path().join("host");
    fs::create_dir_all(&env_root).unwrap();
    let other = dir.path().join("proj");
    fs::create_dir_all(other.join(".paddock")).unwrap();
    std::env::set_var("PADDOCK_DIR", &env_root);
    let paths = Paths::discover(&other);
    std::env::remove_var("PADDOCK_DIR");
    assert_eq!(paths.config_dir, env_root);
    assert_eq!(paths.db_path, env_root.join("paddock.db"));
}

#[test]
fn discover_falls_back_to_xdg() {
    let _g = PATH_ENV.lock().unwrap();
    std::env::remove_var("PADDOCK_DIR");
    let dir = tempfile::tempdir().unwrap();
    let start = dir.path().join("empty");
    fs::create_dir_all(&start).unwrap();
    let xdg_cfg = dir.path().join("xdg-cfg");
    let xdg_data = dir.path().join("xdg-data");
    std::env::set_var("XDG_CONFIG_HOME", &xdg_cfg);
    std::env::set_var("XDG_DATA_HOME", &xdg_data);
    let paths = Paths::discover(&start);
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("XDG_DATA_HOME");
    assert_eq!(paths.config_dir, xdg_cfg.join("paddock"));
    assert_eq!(paths.data_dir, xdg_data.join("paddock"));
    assert_eq!(
        paths.incoming_dir,
        xdg_data.join("paddock").join("incoming")
    );
}

#[test]
fn init_here_creates_dot_paddock() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::here(dir.path());
    init(&paths).unwrap();
    assert!(dir.path().join(".paddock/config.toml").exists());
    assert!(dir.path().join(".paddock/incoming").is_dir());
    assert!(dir.path().join(".paddock/paddock.db").exists());
    assert_eq!(paths.db_path, dir.path().join(".paddock/paddock.db"));
}

#[test]
fn default_init_has_no_brand_exec_sources() {
    let toml = default_config_toml("/tmp/incoming");
    assert!(!toml.contains("gog"));
    assert!(!toml.contains("hey"));
    assert!(!toml.contains("wacli"));
    let cfg: Config = toml::from_str(&toml).unwrap();
    assert!(cfg.source.iter().all(|s| s.kind == "fs"));
    assert_eq!(cfg.source.len(), 1);
    let cal = cfg.inbox[0]
        .inbox
        .iter()
        .find(|i| i.name == "cal")
        .expect("cal child");
    assert!(cal.timed);
}

#[test]
fn source_label_falls_back_to_id() {
    let cfg = Config {
        source: vec![
            SourceSpec {
                id: "chat".into(),
                kind: "exec".into(),
                name: Some("Messages".into()),
                ..Default::default()
            },
            SourceSpec {
                id: "incoming".into(),
                kind: "fs".into(),
                name: None,
                ..Default::default()
            },
            SourceSpec {
                id: "blank".into(),
                kind: "fs".into(),
                name: Some("   ".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(cfg.source_name("chat"), "Messages");
    assert_eq!(cfg.source_name("incoming"), "incoming");
    assert_eq!(cfg.source_name("blank"), "blank");
    assert_eq!(cfg.source_name("missing"), "missing");
}

#[test]
fn the_kernel_runs_at_the_clock_it_is_given() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::write(
        &paths.config_file,
        r#"
forget_after = "7d"

[[inbox]]
name = "all"

[[inbox.inbox]]
name = "recent"
newer_than = "1d"

[[source]]
id = "incoming"
kind = "fs"
path = "/tmp"
"#,
    )
    .unwrap();
    let (cfg, store) = load(&paths).unwrap();
    let today = kernel(&cfg, &store).unwrap();
    let id = today
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "a".into(),
            title: "a".into(),
            body: "b".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let recent = cfg.chain(&["all", "recent"]).unwrap();
    assert_eq!(today.ask(&recent).unwrap().len(), 1);
    assert_eq!(today.forget_stale().unwrap(), 0);

    let next_month = kernel_at(
        &cfg,
        &store,
        chrono::Utc::now() + chrono::Duration::days(30),
    )
    .unwrap();
    assert_eq!(
        next_month.ask(&recent).unwrap().len(),
        0,
        "a month on, nothing is recent"
    );
    assert_eq!(
        next_month.forget_stale().unwrap(),
        1,
        "and the item is stale"
    );
    assert!(store.get(id).is_err());
}

#[test]
fn use_grafts_skills_into_the_config_when_it_loads() {
    let (_tmp, paths) = temp_paths();
    init(&paths).unwrap();
    fs::create_dir_all(paths.config_dir.join("skills")).unwrap();
    fs::write(
        paths.config_dir.join("skills/family.toml"),
        "# family: the people who matter\n[[inbox]]\nname = \"family\"\nfrom = [\"ana@example.com\"]\nthen = [\"notify\"]\n",
    )
    .unwrap();
    fs::write(
        &paths.config_file,
        format!(
            r#"
use = ["codes"]

[[inbox]]
name = "all"

[[inbox.inbox]]
name = "personal"
use = ["family"]

[[source]]
id = "incoming"
kind = "fs"
path = "{}"
"#,
            paths.incoming_dir.display()
        ),
    )
    .unwrap();
    let cfg = load_config(&paths.config_file).unwrap();
    assert!(
        cfg.chain(&["all", "codes"]).is_some(),
        "shipped skill under all"
    );
    assert!(
        cfg.chain(&["all", "personal", "family"]).is_some(),
        "yours under the persona"
    );
    let ids: Vec<String> = cfg.classifiers().iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids, ["codes/detect"]);
    let listed = skills(&paths.config_dir);
    assert_eq!(listed[0].name, "family");
    assert_eq!(listed[0].about, "the people who matter");
    assert!(listed
        .iter()
        .any(|s| s.name == "codes" && s.origin == "shipped"));

    // A fresh host pages on codes out of the box, and a code carries its label.
    let (_tmp2, fresh) = temp_paths();
    init(&fresh).unwrap();
    let (cfg, store) = load(&fresh).unwrap();
    assert!(cfg.chain(&["all", "codes"]).is_some());
    let k = kernel(&cfg, &store).unwrap();
    let admitted = k
        .admit(NewItem {
            source_id: "incoming".into(),
            foreign_id: "c".into(),
            title: "Your verification code is 483920".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(admitted.notices.len(), 1);
    assert_eq!(admitted.notices[0].inbox, "all/codes");
    assert_eq!(admitted.notices[0].labels, ["code"]);
}
