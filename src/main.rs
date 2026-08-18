mod tui;
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand};
use paddock::{init, load_or_init, pull_all, Paths};

#[derive(Parser)]
#[command(name = "paddock", about = "An inbox host", version)]
struct Cli {
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env();
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
    }
    Ok(())
}
