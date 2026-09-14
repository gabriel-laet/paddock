//! What the kernel needs from the outside world. Adapters implement these;
//! the kernel never names SQLite, files, HTTP, or a config format.

use anyhow::Result;

use super::classify::Classifier;
use super::inbox::{ClassifierSpec, Question, SourceSpec};
use super::item::{Draft, Item, NewItem};

/// Thin row for stale cleanup: no body, parts, or actors.
#[derive(Debug, Clone)]
pub struct StaleHint {
    pub id: i64,
    pub source_id: String,
    pub created_at: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub labels: Vec<String>,
}

/// Where items live.
pub trait Store {
    /// Insert, or refresh the row with the same (source_id, foreign_id).
    /// Returns the id and whether it was created.
    fn upsert(&self, item: &NewItem) -> Result<(i64, bool)>;
    fn get(&self, id: i64) -> Result<Item>;
    fn find(&self, source_id: &str, foreign_id: &str) -> Result<Option<i64>>;
    fn ask(&self, q: &Question) -> Result<Vec<Item>>;
    fn count(&self, q: &Question) -> Result<usize>;
    fn thread(&self, thread: &str) -> Result<Vec<Item>>;
    fn set_thread(&self, id: i64, thread: Option<&str>) -> Result<()>;
    fn set_read(&self, id: i64, read: bool) -> Result<()>;
    fn add_label(&self, id: i64, label: &str) -> Result<()>;
    fn remove_label(&self, id: i64, label: &str) -> Result<()>;
    fn delete(&self, id: i64) -> Result<bool>;
    fn stale(&self) -> Result<Vec<StaleHint>>;
    /// Has a run-once classifier already seen this item?
    fn classified(&self, id: i64, classifier_id: &str) -> Result<bool>;
    fn mark_classified(&self, id: i64, classifier_id: &str) -> Result<()>;
    fn counts_by_source(&self) -> Result<Vec<(String, i64)>>;
}

/// Where items come from, and where a draft goes.
pub trait Source {
    fn pull(&self) -> Result<Vec<NewItem>>;
    /// Deliver the draft and return the item as it now exists at the source.
    /// Read-only sources fail with "source cannot send".
    fn send(&self, draft: &Draft, reply_to_foreign: Option<&str>) -> Result<NewItem>;
}

/// Turns specs into live sources and classifiers. The kernel builds the
/// classifier kinds it knows (regex, script) and asks here for the rest.
pub trait Adapters {
    fn source(&self, spec: &SourceSpec) -> Result<Box<dyn Source>>;
    fn classifier(&self, spec: &ClassifierSpec) -> Result<Box<dyn Classifier>>;
}
