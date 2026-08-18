//! Verbs run once here. TUI and web both call `run_verb`.

use anyhow::Result;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::config::{Config, Paths};
use crate::engine::{admit_file, classify_item, items_in_chain, pull_all, relabel, stamp};
use crate::keys::{Verb, HELP};
use crate::store::{Item, Store};
use crate::theme::{list_themes, load_named, write_theme_override};

pub struct VerbCtx {
    pub item_id: Option<i64>,
    pub inbox_path: Vec<String>,
    pub unread_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub status: String,
    pub quit: bool,
    pub reload_config: bool,
    pub unread_only: Option<bool>,
    pub theme_name: Option<String>,
    pub overlay: Option<String>,
}

fn ok_status(s: impl Into<String>) -> Outcome {
    Outcome {
        status: s.into(),
        ..Outcome::default()
    }
}

fn need_item<'a>(store: &'a Store, ctx: &VerbCtx) -> Result<Result<Item, Outcome>> {
    let Some(id) = ctx.item_id else {
        return Ok(Err(ok_status("no item")));
    };
    Ok(Ok(store.get(id)?))
}

pub fn run_verb(
    store: &Store,
    config: &Config,
    paths: &Paths,
    ctx: &VerbCtx,
    verb: &Verb,
) -> Result<Outcome> {
    match verb {
        Verb::Quit => Ok(Outcome {
            quit: true,
            status: String::new(),
            ..Outcome::default()
        }),
        Verb::Help => Ok(Outcome {
            overlay: Some(HELP.to_string()),
            status: String::new(),
            ..Outcome::default()
        }),
        Verb::Pull => {
            let n = pull_all(store, config)?;
            Ok(Outcome {
                status: format!("admitted {n}"),
                reload_config: true,
                ..Outcome::default()
            })
        }
        Verb::ToggleRead => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            let read = store.toggle_read(item.id)?;
            Ok(ok_status(if read { "read" } else { "unread" }))
        }
        Verb::Unread => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            if !item.read {
                return Ok(ok_status("unread"));
            }
            store.set_read(item.id, false)?;
            Ok(ok_status("unread"))
        }
        Verb::Eat => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            store.set_read(item.id, true)?;
            Ok(ok_status("read"))
        }
        Verb::Relabel { label } => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            let on = relabel(store, config, item.id, label)?;
            Ok(ok_status(if on {
                format!("+{label}")
            } else {
                format!("-{label}")
            }))
        }
        Verb::Bury => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            stamp(store, config, item.id, "later")?;
            Ok(ok_status("later"))
        }
        Verb::Todo => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            stamp(store, config, item.id, "todo")?;
            Ok(ok_status("todo"))
        }
        Verb::Again => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            classify_item(store, config, item.id)?;
            Ok(ok_status("classified"))
        }
        Verb::Why => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            Ok(ok_status(explain_why(config, &item, &ctx.inbox_path)))
        }
        Verb::Yank => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            fs::create_dir_all(&paths.data_dir)?;
            let dest = paths.data_dir.join("yank");
            fs::write(&dest, item.title.as_bytes())?;
            Ok(ok_status(format!("yank {}", dest.display())))
        }
        Verb::Open => {
            let item = match need_item(store, ctx)? {
                Ok(i) => i,
                Err(o) => return Ok(o),
            };
            let Some(href) = item.href.as_deref().filter(|s| !s.is_empty()) else {
                return Ok(ok_status("no href"));
            };
            try_open(href);
            Ok(ok_status(href))
        }
        Verb::New { title } => {
            let title = title.trim();
            if title.is_empty() {
                return Ok(ok_status("new TITLE"));
            }
            fs::create_dir_all(&paths.incoming_dir)?;
            let stem = sanitize_filename(title);
            let dest = unique_path(&paths.incoming_dir.join(format!("{stem}.md")));
            let body = format!("{title}\n");
            fs::write(&dest, body.as_bytes())?;
            let id = admit_file(store, config, "incoming", &dest)?;
            Ok(ok_status(format!(
                "new {}{}",
                dest.display(),
                id.map(|i| format!(" #{i}")).unwrap_or_default()
            )))
        }
        Verb::Which => {
            let path = if ctx.inbox_path.is_empty() {
                "all".to_string()
            } else {
                ctx.inbox_path.join("/")
            };
            let refs: Vec<&str> = ctx.inbox_path.iter().map(|s| s.as_str()).collect();
            let (unread, total) = match config.find_chain(&refs) {
                Some(chain) => {
                    let items = items_in_chain(store, &chain).unwrap_or_default();
                    (items.iter().filter(|i| !i.read).count(), items.len())
                }
                None => (0, 0),
            };
            Ok(ok_status(format!("{path}  {unread}/{total}")))
        }
        Verb::Db => Ok(ok_status(paths.db_path.display().to_string())),
        Verb::Only => Ok(Outcome {
            unread_only: Some(!ctx.unread_only),
            status: if ctx.unread_only {
                "all items".into()
            } else {
                "unread only".into()
            },
            ..Outcome::default()
        }),
        Verb::Spill => {
            let refs: Vec<&str> = ctx.inbox_path.iter().map(|s| s.as_str()).collect();
            let chain = config.find_chain(&refs).unwrap_or_default();
            let items = items_in_chain(store, &chain)?;
            let path_s = if ctx.inbox_path.is_empty() {
                "all".to_string()
            } else {
                ctx.inbox_path.join("/")
            };
            let mut md = format!("# {path_s}\n");
            for it in &items {
                md.push_str(&format!("\n## {}\n", it.title));
                md.push_str(&format!("{}  {}\n", it.source_id, it.created_at));
                if !it.labels.is_empty() {
                    md.push_str(&format!("labels: {}\n", it.labels.join(" ")));
                }
                md.push('\n');
                md.push_str(&it.body);
                if !it.body.ends_with('\n') {
                    md.push('\n');
                }
            }
            fs::create_dir_all(&paths.data_dir)?;
            let dest = paths.data_dir.join("spill.md");
            fs::write(&dest, md.as_bytes())?;
            Ok(ok_status(format!("spill {} ({} items)", dest.display(), items.len())))
        }
        Verb::Themes | Verb::Theme { name: None } => {
            let names = list_themes(paths);
            Ok(ok_status(names.join(" ")))
        }
        Verb::Theme { name: Some(name) } => {
            write_theme_override(paths, name)?;
            let t = load_named(name, paths);
            Ok(Outcome {
                status: format!("theme {}", t.name),
                theme_name: Some(t.name),
                ..Outcome::default()
            })
        }
        other if other.is_local() => Ok(Outcome::default()),
        other => Ok(ok_status(format!(
            "not an editor command: {}",
            other.id()
        ))),
    }
}

pub fn explain_why(config: &Config, item: &Item, inbox_path: &[String]) -> String {
    let refs: Vec<&str> = inbox_path.iter().map(|s| s.as_str()).collect();
    let path_s = if inbox_path.is_empty() {
        "all".to_string()
    } else {
        inbox_path.join("/")
    };
    let mut matched = Vec::new();
    let mut fired = Vec::new();

    for c in &config.classifier {
        if let Some(lab) = &c.label {
            if item.labels.iter().any(|l| l == lab) && !fired.iter().any(|f| f == &c.id) {
                fired.push(c.id.clone());
            }
        }
    }
    if let Some(chain) = config.find_chain(&refs) {
        for ib in chain {
            for l in &ib.labels {
                if item.labels.iter().any(|x| x == l) && !matched.iter().any(|m| m == l) {
                    matched.push(l.clone());
                }
            }
            for c in &ib.classifier {
                if let Some(lab) = &c.label {
                    if item.labels.iter().any(|x| x == lab) && !fired.iter().any(|f| f == &c.id) {
                        fired.push(c.id.clone());
                    }
                }
            }
        }
    }
    let labs = if matched.is_empty() {
        "—".to_string()
    } else {
        matched.join(" ")
    };
    let cls = if fired.is_empty() {
        "—".to_string()
    } else {
        fired.join(" ")
    };
    format!("{path_s}  labels: {labs}  classifiers: {cls}")
}

pub fn sanitize_filename(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

fn unique_path(path: &Path) -> std::path::PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("md");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for n in 2..1000 {
        let p = parent.join(format!("{stem}-{n}.{ext}"));
        if !p.exists() {
            return p;
        }
    }
    path.to_path_buf()
}

fn try_open(href: &str) {
    for bin in ["xdg-open", "open"] {
        if Command::new(bin)
            .arg(href)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_filename("Hello World"), "Hello-World");
        assert_eq!(sanitize_filename("../x"), "x");
        assert_eq!(sanitize_filename("***"), "untitled");
    }
}
