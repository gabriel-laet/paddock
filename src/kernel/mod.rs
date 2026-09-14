//! The pure core. Four nouns (item, source, label, inbox), the questions
//! inboxes ask, the classifiers that stamp labels, and the verbs. Nothing in
//! here touches a database, a file, a network, or a config format; those come
//! in through `ports`.

pub mod classify;
pub mod inbox;
pub mod item;
pub mod ports;
pub mod verbs;

pub use classify::{run_classifier, sanitize_label, Classifier};
pub use inbox::{
    parse_duration, parse_when, rfc3339, ClassifierSpec, Config, Inbox, Node, Question, SourceSpec,
};
pub use item::{Actor, ActorKind, Draft, Item, NewItem, NewPart, Part, PartKind};
pub use ports::{Adapters, Source, StaleHint, Store};
pub use verbs::{reply_title, Kernel};
