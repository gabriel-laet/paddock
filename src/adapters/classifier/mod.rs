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

pub(crate) use super::transport::{post, run};

use crate::kernel::sanitize_label;

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
}
