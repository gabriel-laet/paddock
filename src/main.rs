//! Thin CLI over the paddock kernel. Every command has a `--json` form so
//! other programs (and agents) can drive it.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use paddock::{
    classify_item, filter_for_chain, forget, forget_stale, init, label, load_or_init, pull_all,
    send_draft, why, Actor, Config, Draft, Item, Paths, Store,
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
        let cfg_remote = Config::load(&paths.config_file).ok().and_then(|c| c.remote);
        let env = std::env::var("PADDOCK_REMOTE").ok();
        if let Some(host) =
            resolve_remote(cli.remote.as_deref(), env.as_deref(), cfg_remote.as_deref())
        {
            return run_remote(&host);
        }
        if cli.remote.is_some() {
            bail!("no remote host (pass --remote=HOST, set PADDOCK_REMOTE, or config remote)");
        }
    }
    let json = cli.json;
    let out = std::io::stdout();
    let mut out = out.lock();
    match cli.cmd {
        Cmd::Init { here } => {
            let paths = if here {
                let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                Paths::here(&cwd)
            } else {
                paths
            };
            init(&paths)?;
            writeln!(out, "config   {}", paths.config_file.display())?;
            writeln!(out, "data     {}", paths.data_dir.display())?;
            writeln!(out, "incoming {}", paths.incoming_dir.display())?;
        }
        Cmd::Pull => {
            let (config, store) = load_or_init(&paths)?;
            let n = pull_all(&store, &config)?;
            let f = forget_stale(&store, &config)?;
            if json {
                writeln!(out, "{}", serde_json::json!({ "admitted": n, "forgot": f }))?;
            } else {
                writeln!(out, "admitted {n}, forgot {f}")?;
            }
        }
        Cmd::Inboxes => {
            let (config, store) = load_or_init(&paths)?;
            let rows: Vec<serde_json::Value> = config
                .flatten()
                .into_iter()
                .map(|node| {
                    let (unread, total) = counts(&config, &store, &node.path);
                    serde_json::json!({ "path": node.path.join("/"), "unread": unread, "total": total })
                })
                .collect();
            if json {
                writeln!(out, "{}", serde_json::Value::Array(rows))?;
            } else {
                for r in rows {
                    writeln!(
                        out,
                        "{:<24} {}/{}",
                        r["path"].as_str().unwrap_or(""),
                        r["unread"],
                        r["total"]
                    )?;
                }
            }
        }
        Cmd::Ls { inbox, unread } => {
            let (config, store) = load_or_init(&paths)?;
            let path = split_path(inbox.as_deref());
            let chain = chain_or_bail(&config, &path)?;
            let mut filter = filter_for_chain(&chain);
            filter.unread_only = unread;
            let items = store.list_filtered(&filter)?;
            if json {
                writeln!(out, "{}", serde_json::to_string(&items)?)?;
            } else {
                for it in &items {
                    writeln!(out, "{}", line(it))?;
                }
            }
        }
        Cmd::Show { id } => {
            let (_, store) = load_or_init(&paths)?;
            let it = store.get(id)?;
            if json {
                writeln!(out, "{}", serde_json::to_string(&it)?)?;
            } else {
                show(&mut out, &it)?;
            }
        }
        Cmd::Thread { id } => {
            let (_, store) = load_or_init(&paths)?;
            let it = store.get(id)?;
            let items = match it.thread.as_deref() {
                Some(t) => store.items_in_thread(t)?,
                None => vec![it],
            };
            if json {
                writeln!(out, "{}", serde_json::to_string(&items)?)?;
            } else {
                for it in &items {
                    writeln!(out, "{}", line(it))?;
                }
            }
        }
        Cmd::Label { id, labels } => {
            let (config, store) = load_or_init(&paths)?;
            let (add, remove): (Vec<String>, Vec<String>) =
                labels.into_iter().partition(|l| !l.starts_with('-'));
            let add: Vec<String> = add
                .iter()
                .map(|l| l.trim_start_matches('+').to_string())
                .collect();
            let remove: Vec<String> = remove.iter().map(|l| l[1..].to_string()).collect();
            label(&store, &config, id, &add, &remove)?;
            emit_item(&mut out, &store, id, json)?;
        }
        Cmd::Read { id } => {
            let (_, store) = load_or_init(&paths)?;
            store.set_read(id, true)?;
            emit_item(&mut out, &store, id, json)?;
        }
        Cmd::Unread { id } => {
            let (_, store) = load_or_init(&paths)?;
            store.set_read(id, false)?;
            emit_item(&mut out, &store, id, json)?;
        }
        Cmd::Forget { id } => {
            let (_, store) = load_or_init(&paths)?;
            let gone = forget(&store, id)?;
            if json {
                writeln!(out, "{}", serde_json::json!({ "id": id, "forgot": gone }))?;
            } else {
                writeln!(out, "{}", if gone { "forgot" } else { "no item" })?;
            }
        }
        Cmd::Classify { id } => {
            let (config, store) = load_or_init(&paths)?;
            classify_item(&store, &config, id)?;
            emit_item(&mut out, &store, id, json)?;
        }
        Cmd::Why { id, inbox } => {
            let (config, store) = load_or_init(&paths)?;
            let it = store.get(id)?;
            let path = split_path(inbox.as_deref());
            writeln!(out, "{}  {}", path.join("/"), why(&config, &it, &path))?;
        }
        Cmd::Send {
            title,
            source,
            reply,
            to,
            body,
        } => {
            let (config, store) = load_or_init(&paths)?;
            let body = match body {
                Some(b) => b,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let mut title = title.unwrap_or_default();
            let mut source_id = source.unwrap_or_default();
            if let Some(pid) = reply {
                let parent = store.get(pid)?;
                if source_id.is_empty() {
                    source_id = parent.source_id.clone();
                }
                if title.trim().is_empty() {
                    title = paddock::reply_title(&parent);
                }
            }
            if title.trim().is_empty() {
                title = body
                    .lines()
                    .next()
                    .unwrap_or("untitled")
                    .chars()
                    .take(80)
                    .collect();
            }
            let draft = Draft {
                source_id,
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
            let id = send_draft(&store, &config, &paths, draft)?;
            emit_item(&mut out, &store, id, json)?;
        }
        Cmd::Context => {
            let (config, store) = load_or_init(&paths)?;
            write_context(&paths, &config, &store, &mut out)?;
        }
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

fn chain_or_bail<'a>(config: &'a Config, path: &[String]) -> Result<Vec<&'a paddock::InboxConfig>> {
    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    config
        .find_chain(&refs)
        .ok_or_else(|| anyhow::anyhow!("no inbox {}", path.join("/")))
}

fn counts(config: &Config, store: &Store, path: &[String]) -> (usize, usize) {
    let Ok(chain) = chain_or_bail(config, path) else {
        return (0, 0);
    };
    let mut filter = filter_for_chain(&chain);
    let total = store.count_filtered(&filter).unwrap_or(0);
    filter.unread_only = true;
    let unread = store.count_filtered(&filter).unwrap_or(0);
    (unread, total)
}

fn emit_item(out: &mut impl Write, store: &Store, id: i64, json: bool) -> Result<()> {
    let it = store.get(id)?;
    if json {
        writeln!(out, "{}", serde_json::to_string(&it)?)?;
    } else {
        writeln!(out, "{}", line(&it))?;
    }
    Ok(())
}

/// One line per item: id, unread mark, date, source, from, title, labels.
fn line(it: &Item) -> String {
    let mark = if it.read { " " } else { "*" };
    let when = it.start.as_deref().unwrap_or(&it.created_at);
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
    let first_line = it.title.lines().next().unwrap_or("");
    format!(
        "{:>5} {mark} {:.10}  {:<10} {:<20} {first_line}{labels}",
        it.id, when, it.source_id, from
    )
}

fn show(out: &mut impl Write, it: &Item) -> Result<()> {
    writeln!(out, "id       {}", it.id)?;
    writeln!(out, "source   {}", it.source_id)?;
    writeln!(out, "foreign  {}", it.foreign_id)?;
    writeln!(out, "title    {}", it.title)?;
    if let Some(a) = &it.from {
        writeln!(out, "from     {}", actor(a))?;
    }
    if !it.to.is_empty() {
        writeln!(
            out,
            "to       {}",
            it.to.iter().map(actor).collect::<Vec<_>>().join(", ")
        )?;
    }
    writeln!(out, "created  {}", it.created_at)?;
    for (k, v) in [
        ("start", &it.start),
        ("end", &it.end),
        ("thread", &it.thread),
        ("href", &it.href),
    ] {
        if let Some(v) = v {
            writeln!(out, "{k:<8} {v}")?;
        }
    }
    if let Some(p) = it.in_reply_to {
        writeln!(out, "reply-to #{p}")?;
    }
    if let Some(p) = it.forward_of {
        writeln!(out, "forward  #{p}")?;
    }
    if let Some(ex) = &it.cite_excerpt {
        writeln!(
            out,
            "cites    {}{}",
            it.cite_actor
                .as_ref()
                .map(|a| format!("{}: ", actor(a)))
                .unwrap_or_default(),
            ex
        )?;
    }
    writeln!(out, "read     {}", it.read)?;
    writeln!(out, "labels   {}", it.labels.join(" "))?;
    writeln!(out)?;
    for p in &it.parts {
        match (&p.text, &p.path) {
            (Some(t), _) => writeln!(out, "{t}")?,
            (None, Some(path)) => writeln!(out, "[{} {} {path}]", p.kind.as_str(), p.mime)?,
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
fn write_context(paths: &Paths, config: &Config, store: &Store, w: &mut impl Write) -> Result<()> {
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
    writeln!(w, "Classifiers are per-inbox, ordered, kinds regex | script | llm. They stamp labels. They are not sources.")?;
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
        if !config.source.iter().any(|s| s.id == *id) {
            writeln!(w, "{id}  items={n}  (not in config)")?;
        }
    }
    writeln!(
        w,
        "total {}  timed {}\n",
        store.count_all()?,
        store.count_timed()?
    )?;
    writeln!(w, "## inboxes")?;
    for node in config.flatten() {
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
    writeln!(w, "paddock pull | inboxes | ls [INBOX] [--unread] | show ID | thread ID | label ID [+l|-l]... | read ID | unread ID | forget ID | classify ID | why ID [INBOX] | send [--title T] [--reply ID] [--to A]... [BODY]")?;
    writeln!(w, "Add --json to any command for machine output. Edit config.toml, then `paddock pull`. Do not invent nouns.")?;
    Ok(())
}

fn resolve_remote(flag: Option<&str>, env: Option<&str>, config: Option<&str>) -> Option<String> {
    let nonempty = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    nonempty(flag)
        .or_else(|| nonempty(env))
        .or_else(|| nonempty(config))
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
