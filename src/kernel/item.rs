//! The item and what hangs off it: parts, actors, and a draft on its way to becoming one.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PartKind {
    #[default]
    Text,
    File,
    Image,
    Audio,
    Video,
}

impl PartKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::File => "file",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "text" => Self::Text,
            "image" => Self::Image,
            "audio" => Self::Audio,
            "video" => Self::Video,
            _ => Self::File,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    #[default]
    Person,
    Group,
    List,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Group => "group",
            Self::List => "list",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "group" => Self::Group,
            "list" => Self::List,
            _ => Self::Person,
        }
    }
}

/// Someone an item is from or to: a person, a group, or a list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Actor {
    pub id: String,
    pub name: Option<String>,
    pub kind: ActorKind,
}

/// One piece of an item's content. Text is inline; anything else is a path the store owns.
#[derive(Debug, Clone, Serialize)]
pub struct Part {
    pub id: i64,
    pub seq: i64,
    pub kind: PartKind,
    pub mime: String,
    pub text: Option<String>,
    pub path: Option<String>,
}

/// A part on its way in: inline text, inline bytes, or a file to copy from.
#[derive(Debug, Clone, Default)]
pub struct NewPart {
    pub kind: PartKind,
    pub mime: String,
    pub text: Option<String>,
    pub bytes: Option<Vec<u8>>,
    pub src: Option<String>,
}

/// One thing that arrived, stripped of its source's shape.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Item {
    pub id: i64,
    pub source_id: String,
    pub foreign_id: String,
    pub title: String,
    /// Preview: the first text part, else what the source sent as body.
    pub body: String,
    pub href: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub thread: Option<String>,
    /// When paddock first admitted it, RFC3339.
    pub created_at: String,
    pub read: bool,
    pub labels: Vec<String>,
    pub parts: Vec<Part>,
    pub from: Option<Actor>,
    pub to: Vec<Actor>,
    pub in_reply_to: Option<i64>,
    pub forward_of: Option<i64>,
    pub cite_excerpt: Option<String>,
    pub cite_actor: Option<Actor>,
}

impl Item {
    /// Everything searchable: title, then every text part (or the body).
    pub fn text(&self) -> String {
        let parts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|p| p.text.as_deref())
            .collect();
        let body = if parts.is_empty() {
            self.body.clone()
        } else {
            parts.join("\n")
        };
        format!("{}\n{}", self.title, body)
    }

    /// The item's own moment: `start` when set, else when it was admitted.
    pub fn when(&self) -> &str {
        self.start
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.created_at)
    }
}

/// An item as a source hands it over, before it has an id.
#[derive(Debug, Clone, Default)]
pub struct NewItem {
    pub source_id: String,
    pub foreign_id: String,
    pub title: String,
    pub body: String,
    pub href: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub thread: Option<String>,
    pub parts: Vec<NewPart>,
    pub from: Option<Actor>,
    pub to: Vec<Actor>,
    /// Foreign id on the same source. Resolved to a local id on admit.
    pub in_reply_to: Option<String>,
    /// Foreign id on the same source. Resolved to a local id on admit.
    pub forward_of: Option<String>,
    pub cite_excerpt: Option<String>,
    pub cite_actor: Option<Actor>,
    /// Read state the source tracks, if any. `None` means the source has no opinion.
    pub read: Option<bool>,
}

/// A compose or reply waiting to become an item.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub thread: Option<String>,
    pub reply_to: Option<i64>,
    pub foreign_id: Option<String>,
    pub parts: Vec<NewPart>,
    pub to: Vec<Actor>,
}
