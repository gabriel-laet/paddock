//! The pure core. Four nouns (item, source, label, inbox), the questions
//! inboxes ask, the verbs, and the ports the verbs run through. Nothing in
//! here touches a database, a file, a network, a clock, or a config format;
//! the host resolves all of that and hands it in.

pub mod classify;
pub mod inbox;
pub mod item;
pub mod ports;
pub mod verbs;

pub use classify::{run_classifier, sanitize_label, RegexClassifier};
pub use inbox::{
    parse_duration, parse_when, rfc3339, setting, setting_list, AdapterSpec, ClassifierSpec,
    Config, Inbox, Node, Question, Settings, SourceSpec,
};
pub use item::{
    Actor, ActorKind, By, Cite, CiteKind, Draft, Item, Label, NewItem, NewPart, Part, PartKind,
};
pub use ports::{Brief, Classifier, Embedder, Fact, Model, Source, StaleHint, Store};
pub use verbs::{reply_title, Admitted, Answer, Kernel, Report, Why};
