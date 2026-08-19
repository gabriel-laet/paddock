mod tui;
mod web;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use paddock::{init, load_or_init, pull_all, resolve_remote, shell_quote, write_context, Config, Paths};
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(name = "paddock", about = "An inbox host", version)]
struct Cli {
    /// Run on a remote host over ssh (host from flag, PADDOCK_REMOTE, or config remote)
    #[arg(long, value_name = "HOST", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    remote: Option<String>,
    /// Force this machine even if a remote is configured
    #[arg(long)]
    local: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create config, data dir, and incoming directory
    Init {
        /// Create ./.paddock in the current directory (instead of XDG)
        #[arg(long)]
        here: bool,
    },
    /// Pull every source, classify new items, persist
    Pull,
    /// HTTP on 127.0.0.1:4736
    Serve {
        #[arg(long, default_value = "127.0.0.1:4736")]
        bind: String,
    },
    /// Dump this host for an agent (pipeable)
    Context,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env();
    if !cli.local && !matches!(cli.cmd, Some(Cmd::Init { .. })) {
        let cfg_remote = Config::load(&paths.config_file)
            .ok()
            .and_then(|c| c.remote);
        let env = std::env::var("PADDOCK_REMOTE").ok();
        if let Some(host) = resolve_remote(
            cli.remote.as_deref(),
            env.as_deref(),
            cfg_remote.as_deref(),
        ) {
            return run_remote(&host, &cli);
        }
        if cli.remote.is_some() {
            bail!("no remote host (pass --remote HOST, set PADDOCK_REMOTE, or config remote)");
        }
    }
    match cli.cmd {
        None => {
            load_or_init(&paths)?;
            tui::run(paths)?;
        }
        Some(Cmd::Init { here }) => {
            let paths = if here {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                Paths::here(&cwd)
            } else {
                paths
            };
            init(&paths)?;
            println!("config  {}", paths.config_file.display());
            println!("data    {}", paths.data_dir.display());
            println!("incoming {}", paths.incoming_dir.display());
        }
        Some(Cmd::Pull) => {
            let (config, store) = load_or_init(&paths)?;
            let n = pull_all(&store, &config)?;
            println!("admitted {n}");
        }
        Some(Cmd::Serve { bind }) => {
            load_or_init(&paths)?;
            web::serve(paths, bind)?;
        }
        Some(Cmd::Context) => {
            let (config, store) = load_or_init(&paths)?;
            write_context(&paths, &config, &store, std::io::stdout())?;
        }
    }
    Ok(())
}

fn run_remote(host: &str, cli: &Cli) -> Result<()> {
    let mut args: Vec<String> = Vec::new();
    let tty = match &cli.cmd {
        None => true,
        Some(Cmd::Serve { bind }) => {
            args.push("serve".into());
            args.push("--bind".into());
            args.push(bind.clone());
            true
        }
        Some(Cmd::Pull) => {
            args.push("pull".into());
            false
        }
        Some(Cmd::Context) => {
            args.push("context".into());
            false
        }
        Some(Cmd::Init { .. }) => return Ok(()),
    };
    let mut remote = String::from("PATH=\"$HOME/.local/bin:$PATH\" paddock --local");
    for a in &args {
        remote.push(' ');
        remote.push_str(&shell_quote(a));
    }
    let mut cmd = Command::new("ssh");
    if tty && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        cmd.arg("-t");
    }
    cmd.arg(host).arg(remote);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    let status = cmd.status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
