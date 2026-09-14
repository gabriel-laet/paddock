//! The façade a native app talks to. One [`Session`] per host: every verb
//! the CLI has, as a method; what a pass said (warnings, notices) as
//! events; and a way to learn that the store changed under you, whether
//! this session wrote or another process did.
//!
//! A GUI on Linux uses this crate directly. Other languages get bindings
//! generated over it; nothing here has a lifetime or a borrowed return,
//! so it binds cleanly.
//!
//! Settings are not a screen: [`Session::setup`] hands the host and a task
//! in words to the agent named in `[agent]`, which edits the config and
//! runs `paddock check` and `paddock pull`; the session reloads after.

use anyhow::{Context, Result};
use paddock::adapters::agent::counts;
use paddock::{
    agents_on_path, briefing, kernel, load, load_config, load_config_in, mirror_after_pull,
    push_mirror, resolve_secrets, setup, Admitted, AgentSpec, Answer, Config, Draft, Item, Kernel,
    Notice, Paths, Question, Report, Sqlite, Store, Told, Why, READ,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// What a session tells its subscribers.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Items, labels, or the config changed; views should ask again.
    Changed,
    /// An inbox with `then = ["notify"]` raised its hand for an item.
    Notice(Notice),
    /// Something did not stop the verb but should be seen.
    Warning(String),
    /// A line the setup agent printed.
    Setup(String),
}

pub type Callback = Box<dyn Fn(&Event) + Send + Sync>;

/// How to list an inbox: the CLI's `ls` flags.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub unread: bool,
    /// Words that must all appear in the title or text.
    pub text: Option<String>,
    /// Closest in meaning to this, through the embedder.
    pub like: Option<String>,
    pub limit: Option<usize>,
    pub from: Vec<String>,
    pub to: Vec<String>,
}

/// One row of the inbox tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InboxCount {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub unread: usize,
    pub total: usize,
}

/// One host, open. Cheap to call; safe to share between threads.
pub struct Session {
    paths: Paths,
    config: RwLock<Config>,
    store: Sqlite,
    subscribers: Mutex<Vec<(u64, Callback)>>,
    next_subscriber: AtomicU64,
    /// The store's data version as of the last look: others' commits move it.
    seen_version: AtomicI64,
}

impl Session {
    /// Open the host at `dir`, else the one the environment names
    /// (`PADDOCK_DIR`, a `.paddock/` above, XDG). Initializes a fresh host.
    /// Every `NAME_cmd` secret is run once, here.
    pub fn open(dir: Option<&Path>) -> Result<Session> {
        let paths = match dir {
            Some(d) => Paths::from_root(d.to_path_buf()),
            None => Paths::from_env(),
        };
        let (config, store) = load(&paths)?;
        let config = resolve_secrets(config)?;
        let seen_version = AtomicI64::new(store.data_version()?);
        Ok(Session {
            paths,
            config: RwLock::new(config),
            store,
            subscribers: Mutex::new(Vec::new()),
            next_subscriber: AtomicU64::new(1),
            seen_version,
        })
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn config_file(&self) -> PathBuf {
        self.paths.config_file.clone()
    }

    /// The config as loaded, secrets resolved.
    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    /// Re-read the config from disk. A broken file leaves the old config
    /// in place and returns the error.
    pub fn reload(&self) -> Result<()> {
        let config = resolve_secrets(load_config(&self.paths.config_file)?)?;
        *self.config.write().unwrap() = config;
        self.emit(&Event::Changed);
        Ok(())
    }

    /// Run a verb on a kernel built from the current config.
    fn with<T>(&self, f: impl FnOnce(&Kernel, &Config) -> Result<T>) -> Result<T> {
        let config = self.config.read().unwrap();
        let k = kernel(&config, &self.store)?;
        f(&k, &config)
    }

    // ----- reading -----

    /// The inbox tree with counts, depth first, in config order.
    pub fn inboxes(&self) -> Result<Vec<InboxCount>> {
        let config = self.config.read().unwrap();
        Ok(config
            .nodes()
            .iter()
            .map(|node| {
                let (unread, total) = counts(&config, &self.store, &node.path);
                InboxCount {
                    path: node.path.join("/"),
                    name: node.inbox.name.clone(),
                    depth: node.depth,
                    unread,
                    total,
                }
            })
            .collect())
    }

    /// Items answering an inbox path (`all`, `all/todo`), narrowed by the listing.
    pub fn items(&self, inbox: &str, listing: &Listing) -> Result<Vec<Item>> {
        self.with(|k, config| {
            let chain = chain(config, inbox)?;
            let mut q = k.question(&chain);
            if listing.unread {
                q.without.push(READ.into());
            }
            q.text = listing.text.clone();
            q.limit = listing.limit;
            if !listing.from.is_empty() {
                q.from = Some(listing.from.clone());
            }
            if !listing.to.is_empty() {
                q.to = Some(listing.to.clone());
            }
            if let Some(like) = &listing.like {
                q.near = Some(k.near(like)?);
            }
            self.store.ask(&q)
        })
    }

    pub fn item(&self, id: i64) -> Result<Item> {
        self.store.get(id)
    }

    /// The source's thread, else what replies and forwards join.
    pub fn thread(&self, id: i64) -> Result<Vec<Item>> {
        self.with(|k, _| k.thread(id))
    }

    pub fn cited(&self, id: i64) -> Result<Vec<Item>> {
        self.store.citing(id)
    }

    /// The bytes of a non-text part.
    pub fn part(&self, part_id: i64) -> Result<Vec<u8>> {
        self.store.blob(part_id)
    }

    /// Why an item sits in an inbox.
    pub fn why(&self, id: i64, inbox: &str) -> Result<Why> {
        self.with(|k, _| {
            let it = self.store.get(id)?;
            Ok(k.why(&it, &split_path(inbox)))
        })
    }

    // ----- writing -----

    pub fn label(&self, id: i64, add: &[String], remove: &[String]) -> Result<Told> {
        let told = self.with(|k, _| k.label(id, add, remove))?;
        self.told(&told);
        Ok(told)
    }

    pub fn read(&self, id: i64, read: bool) -> Result<Told> {
        let told = self.with(|k, _| k.read(id, read))?;
        self.told(&told);
        Ok(told)
    }

    pub fn classify(&self, id: i64) -> Result<Told> {
        let told = self.with(|k, _| k.classify(id))?;
        self.told(&told);
        Ok(told)
    }

    pub fn forget(&self, id: i64) -> Result<bool> {
        let gone = self.with(|k, _| k.forget(id))?;
        if gone {
            self.emit(&Event::Changed);
        }
        Ok(gone)
    }

    /// Hand a draft to its source (the first one when the draft names
    /// none; `--in` semantics via `source_in`).
    pub fn send(&self, draft: Draft) -> Result<Admitted> {
        let sent = self.with(|k, _| k.send(draft))?;
        self.told(&Told {
            warnings: sent.warnings.clone(),
            notices: sent.notices.clone(),
        });
        Ok(sent)
    }

    /// The source a draft goes to from inside an inbox (a persona's).
    pub fn source_in(&self, inbox: &str) -> Result<Option<String>> {
        self.with(|k, config| Ok(k.source_for(&chain(config, inbox)?)))
    }

    /// Pull every source, classify, forget stale, mirror if the config says
    /// so. Long: call it off the UI thread.
    pub fn pull(&self) -> Result<Report> {
        let mut report = self.with(|k, _| {
            let report = k.pull()?;
            k.forget_stale()?;
            Ok(report)
        })?;
        {
            let config = self.config.read().unwrap();
            if mirror_after_pull(&config) {
                if let Err(e) = push_mirror(&self.paths, &config, &self.store) {
                    report.warnings.push(format!("mirror: {e:#}"));
                }
            }
        }
        self.told(&Told {
            warnings: report.warnings.clone(),
            notices: report.notices.clone(),
        });
        Ok(report)
    }

    pub fn answer(&self, question: &str, inbox: &str) -> Result<Answer> {
        self.with(|k, config| k.answer(&chain(config, inbox)?, question))
    }

    pub fn embed_missing(&self) -> Result<Report> {
        let report = self.with(|k, _| k.embed_missing())?;
        if report.count > 0 {
            self.emit(&Event::Changed);
        }
        Ok(report)
    }

    /// What `candidate` (a config file) would change on this host's items,
    /// written nowhere. See `paddock replay`.
    pub fn replay(
        &self,
        candidate: &Path,
        inbox: Option<&str>,
        limit: Option<usize>,
    ) -> Result<paddock::Replay> {
        let candidate = resolve_secrets(load_config_in(candidate, &self.paths.config_dir)?)?;
        let config = self.config.read().unwrap();
        paddock::replay(&self.paths, &config, &self.store, &candidate, inbox, limit)
    }

    // ----- setup by an agent -----

    /// The agents this machine has, for a first-run picker.
    pub fn agents(&self) -> Vec<AgentSpec> {
        agents_on_path()
    }

    /// The briefing an agent gets: `paddock context`.
    pub fn briefing(&self) -> Result<String> {
        let config = self.config.read().unwrap();
        briefing(&self.paths, &config, &self.store)
    }

    /// Hand the host and a task in words to the agent. Every line it prints
    /// arrives as `Event::Setup`; when it is done the config is reloaded.
    /// Long: call it off the UI thread. Returns the agent's exit code.
    pub fn setup(&self, task: &str) -> Result<i32> {
        let brief = self.briefing()?;
        let status = {
            let config = self.config.read().unwrap();
            setup(&config, &self.paths, &brief, task, &mut |line| {
                self.emit(&Event::Setup(line.to_string()))
            })?
        };
        self.reload().context("the config the agent left")?;
        Ok(status)
    }

    // ----- change -----

    /// Be told what happens. Returns a handle for `unsubscribe`.
    pub fn subscribe(&self, f: Callback) -> u64 {
        let id = self.next_subscriber.fetch_add(1, Ordering::Relaxed);
        self.subscribers.lock().unwrap().push((id, f));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.subscribers.lock().unwrap().retain(|(i, _)| *i != id);
    }

    /// Did another process write to the store since the last look? Emits
    /// `Changed` when so. Cheap: one pragma.
    pub fn poll(&self) -> Result<bool> {
        let now = self.store.data_version()?;
        let before = self.seen_version.swap(now, Ordering::Relaxed);
        let changed = now != before;
        if changed {
            self.emit(&Event::Changed);
        }
        Ok(changed)
    }

    /// Poll on a thread every `every`, until the watcher is dropped.
    pub fn watch(self: &Arc<Self>, every: Duration) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let session = Arc::clone(self);
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let _ = session.poll();
                std::thread::sleep(every);
            }
        });
        Watcher {
            stop,
            handle: Some(handle),
        }
    }

    fn told(&self, told: &Told) {
        self.emit(&Event::Changed);
        for n in &told.notices {
            self.emit(&Event::Notice(n.clone()));
        }
        for w in &told.warnings {
            self.emit(&Event::Warning(w.clone()));
        }
    }

    fn emit(&self, event: &Event) {
        // Our own write moves nothing for us; note the version so a later
        // poll does not report it as someone else's.
        if let Ok(v) = self.store.data_version() {
            self.seen_version.store(v, Ordering::Relaxed);
        }
        for (_, f) in self.subscribers.lock().unwrap().iter() {
            f(event);
        }
    }
}

/// Stops polling when dropped.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn split_path(inbox: &str) -> Vec<String> {
    let path: Vec<String> = inbox
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    if path.is_empty() {
        vec!["all".into()]
    } else {
        path
    }
}

fn chain<'a>(config: &'a Config, inbox: &str) -> Result<Vec<&'a paddock::Inbox>> {
    let path = split_path(inbox);
    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    config
        .chain(&refs)
        .ok_or_else(|| anyhow::anyhow!("no inbox {}", path.join("/")))
}

/// A question over an inbox path, for callers that want the raw store.
pub fn question(config: &Config, inbox: &str) -> Result<Question> {
    Ok(Question::of(&chain(config, inbox)?))
}
