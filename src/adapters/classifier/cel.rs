//! A classifier written as a CEL expression over `item`. CEL cannot loop or
//! do IO, so a script can be wrong but never hang.
//!
//! The expression yields a label (a string), `true` to use the spec's
//! `label`, or anything else for nothing:
//!
//! ```text
//! item.title.contains("invoice") ? "money" : ""
//! "todo" in item.labels && item.start != ""
//! ```

use anyhow::Result;
use cel_interpreter::{Program, Value};

use crate::kernel::{sanitize_label, Classifier, ClassifierSpec, Item};

pub struct CelClassifier {
    id: String,
    program: Program,
    label: Option<String>,
}

impl CelClassifier {
    pub fn new(spec: &ClassifierSpec, source: &str) -> Result<Self> {
        let program = Program::compile(source)
            .map_err(|e| anyhow::anyhow!("classifier {}: bad script: {e}", spec.id))?;
        Ok(Self {
            id: spec.id.clone(),
            program,
            label: spec.label.clone(),
        })
    }
}

impl Classifier for CelClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn classify(&self, item: &Item) -> Result<Option<String>> {
        let mut ctx = cel_interpreter::Context::default();
        ctx.add_variable("item", item_value(item))
            .map_err(|e| anyhow::anyhow!("classifier {}: {e}", self.id))?;
        let value = self
            .program
            .execute(&ctx)
            .map_err(|e| anyhow::anyhow!("classifier {}: {e}", self.id))?;
        Ok(match value {
            Value::String(s) => sanitize_label(&s),
            Value::Bool(true) => self.label.as_deref().and_then(sanitize_label),
            _ => None,
        })
    }
}

/// What a script sees. Absent strings are `""`, so `item.start != ""` reads naturally.
fn item_value(item: &Item) -> serde_json::Value {
    let s = |v: &Option<String>| v.clone().unwrap_or_default();
    let actor = |a: &crate::kernel::Actor| serde_json::json!({ "id": a.id, "name": s(&a.name), "kind": a.kind.as_str() });
    serde_json::json!({
        "id": item.id,
        "source": item.source_id,
        "title": item.title,
        "body": item.body,
        "href": s(&item.href),
        "start": s(&item.start),
        "end": s(&item.end),
        "thread": s(&item.thread),
        "read": item.read,
        "labels": item.labels,
        "parts": item.parts.iter().map(|p| p.kind.as_str()).collect::<Vec<_>>(),
        "from": item.from.as_ref().map(actor).unwrap_or(serde_json::Value::Null),
        "to": item.to.iter().map(actor).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Part, PartKind};

    fn item(title: &str, body: &str) -> Item {
        Item {
            id: 1,
            source_id: "incoming".into(),
            title: title.into(),
            body: body.into(),
            ..Default::default()
        }
    }

    fn run(script: &str, label: Option<&str>, it: &Item) -> Option<String> {
        let spec = ClassifierSpec {
            id: "s".into(),
            kind: "script".into(),
            label: label.map(str::to_string),
            ..Default::default()
        };
        CelClassifier::new(&spec, script)
            .unwrap()
            .classify(it)
            .unwrap()
    }

    #[test]
    fn string_result_is_the_label() {
        let script = r#"item.title.contains("invoice") ? "money" : """#;
        assert_eq!(
            run(script, None, &item("pay invoice 12", "")),
            Some("money".into())
        );
        assert_eq!(run(script, None, &item("hello", "")), None);
    }

    #[test]
    fn true_uses_spec_label() {
        let script = r#"item.title.contains("someday")"#;
        assert_eq!(
            run(script, Some("later"), &item("someday maybe", "")),
            Some("later".into())
        );
        assert_eq!(run(script, Some("later"), &item("now", "")), None);
    }

    #[test]
    fn sees_start_labels_thread_and_parts() {
        let mut it = item("x", "");
        it.start = Some("2026-08-18T12:00:00Z".into());
        it.labels = vec!["todo".into()];
        it.thread = Some("t1".into());
        it.parts = vec![
            Part {
                id: 1,
                seq: 0,
                kind: PartKind::Text,
                mime: "text/plain".into(),
                text: Some("a".into()),
                size: None,
            },
            Part {
                id: 2,
                seq: 1,
                kind: PartKind::Image,
                mime: "image/png".into(),
                text: None,
                size: Some(3),
            },
        ];
        assert_eq!(
            run(
                r#"item.start != "" && "todo" in item.labels ? "cal" : """#,
                None,
                &it
            ),
            Some("cal".into())
        );
        assert_eq!(
            run(
                r#"item.thread == "t1" && size(item.parts) == 2 && "image" in item.parts"#,
                Some("ok"),
                &it
            ),
            Some("ok".into())
        );
    }

    #[test]
    fn bad_script_fails_at_build() {
        let spec = ClassifierSpec {
            id: "s".into(),
            kind: "script".into(),
            ..Default::default()
        };
        assert!(CelClassifier::new(&spec, "item.title ?").is_err());
    }
}
