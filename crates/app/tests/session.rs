//! A Session over a throwaway host: verbs, events, notices, change from
//! another process, and setup by a fake agent.

use paddock_app::{Event, Listing, Session};
use std::fs;
use std::sync::{Arc, Mutex};

fn host(tmp: &std::path::Path, agent: &str) -> std::path::PathBuf {
    let root = tmp.join("host");
    fs::create_dir_all(root.join("incoming")).unwrap();
    fs::write(
        root.join("config.toml"),
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
then = ["notify"]

[[source]]
id = "incoming"
kind = "fs"
path = "{incoming}"
{agent}
"#,
            incoming = root.join("incoming").display(),
            agent = agent
        ),
    )
    .unwrap();
    root
}

fn events(session: &Session) -> Arc<Mutex<Vec<Event>>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    session.subscribe(Box::new(move |e| sink.lock().unwrap().push(e.clone())));
    log
}

#[test]
fn a_pull_admits_counts_and_raises_a_notice_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = host(tmp.path(), "");
    fs::write(root.join("incoming/a.md"), "todo: buy milk").unwrap();
    fs::write(root.join("incoming/b.md"), "just a note").unwrap();
    let session = Session::open(Some(&root)).unwrap();
    let log = events(&session);

    let report = session.pull().unwrap();
    assert_eq!(report.count, 2);
    assert_eq!(report.notices.len(), 1);
    assert_eq!(report.notices[0].inbox, "all/todo");
    assert_eq!(report.notices[0].title, "a");
    let seen = log.lock().unwrap().clone();
    assert!(seen.contains(&Event::Changed));
    assert!(
        matches!(seen.iter().find(|e| matches!(e, Event::Notice(_))), Some(Event::Notice(n)) if n.inbox == "all/todo")
    );

    let tree = session.inboxes().unwrap();
    assert_eq!(tree[0].path, "all");
    assert_eq!((tree[0].unread, tree[0].total), (2, 2));
    assert_eq!(tree[1].path, "all/todo");
    assert_eq!((tree[1].depth, tree[1].total), (1, 1));

    // The same item entering again raises nothing: once per entry.
    let again = session.pull().unwrap();
    assert_eq!(again.count, 0);
    assert!(again.notices.is_empty());

    // A hand's label opens the child and the notice fires for that item, once.
    let note = session
        .items("all", &Listing::default())
        .unwrap()
        .into_iter()
        .find(|i| i.title == "b")
        .unwrap();
    let told = session.label(note.id, &["todo".into()], &[]).unwrap();
    assert_eq!(told.notices.len(), 1);
    assert_eq!(told.notices[0].id, note.id);
    assert!(session.classify(note.id).unwrap().notices.is_empty());
    let unread = session
        .items(
            "all/todo",
            &Listing {
                unread: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(unread.len(), 2);
    session.read(note.id, true).unwrap();
    assert_eq!(session.inboxes().unwrap()[1].unread, 1);
    assert_eq!(session.why(note.id, "all/todo").unwrap().matched.len(), 1);
}

#[test]
fn another_process_s_write_shows_up_on_poll_but_our_own_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let root = host(tmp.path(), "");
    fs::write(root.join("incoming/a.md"), "hello").unwrap();
    let ours = Session::open(Some(&root)).unwrap();
    let theirs = Session::open(Some(&root)).unwrap();
    assert!(!ours.poll().unwrap(), "nothing yet");
    ours.pull().unwrap();
    assert!(!ours.poll().unwrap(), "our own write is not news");
    let id = ours.items("all", &Listing::default()).unwrap()[0].id;
    theirs.label(id, &["later".into()], &[]).unwrap();
    let log = events(&ours);
    assert!(ours.poll().unwrap(), "their write is");
    assert_eq!(log.lock().unwrap().as_slice(), &[Event::Changed]);
    assert!(!ours.poll().unwrap(), "and only once");
    assert!(ours.item(id).unwrap().has("later"));

    let shared = Arc::new(theirs);
    let log = events(&shared);
    let watcher = shared.watch(std::time::Duration::from_millis(10));
    ours.read(id, true).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while log.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(watcher);
    assert!(
        log.lock().unwrap().contains(&Event::Changed),
        "the watcher saw it"
    );
}

#[test]
fn setup_briefs_the_agent_streams_its_lines_and_reloads_the_config() {
    let tmp = tempfile::tempdir().unwrap();
    let agent = tmp.path().join("agent.sh");
    let seen = tmp.path().join("prompt.txt");
    // A fake agent: keeps its prompt, appends a source to the config it was
    // told about, says two lines.
    fs::write(
        &agent,
        format!(
            r#"#!/bin/sh
cat > "{seen}"
printf '\n[[source]]\nid = "added"\nkind = "fs"\npath = "%s"\n' "$PWD/added" >> config.toml
echo "added a source"
echo "done" >&2
"#,
            seen = seen.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    let root = host(
        tmp.path(),
        &format!("[agent]\ncmd = \"sh\"\nargs = [\"{}\"]\n", agent.display()),
    );
    let session = Session::open(Some(&root)).unwrap();
    let log = events(&session);
    assert_eq!(session.config().source.len(), 1);

    let status = session.setup("add a folder called added").unwrap();
    assert_eq!(status, 0);
    let prompt = fs::read_to_string(&seen).unwrap();
    assert!(
        prompt.contains("## task\nadd a folder called added"),
        "{prompt}"
    );
    assert!(prompt.contains(&root.join("config.toml").display().to_string()));
    assert!(
        prompt.contains("kind = \"fs\""),
        "the config text is in the briefing"
    );
    assert!(
        prompt.contains("NAME_cmd"),
        "the secrets rule is in the briefing"
    );
    let lines: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::Setup(l) => Some(l.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(lines, ["added a source", "done"]);
    assert_eq!(session.config().source.len(), 2, "reloaded");
    assert!(log.lock().unwrap().contains(&Event::Changed));

    let broken = tmp.path().join("broken.sh");
    fs::write(
        &broken,
        "#!/bin/sh\ncat >/dev/null\necho 'not = [toml' >> config.toml\n",
    )
    .unwrap();
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        root.join("config.toml"),
        fs::read_to_string(root.join("config.toml"))
            .unwrap()
            .replace(&agent.display().to_string(), &broken.display().to_string()),
    )
    .unwrap();
    session.reload().unwrap();
    let err = session.setup("break it").unwrap_err();
    assert!(
        err.to_string().contains("the config the agent left"),
        "{err:#}"
    );
    assert_eq!(session.config().source.len(), 2, "the old config stands");
}
