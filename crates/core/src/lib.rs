//! The four nouns.
//!
//! An [`Item`] is the atom. A [`Source`] admits items. A [`Label`] is a mark
//! an organizer put on an item. An [`Inbox`] is a named question over the pile.

use std::fmt;

/// One thing that arrived, stripped of source shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    pub source: SourceId,
    pub title: String,
    pub body: String,
    pub labels: Vec<Label>,
}

/// Stable id inside a source.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemId(pub String);

/// Plugin that admits items.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(pub String);

/// A mark an organizer put on an item.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub String);

/// A named question over the pile: labels + sources + sort.
///
/// Not an account. Not a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inbox {
    pub name: String,
    pub labels: Vec<Label>,
    pub sources: Vec<SourceId>,
}

/// A plugin that yields items.
pub trait Source {
    fn id(&self) -> &SourceId;
    fn pull(&mut self) -> Result<Vec<Item>, SourceError>;
}

/// A plugin that marks items. Heuristics and LLMs are both organizers.
pub trait Organizer {
    fn organize(&self, item: &Item) -> Vec<Label>;
}

#[derive(Debug)]
pub struct SourceError(pub String);

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SourceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_is_a_question_not_an_account() {
        let inbox = Inbox {
            name: "later".into(),
            labels: vec![Label("later".into())],
            sources: vec![],
        };
        assert_eq!(inbox.name, "later");
    }
}
