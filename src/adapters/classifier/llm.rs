//! A classifier that asks a chat model: a prompt built from the item, one
//! token back. Runs once per item. The model is any `Model` adapter, so
//! `cmd = "claude", args = ["-p"]` works as well as Ollama or OpenAI.
//!
//! ```toml
//! [[inbox.classifier]]
//! id = "by-claude"
//! kind = "llm"
//! cmd = "claude"
//! args = ["-p"]
//! labels = ["later", "todo"]
//! # or: provider = "ollama" | "openai", url, model, key
//! ```

use anyhow::{Context, Result};

use super::super::model;
use crate::kernel::{
    sanitize_label, setting, AdapterSpec, Classifier, ClassifierSpec, Item, Model,
};

pub const SYSTEM: &str =
    "you label one inbox item. Reply with a single token: a label or NONE. No prose.";
const BODY_LIMIT: usize = 4096;

pub struct LlmClassifier {
    id: String,
    model: Box<dyn Model>,
    prompt: Option<String>,
    label: Option<String>,
    labels: Vec<String>,
}

impl LlmClassifier {
    pub fn new(spec: &ClassifierSpec) -> Result<Self> {
        let model = model::build(&AdapterSpec {
            kind: setting(&spec.settings, "provider").unwrap_or_default(),
            settings: spec.settings.clone(),
        })
        .with_context(|| format!("classifier {}", spec.id))?;
        Ok(Self {
            id: spec.id.clone(),
            model,
            prompt: setting(&spec.settings, "prompt"),
            label: spec.label.clone(),
            labels: spec.labels.clone(),
        })
    }
}

impl Classifier for LlmClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn once(&self) -> bool {
        true
    }

    fn classify(&self, item: &Item) -> Result<Option<String>> {
        let user = prompt(
            self.prompt.as_deref(),
            item,
            self.label.as_deref(),
            &self.labels,
        );
        let raw = self.model.complete(SYSTEM, &user)?;
        Ok(interpret(&raw, self.label.as_deref(), &self.labels))
    }
}

/// With a fixed label the model answers yes or no; otherwise it names a label,
/// which must be on the allow-list when one is given.
pub fn interpret(raw: &str, label: Option<&str>, allow: &[String]) -> Option<String> {
    let token = raw.split_whitespace().next().unwrap_or("");
    if token.is_empty() || token.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(want) = label {
        let want = sanitize_label(want)?;
        let t = token.to_ascii_lowercase();
        let yes = matches!(t.as_str(), "yes" | "y" | "true")
            || sanitize_label(token).as_deref() == Some(&want);
        return yes.then_some(want);
    }
    let got = sanitize_label(token)?;
    let allowed = allow.is_empty()
        || allow
            .iter()
            .any(|a| sanitize_label(a).as_deref() == Some(&got));
    allowed.then_some(got)
}

fn prompt(prompt: Option<&str>, item: &Item, label: Option<&str>, allow: &[String]) -> String {
    let mut s = String::new();
    if let Some(p) = prompt.filter(|p| !p.is_empty()) {
        s.push_str(p);
        s.push('\n');
    }
    if label.is_some() {
        s.push_str("Reply yes or no.\n");
    } else if !allow.is_empty() {
        s.push_str(&format!("Pick one of: {}, NONE\n", allow.join(", ")));
    }
    s.push_str(&format!("title: {}\n", item.title));
    s.push_str(&format!("body: {}\n", truncate(&item.body, BODY_LIMIT)));
    s.push_str(&format!("labels: {}", item.labels.join(", ")));
    for (k, v) in [("start", &item.start), ("end", &item.end)] {
        if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
            s.push_str(&format!("\n{k}: {v}"));
        }
    }
    s
}

fn truncate(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_rejects_strangers() {
        let allow = vec!["later".into(), "todo".into()];
        assert_eq!(interpret("later", None, &allow), Some("later".into()));
        assert_eq!(interpret("money", None, &allow), None);
        assert_eq!(interpret("NONE", None, &allow), None);
    }

    #[test]
    fn yes_no_with_label() {
        for yes in ["yes", "y", "true", "later"] {
            assert_eq!(interpret(yes, Some("later"), &[]), Some("later".into()));
        }
        assert_eq!(interpret("no", Some("later"), &[]), None);
        assert_eq!(interpret("nope", Some("later"), &[]), None);
    }

    #[test]
    fn over_a_cli_the_prompt_goes_to_stdin() {
        let spec = ClassifierSpec {
            id: "l".into(),
            kind: "llm".into(),
            settings: [
                ("cmd".to_string(), serde_json::json!("sh")),
                (
                    "args".to_string(),
                    serde_json::json!(["-c", "grep -q 'title: pay' && echo money || echo NONE"]),
                ),
            ]
            .into_iter()
            .collect(),
            labels: vec!["money".into()],
            ..Default::default()
        };
        let c = LlmClassifier::new(&spec).unwrap();
        let mut it = Item::default();
        it.title = "pay".into();
        assert_eq!(c.classify(&it).unwrap(), Some("money".into()));
        it.title = "hi".into();
        assert_eq!(c.classify(&it).unwrap(), None);
    }
}
