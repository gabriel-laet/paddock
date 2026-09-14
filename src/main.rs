//! Thin CLI over the kernel. Every command has a `--json` form so other
//! programs and agents can drive it.

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
    /// Run on a remote host over ssh (host from flag, PADDOCK_REMOTE, or config remote)
    #[arg(long, value_name = "HOST", num_args = 0..=1, require_equals = true, default_missing_value = "", global = true)]
    remote: Option<String>,
    /// Force this machine even if a remote is configured
    #[arg(long, global = true)]
    local: bool,
    /// Machine output
    #[arg(long, global = true)]
    json: bool,
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
    },
    /// One item in full
    Show { id: i64 },
    /// Every item in the same thread as ID
    Thread { id: i64 },
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
        body: Option<String>,
    },
    /// Dump this host for an agent (pipeable)
    Context,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env();
    if !cli.local && !matches!(cli.cmd, Cmd::Init { .. }) {
        let from_config = load_config(&paths.config_file).ok().and_then(|c| c.remote);
        let from_env = std::env::var("PADDOCK_REMOTE").ok();
        let host = [
            cli.remote.as_deref(),
            from_env.as_deref(),
            from_config.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|h| !h.is_empty());
        if let Some(host) = host {
            return run_remote(host);
        }
        if cli.remote.is_some() {
            bail!("no remote host (pass --remote=HOST, set PADDOCK_REMOTE, or config remote)");
        }
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
    let k = kernel(&config, &store);
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
        Cmd::Init { .. } => unreachable!(),
        Cmd::Pull => {
            let admitted = k.pull()?;
            let forgot = k.forget_stale()?;
            let warnings = k.take_warnings();
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({ "admitted": admitted, "forgot": forgot, "warnings": warnings })
                );
            } else {
                println!("admitted {admitted}, forgot {forgot}");
                for w in warnings {
                    eprintln!("warning: {w}");
                }
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
        } => {
            let path = split_path(inbox.as_deref());
            let mut q = Question::of(&chain(&config, &path)?);
            q.unread = unread;
            q.text = text;
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
        Cmd::Thread { id } => {
            let it = store.get(id)?;
            let items = match it.thread.as_deref() {
                Some(t) => store.thread(t)?,
                None => vec![it],
            };
            list(&items, cli.json)?;
        }
        Cmd::Label { id, labels } => {
            let (add, remove): (Vec<String>, Vec<String>) =
                labels.into_iter().partition(|l| !l.starts_with('-'));
            let add: Vec<String> = add
                .iter()
                .map(|l| l.trim_start_matches('+').to_string())
                .collect();
            let remove: Vec<String> = remove.iter().map(|l| l[1..].to_string()).collect();
            k.label(id, &add, &remove)?;
            emit(id)?;
        }
        Cmd::Read { id } => {
            store.set_read(id, true)?;
            emit(id)?;
        }
        Cmd::Unread { id } => {
            store.set_read(id, false)?;
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
            k.classify(id)?;
            emit(id)?;
        }
        Cmd::Why { id, inbox } => {
            let it = store.get(id)?;
            let path = split_path(inbox.as_deref());
            println!("{}  {}", path.join("/"), k.why(&it, &path));
        }
        Cmd::Send {
            title,
            source,
            reply,
            to,
            body,
        } => {
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
            let id = k.send(draft)?;
            emit(id)?;
        }
        Cmd::Context => context(&paths, &k)?,
    }
    Ok(())
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
    q.unread = true;
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
    let mark = if it.read { " " } else { "*" };
    let from = it
        .from
        .as_ref()
        .map(|a| a.name.clone().unwrap_or_else(|| a.id.clone()))
        .unwrap_or_default();
    let labels = if it.labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", it.labels.join(" "))
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
    if let Some(p) = it.in_reply_to {
        writeln!(w, "reply-to #{p}")?;
    }
    if let Some(p) = it.forward_of {
        writeln!(w, "forward  #{p}")?;
    }
    if let Some(ex) = &it.cite_excerpt {
        let who = it
            .cite_actor
            .as_ref()
            .map(|a| format!("{}: ", actor(a)))
            .unwrap_or_default();
        writeln!(w, "cites    {who}{ex}")?;
    }
    writeln!(w, "read     {}", it.read)?;
    writeln!(w, "labels   {}", it.labels.join(" "))?;
    writeln!(w)?;
    for p in &it.parts {
        match (&p.text, &p.path) {
            (Some(t), _) => writeln!(w, "{t}")?,
            (None, Some(path)) => writeln!(w, "[{} {} {path}]", p.kind.as_str(), p.mime)?,
            _ => {}
        }
    }
    Ok(())
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
    writeln!(
        w,
        "A cite arrives as a foreign id and resolves on admit (late parent still stitches)."
    )?;
    writeln!(w, "A source admits items and may send. kinds: fs, rss, exec. rss cannot send. exec runs `{{cmd}} {{args}} pull|send`.")?;
    writeln!(w, "Inboxes nest. A child is a tighter question over the parent. Match: sources AND labels (all) AND timed (start set) AND age.")?;
    writeln!(w, "Classifiers are per-inbox, ordered, kinds regex | script (CEL) | llm. They stamp labels. They are not sources.")?;
    writeln!(w, "Actor kind is person | group | list.")?;
    writeln!(w, "Admit upserts on (source_id, foreign_id). Re-admit refreshes the item and keeps read + labels.\n")?;
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
        let extra = src
            .path
            .as_deref()
            .or(src.url.as_deref())
            .or(src.cmd.as_deref())
            .unwrap_or("");
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
    writeln!(w, "paddock pull | inboxes | ls [INBOX] [--unread] [--text WORDS] | show ID | thread ID | label ID [+l|-l]... | read ID | unread ID | forget ID | classify ID | why ID [INBOX] | send [--title T] [--reply ID] [--to A]... [BODY]")?;
    writeln!(w, "Add --json to any command for machine output. Edit config.toml, then `paddock pull`. Do not invent nouns.")?;
    Ok(())
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
