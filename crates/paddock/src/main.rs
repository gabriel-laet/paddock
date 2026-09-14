//! Thin CLI over the kernel. Every command has a `--json` form so other
//! programs and agents can drive it.

use anyhow::Context as _;
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use paddock::{
    init, kernel, load, load_config, Actor, Config, Draft, Item, Kernel, Paths, Question, Store,
};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(name = "paddock", about = "An inbox kernel", version)]
struct Cli {
    /// Run on a remote host over ssh (host from the flag, else `remote` in config)
    #[arg(long, value_name = "HOST", num_args = 0..=1, require_equals = true, default_missing_value = "", global = true)]
    remote: Option<String>,
    /// Force this machine even if a remote is configured
    #[arg(long, global = true)]
    local: bool,
    /// Machine output
    #[arg(long, global = true)]
    json: bool,
    /// The host directory (config, store, incoming); else PADDOCK_DIR, a `.paddock/` above, or XDG
    #[arg(long, global = true, value_name = "DIR")]
    dir: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create config, data dir, and incoming directory
    Init {
        /// Create ./.paddock in the current directory (instead of XDG)
        #[arg(long)]
        here: bool,
    },
    /// Pull every source, classify new items, forget stale ones
    Pull,
    /// The inbox tree with unread/total counts
    Inboxes,
    /// Items in an inbox (path like all/todo; default all)
    Ls {
        inbox: Option<String>,
        #[arg(long)]
        unread: bool,
        /// Words that must all appear in the title or text (prefix match)
        #[arg(long)]
        text: Option<String>,
        /// Closest in meaning to this, through the config's embedder
        #[arg(long)]
        like: Option<String>,
        /// At most this many
        #[arg(long)]
        limit: Option<usize>,
    },
    /// One item in full
    Show { id: i64 },
    /// Every item in the same thread as ID: the source's thread, else what replies join
    Thread { id: i64 },
    /// Items that cite ID
    Cited { id: i64 },
    /// The bytes of a part, to stdout
    Part { id: i64 },
    /// Add (+l or l) and remove (-l) labels, then reclassify
    Label {
        id: i64,
        #[arg(allow_hyphen_values = true)]
        labels: Vec<String>,
    },
    /// Mark read
    Read { id: i64 },
    /// Mark unread
    Unread { id: i64 },
    /// Delete an item
    Forget { id: i64 },
    /// Re-run classifiers on an item
    Classify { id: i64 },
    /// Why an item sits in an inbox (labels matched, classifiers that fired)
    Why { id: i64, inbox: Option<String> },
    /// Compose an item; body from BODY or stdin. --reply keeps the thread.
    Send {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        reply: Option<i64>,
        #[arg(long)]
        to: Vec<String>,
        /// Send from inside an inbox: its first source (a persona's, say)
        #[arg(long = "in")]
        inbox: Option<String>,
        body: Option<String>,
    },
    /// Embed every item that has no vector yet
    Embed,
    /// Ask the model a question over an inbox; it answers from the items and cites them
    Answer {
        question: String,
        /// Inbox path to answer from (default all)
        #[arg(long = "in")]
        inbox: Option<String>,
    },
    /// Run a plugin's `pull` (and `send`, with --send) and validate what it says
    Check {
        /// The program: ./target/debug/paddock-rss, or a `paddock-x` on PATH
        cmd: String,
        /// Settings to hand it, as key=value
        #[arg(long = "set", value_name = "KEY=VALUE")]
        settings: Vec<String>,
        /// Also try `send` with a sample draft
        #[arg(long)]
        send: bool,
    },
    /// Dump this host for an agent (pipeable)
    Context,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = match &cli.dir {
        Some(dir) => Paths::from_root(paddock::expand_path(dir)),
        None => Paths::from_env(),
    };
    if !cli.local && !matches!(cli.cmd, Cmd::Init { .. }) {
        let from_config = load_config(&paths.config_file).ok().and_then(|c| c.remote);
        let host = [cli.remote.as_deref(), from_config.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|h| !h.is_empty());
        if let Some(host) = host {
            return run_remote(host);
        }
        if cli.remote.is_some() {
            bail!("no remote host (pass --remote=HOST or set `remote` in config)");
        }
    }
    if let Cmd::Check {
        cmd,
        settings,
        send,
    } = &cli.cmd
    {
        return check(cmd, settings, *send, cli.json);
    }
    if let Cmd::Init { here } = cli.cmd {
        let paths = if here {
            Paths::here(&std::env::current_dir()?)
        } else {
            paths
        };
        init(&paths)?;
        println!("config   {}", paths.config_file.display());
        println!("data     {}", paths.data_dir.display());
        println!("incoming {}", paths.incoming_dir.display());
        return Ok(());
    }
    let (config, store) = load(&paths)?;
    let k = kernel(&config, &store)?;
    let out = std::io::stdout();
    let mut out = out.lock();
    let mut emit = |id: i64| -> Result<()> {
        let it = store.get(id)?;
        if cli.json {
            writeln!(out, "{}", serde_json::to_string(&it)?)?;
        } else {
            writeln!(out, "{}", line(&it))?;
        }
        Ok(())
    };
    match cli.cmd {
        Cmd::Init { .. } | Cmd::Check { .. } => unreachable!(),
        Cmd::Pull => {
            let pulled = k.pull()?;
            let forgot = k.forget_stale()?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "admitted": pulled.count, "forgot": forgot, "warnings": pulled.warnings })
                );
            } else {
                println!("admitted {}, forgot {forgot}", pulled.count);
                warn(&pulled.warnings);
            }
        }
        Cmd::Inboxes => {
            let rows: Vec<serde_json::Value> = config
                .nodes()
                .iter()
                .map(|node| {
                    let (unread, total) = counts(&config, &store, &node.path);
                    serde_json::json!({ "path": node.path.join("/"), "unread": unread, "total": total })
                })
                .collect();
            if cli.json {
                println!("{}", serde_json::Value::Array(rows));
            } else {
                for r in rows {
                    println!(
                        "{:<24} {}/{}",
                        r["path"].as_str().unwrap_or(""),
                        r["unread"],
                        r["total"]
                    );
                }
            }
        }
        Cmd::Ls {
            inbox,
            unread,
            text,
            like,
            limit,
        } => {
            let path = split_path(inbox.as_deref());
            let mut q = k.question(&chain(&config, &path)?);
            if unread {
                q.without.push(paddock::READ.into());
            }
            q.text = text;
            q.limit = limit;
            if let Some(like) = like {
                q.near = Some(k.near(&like)?);
            }
            list(&store.ask(&q)?, cli.json)?;
        }
        Cmd::Show { id } => {
            let it = store.get(id)?;
            if cli.json {
                println!("{}", serde_json::to_string(&it)?);
            } else {
                show(&it)?;
            }
        }
        Cmd::Thread { id } => list(&k.thread(id)?, cli.json)?,
        Cmd::Cited { id } => list(&store.citing(id)?, cli.json)?,
        Cmd::Part { id } => {
            out.write_all(&store.blob(id)?)?;
        }
        Cmd::Label { id, labels } => {
            let (add, remove): (Vec<String>, Vec<String>) =
                labels.into_iter().partition(|l| !l.starts_with('-'));
            let add: Vec<String> = add
                .iter()
                .map(|l| l.trim_start_matches('+').to_string())
                .collect();
            let remove: Vec<String> = remove.iter().map(|l| l[1..].to_string()).collect();
            warn(&k.label(id, &add, &remove)?);
            emit(id)?;
        }
        Cmd::Read { id } => {
            warn(&k.read(id, true)?);
            emit(id)?;
        }
        Cmd::Unread { id } => {
            warn(&k.read(id, false)?);
            emit(id)?;
        }
        Cmd::Forget { id } => {
            let gone = k.forget(id)?;
            if cli.json {
                println!("{}", serde_json::json!({ "id": id, "forgot": gone }));
            } else {
                println!("{}", if gone { "forgot" } else { "no item" });
            }
        }
        Cmd::Classify { id } => {
            warn(&k.classify(id)?);
            emit(id)?;
        }
        Cmd::Why { id, inbox } => {
            let it = store.get(id)?;
            let path = split_path(inbox.as_deref());
            let why = k.why(&it, &path);
            if cli.json {
                println!("{}", serde_json::to_string(&why)?);
            } else {
                let show = |v: &[paddock::Label]| {
                    if v.is_empty() {
                        "-".to_string()
                    } else {
                        v.iter().map(label_with_by).collect::<Vec<_>>().join(" ")
                    }
                };
                println!(
                    "{}  labels: {}  denied: {}",
                    path.join("/"),
                    show(&why.matched),
                    show(&why.denied)
                );
            }
        }
        Cmd::Send {
            title,
            source,
            reply,
            to,
            inbox,
            body,
        } => {
            let source = match (source, inbox) {
                (Some(s), _) => Some(s),
                (None, Some(path)) => {
                    let path = split_path(Some(&path));
                    k.source_for(&chain(&config, &path)?)
                }
                (None, None) => None,
            };
            let body = match body {
                Some(b) => b,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let title = title.unwrap_or_else(|| {
                if reply.is_some() {
                    String::new()
                } else {
                    body.lines()
                        .next()
                        .unwrap_or("untitled")
                        .chars()
                        .take(80)
                        .collect()
                }
            });
            let draft = Draft {
                source_id: source.unwrap_or_default(),
                title,
                body,
                reply_to: reply,
                to: to
                    .into_iter()
                    .map(|id| Actor {
                        id,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            let sent = k.send(draft)?;
            warn(&sent.warnings);
            emit(sent.id)?;
        }
        Cmd::Embed => {
            let done = k.embed_missing()?;
            if cli.json {
                println!("{}", serde_json::to_string(&done)?);
            } else {
                println!("embedded {}", done.count);
                warn(&done.warnings);
            }
        }
        Cmd::Answer { question, inbox } => {
            let path = split_path(inbox.as_deref());
            let answer = k.answer(&chain(&config, &path)?, &question)?;
            if cli.json {
                println!("{}", serde_json::to_string(&answer)?);
            } else {
                println!("{}", answer.text.trim());
                if !answer.cites.is_empty() {
                    println!();
                    for id in &answer.cites {
                        println!("{}", line(&store.get(*id)?));
                    }
                }
            }
        }
        Cmd::Context => context(&paths, &k)?,
    }
    Ok(())
}

fn warn(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

fn split_path(s: Option<&str>) -> Vec<String> {
    s.unwrap_or("all")
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

fn chain<'a>(config: &'a Config, path: &[String]) -> Result<Vec<&'a paddock::Inbox>> {
    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    config
        .chain(&refs)
        .ok_or_else(|| anyhow::anyhow!("no inbox {}", path.join("/")))
}

fn counts(config: &Config, store: &dyn Store, path: &[String]) -> (usize, usize) {
    let Ok(chain) = chain(config, path) else {
        return (0, 0);
    };
    let mut q = Question::of(&chain);
    let total = store.count(&q).unwrap_or(0);
    q.without.push(paddock::READ.into());
    let unread = store.count(&q).unwrap_or(0);
    (unread, total)
}

fn list(items: &[Item], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(items)?);
    } else {
        for it in items {
            println!("{}", line(it));
        }
    }
    Ok(())
}

/// One line per item: id, unread mark, date, source, from, title, labels.
fn line(it: &Item) -> String {
    let mark = if it.read() { " " } else { "*" };
    let from = it
        .from
        .as_ref()
        .map(|a| a.name.clone().unwrap_or_else(|| a.id.clone()))
        .unwrap_or_default();
    let labels = if it.labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", it.label_names().join(" "))
    };
    let title = it.title.lines().next().unwrap_or("");
    format!(
        "{:>5} {mark} {:.10}  {:<10} {:<20} {title}{labels}",
        it.id,
        it.when(),
        it.source_id,
        from
    )
}

fn show(it: &Item) -> Result<()> {
    let out = std::io::stdout();
    let mut w = out.lock();
    writeln!(w, "id       {}", it.id)?;
    writeln!(w, "source   {}", it.source_id)?;
    writeln!(w, "foreign  {}", it.foreign_id)?;
    writeln!(w, "title    {}", it.title)?;
    if let Some(a) = &it.from {
        writeln!(w, "from     {}", actor(a))?;
    }
    if !it.to.is_empty() {
        writeln!(
            w,
            "to       {}",
            it.to.iter().map(actor).collect::<Vec<_>>().join(", ")
        )?;
    }
    writeln!(w, "created  {}", it.created_at)?;
    for (k, v) in [
        ("start", &it.start),
        ("end", &it.end),
        ("thread", &it.thread),
        ("href", &it.href),
    ] {
        if let Some(v) = v {
            writeln!(w, "{k:<8} {v}")?;
        }
    }
    for c in &it.cites {
        let target = match (c.id, &c.foreign_id, &c.href) {
            (Some(id), _, _) => format!("#{id}"),
            (None, Some(f), _) => format!(
                "{}/{f} (not here)",
                c.source_id.as_deref().unwrap_or(&it.source_id)
            ),
            (None, None, Some(h)) => h.clone(),
            _ => "?".into(),
        };
        let who = c
            .actor
            .as_ref()
            .map(|a| format!(" {}:", actor(a)))
            .unwrap_or_default();
        let excerpt = c
            .excerpt
            .as_deref()
            .map(|e| format!(" \"{e}\""))
            .unwrap_or_default();
        writeln!(w, "{:<8} {target}{who}{excerpt}", c.kind.as_str())?;
    }
    let labels: Vec<String> = it.labels.iter().map(label_with_by).collect();
    writeln!(w, "labels   {}", labels.join("  "))?;
    if !it.denied.is_empty() {
        let denied: Vec<String> = it.denied.iter().map(|l| l.name.clone()).collect();
        writeln!(w, "denied   {}", denied.join("  "))?;
    }
    writeln!(w)?;
    for p in &it.parts {
        match (&p.text, p.size) {
            (Some(t), _) => writeln!(w, "{t}")?,
            (None, Some(size)) => writeln!(
                w,
                "[{} {} part {} {size} bytes]",
                p.kind.as_str(),
                p.mime,
                p.id
            )?,
            _ => {}
        }
    }
    Ok(())
}

/// `todo(flag-todo)` for a classifier's label, `todo(hand)` for yours.
fn label_with_by(l: &paddock::Label) -> String {
    match &l.by {
        paddock::By::Hand => format!("{}(hand)", l.name),
        paddock::By::Source => format!("{}(source)", l.name),
        paddock::By::Classifier(id) => format!("{}({id})", l.name),
        paddock::By::Inbox(path) => format!("{}({path})", l.name),
    }
}

fn actor(a: &Actor) -> String {
    match &a.name {
        Some(n) => format!("{n} <{}>", a.id),
        None => a.id.clone(),
    }
}

/// Agent-ready dump of this host. No secrets. Safe to pipe.
fn context(paths: &Paths, k: &Kernel) -> Result<()> {
    let config = k.config;
    let store = k.store;
    let out = std::io::stdout();
    let mut w = out.lock();
    writeln!(w, "# paddock\n")?;
    writeln!(
        w,
        "Inbox kernel. Four nouns: item, source, label, inbox. No other product nouns."
    )?;
    writeln!(w, "An item is source-shaped data stripped: foreign_id, title, body, href, start, end, thread, parts, from, to[], cites.")?;
    writeln!(w, "A cite is {{kind: reply|forward|quote|mention|attach, foreign_id or href, excerpt?, actor?}}; it resolves to an id when the cited item is here, early or late.")?;
    writeln!(w, "A source admits items and may send. kinds: fs, rss, exec. rss cannot send. exec runs `{{cmd}} {{args}} pull|send`.")?;
    writeln!(w, "Inboxes nest. A child is a tighter question over the parent. Match: sources AND labels (all) AND timed (start set) AND age.")?;
    writeln!(w, "Classifiers are per-inbox, ordered, kinds regex | script (CEL) | llm. They stamp labels. They are not sources.")?;
    writeln!(w, "Actor kind is person | group | list.")?;
    writeln!(w, "Admit upserts on (source_id, foreign_id). Re-admit refreshes the item and keeps read + labels.")?;
    writeln!(w, "A label remembers who put it there (hand, source, classifier, or an inbox effect). A label a hand removed is denied: nothing but a hand puts it back. Read is the label `read`.")?;
    writeln!(w, "An inbox may say `without = [...]` (item carries none) and `then = [...]` (effects on enter: label:NAME, read, send:SOURCE).\n")?;
    writeln!(w, "## this host")?;
    writeln!(w, "config   {}", paths.config_file.display())?;
    writeln!(w, "db       {}", paths.db_path.display())?;
    writeln!(w, "data     {}", paths.data_dir.display())?;
    writeln!(w, "incoming {}\n", paths.incoming_dir.display())?;
    writeln!(w, "## sources")?;
    let by_src: std::collections::BTreeMap<String, i64> =
        store.counts_by_source()?.into_iter().collect();
    for src in &config.source {
        let n = by_src.get(&src.id).copied().unwrap_or(0);
        let extra = ["path", "url", "cmd"]
            .into_iter()
            .find_map(|k| paddock::setting(&src.settings, k))
            .unwrap_or_default();
        writeln!(w, "{}  kind={}  items={n}  {extra}", src.id, src.kind)?;
    }
    for (id, n) in &by_src {
        if config.source(id).is_none() {
            writeln!(w, "{id}  items={n}  (not in config)")?;
        }
    }
    let total = store.count(&Question::default())?;
    let timed = store.count(&Question {
        timed: true,
        ..Default::default()
    })?;
    writeln!(w, "total {total}  timed {timed}\n")?;
    writeln!(w, "## inboxes")?;
    for node in config.nodes() {
        let (unread, total) = counts(config, store, &node.path);
        let ib = &node.inbox;
        write!(w, "{}  unread={unread}  items={total}", node.path.join("/"))?;
        if ib.timed {
            write!(w, "  timed")?;
        }
        for (k, v) in [("labels", &ib.labels), ("sources", &ib.sources)] {
            if !v.is_empty() {
                write!(w, "  {k}={}", v.join(","))?;
            }
        }
        if !ib.classifier.is_empty() {
            let ids: Vec<&str> = ib.classifier.iter().map(|c| c.id.as_str()).collect();
            write!(w, "  classifiers={}", ids.join(","))?;
        }
        writeln!(w)?;
    }
    writeln!(w, "\n## use")?;
    writeln!(w, "paddock pull | inboxes | ls [INBOX] [--unread] [--text WORDS] [--like TEXT] | answer QUESTION [--in INBOX] | embed | show ID | thread ID | cited ID | part ID | label ID [+l|-l]... | read ID | unread ID | forget ID | classify ID | why ID [INBOX] | send [--title T] [--reply ID] [--to A]... [--in INBOX] [BODY]")?;
    writeln!(w, "Add --json to any command for machine output. Edit config.toml, then `paddock pull`. Do not invent nouns.")?;
    Ok(())
}

/// Speak the protocol to a plugin and say what came back. This is the only
/// thing in this repository that runs a plugin; the kernel's tests never do.
fn check(cmd: &str, settings: &[String], try_send: bool, json: bool) -> Result<()> {
    use paddock::adapters::source::exec::parse_wire_items;
    let mut map = serde_json::Map::new();
    for kv in settings {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--set wants KEY=VALUE, got `{kv}`"))?;
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    let request = paddock::protocol::Request {
        id: "check".into(),
        settings: map,
        draft: None,
    };
    let run = |verb: &str, req: &paddock::protocol::Request| -> Result<(i32, String, String)> {
        let mut child = Command::new(cmd)
            .arg(verb)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot run `{cmd}`"))?;
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(&serde_json::to_vec(req)?);
        }
        let out = child.wait_with_output()?;
        Ok((
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    };
    let mut problems: Vec<String> = Vec::new();
    let (code, stdout, stderr) = run("pull", &request)?;
    let mut count = 0;
    if code != 0 {
        problems.push(format!("pull exited {code}: {}", stderr.trim()));
    } else {
        match parse_wire_items(&stdout) {
            Ok(items) => {
                count = items.len();
                for (i, it) in items.iter().enumerate() {
                    if it.foreign_id.trim().is_empty() {
                        problems.push(format!("item {i}: empty foreign_id"));
                    }
                    for c in &it.cites {
                        if !["reply", "forward", "quote", "mention", "attach"]
                            .contains(&c.kind.as_str())
                        {
                            problems.push(format!("item {i}: cite kind `{}`", c.kind));
                        }
                        if c.foreign_id.is_none() && c.href.is_none() && c.actor.is_none() {
                            problems.push(format!(
                                "item {i}: a cite with no foreign_id, href, or actor"
                            ));
                        }
                    }
                    for p in &it.parts {
                        if !["text", "file", "image", "audio", "video"].contains(&p.kind.as_str()) {
                            problems.push(format!("item {i}: part kind `{}`", p.kind));
                        }
                    }
                    for a in it.from.iter().chain(it.to.iter()) {
                        if a.id.trim().is_empty() {
                            problems.push(format!("item {i}: an actor with no id"));
                        }
                    }
                    for t in [&it.start, &it.end].into_iter().flatten() {
                        if paddock::parse_when(t).is_none() {
                            problems.push(format!("item {i}: time `{t}` is not RFC3339 or a date"));
                        }
                    }
                }
            }
            Err(e) => problems.push(format!("pull output: {e:#}")),
        }
    }
    let mut sent = None;
    if try_send {
        let mut req = request.clone();
        req.draft = Some(paddock::protocol::Draft {
            title: "paddock check".into(),
            body: "a draft from paddock check".into(),
            ..Default::default()
        });
        let (code, stdout, stderr) = run("send", &req)?;
        if code == 2 || stderr.contains("source cannot send") {
            sent = Some("refused (cannot send)".to_string());
        } else if code != 0 {
            problems.push(format!("send exited {code}: {}", stderr.trim()));
        } else {
            match serde_json::from_str::<paddock::protocol::Sent>(stdout.trim()) {
                Ok(s) if !s.foreign_id.trim().is_empty() => {
                    sent = Some(format!("ok, foreign_id {}", s.foreign_id))
                }
                Ok(_) => problems.push("send: empty foreign_id".into()),
                Err(e) => problems.push(format!("send output: {e}")),
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({ "cmd": cmd, "items": count, "send": sent, "problems": problems })
        );
    } else {
        println!("{cmd}: pull gave {count} item(s)");
        if let Some(s) = &sent {
            println!("send: {s}");
        }
        for p in &problems {
            println!("problem: {p}");
        }
        if problems.is_empty() {
            println!("ok");
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

/// Re-run this exact command line on `host` over ssh, forcing `--local` there.
fn run_remote(host: &str) -> Result<()> {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--local" && !a.starts_with("--remote"))
        .collect();
    let mut remote = String::from("PATH=\"$HOME/.local/bin:$PATH\" paddock --local");
    for a in &args {
        remote.push(' ');
        remote.push_str(&shell_quote(a));
    }
    let status = Command::new("ssh")
        .arg(host)
        .arg(remote)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}
