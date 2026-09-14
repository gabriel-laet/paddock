//! A classifier that is a program. The item goes in as JSON on stdin, the
//! label comes out on stdout. What the program does in between is its business.
//!
//! ```toml
//! [[inbox.classifier]]
//! id = "by-script"
//! kind = "exec"
//! cmd = "~/bin/label-it"
//! args = ["--strict"]
//! once = true            # remember the verdict per item
//! ```

use anyhow::{Context, Result};

use super::{label_of, run};
use crate::kernel::{Classifier, ClassifierSpec, Item};

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
            args: spec.args.clone(),
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
        let reply = run(&self.cmd, &self.args, &stdin)
            .with_context(|| format!("classifier {}", self.id))?;
        Ok(label_of(&reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(script: &str) -> (ClassifierSpec, String) {
        let spec = ClassifierSpec {
            id: "x".into(),
            kind: "exec".into(),
            args: vec!["-c".into(), script.into()],
            ..Default::default()
        };
        (spec, "sh".into())
    }

    #[test]
    fn program_reads_item_json_and_answers() {
        let (spec, cmd) = spec(r#"grep -q '"title":"pay invoice"' && echo money || echo NONE"#);
        let c = ExecClassifier::new(&spec, cmd);
        let mut it = Item::default();
        it.title = "pay invoice".into();
        assert_eq!(c.classify(&it).unwrap(), Some("money".into()));
        it.title = "hello".into();
        assert_eq!(c.classify(&it).unwrap(), None);
    }

    #[test]
    fn failure_is_an_error_not_a_verdict() {
        let (spec, cmd) = spec("exit 1");
        let c = ExecClassifier::new(&spec, cmd);
        assert!(c.classify(&Item::default()).is_err());
    }
}
