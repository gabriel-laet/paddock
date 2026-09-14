//! A classifier behind a URL. The item is POSTed as JSON; the response body is
//! the label, as text or as `{"label": "..."}`.
//!
//! ```toml
//! [[inbox.classifier]]
//! id = "by-service"
//! kind = "http"
//! url = "http://127.0.0.1:8080/label"
//! once = true
//! ```
//!
//! `key` is sent as a bearer token when set.

use anyhow::{Context, Result};

use super::{label_of, post};
use crate::kernel::{Classifier, ClassifierSpec, Item};

pub struct HttpClassifier {
    id: String,
    url: String,
    key: Option<String>,
    once: bool,
}

impl HttpClassifier {
    pub fn new(spec: &ClassifierSpec, url: String) -> Self {
        Self {
            id: spec.id.clone(),
            url,
            key: spec.key.clone(),
            once: spec.once,
        }
    }
}

impl Classifier for HttpClassifier {
    fn id(&self) -> &str {
        &self.id
    }

    fn once(&self) -> bool {
        self.once
    }

    fn classify(&self, item: &Item) -> Result<Option<String>> {
        let body = serde_json::to_value(item)?;
        let reply = post(&self.url, self.key.as_deref(), &body)
            .with_context(|| format!("classifier {}", self.id))?;
        Ok(label_of(&reply))
    }
}
