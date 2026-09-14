//! Skills: named fragments of classifiers and inboxes a config grafts in
//! with `use`. A skill is one TOML file whose first line says what it is
//! for; the host merges it into the config before the kernel sees it, so
//! the kernel keeps four nouns and `why` still names the classifier that
//! fired (`codes/detect`).
//!
//! ```toml
//! # codes: one-time codes, paged, interesting for an hour.
//!
//! [[classifier]]
//! id = "detect"
//! kind = "regex"
//! pattern = "\\b\\d{6}\\b"
//! label = "code"
//!
//! [[inbox]]
//! name = "codes"
//! labels = ["code"]
//! newer_than = "1h"
//! then = ["notify"]
//! ```
//!
//! Yours live in `<config dir>/skills/NAME.toml` and win over the shipped
//! ones of the same name. `use = ["codes"]` at the top of a config grafts
//! under the first top-level inbox; on an inbox it grafts under that inbox.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::kernel::{ClassifierSpec, Config, Inbox};

/// The skills built into the binary, as `(name, text)`.
const SHIPPED: &[(&str, &str)] = &[
    ("codes", include_str!("../../skills/codes.toml")),
    ("mentions", include_str!("../../skills/mentions.toml")),
    ("receipts", include_str!("../../skills/receipts.toml")),
    ("newsletters", include_str!("../../skills/newsletters.toml")),
];

/// One skill as listed: its name, its first line, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Skill {
    pub name: String,
    pub about: String,
    /// `yours` (the config directory) or `shipped` (the binary).
    pub origin: String,
}

#[derive(Debug, Default, serde::Deserialize)]
struct Fragment {
    #[serde(default)]
    classifier: Vec<ClassifierSpec>,
    #[serde(default)]
    inbox: Vec<Inbox>,
}

fn yours_dir(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join("skills")
}

/// The text of a skill: yours first, else shipped.
fn text(config_dir: &Path, name: &str) -> Result<(String, &'static str)> {
    let path = yours_dir(config_dir).join(format!("{name}.toml"));
    if path.is_file() {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        return Ok((text, "yours"));
    }
    if let Some((_, text)) = SHIPPED.iter().find(|(n, _)| *n == name) {
        return Ok((text.to_string(), "shipped"));
    }
    let known: Vec<String> = skills(config_dir).into_iter().map(|s| s.name).collect();
    bail!("no skill `{name}` (have: {})", known.join(", "))
}

/// The first comment line, without its `#` and its `name:` prefix.
fn about(text: &str, name: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim())
        .map(|l| {
            l.strip_prefix(name)
                .and_then(|r| r.strip_prefix(':'))
                .map(str::trim)
                .unwrap_or(l)
                .to_string()
        })
        .unwrap_or_default()
}

/// Every skill this host can use, yours before shipped, no name twice.
pub fn skills(config_dir: &Path) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(yours_dir(config_dir)) {
        let mut files: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".toml"))
            else {
                continue;
            };
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            out.push(Skill {
                name: name.to_string(),
                about: about(&text, name),
                origin: "yours".into(),
            });
        }
    }
    for (name, text) in SHIPPED {
        if !out.iter().any(|s| s.name == *name) {
            out.push(Skill {
                name: name.to_string(),
                about: about(text, name),
                origin: "shipped".into(),
            });
        }
    }
    out
}

/// One skill, parsed.
pub fn skill(config_dir: &Path, name: &str) -> Result<(Skill, Vec<ClassifierSpec>, Vec<Inbox>)> {
    let (text, origin) = text(config_dir, name)?;
    let fragment: Fragment =
        toml::from_str(&text).with_context(|| format!("skill `{name}` does not parse"))?;
    if fragment.classifier.is_empty() && fragment.inbox.is_empty() {
        bail!("skill `{name}` has no classifier and no inbox");
    }
    Ok((
        Skill {
            name: name.to_string(),
            about: about(&text, name),
            origin: origin.into(),
        },
        fragment.classifier,
        fragment.inbox,
    ))
}

/// Resolve every `use` in the config: the top-level list under the first
/// top-level inbox, an inbox's list under that inbox. Classifier ids are
/// namespaced `skill/id`; an inbox a skill brings must not already exist
/// under its parent.
pub fn graft(mut config: Config, config_dir: &Path) -> Result<Config> {
    let top = std::mem::take(&mut config.use_);
    if !top.is_empty() {
        if config.inbox.is_empty() {
            config.inbox.push(Inbox {
                name: "all".into(),
                ..Default::default()
            });
        }
        config.inbox[0].use_.splice(0..0, top);
    }
    for ib in &mut config.inbox {
        graft_into(ib, config_dir, &ib.name.clone())?;
    }
    Ok(config)
}

fn graft_into(inbox: &mut Inbox, config_dir: &Path, path: &str) -> Result<()> {
    for name in std::mem::take(&mut inbox.use_) {
        let (_, classifiers, inboxes) =
            skill(config_dir, &name).with_context(|| format!("inbox {path}: use"))?;
        for mut c in classifiers {
            c.id = format!("{name}/{}", c.id);
            inbox.classifier.push(c);
        }
        for child in inboxes {
            if inbox.inbox.iter().any(|i| i.name == child.name) {
                bail!(
                    "inbox {path}: skill `{name}` brings an inbox `{}` that is already there",
                    child.name
                );
            }
            inbox.inbox.push(child);
        }
    }
    for child in &mut inbox.inbox {
        let child_path = format!("{path}/{}", child.name);
        graft_into(child, config_dir, &child_path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_skill_parses_and_says_what_it_is_for() {
        let none = std::env::temp_dir().join("paddock-no-such-dir");
        for (name, _) in SHIPPED {
            let (s, classifiers, inboxes) = skill(&none, name).unwrap();
            assert!(!s.about.is_empty(), "{name} has no first line");
            assert!(!classifiers.is_empty() || !inboxes.is_empty());
            assert_eq!(s.origin, "shipped");
        }
        assert!(skill(&none, "nope")
            .unwrap_err()
            .to_string()
            .contains("no skill `nope`"));
    }

    #[test]
    fn use_grafts_under_all_and_under_an_inbox_with_namespaced_ids() {
        let tmp = std::env::temp_dir().join(format!("paddock-skills-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("skills")).unwrap();
        std::fs::write(
            tmp.join("skills/mine.toml"),
            "# mine: a test skill\n[[classifier]]\nid = \"flag\"\nkind = \"regex\"\npattern = \"x\"\nlabel = \"mine\"\n[[inbox]]\nname = \"mine\"\nlabels = [\"mine\"]\n",
        )
        .unwrap();
        let config: Config = toml::from_str(
            r#"
use = ["codes"]

[[inbox]]
name = "all"

[[inbox.inbox]]
name = "work"
use = ["mine", "receipts"]
"#,
        )
        .unwrap();
        let config = graft(config, &tmp).unwrap();
        let all = &config.inbox[0];
        assert_eq!(all.classifier[0].id, "codes/detect");
        let names: Vec<&str> = all.inbox.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["work", "codes"]);
        let work = &all.inbox[0];
        assert_eq!(work.classifier[0].id, "mine/flag");
        assert_eq!(work.classifier[1].id, "receipts/detect");
        let names: Vec<&str> = work.inbox.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["mine", "receipts"]);
        assert!(all.use_.is_empty() && work.use_.is_empty(), "consumed");

        let listed = skills(&tmp);
        assert_eq!(
            (listed[0].name.as_str(), listed[0].origin.as_str()),
            ("mine", "yours")
        );
        assert_eq!(listed[0].about, "a test skill");
        assert!(listed
            .iter()
            .any(|s| s.name == "codes" && s.origin == "shipped"));

        let twice: Config = toml::from_str(
            "[[inbox]]\nname = \"all\"\nuse = [\"codes\"]\n[[inbox.inbox]]\nname = \"codes\"\n",
        )
        .unwrap();
        let err = graft(twice, &tmp).unwrap_err().to_string();
        assert!(err.contains("already there"), "{err}");
        let unknown: Config = toml::from_str("use = [\"nope\"]\n").unwrap();
        assert!(graft(unknown, &tmp).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
