//! Text to vector, behind the `Embedder` port. Items and queries must go
//! through the same embedder to share a space.
//!
//! ```toml
//! [embedder]
//! kind = "ollama"          # or "openai" | "http" | "exec"
//! model = "nomic-embed-text"
//! ```
//!
//! `exec` gets the text on stdin and prints a JSON array of numbers.
//! `http` POSTs `{"text": ..., "model"?: ...}` and reads a JSON array, or an
//! object with `embedding`, `vector`, or `data[0].embedding`.

use anyhow::{bail, Context, Result};

use super::transport::{join, post_json, run, secret};
use crate::kernel::{setting, setting_list, AdapterSpec, Embedder};

pub struct Exec {
    pub cmd: String,
    pub args: Vec<String>,
}

impl Embedder for Exec {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let out = run(&self.cmd, &self.args, text.as_bytes())?;
        let v: serde_json::Value =
            serde_json::from_str(out.trim()).context("embedder output is not JSON")?;
        vector(&v).ok_or_else(|| anyhow::anyhow!("embedder output has no vector"))
    }
}

#[derive(Clone, Copy)]
pub enum Shape {
    Plain,
    Ollama,
    OpenAi,
}

pub struct Http {
    pub shape: Shape,
    pub url: String,
    pub model: Option<String>,
    pub key: Option<String>,
}

impl Embedder for Http {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let (url, key, body) = match self.shape {
            Shape::Plain => (
                self.url.clone(),
                self.key.clone(),
                serde_json::json!({ "text": text, "model": self.model }),
            ),
            Shape::Ollama => (
                join(&self.url, "api/embeddings"),
                None,
                serde_json::json!({ "model": self.model, "prompt": text }),
            ),
            Shape::OpenAi => (
                join(&self.url, "embeddings"),
                self.key.clone(),
                serde_json::json!({ "model": self.model, "input": text }),
            ),
        };
        let v = post_json(&url, key.as_deref(), &body)?;
        vector(&v).ok_or_else(|| anyhow::anyhow!("embedder reply has no vector"))
    }
}

/// Model2Vec static embeddings, in-process. Loaded once when the kernel is
/// resolved; `model` is a folder holding `model.safetensors`,
/// `tokenizer.json`, and `config.json` (or a Hugging Face repo id, which
/// downloads on first use).
#[cfg(feature = "local")]
pub struct Local {
    model: model2vec_rs::model::StaticModel,
}

#[cfg(feature = "local")]
impl Local {
    pub fn load(path: &str) -> Result<Self> {
        let model = model2vec_rs::model::StaticModel::from_pretrained(path, None, None, None)
            .map_err(|e| anyhow::anyhow!("load model2vec `{path}`: {e}"))?;
        Ok(Self { model })
    }
}

#[cfg(feature = "local")]
impl Embedder for Local {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.model
            .encode(&[text.to_string()])
            .into_iter()
            .next()
            .filter(|v| !v.is_empty())
            .context("model2vec returned no vector")
    }
}

/// A bare array, or `embedding`, `vector`, or `data[0].embedding`.
fn vector(v: &serde_json::Value) -> Option<Vec<f32>> {
    let arr = v
        .as_array()
        .or_else(|| v.get("embedding").and_then(|e| e.as_array()))
        .or_else(|| v.get("vector").and_then(|e| e.as_array()))
        .or_else(|| v.pointer("/data/0/embedding").and_then(|e| e.as_array()))?;
    let out: Vec<f32> = arr
        .iter()
        .filter_map(|x| x.as_f64())
        .map(|x| x as f32)
        .collect();
    (!out.is_empty() && out.len() == arr.len()).then_some(out)
}

pub fn build(spec: &AdapterSpec) -> Result<Box<dyn Embedder>> {
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
                bail!("embedder exec needs cmd")
            };
            Box::new(Exec {
                cmd,
                args: setting_list(s, "args"),
            })
        }
        "http" => {
            let Some(url) = url else {
                bail!("embedder http needs url")
            };
            Box::new(Http {
                shape: Shape::Plain,
                url,
                model,
                key,
            })
        }
        "ollama" => Box::new(Http {
            shape: Shape::Ollama,
            url: url.unwrap_or_else(|| "http://127.0.0.1:11434".into()),
            model: Some(model.unwrap_or_else(|| "nomic-embed-text".into())),
            key: None,
        }),
        "openai" => Box::new(Http {
            shape: Shape::OpenAi,
            url: url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            model: Some(model.unwrap_or_else(|| "text-embedding-3-small".into())),
            key,
        }),
        #[cfg(feature = "local")]
        "local" => {
            let Some(model) = model else {
                bail!("embedder local needs model (a folder with model.safetensors)")
            };
            Box::new(Local::load(
                &super::host::expand_path(&model).display().to_string(),
            )?)
        }
        #[cfg(not(feature = "local"))]
        "local" => bail!("embedder kind `local` needs a build with `--features local`"),
        other => bail!("unknown embedder kind `{other}` (exec, http, ollama, openai, local)"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_reads_every_shape() {
        let bare = serde_json::json!([1.0, 2.0]);
        let oll = serde_json::json!({"embedding": [0.5]});
        let oai = serde_json::json!({"data": [{"embedding": [1, 2, 3]}]});
        let bad = serde_json::json!({"embedding": ["x"]});
        assert_eq!(vector(&bare), Some(vec![1.0, 2.0]));
        assert_eq!(vector(&oll), Some(vec![0.5]));
        assert_eq!(vector(&oai), Some(vec![1.0, 2.0, 3.0]));
        assert_eq!(vector(&bad), None);
    }

    #[test]
    fn local_kind_explains_itself() {
        let spec = AdapterSpec {
            kind: "local".into(),
            ..Default::default()
        };
        let err = build(&spec).err().expect("no model, or no feature");
        let msg = err.to_string();
        assert!(msg.contains("local"), "{msg}");
    }

    #[test]
    fn exec_embeds_over_stdin() {
        let e = Exec {
            cmd: "sh".into(),
            args: vec![
                "-c".into(),
                r#"grep -q money && echo '[1,0]' || echo '[0,1]'"#.into(),
            ],
        };
        assert_eq!(e.embed("send money").unwrap(), vec![1.0, 0.0]);
        assert_eq!(e.embed("lunch").unwrap(), vec![0.0, 1.0]);
    }
}
