//! A chat model behind the `Model` port: a CLI that reads the prompt on
//! stdin, or Ollama / an OpenAI-compatible service over HTTP.
//!
//! ```toml
//! [model]
//! kind = "exec"            # or "ollama" | "openai"
//! cmd = "claude"
//! args = ["-p"]
//! # url = "http://127.0.0.1:11434"
//! # model = "llama3.2"
//! # key = "..."            # bearer token for openai-compatible services
//! ```

use anyhow::{bail, Result};

use super::transport::{join, post_json, run};
use crate::kernel::{Model, ModelSpec};

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
pub fn build(spec: &ModelSpec) -> Result<Box<dyn Model>> {
    let cmd = spec.cmd.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let kind = match spec.kind.trim().to_ascii_lowercase().as_str() {
        "" if cmd.is_some() => "exec".to_string(),
        "" if spec.key.is_some() => "openai".to_string(),
        "" => "ollama".to_string(),
        k => k.to_string(),
    };
    let url = spec.url.clone();
    let model = spec.model.clone();
    Ok(match kind.as_str() {
        "exec" => {
            let Some(cmd) = cmd else {
                bail!("model exec needs cmd")
            };
            Box::new(Exec {
                cmd: cmd.to_string(),
                args: spec.args.clone(),
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
            key: spec.key.clone(),
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
        let spec = ModelSpec {
            cmd: Some("true".into()),
            ..Default::default()
        };
        assert!(build(&spec).is_ok());
        assert!(build(&ModelSpec {
            kind: "weird".into(),
            ..Default::default()
        })
        .is_err());
    }
}
