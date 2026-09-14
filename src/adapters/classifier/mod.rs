//! Classifiers beyond the kernel's regex. Two primitives that do not care
//! what is on the other side, and two conveniences built on them:
//!
//! - `exec`: run a program with the item as JSON on stdin; stdout is the label
//! - `http`: POST the item as JSON; the body is the label
//! - `script`: a CEL expression over the item, in-process
//! - `llm`: a prompt built from the item, sent over exec or http, one token back
//!
//! A label reply is its first token; `NONE` or nothing means no label.
//! A JSON object reply may say `{"label": "..."}` instead.

pub mod cel;
pub mod exec;
pub mod http;
pub mod llm;

pub use cel::CelClassifier;
pub use exec::ExecClassifier;
pub use http::HttpClassifier;
pub use llm::LlmClassifier;

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::kernel::sanitize_label;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `{cmd} {args...}` with `stdin`, return stdout. Non-zero exit is an error.
pub(crate) fn run(cmd: &str, args: &[String], stdin: &[u8]) -> Result<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run `{cmd}`"))?;
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(stdin)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "`{cmd}` exited {}: {}",
            out.status.code().unwrap_or(-1),
            err.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// POST a JSON body, return the response body as text.
pub(crate) fn post(url: &str, bearer: Option<&str>, body: &serde_json::Value) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("paddock/0.1")
        .timeout(HTTP_TIMEOUT)
        .build()?;
    let mut req = client.post(url).json(body);
    if let Some(k) = bearer {
        req = req.bearer_auth(k);
    }
    let resp = req.send().with_context(|| format!("post {url}"))?;
    if !resp.status().is_success() {
        bail!("http {} from {url}", resp.status());
    }
    Ok(resp.text()?)
}

/// A reply's label: `{"label": "x"}` if it is JSON, else the first token.
/// `NONE`, empty, or JSON null means no label.
pub(crate) fn label_of(reply: &str) -> Option<String> {
    let reply = reply.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(reply) {
        return v
            .get("label")
            .and_then(|l| l.as_str())
            .and_then(sanitize_label);
    }
    let token = reply.split_whitespace().next().unwrap_or("");
    if token.is_empty() || token.eq_ignore_ascii_case("none") {
        return None;
    }
    sanitize_label(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_of_text_and_json() {
        assert_eq!(label_of("later\n"), Some("later".into()));
        assert_eq!(label_of("NONE"), None);
        assert_eq!(label_of(""), None);
        assert_eq!(label_of("foo bar"), Some("foo".into()));
        assert_eq!(label_of(r#"{"label": "Todo"}"#), Some("todo".into()));
        assert_eq!(label_of(r#"{"label": null}"#), None);
        assert_eq!(label_of(r#"{"other": 1}"#), None);
    }

    #[test]
    fn run_pipes_stdin_and_fails_loudly() {
        let out = run("sh", &["-c".into(), "cat".into()], b"hello").unwrap();
        assert_eq!(out, "hello");
        let err = run("sh", &["-c".into(), "echo bad >&2; exit 3".into()], b"").unwrap_err();
        assert!(err.to_string().contains("exited 3"), "{err}");
        assert!(run("paddock-no-such-cmd", &[], b"").is_err());
    }
}
