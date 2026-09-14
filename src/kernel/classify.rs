//! Classifiers stamp labels. The kernel knows one pure kind, regex; the
//! adapters bring the rest (script, llm).

use anyhow::{Context, Result};

use super::inbox::ClassifierSpec;
use super::item::Item;

pub trait Classifier {
    fn id(&self) -> &str;
    /// The label to stamp, or nothing. An error means "could not decide".
    fn classify(&self, item: &Item) -> Result<Option<String>>;
    /// Expensive classifiers run once per item and the verdict is remembered.
    fn once(&self) -> bool {
        false
    }
}

/// Regex over title or body.
pub struct RegexClassifier {
    id: String,
    re: regex::Regex,
    label: String,
}

impl RegexClassifier {
    pub fn new(spec: &ClassifierSpec) -> Result<Self> {
        let pattern = spec
            .pattern
            .as_deref()
            .context("regex classifier needs pattern")?;
        let label = spec.label.clone().context("regex classifier needs label")?;
        let re = regex::Regex::new(pattern)
            .with_context(|| format!("classifier {}: bad pattern", spec.id))?;
        Ok(Self {
            id: spec.id.clone(),
            re,
            label,
        })
    }
}

impl Classifier for RegexClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn classify(&self, item: &Item) -> Result<Option<String>> {
        let hit = self.re.is_match(&item.title) || self.re.is_match(&item.body);
        Ok(hit.then(|| self.label.clone()))
    }
}

/// Kernel-known kinds only. `None` means "ask the adapters".
pub fn build(spec: &ClassifierSpec) -> Result<Option<Box<dyn Classifier>>> {
    Ok(match spec.kind.as_str() {
        "regex" => Some(Box::new(RegexClassifier::new(spec)?)),
        _ => None,
    })
}

/// Run a kernel-known classifier once, for tests and one-offs.
pub fn run_classifier(spec: &ClassifierSpec, item: &Item) -> Result<Option<String>> {
    build(spec)?
        .with_context(|| format!("unknown classifier kind `{}`", spec.kind))?
        .classify(item)
}

/// Lowercase ascii letters, digits, `_` and `-`, at most 40 chars. Else nothing.
pub fn sanitize_label(s: &str) -> Option<String> {
    let out: String = s
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .take(40)
        .collect();
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_sanitized() {
        assert_eq!(sanitize_label("Hello-World!!"), Some("hello-world".into()));
        assert_eq!(sanitize_label("***"), None);
    }

    #[test]
    fn regex_hits_title_or_body() {
        let spec = ClassifierSpec {
            id: "r".into(),
            kind: "regex".into(),
            pattern: Some("(?i)invoice".into()),
            label: Some("money".into()),
            ..Default::default()
        };
        let mut it = Item::default();
        it.body = "your INVOICE".into();
        assert_eq!(run_classifier(&spec, &it).unwrap(), Some("money".into()));
        it.body.clear();
        assert_eq!(run_classifier(&spec, &it).unwrap(), None);
    }
}
