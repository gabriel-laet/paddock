//! A chat model behind the `Model` port: a CLI that reads the prompt on
//! stdin, or Ollama / an OpenAI-compatible service over HTTP. The prompts
//! live here, not in the kernel.
//!
//! ```toml
//! [model]
//! kind = "exec"            # or "ollama" | "openai"
//! cmd = "claude"
//! args = ["-p"]
//! # url = "http://127.0.0.1:11434"
//! # model = "llama3.2"
//! # key = "..."            # bearer token, or key_cmd = "pass show openai"
//! ```

use anyhow::{bail, Result};

use super::transport::{join, post_json, run, secret};
use crate::kernel::{setting, setting_list, AdapterSpec, Brief, Model};

const ANSWER_SYSTEM: &str =
    "You answer a question from someone's inbox using only the items given. \
Cite every item you rely on as #id. If the items do not answer the question, say so plainly.";
const ANSWER_CLIP: usize = 1500;

/// Prompt on stdin, reply on stdout.
pub struct Exec {
    pub cmd: String,
    pub args: Vec<String>,
}

impl Model for Exec {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        let stdin = format!("{system}\n\n{user}");
        run(&self.cmd, &self.args, stdin.as_bytes())
    }

    fn answer(&self, brief: &Brief) -> Result<String> {
        self.complete(ANSWER_SYSTEM, &render(brief))
    }
}

#[derive(Clone, Copy)]
pub enum Shape {
    Ollama,
    OpenAi,
}

/// Ollama `/api/chat` or OpenAI-compatible `/chat/completions`.
pub struct Chat {
    pub shape: Shape,
    pub url: String,
    pub model: String,
    pub key: Option<String>,
}

impl Model for Chat {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        let messages = serde_json::json!([
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]);
        let (url, key, body) = match self.shape {
            Shape::Ollama => (
                join(&self.url, "api/chat"),
                None,
                serde_json::json!({ "model": self.model, "stream": false, "messages": messages }),
            ),
            Shape::OpenAi => (
                join(&self.url, "chat/completions"),
                self.key.clone(),
                serde_json::json!({ "model": self.model, "messages": messages }),
            ),
        };
        let v = post_json(&url, key.as_deref(), &body)?;
        content(&v).ok_or_else(|| anyhow::anyhow!("model reply has no content"))
    }

    fn answer(&self, brief: &Brief) -> Result<String> {
        self.complete(ANSWER_SYSTEM, &render(brief))
    }
}

/// The user message for a brief: the question, then every item with its id.
fn render(brief: &Brief) -> String {
    let mut s = format!("question: {}\n\nitems:\n", brief.question);
    for it in &brief.items {
        let from = it
            .from
            .as_ref()
            .map(|a| a.name.clone().unwrap_or_else(|| a.id.clone()))
            .unwrap_or_default();
        s.push_str(&format!(
            "\n### #{} {}\nfrom: {from}  when: {}  source: {}\n{}\n",
            it.id,
            it.title,
            it.when(),
            it.source_id,
            clip(&it.text(), ANSWER_CLIP)
        ));
    }
    s
}

fn clip(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
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

/// `kind` picks the transport. Missing kind means exec when `cmd` is set,
/// else openai when `key` is set, else ollama.
pub fn build(spec: &AdapterSpec) -> Result<Box<dyn Model>> {
    let s = &spec.settings;
    let cmd = setting(s, "cmd");
    let key = secret(s, "key")?;
    let kind = match spec.kind.trim().to_ascii_lowercase().as_str() {
        "" if cmd.is_some() => "exec".to_string(),
        "" if key.is_some() => "openai".to_string(),
        "" => "ollama".to_string(),
        k => k.to_string(),
    };
    let url = setting(s, "url");
    let model = setting(s, "model");
    Ok(match kind.as_str() {
        "exec" => {
            let Some(cmd) = cmd else {
                bail!("model exec needs cmd")
            };
            Box::new(Exec {
                cmd,
                args: setting_list(s, "args"),
            })
        }
        "ollama" => Box::new(Chat {
            shape: Shape::Ollama,
            url: url.unwrap_or_else(|| "http://127.0.0.1:11434".into()),
            model: model.unwrap_or_else(|| "llama3.2".into()),
            key: None,
        }),
        "openai" => Box::new(Chat {
            shape: Shape::OpenAi,
            url: url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            model: model.unwrap_or_else(|| "gpt-4o-mini".into()),
            key,
        }),
        other => bail!("unknown model kind `{other}` (exec, ollama, openai)"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_gets_system_and_user_on_stdin() {
        let m = Exec {
            cmd: "sh".into(),
            args: vec!["-c".into(), "grep -c . ".into()],
        };
        assert_eq!(m.complete("sys", "usr").unwrap().trim(), "2");
    }

    #[test]
    fn brief_renders_every_item_with_its_id() {
        let mut it = crate::kernel::Item::default();
        it.id = 7;
        it.title = "Invoice".into();
        it.body = "pay".into();
        let s = render(&Brief {
            question: "what?".into(),
            items: vec![it],
        });
        assert!(s.contains("question: what?"));
        assert!(s.contains("### #7 Invoice"));
        assert!(s.contains("pay"));
    }

    #[test]
    fn content_reads_every_shape() {
        let oai = serde_json::json!({"choices":[{"message":{"content":"a"}}]});
        let oll = serde_json::json!({"message":{"content":"b"}});
        let gen = serde_json::json!({"response":"c"});
        assert_eq!(content(&oai).as_deref(), Some("a"));
        assert_eq!(content(&oll).as_deref(), Some("b"));
        assert_eq!(content(&gen).as_deref(), Some("c"));
    }

    #[test]
    fn kind_defaults_to_exec_when_cmd_is_set() {
        let spec = AdapterSpec {
            settings: [("cmd".to_string(), serde_json::json!("true"))]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert!(build(&spec).is_ok());
        assert!(build(&AdapterSpec {
            kind: "weird".into(),
            ..Default::default()
        })
        .is_err());
    }
}
