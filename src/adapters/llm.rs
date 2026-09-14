//! A classifier that asks a chat model. Ollama `/api/chat` or an
//! OpenAI-compatible `/chat/completions`. Runs once per item.

use anyhow::{bail, Result};
use std::time::Duration;

use crate::kernel::{sanitize_label, Classifier, ClassifierSpec, Item};

pub const SYSTEM: &str =
    "you label one inbox item. Reply with a single token: a label or NONE. No prose.";
const TIMEOUT: Duration = Duration::from_secs(8);
const BODY_LIMIT: usize = 4096;

pub struct LlmClassifier {
    id: String,
    model: Option<String>,
    provider: Option<String>,
    url: Option<String>,
    prompt: Option<String>,
    label: Option<String>,
    labels: Vec<String>,
}

impl LlmClassifier {
    pub fn new(spec: &ClassifierSpec) -> Self {
        Self {
            id: spec.id.clone(),
            model: spec.model.clone(),
            provider: spec.provider.clone(),
            url: spec.url.clone(),
            prompt: spec.prompt.clone(),
            label: spec.label.clone(),
            labels: spec.labels.clone(),
        }
    }

    fn call(&self, item: &Item) -> Result<String> {
        let provider = resolve_provider(self.provider.as_deref());
        let model = self
            .model
            .clone()
            .or_else(|| env_nonempty("PADDOCK_LLM_MODEL"))
            .unwrap_or_else(|| "llama3.2".into());
        let user = user_message(
            self.prompt.as_deref(),
            item,
            self.label.as_deref(),
            &self.labels,
        );
        let base = self.url.clone().or_else(|| env_nonempty("PADDOCK_LLM_URL"));
        let (url, key, body) = if provider == "openai" {
            let base = base.unwrap_or_else(|| "https://api.openai.com/v1".into());
            let key = env_nonempty("PADDOCK_LLM_KEY").or_else(|| env_nonempty("OPENAI_API_KEY"));
            let Some(key) = key else {
                bail!("openai classifier needs PADDOCK_LLM_KEY or OPENAI_API_KEY");
            };
            (
                join_url(&base, "chat/completions"),
                Some(key),
                openai_body(&model, SYSTEM, &user),
            )
        } else {
            let base = base.unwrap_or_else(|| "http://127.0.0.1:11434".into());
            (
                join_url(&base, "api/chat"),
                None,
                ollama_body(&model, SYSTEM, &user),
            )
        };
        post(&url, key.as_deref(), &body)
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
        let raw = match env_nonempty("PADDOCK_LLM_FIXTURE") {
            Some(fixture) => fixture,
            None => self.call(item)?,
        };
        Ok(interpret(&raw, self.label.as_deref(), &self.labels))
    }
}

/// First line, first token. NONE means nothing. Then sanitize.
pub fn first_token(raw: &str) -> Option<String> {
    let token = raw
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    if token.is_empty() || token.eq_ignore_ascii_case("none") {
        return None;
    }
    sanitize_label(token)
}

/// With a fixed label the model answers yes or no; otherwise it names a label,
/// which must be on the allow-list when one is given.
pub fn interpret(raw: &str, label: Option<&str>, allow: &[String]) -> Option<String> {
    let token = raw
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    if token.is_empty() || token.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(want) = label {
        let want = sanitize_label(want)?;
        let t = token.to_ascii_lowercase();
        let yes = t == "yes"
            || t == "y"
            || t == "true"
            || sanitize_label(token).as_deref() == Some(&want);
        return yes.then_some(want);
    }
    let got = first_token(raw)?;
    let allowed = allow.is_empty()
        || allow
            .iter()
            .any(|a| sanitize_label(a).as_deref() == Some(&got));
    allowed.then_some(got)
}

pub fn openai_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    })
}

pub fn ollama_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    })
}

fn user_message(
    prompt: Option<&str>,
    item: &Item,
    label: Option<&str>,
    allow: &[String],
) -> String {
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

fn resolve_provider(spec: Option<&str>) -> String {
    if let Some(p) = spec.map(str::trim).filter(|p| !p.is_empty()) {
        return p.to_ascii_lowercase();
    }
    if env_nonempty("PADDOCK_LLM_KEY").is_some() || env_nonempty("OPENAI_API_KEY").is_some() {
        "openai".into()
    } else {
        "ollama".into()
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn post(url: &str, key: Option<&str>, body: &serde_json::Value) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("paddock/0.1")
        .timeout(TIMEOUT)
        .build()?;
    let mut req = client.post(url).json(body);
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let resp = req.send()?;
    if !resp.status().is_success() {
        bail!("llm http {}", resp.status());
    }
    let v: serde_json::Value = resp.json()?;
    content(&v).ok_or_else(|| anyhow::anyhow!("llm response missing content"))
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
    fn first_token_and_none() {
        assert_eq!(first_token("later\n"), Some("later".into()));
        assert_eq!(first_token("NONE"), None);
        assert_eq!(first_token("foo bar"), Some("foo".into()));
        assert_eq!(
            first_token("Hello-World!! extra"),
            Some("hello-world".into())
        );
    }

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
    fn bodies() {
        let oai = openai_body("gpt-4o-mini", SYSTEM, "title: hi");
        assert_eq!(oai["model"], "gpt-4o-mini");
        assert_eq!(oai["messages"][0]["role"], "system");
        let oll = ollama_body("llama3.2", SYSTEM, "title: hi");
        assert_eq!(oll["stream"], false);
    }

    #[test]
    fn fixture_skips_http() {
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
