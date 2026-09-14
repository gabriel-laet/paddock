//! What the kernel needs from the outside world. Adapters implement these;
//! the kernel never names SQLite, files, HTTP, or a config format, and it
//! never builds an adapter: the host resolves them and hands them in.

use anyhow::Result;

use super::inbox::Question;
use super::item::{Draft, Item, Label, NewItem};

/// Thin row for stale cleanup: no body, parts, or actors.
#[derive(Debug, Clone)]
pub struct StaleHint {
    pub id: i64,
    pub source_id: String,
    pub created_at: String,
    pub start: Option<String>,
    pub end: Option<String>,
}

/// Something the kernel learned about an item and wants kept.
#[derive(Debug, Clone, PartialEq)]
pub enum Fact {
    /// Put on, by someone, at some time. Clears any denial of the same name.
    Label(Label),
    /// Taken off. By a hand, this leaves a denial behind.
    Unlabel(Label),
    Thread(Option<String>),
    /// Something that runs once per item has run: a run-once classifier
    /// (its id), or an inbox effect (`then:<path>:<effect>`).
    Seen(String),
    Vector(Vec<f32>),
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
    /// Items that cite this one.
    fn citing(&self, id: i64) -> Result<Vec<Item>>;
    /// The bytes of a non-text part.
    fn blob(&self, part_id: i64) -> Result<Vec<u8>>;
    fn note(&self, id: i64, fact: Fact) -> Result<()>;
    fn delete(&self, id: i64) -> Result<bool>;
    /// Thin rows for the items a question matches, for stale cleanup.
    fn stale(&self, q: &Question) -> Result<Vec<StaleHint>>;
    fn seen(&self, id: i64, key: &str) -> Result<bool>;
    /// Items with no vector yet.
    fn unembedded(&self) -> Result<Vec<i64>>;
    fn counts_by_source(&self) -> Result<Vec<(String, i64)>>;
}

/// Where items come from, and where a draft goes.
pub trait Source {
    fn pull(&self) -> Result<Vec<NewItem>>;
    /// Deliver the draft and return the item as it now exists at the source.
    /// Read-only sources fail with "source cannot send".
    fn send(&self, draft: &Draft, reply_to_foreign: Option<&str>) -> Result<NewItem>;
}

/// Stamps a label, or not. An error means "could not decide".
pub trait Classifier {
    fn id(&self) -> &str;
    fn classify(&self, item: &Item) -> Result<Option<String>>;
    /// Expensive classifiers run once per item and the verdict is remembered.
    fn once(&self) -> bool {
        false
    }
}

/// Text to a vector. Items and queries share one embedder, so one space.
pub trait Embedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}

/// What `answer` hands a model: the question and the items it may draw on.
/// The adapter turns this into a prompt; the kernel holds no prose.
#[derive(Debug, Clone)]
pub struct Brief {
    pub question: String,
    pub items: Vec<Item>,
}

/// A chat model.
pub trait Model {
    /// A system and a user message in, text out.
    fn complete(&self, system: &str, user: &str) -> Result<String>;
    /// Answer from the items, citing them as `#id`.
    fn answer(&self, brief: &Brief) -> Result<String>;
}
