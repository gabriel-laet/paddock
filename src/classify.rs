use anyhow::{bail, Context, Result};

use crate::config::ClassifierConfig;
use crate::store::Item;

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

/// script / llm — not implemented. Present so config can name them.
pub struct StubClassifier {
    id: String,
}

impl Classifier for StubClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn classify(&self, _item: &Item) -> Option<String> {
        None
    }
}

pub fn build_classifier(cfg: &ClassifierConfig) -> Result<Box<dyn Classifier>> {
    match cfg.kind.as_str() {
        "regex" => Ok(Box::new(RegexClassifier::new(cfg)?)),
        "script" | "llm" => Ok(Box::new(StubClassifier { id: cfg.id.clone() })),
        other => bail!("unknown classifier kind `{other}` (regex, script, llm)"),
    }
}

pub fn run_classifier(cfg: &ClassifierConfig, item: &Item) -> Result<Option<String>> {
    Ok(build_classifier(cfg)?.classify(item))
}
