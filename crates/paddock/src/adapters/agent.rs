//! Setting a host up is a task for an agent, not a settings screen. The
//! host briefs it (the same text `paddock context` prints), the agent edits
//! the config, runs `paddock check` and `paddock pull`, and the host
//! reloads. The kernel knows nothing of this; `[agent]` is host business
//! like `remote`.
//!
//! Also here: the host's side of a notice. The kernel says when an inbox
//! raised one; `notify_cmd` says what happens.
//!
//! ```toml
//! [agent]
//! cmd = "claude"
//! args = ["-p", "--allowedTools", "Bash(paddock:*),Edit,Write,Read"]
//!
//! notify_cmd = "notify-send 'paddock {inbox}' {title}"   # {id} too; the notice is JSON on stdin
//! ```

use anyhow::{Context, Result};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::host::Paths;
use crate::kernel::{setting, setting_list, Config, Notice, Question, Store};

/// An agent CLI and how to call it so the prompt arrives on stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub cmd: String,
    pub args: Vec<String>,
}

/// The agents this host knows how to call, in order of preference, with
/// the arguments that make each read a prompt from stdin and act on the
/// host directory. Only the ones found on PATH.
pub fn agents_on_path() -> Vec<AgentSpec> {
    let known: [(&str, &[&str]); 4] = [
        (
            "claude",
            &["-p", "--allowedTools", "Bash(paddock:*),Edit,Write,Read"],
        ),
        ("codex", &["exec", "--full-auto", "-"]),
        ("opencode", &["run"]),
        ("gemini", &["-p", ""]),
    ];
    known
        .into_iter()
        .filter(|(cmd, _)| on_path(cmd).is_some())
        .map(|(cmd, args)| AgentSpec {
            cmd: cmd.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        })
        .collect()
}

/// Every `paddock-*` program on PATH: the plugins a config may name by kind.
pub fn plugins_on_path() -> Vec<String> {
    let mut out: Vec<String> = path_dirs()
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter_map(|n| n.strip_prefix("paddock-").map(str::to_string))
        .filter(|n| !n.is_empty() && !n.contains('.'))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn on_path(cmd: &str) -> Option<PathBuf> {
    path_dirs()
        .into_iter()
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}

/// The agent the config names, or the first one on PATH.
pub fn agent(config: &Config) -> Result<AgentSpec> {
    if let Some(spec) = &config.agent {
        let cmd = setting(&spec.settings, "cmd").context("[agent] needs cmd")?;
        return Ok(AgentSpec {
            cmd,
            args: setting_list(&spec.settings, "args"),
        });
    }
    agents_on_path().into_iter().next().ok_or_else(|| {
        anyhow::anyhow!("no [agent] in config and none of claude, codex, opencode, gemini on PATH")
    })
}

/// Run the agent on a task, from the config directory, with the briefing
/// and the task as its prompt on stdin. Every line it prints goes to
/// `out`. Returns its exit code.
pub fn setup(
    config: &Config,
    paths: &Paths,
    briefing: &str,
    task: &str,
    out: &mut dyn FnMut(&str),
) -> Result<i32> {
    let spec = agent(config)?;
    let prompt = format!(
        "{briefing}\n## task\n{task}\n\n\
         Do it now. Edit the config file named above (keep what is there), run `paddock check` \
         on any plugin you configure, then `paddock pull`, and say in a few lines what you changed. \
         Never write a secret into the config: use NAME_cmd with a command that prints it \
         (pass, security, op, gopass). Do not invent nouns or settings; a plugin's own settings \
         are listed at the top of its source.\n"
    );
    let mut child = Command::new(&spec.cmd)
        .args(&spec.args)
        .current_dir(&paths.config_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run agent `{}`", spec.cmd))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let stderr = child.stderr.take();
    let errs = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut e) = stderr {
            let _ = std::io::Read::read_to_string(&mut e, &mut text);
        }
        text
    });
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines() {
            out(&line?);
        }
    }
    let status = child.wait()?;
    let errs = errs.join().unwrap_or_default();
    for line in errs.lines().filter(|l| !l.trim().is_empty()) {
        out(line);
    }
    Ok(status.code().unwrap_or(-1))
}

/// Run `notify_cmd` once per notice: `{id}`, `{inbox}`, and `{title}`
/// substituted (shell-quoted), the notice as JSON on stdin. Failures come
/// back as warnings; a host with no `notify_cmd` does nothing.
pub fn notify(config: &Config, notices: &[Notice]) -> Vec<String> {
    let Some(template) = config
        .notify_cmd
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    else {
        return Vec::new();
    };
    notices
        .iter()
        .filter_map(|n| {
            let cmd = template
                .replace("{id}", &n.id.to_string())
                .replace("{inbox}", &quote(&n.inbox))
                .replace("{title}", &quote(&n.title));
            let json = serde_json::to_vec(n).unwrap_or_default();
            super::transport::run("sh", &["-c".to_string(), cmd.clone()], &json)
                .err()
                .map(|e| format!("notify_cmd `{cmd}`: {e:#}"))
        })
        .collect()
}

/// Single-quoted for `sh`.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The host, briefed for an agent: what paddock is, this host's files and
/// config, its sources and inboxes with counts, the plugins and agents
/// on PATH, and the commands. `paddock context` prints this.
pub fn briefing(paths: &Paths, config: &Config, store: &dyn Store) -> Result<String> {
    let mut w = String::new();
    w.push_str("# paddock\n\n");
    w.push_str("Inbox kernel. Four nouns: item, source, label, inbox. No other product nouns.\n");
    w.push_str("An item is source-shaped data stripped: foreign_id, title, body, href, start, end, thread, parts, from, to[], cites.\n");
    w.push_str("A cite is {kind: reply|forward|quote|mention|attach, foreign_id or href, excerpt?, actor?}; it resolves to an id when the cited item is here, early or late.\n");
    w.push_str("A source admits items and may send. Built-in kinds: fs (a directory), exec (a program). Any other kind names a plugin `paddock-<kind>` on PATH; every other key on its block is its settings, and a setting NAME_cmd is run by the host and handed on as NAME.\n");
    w.push_str("Inboxes nest. A child is a tighter question over the parent. Match: sources AND from AND to AND labels (all) AND without (none) AND timed (start set) AND age (newer_than, older_than).\n");
    w.push_str("Classifiers are per-inbox, ordered, kinds regex | script (CEL) | exec | http | llm. They stamp labels. They are not sources.\n");
    w.push_str("Actor kind is person | group | list | agent.\n");
    w.push_str("Admit upserts on (source_id, foreign_id). Re-admit refreshes the item and keeps read + labels.\n");
    w.push_str("A label remembers who put it there (hand, source, classifier, or an inbox effect). A label a hand removed is denied: nothing but a hand puts it back. Read is the label `read`.\n");
    w.push_str("An inbox may say `without = [...]` (item carries none) and `then = [...]` (effects on enter, once per item: label:NAME, read, send:SOURCE, notify). `notify` raises a notice the host acts on (notify_cmd, or an app).\n");
    w.push_str("Optional blocks: [embedder] (ollama|openai|http|exec|local), [model] (exec|ollama|openai) for `answer`, [store] (sqlite; key or key_cmd encrypts; [store.mirror] s3|exec), [agent] (the CLI `paddock setup` runs), notify_cmd.\n\n");

    w.push_str("## this host\n");
    w.push_str(&format!("config   {}\n", paths.config_file.display()));
    w.push_str(&format!("db       {}\n", paths.db_path.display()));
    w.push_str(&format!("data     {}\n", paths.data_dir.display()));
    w.push_str(&format!("incoming {}\n", paths.incoming_dir.display()));
    let plugins = plugins_on_path();
    w.push_str(&format!(
        "plugins on PATH: {}\n",
        if plugins.is_empty() {
            "none".to_string()
        } else {
            plugins.join(", ")
        }
    ));
    let agents: Vec<String> = agents_on_path().into_iter().map(|a| a.cmd).collect();
    w.push_str(&format!(
        "agents on PATH: {}\n\n",
        if agents.is_empty() {
            "none".to_string()
        } else {
            agents.join(", ")
        }
    ));

    w.push_str("## config.toml as it is\n```toml\n");
    w.push_str(
        std::fs::read_to_string(&paths.config_file)
            .unwrap_or_default()
            .trim_end(),
    );
    w.push_str("\n```\n\n");

    w.push_str("## sources\n");
    let by_src: std::collections::BTreeMap<String, i64> =
        store.counts_by_source()?.into_iter().collect();
    for src in &config.source {
        let n = by_src.get(&src.id).copied().unwrap_or(0);
        let extra = ["path", "url", "cmd", "host", "account", "box"]
            .into_iter()
            .find_map(|k| setting(&src.settings, k))
            .unwrap_or_default();
        w.push_str(&format!(
            "{}  kind={}  items={n}  {extra}\n",
            src.id, src.kind
        ));
    }
    for (id, n) in &by_src {
        if config.source(id).is_none() {
            w.push_str(&format!("{id}  items={n}  (not in config)\n"));
        }
    }
    let total = store.count(&Question::default())?;
    let timed = store.count(&Question {
        timed: true,
        ..Default::default()
    })?;
    w.push_str(&format!("total {total}  timed {timed}\n\n"));

    w.push_str("## inboxes\n");
    for node in config.nodes() {
        let (unread, total) = counts(config, store, &node.path);
        let ib = &node.inbox;
        w.push_str(&format!(
            "{}  unread={unread}  items={total}",
            node.path.join("/")
        ));
        if ib.timed {
            w.push_str("  timed");
        }
        for (k, v) in [
            ("labels", &ib.labels),
            ("without", &ib.without),
            ("sources", &ib.sources),
            ("then", &ib.then),
        ] {
            if !v.is_empty() {
                w.push_str(&format!("  {k}={}", v.join(",")));
            }
        }
        if !ib.classifier.is_empty() {
            let ids: Vec<&str> = ib.classifier.iter().map(|c| c.id.as_str()).collect();
            w.push_str(&format!("  classifiers={}", ids.join(",")));
        }
        w.push('\n');
    }
    w.push_str("\n## use\n");
    w.push_str("paddock pull | inboxes | ls [INBOX] [--unread] [--from ID] [--to ID] [--text WORDS] [--like TEXT] | answer QUESTION [--in INBOX] | embed | show ID | thread ID | cited ID | part ID | label ID [+l|-l]... | read ID | unread ID | forget ID | classify ID | why ID [INBOX] | send [--title T] [--reply ID] [--to A]... [--in INBOX] [BODY] | check CMD [--set k=v]... [--send] | mirror [--restore] | setup TASK\n");
    w.push_str("Add --json to any command for machine output. Edit config.toml, then `paddock pull`. `paddock check paddock-<kind> --set k=v` tries a plugin before you rely on it. Do not invent nouns.\n");
    Ok(w)
}

/// Unread and total for an inbox path.
pub fn counts(config: &Config, store: &dyn Store, path: &[String]) -> (usize, usize) {
    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    let Some(chain) = config.chain(&refs) else {
        return (0, 0);
    };
    let mut q = Question::of(&chain);
    let total = store.count(&q).unwrap_or(0);
    q.without.push(crate::kernel::READ.into());
    let unread = store.count(&q).unwrap_or(0);
    (unread, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_cmd_gets_the_notice_quoted_and_on_stdin() {
        let tmp = std::env::temp_dir().join(format!("paddock-notify-{}", std::process::id()));
        let config = Config {
            notify_cmd: Some(format!(
                "printf '%s|%s|' {{id}} {{inbox}} > {0}; printf '%s|' {{title}} >> {0}; cat >> {0}",
                tmp.display()
            )),
            ..Default::default()
        };
        let notice = Notice {
            id: 7,
            inbox: "all/todo".into(),
            title: "it's due; \"soon\"".into(),
        };
        let warnings = notify(&config, std::slice::from_ref(&notice));
        assert!(warnings.is_empty(), "{warnings:?}");
        let got = std::fs::read_to_string(&tmp).unwrap();
        assert!(got.starts_with("7|all/todo|it's due; \"soon\"|"), "{got}");
        assert!(
            got.ends_with(&serde_json::to_string(&notice).unwrap()),
            "{got}"
        );
        let _ = std::fs::remove_file(&tmp);
        let none = Config::default();
        assert!(notify(&none, &[notice]).is_empty());
        let bad = Config {
            notify_cmd: Some("exit 3".into()),
            ..Default::default()
        };
        let w = notify(
            &bad,
            &[Notice {
                id: 1,
                inbox: "all".into(),
                title: "x".into(),
            }],
        );
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn an_agent_is_the_config_s_or_the_first_on_path() {
        let cfg: Config = toml::from_str(
            r#"
[agent]
cmd = "my-agent"
args = ["--go"]
"#,
        )
        .unwrap();
        let a = agent(&cfg).unwrap();
        assert_eq!(a.cmd, "my-agent");
        assert_eq!(a.args, ["--go"]);
        assert!(quote("a'b") == "'a'\\''b'");
    }
}
