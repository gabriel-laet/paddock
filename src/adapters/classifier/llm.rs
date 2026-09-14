//! A classifier that asks a chat model. Builds a prompt from the item, sends
//! it over a CLI or over HTTP, and reads one token back. Runs once per item.
//!
//! Over a CLI, the prompt goes to stdin and stdout is the reply:
//!
//! ```toml
//! [[inbox.classifier]]
//! id = "by-claude"
//! kind = "llm"
//! cmd = "claude"
//! args = ["-p"]
//! labels = ["later", "todo"]
//! ```
//!
//! Over HTTP, `provider` is `ollama` (`/api/chat`) or `openai`
//! (`/chat/completions`), with `url` and `model` from the spec or from
//! `PADDOCK_LLM_URL` / `PADDOCK_LLM_MODEL`. The key comes from
//! `PADDOCK_LLM_KEY` or `OPENAI_API_KEY`, never from the config.

use anyhow::{bail, Context, Result};

use super::{post, run};
use crate::kernel::{sanitize_label, Classifier, ClassifierSpec, Item};

pub const SYSTEM: &str =
    "you label one inbox item. Reply with a single token: a label or NONE. No prose.";
const BODY_LIMIT: usize = 4096;

pub struct LlmClassifier {
    id: String,
    spec: ClassifierSpec,
}

impl LlmClassifier {
    pub fn new(spec: &ClassifierSpec) -> Self {
        Self {
            id: spec.id.clone(),
            spec: spec.clone(),
        }
    }

    fn ask(&self, item: &Item) -> Result<String> {
        let user = prompt(
            self.spec.prompt.as_deref(),
            item,
            self.spec.label.as_deref(),
            &self.spec.labels,
        );
        if let Some(cmd) = self
            .spec
            .cmd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            let stdin = format!("{SYSTEM}\n\n{user}");
            return run(cmd, &self.spec.args, stdin.as_bytes());
        }
        let provider = provider(self.spec.provider.as_deref());
        let model = self
            .spec
            .model
            .clone()
            .or_else(|| env("PADDOCK_LLM_MODEL"))
            .unwrap_or_else(|| "llama3.2".into());
        let base = self.spec.url.clone().or_else(|| env("PADDOCK_LLM_URL"));
        let messages = serde_json::json!([
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": user}
        ]);
        let (url, key, body) = if provider == "openai" {
            let base = base.unwrap_or_else(|| "https://api.openai.com/v1".into());
            let Some(key) = env("PADDOCK_LLM_KEY").or_else(|| env("OPENAI_API_KEY")) else {
                bail!("openai classifier needs PADDOCK_LLM_KEY or OPENAI_API_KEY");
            };
            let body = serde_json::json!({ "model": model, "messages": messages });
            (join(&base, "chat/completions"), Some(key), body)
        } else {
            let base = base.unwrap_or_else(|| "http://127.0.0.1:11434".into());
            let body = serde_json::json!({ "model": model, "stream": false, "messages": messages });
            (join(&base, "api/chat"), None, body)
        };
        let text = post(&url, key.as_deref(), &body)?;
        let v: serde_json::Value = serde_json::from_str(&text).context("llm reply is not JSON")?;
        content(&v).ok_or_else(|| anyhow::anyhow!("llm reply has no content"))
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
        let raw = match env("PADDOCK_LLM_FIXTURE") {
            Some(fixture) => fixture,
            None => self
                .ask(item)
                .with_context(|| format!("classifier {}", self.id))?,
        };
        Ok(interpret(
            &raw,
            self.spec.label.as_deref(),
            &self.spec.labels,
        ))
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

fn provider(spec: Option<&str>) -> String {
    if let Some(p) = spec.map(str::trim).filter(|p| !p.is_empty()) {
        return p.to_ascii_lowercase();
    }
    if env("PADDOCK_LLM_KEY").is_some() || env("OPENAI_API_KEY").is_some() {
        "openai".into()
    } else {
        "ollama".into()
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn content(v: &serde_json::Value) -> Option<String> {
    let openai = v.pointer("/choices/0/message/content");
    let ollama = v.pointer("/message/content");
    let generate = v.get("response");
    openai
        .or(ollama)
        .or(generate)
        .and_then(|c| c.as_str())
        .map(str::to_string)
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
        let _g = ENV_LOCK.lock().unwrap();
        let spec = ClassifierSpec {
            id: "l".into(),
            kind: "llm".into(),
            cmd: Some("sh".into()),
            args: vec![
                "-c".into(),
                "grep -q 'title: pay' && echo money || echo NONE".into(),
            ],
            labels: vec!["money".into()],
            ..Default::default()
        };
        let c = LlmClassifier::new(&spec);
        let mut it = Item::default();
        it.title = "pay".into();
        assert_eq!(c.classify(&it).unwrap(), Some("money".into()));
        it.title = "hi".into();
        assert_eq!(c.classify(&it).unwrap(), None);
    }

    #[test]
    fn fixture_skips_the_model() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("PADDOCK_LLM_FIXTURE", "later");
        let spec = ClassifierSpec {
            id: "l".into(),
            kind: "llm".into(),
            labels: vec!["later".into(), "todo".into()],
            ..Default::default()
        };
        let got = LlmClassifier::new(&spec)
            .classify(&Item::default())
            .unwrap();
        std::env::remove_var("PADDOCK_LLM_FIXTURE");
        assert_eq!(got, Some("later".into()));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
