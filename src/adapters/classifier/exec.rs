//! A classifier that is a program. The item goes in as JSON on stdin, the
//! label comes out on stdout. What the program does in between is its business.
//!
//! ```toml
//! [[inbox.classifier]]
//! id = "by-program"
//! kind = "exec"
//! cmd = "~/bin/label-it"
//! args = ["--strict"]
//! once = true            # remember the verdict per item
//! ```

use anyhow::Result;

use super::label_of;
use crate::adapters::transport::run;
use crate::kernel::{setting_list, Classifier, ClassifierSpec, Item};

pub struct ExecClassifier {
    id: String,
    cmd: String,
    args: Vec<String>,
    once: bool,
}

impl ExecClassifier {
    pub fn new(spec: &ClassifierSpec, cmd: String) -> Self {
        Self {
            id: spec.id.clone(),
            cmd,
            args: setting_list(&spec.settings, "args"),
            once: spec.once,
        }
    }
}

impl Classifier for ExecClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn once(&self) -> bool {
        self.once
    }

    fn classify(&self, item: &Item) -> Result<Option<String>> {
        let stdin = serde_json::to_vec(item)?;
        let reply = run(&self.cmd, &self.args, &stdin)?;
        Ok(label_of(&reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier(script: &str) -> ExecClassifier {
        let spec = ClassifierSpec {
            id: "x".into(),
            kind: "exec".into(),
            settings: [("args".to_string(), serde_json::json!(["-c", script]))]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        ExecClassifier::new(&spec, "sh".into())
    }

    #[test]
    fn program_reads_item_json_and_answers() {
        let c = classifier(r#"grep -q '"title":"pay invoice"' && echo money || echo NONE"#);
        let mut it = Item::default();
        it.title = "pay invoice".into();
        assert_eq!(c.classify(&it).unwrap(), Some("money".into()));
        it.title = "hello".into();
        assert_eq!(c.classify(&it).unwrap(), None);
    }

    #[test]
    fn failure_is_an_error_not_a_verdict() {
        assert!(classifier("exit 1").classify(&Item::default()).is_err());
    }
}
