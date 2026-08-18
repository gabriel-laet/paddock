use anyhow::{bail, Context, Result};
use std::time::Duration;

use crate::config::ClassifierConfig;
use crate::store::Item;

pub const LLM_SYSTEM: &str =
    "you label one inbox item. Reply with a single token: a label or NONE. No prose.";

const LLM_TIMEOUT: Duration = Duration::from_secs(8);
const BODY_LIMIT: usize = 4096;
const SCRIPT_MAX_OPS: u64 = 50_000;

pub trait Classifier: Send + Sync {
    fn id(&self) -> &str;
    /// Label to attach when this classifier fires.
    fn classify(&self, item: &Item) -> Option<String>;
}

pub struct RegexClassifier {
    id: String,
    re: regex::Regex,
    label: String,
}

impl RegexClassifier {
    pub fn new(cfg: &ClassifierConfig) -> Result<Self> {
        let pattern = cfg
            .pattern
            .as_deref()
            .context("regex classifier needs pattern")?;
        let label = cfg
            .label
            .clone()
            .context("regex classifier needs label")?;
        let re = regex::Regex::new(pattern)
            .with_context(|| format!("classifier {}: bad pattern", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            re,
            label,
        })
    }
}

impl Classifier for RegexClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn classify(&self, item: &Item) -> Option<String> {
        if self.re.is_match(&item.title) || self.re.is_match(&item.body) {
            Some(self.label.clone())
        } else {
            None
        }
    }
}

pub struct ScriptClassifier {
    id: String,
    script: String,
    label: Option<String>,
}

impl ScriptClassifier {
    pub fn new(cfg: &ClassifierConfig) -> Result<Self> {
        let script = cfg
            .script
            .as_deref()
            .context("script classifier needs script")?
            .to_string();
        let engine = script_engine();
        engine
            .compile(&script)
            .with_context(|| format!("classifier {}: bad script", cfg.id))?;
        Ok(Self {
            id: cfg.id.clone(),
            script,
            label: cfg.label.clone(),
        })
    }
}

impl Classifier for ScriptClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn classify(&self, item: &Item) -> Option<String> {
        let engine = script_engine();
        let mut scope = rhai::Scope::new();
        scope.push("item", item_map(item));
        let val: rhai::Dynamic = engine.eval_with_scope(&mut scope, &self.script).ok()?;
        script_value_to_label(val, self.label.as_deref())
    }
}

fn script_engine() -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_max_operations(SCRIPT_MAX_OPS);
    engine.set_max_modules(0);
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.disable_symbol("import");
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});
    engine
}

fn item_map(item: &Item) -> rhai::Map {
    let mut map = rhai::Map::new();
    map.insert("title".into(), item.title.clone().into());
    map.insert("body".into(), item.body.clone().into());
    map.insert("source".into(), item.source_id.clone().into());
    map.insert(
        "href".into(),
        item.href.clone().unwrap_or_default().into(),
    );
    map.insert(
        "start".into(),
        item.start.clone().unwrap_or_default().into(),
    );
    map.insert("end".into(), item.end.clone().unwrap_or_default().into());
    map.insert(
        "thread".into(),
        item.thread.clone().unwrap_or_default().into(),
    );
    let parts: rhai::Array = item
        .parts
        .iter()
        .map(|p| rhai::Dynamic::from(p.kind.as_str().to_string()))
        .collect();
    map.insert("parts".into(), parts.into());
    let labels: rhai::Array = item
        .labels
        .iter()
        .cloned()
        .map(rhai::Dynamic::from)
        .collect();
    map.insert("labels".into(), labels.into());
    map
}

fn script_value_to_label(val: rhai::Dynamic, config_label: Option<&str>) -> Option<String> {
    if val.is_unit() {
        return None;
    }
    if let Ok(b) = val.as_bool() {
        if b {
            return config_label.and_then(sanitize_label);
        }
        return None;
    }
    if val.is_string() {
        let s = val.into_string().ok()?;
        if s.is_empty() {
            return None;
        }
        return sanitize_label(&s);
    }
    None
}

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
    pub fn new(cfg: &ClassifierConfig) -> Result<Self> {
        Ok(Self {
            id: cfg.id.clone(),
            model: cfg.model.clone(),
            provider: cfg.provider.clone(),
            url: cfg.url.clone(),
            prompt: cfg.prompt.clone(),
            label: cfg.label.clone(),
            labels: cfg.labels.clone(),
        })
    }

    fn classify_inner(&self, item: &Item) -> Result<Option<String>> {
        let raw = if let Some(fix) = env_nonempty("PADDOCK_LLM_FIXTURE") {
            fix
        } else {
            self.call_model(item)?
        };
        Ok(interpret_llm_reply(
            &raw,
            self.label.as_deref(),
            &self.labels,
        ))
    }

    fn call_model(&self, item: &Item) -> Result<String> {
        let provider = resolve_provider(self.provider.as_deref());
        let model = self
            .model
            .clone()
            .or_else(|| env_nonempty("PADDOCK_LLM_MODEL"))
            .unwrap_or_else(|| "llama3.2".into());
        let user = build_user_message(self.prompt.as_deref(), item, self.label.as_deref(), &self.labels);
        let (url, key, body) = if provider == "openai" {
            let base = self
                .url
                .clone()
                .or_else(|| env_nonempty("PADDOCK_LLM_URL"))
                .unwrap_or_else(|| "https://api.openai.com/v1".into());
            let key = env_nonempty("PADDOCK_LLM_KEY").or_else(|| env_nonempty("OPENAI_API_KEY"));
            let Some(key) = key else {
                anyhow::bail!("openai classifier needs PADDOCK_LLM_KEY or OPENAI_API_KEY");
            };
            (
                join_url(&base, "chat/completions"),
                Some(key),
                build_openai_body(&model, LLM_SYSTEM, &user),
            )
        } else {
            let base = self
                .url
                .clone()
                .or_else(|| env_nonempty("PADDOCK_LLM_URL"))
                .unwrap_or_else(|| "http://127.0.0.1:11434".into());
            (
                join_url(&base, "api/chat"),
                None,
                build_ollama_body(&model, LLM_SYSTEM, &user),
            )
        };
        llm_http(&url, key.as_deref(), &body)
    }
}

impl Classifier for LlmClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn classify(&self, item: &Item) -> Option<String> {
        self.classify_inner(item).ok().flatten()
    }
}

pub fn build_classifier(cfg: &ClassifierConfig) -> Result<Box<dyn Classifier>> {
    match cfg.kind.as_str() {
        "regex" => Ok(Box::new(RegexClassifier::new(cfg)?)),
        "script" => Ok(Box::new(ScriptClassifier::new(cfg)?)),
        "llm" => Ok(Box::new(LlmClassifier::new(cfg)?)),
        other => bail!("unknown classifier kind `{other}` (regex, script, llm)"),
    }
}

pub fn run_classifier(cfg: &ClassifierConfig, item: &Item) -> Result<Option<String>> {
    Ok(build_classifier(cfg)?.classify(item))
}

pub fn sanitize_label(s: &str) -> Option<String> {
    let mut out = String::new();
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            out.push(c);
            if out.len() >= 40 {
                break;
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// First line, first token. NONE → None. Then sanitize.
pub fn parse_llm_token(raw: &str) -> Option<String> {
    let line = raw.lines().next().unwrap_or("").trim();
    let token = line.split_whitespace().next().unwrap_or("");
    if token.is_empty() || token.eq_ignore_ascii_case("none") {
        return None;
    }
    sanitize_label(token)
}

pub fn interpret_llm_reply(
    raw: &str,
    cfg_label: Option<&str>,
    allow: &[String],
) -> Option<String> {
    let line = raw.lines().next().unwrap_or("").trim();
    let token = line.split_whitespace().next().unwrap_or("");
    if token.is_empty() || token.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(want) = cfg_label {
        let want_s = sanitize_label(want)?;
        let t = token.to_ascii_lowercase();
        let tok_s = sanitize_label(token);
        if t == "yes" || t == "y" || t == "true" || tok_s.as_deref() == Some(want_s.as_str()) {
            return Some(want_s);
        }
        return None;
    }
    let label = parse_llm_token(raw)?;
    if !allow.is_empty()
        && !allow
            .iter()
            .any(|a| sanitize_label(a).as_deref() == Some(label.as_str()))
    {
        return None;
    }
    Some(label)
}

pub fn build_openai_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    })
}

pub fn build_ollama_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    })
}

fn build_user_message(
    prompt: Option<&str>,
    item: &Item,
    cfg_label: Option<&str>,
    allow: &[String],
) -> String {
    let mut s = String::new();
    if let Some(p) = prompt {
        if !p.is_empty() {
            s.push_str(p);
            s.push('\n');
        }
    }
    if cfg_label.is_some() {
        s.push_str("Reply yes or no.\n");
    } else if !allow.is_empty() {
        s.push_str("Pick one of: ");
        s.push_str(&allow.join(", "));
        s.push_str(", NONE\n");
    }
    s.push_str("title: ");
    s.push_str(&item.title);
    s.push('\n');
    s.push_str("body: ");
    s.push_str(truncate_bytes(&item.body, BODY_LIMIT));
    s.push('\n');
    s.push_str("labels: ");
    s.push_str(&item.labels.join(", "));
    if let Some(start) = item.start.as_deref() {
        if !start.is_empty() {
            s.push_str("\nstart: ");
            s.push_str(start);
        }
    }
    if let Some(end) = item.end.as_deref() {
        if !end.is_empty() {
            s.push_str("\nend: ");
            s.push_str(end);
        }
    }
    s
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn resolve_provider(cfg: Option<&str>) -> String {
    if let Some(p) = cfg {
        let p = p.trim();
        if !p.is_empty() {
            return p.to_ascii_lowercase();
        }
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

fn llm_http(url: &str, key: Option<&str>, body: &serde_json::Value) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("paddock/0.1")
        .timeout(LLM_TIMEOUT)
        .build()?;
    let mut req = client.post(url).json(body);
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let resp = req.send()?;
    if !resp.status().is_success() {
        anyhow::bail!("llm http {}", resp.status());
    }
    let v: serde_json::Value = resp.json()?;
    extract_content(&v).ok_or_else(|| anyhow::anyhow!("llm response missing content"))
}

fn extract_content(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return Some(s.to_string());
    }
    if let Some(s) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return Some(s.to_string());
    }
    v.get("response").and_then(|r| r.as_str()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClassifierConfig;

    fn item(title: &str, body: &str) -> Item {
        Item {
            id: 1,
            source_id: "incoming".into(),
            foreign_id: "x".into(),
            title: title.into(),
            body: body.into(),
            href: None,
            start: None,
            end: None,
            thread: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            read: false,
            labels: vec![],
            parts: vec![],
            ..Default::default()
        }
    }

    fn script_cfg(script: &str, label: Option<&str>) -> ClassifierConfig {
        ClassifierConfig {
            id: "s".into(),
            kind: "script".into(),
            script: Some(script.into()),
            label: label.map(|s| s.into()),
            ..Default::default()
        }
    }

    #[test]
    fn script_invoice_hits_and_misses() {
        let cfg = script_cfg(
            r#"if item.title.contains("invoice") { "money" } else { () }"#,
            None,
        );
        let hit = item("please pay invoice 12", "");
        assert_eq!(run_classifier(&cfg, &hit).unwrap(), Some("money".into()));
        let miss = item("hello", "");
        assert_eq!(run_classifier(&cfg, &miss).unwrap(), None);
    }

    #[test]
    fn script_predicate_uses_config_label() {
        let cfg = script_cfg(r#"item.title.contains("someday")"#, Some("later"));
        let hit = item("someday maybe", "");
        assert_eq!(run_classifier(&cfg, &hit).unwrap(), Some("later".into()));
        let miss = item("now", "");
        assert_eq!(run_classifier(&cfg, &miss).unwrap(), None);
    }

    #[test]
    fn script_reads_start_and_labels() {
        let cfg = script_cfg(
            r#"if item.start != "" && item.labels.contains("todo") { "cal" } else { () }"#,
            None,
        );
        let mut it = item("x", "");
        it.start = Some("2026-08-18T12:00:00Z".into());
        it.labels = vec!["todo".into()];
        assert_eq!(run_classifier(&cfg, &it).unwrap(), Some("cal".into()));
    }

    #[test]
    fn script_reads_thread_and_parts() {
        let cfg = script_cfg(
            r#"if item["thread"] == "t1" && item.parts.len() == 2 { "ok" } else { () }"#,
            None,
        );
        let mut it = item("x", "");
        it.thread = Some("t1".into());
        it.parts = vec![
            crate::store::Part {
                id: 1,
                seq: 0,
                kind: crate::store::PartKind::Text,
                mime: "text/plain".into(),
                text: Some("a".into()),
                path: None,
            },
            crate::store::Part {
                id: 2,
                seq: 1,
                kind: crate::store::PartKind::Image,
                mime: "image/png".into(),
                text: None,
                path: Some("parts/1-1.png".into()),
            },
        ];
        assert_eq!(run_classifier(&cfg, &it).unwrap(), Some("ok".into()));
    }

    #[test]
    fn parse_llm_helpers() {
        assert_eq!(parse_llm_token("later\n"), Some("later".into()));
        assert_eq!(parse_llm_token("NONE"), None);
        assert_eq!(parse_llm_token("none"), None);
        assert_eq!(parse_llm_token("foo bar"), Some("foo".into()));
        assert_eq!(parse_llm_token("Hello-World!! extra"), Some("hello-world".into()));
    }

    #[test]
    fn llm_allow_list_rejection() {
        let allow = vec!["later".into(), "todo".into()];
        assert_eq!(
            interpret_llm_reply("later", None, &allow),
            Some("later".into())
        );
        assert_eq!(interpret_llm_reply("money", None, &allow), None);
        assert_eq!(interpret_llm_reply("NONE", None, &allow), None);
    }

    #[test]
    fn llm_yes_no_with_label() {
        assert_eq!(
            interpret_llm_reply("yes", Some("later"), &[]),
            Some("later".into())
        );
        assert_eq!(
            interpret_llm_reply("y", Some("later"), &[]),
            Some("later".into())
        );
        assert_eq!(
            interpret_llm_reply("true", Some("later"), &[]),
            Some("later".into())
        );
        assert_eq!(
            interpret_llm_reply("later", Some("later"), &[]),
            Some("later".into())
        );
        assert_eq!(interpret_llm_reply("no", Some("later"), &[]), None);
        assert_eq!(interpret_llm_reply("nope", Some("later"), &[]), None);
    }

    #[test]
    fn openai_and_ollama_bodies() {
        let oai = build_openai_body("gpt-4o-mini", LLM_SYSTEM, "title: hi");
        assert_eq!(oai["model"], "gpt-4o-mini");
        assert!(oai["messages"].is_array());
        assert_eq!(oai["messages"][0]["role"], "system");
        assert_eq!(oai["messages"][1]["role"], "user");
        let oll = build_ollama_body("llama3.2", LLM_SYSTEM, "title: hi");
        assert_eq!(oll["model"], "llama3.2");
        assert!(oll["messages"].is_array());
        assert_eq!(oll["stream"], false);
    }

    #[test]
    fn llm_fixture_skips_http() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("PADDOCK_LLM_FIXTURE", "later");
        let cfg = ClassifierConfig {
            id: "l".into(),
            kind: "llm".into(),
            labels: vec!["later".into(), "todo".into()],
            ..Default::default()
        };
        let got = run_classifier(&cfg, &item("x", "")).unwrap();
        std::env::remove_var("PADDOCK_LLM_FIXTURE");
        assert_eq!(got, Some("later".into()));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
