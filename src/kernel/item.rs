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

/// Who put a label on an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Hand,
    Classifier(String),
}

impl By {
    /// `hand`, or `classifier:<id>`.
    pub fn as_str(&self) -> String {
        match self {
            By::Hand => "hand".into(),
            By::Classifier(id) => format!("classifier:{id}"),
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.strip_prefix("classifier:") {
            Some(id) => By::Classifier(id.to_string()),
            None => By::Hand,
        }
    }
}

/// A label on an item, and how it got there. A hand outranks a classifier:
/// a label a hand removed stays denied, and no classifier puts it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Label {
    pub name: String,
    pub by: By,
    /// RFC3339, or empty when unknown.
    pub at: String,
}

impl Label {
    pub fn hand(name: &str) -> Self {
        Self {
            name: name.into(),
            by: By::Hand,
            at: String::new(),
        }
    }
}

/// How one item points at another thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CiteKind {
    Reply,
    Forward,
    Quote,
    #[default]
    Mention,
    Attach,
}

impl CiteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Forward => "forward",
            Self::Quote => "quote",
            Self::Mention => "mention",
            Self::Attach => "attach",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "reply" => Self::Reply,
            "forward" => Self::Forward,
            "quote" => Self::Quote,
            "attach" => Self::Attach,
            _ => Self::Mention,
        }
    }
}

/// One item pointing at another thing: an item in the pile (by the source's
/// name for it, resolved to our `id` once it is here, early or late), or
/// something outside it by `href`. One shape for replies, forwards, quotes,
/// mentions, and attachments.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Cite {
    pub kind: CiteKind,
    /// The cited item's source. None means the citing item's own.
    pub source_id: Option<String>,
    /// How that source names the cited item.
    pub foreign_id: Option<String>,
    /// Our id for it, once it is in the pile.
    pub id: Option<i64>,
    /// Something outside the pile: a URL, a path.
    pub href: Option<String>,
    pub excerpt: Option<String>,
    pub actor: Option<Actor>,
}

impl Cite {
    /// A cite to an item of the same source.
    pub fn to(kind: CiteKind, foreign_id: &str) -> Self {
        Self {
            kind,
            foreign_id: Some(foreign_id.into()),
            ..Default::default()
        }
    }

    pub fn reply(foreign_id: &str) -> Self {
        Self::to(CiteKind::Reply, foreign_id)
    }

    pub fn forward(foreign_id: &str) -> Self {
        Self::to(CiteKind::Forward, foreign_id)
    }
}

/// One piece of an item's content. Text is inline; anything else is bytes
/// the store keeps, `size` long, read back with `Store::blob`.
#[derive(Debug, Clone, Serialize)]
pub struct Part {
    pub id: i64,
    pub seq: i64,
    pub kind: PartKind,
    pub mime: String,
    pub text: Option<String>,
    pub size: Option<i64>,
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
    pub labels: Vec<Label>,
    /// Labels a hand removed. A classifier may not stamp these again.
    pub denied: Vec<Label>,
    pub parts: Vec<Part>,
    pub from: Option<Actor>,
    pub to: Vec<Actor>,
    pub cites: Vec<Cite>,
}

impl Item {
    /// The item this one replies to, when that item is in the pile.
    pub fn reply_to(&self) -> Option<i64> {
        self.cites
            .iter()
            .find(|c| c.kind == CiteKind::Reply)
            .and_then(|c| c.id)
    }

    pub fn has(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l.name == label)
    }

    pub fn denies(&self, label: &str) -> bool {
        self.denied.iter().any(|l| l.name == label)
    }

    /// Label names, in store order.
    pub fn label_names(&self) -> Vec<String> {
        self.labels.iter().map(|l| l.name.clone()).collect()
    }

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
    /// Resolved to local ids on admit; a cite to an item not here yet
    /// resolves when that item arrives.
    pub cites: Vec<Cite>,
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
