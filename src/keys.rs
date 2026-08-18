//! Key sequences and colon commands — single source of truth.
//!
//! TUI: KeyEvent → `feed` → Verb → `run_verb`.
//! Web: `bindings_json()` is embedded; JS matches the same tables.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    Down,
    Up,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    PaneTree,
    PaneItems,
    SwapPane,
    OpenRead,
    NextInbox,
    PrevInbox,
    Search,
    SearchNext,
    SearchPrev,
    Command,
    Help,
    Quit,
    Escape,
    LabelPrompt,
    ToggleRead,
    Unread,
    Eat,
    Relabel { label: String },
    Bury,
    Todo,
    Again,
    Why,
    Yank,
    Open,
    New { title: String },
    Compose,
    Reply,
    Send {
        title: String,
        body: String,
        reply_to: Option<i64>,
    },
    Which,
    Db,
    Only,
    Spill,
    Pull,
    Theme { name: Option<String> },
    Themes,
}

impl Verb {
    pub fn id(&self) -> &'static str {
        match self {
            Verb::Down => "down",
            Verb::Up => "up",
            Verb::Top => "top",
            Verb::Bottom => "bottom",
            Verb::HalfPageDown => "half-page-down",
            Verb::HalfPageUp => "half-page-up",
            Verb::PageDown => "page-down",
            Verb::PageUp => "page-up",
            Verb::PaneTree => "pane-tree",
            Verb::PaneItems => "pane-items",
            Verb::SwapPane => "swap-pane",
            Verb::OpenRead => "open-read",
            Verb::NextInbox => "next-inbox",
            Verb::PrevInbox => "prev-inbox",
            Verb::Search => "search",
            Verb::SearchNext => "search-next",
            Verb::SearchPrev => "search-prev",
            Verb::Command => "command",
            Verb::Help => "help",
            Verb::Quit => "quit",
            Verb::Escape => "escape",
            Verb::LabelPrompt => "label-prompt",
            Verb::ToggleRead => "toggle-read",
            Verb::Unread => "unread",
            Verb::Eat => "eat",
            Verb::Relabel { .. } => "relabel",
            Verb::Bury => "bury",
            Verb::Todo => "todo",
            Verb::Again => "again",
            Verb::Why => "why",
            Verb::Yank => "yank",
            Verb::Open => "open",
            Verb::New { .. } => "new",
            Verb::Compose => "compose",
            Verb::Reply => "reply",
            Verb::Send { .. } => "send",
            Verb::Which => "which",
            Verb::Db => "db",
            Verb::Only => "only",
            Verb::Spill => "spill",
            Verb::Pull => "pull",
            Verb::Theme { .. } => "theme",
            Verb::Themes => "themes",
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(
            self,
            Verb::Down
                | Verb::Up
                | Verb::Top
                | Verb::Bottom
                | Verb::HalfPageDown
                | Verb::HalfPageUp
                | Verb::PageDown
                | Verb::PageUp
                | Verb::PaneTree
                | Verb::PaneItems
                | Verb::SwapPane
                | Verb::OpenRead
                | Verb::NextInbox
                | Verb::PrevInbox
                | Verb::Search
                | Verb::SearchNext
                | Verb::SearchPrev
                | Verb::Command
                | Verb::Help
                | Verb::Quit
                | Verb::Escape
                | Verb::LabelPrompt
        )
    }

    pub fn from_id(id: &str, arg: Option<&str>) -> Option<Self> {
        Some(match id {
            "down" => Verb::Down,
            "up" => Verb::Up,
            "top" => Verb::Top,
            "bottom" => Verb::Bottom,
            "half-page-down" => Verb::HalfPageDown,
            "half-page-up" => Verb::HalfPageUp,
            "page-down" => Verb::PageDown,
            "page-up" => Verb::PageUp,
            "pane-tree" => Verb::PaneTree,
            "pane-items" => Verb::PaneItems,
            "swap-pane" => Verb::SwapPane,
            "open-read" => Verb::OpenRead,
            "next-inbox" => Verb::NextInbox,
            "prev-inbox" => Verb::PrevInbox,
            "search" => Verb::Search,
            "search-next" => Verb::SearchNext,
            "search-prev" => Verb::SearchPrev,
            "command" => Verb::Command,
            "help" => Verb::Help,
            "quit" => Verb::Quit,
            "escape" => Verb::Escape,
            "label-prompt" => Verb::LabelPrompt,
            "toggle-read" => Verb::ToggleRead,
            "unread" => Verb::Unread,
            "eat" => Verb::Eat,
            "relabel" => Verb::Relabel {
                label: arg.unwrap_or("").to_string(),
            },
            "bury" => Verb::Bury,
            "todo" => Verb::Todo,
            "again" => Verb::Again,
            "why" => Verb::Why,
            "yank" => Verb::Yank,
            "open" => Verb::Open,
            "new" => Verb::New {
                title: arg.unwrap_or("").to_string(),
            },
            "compose" => Verb::Compose,
            "reply" => Verb::Reply,
            "send" => Verb::Send {
                title: arg.unwrap_or("").to_string(),
                body: String::new(),
                reply_to: None,
            },
            "which" => Verb::Which,
            "db" => Verb::Db,
            "only" => Verb::Only,
            "spill" => Verb::Spill,
            "pull" => Verb::Pull,
            "theme" => Verb::Theme {
                name: arg
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            },
            "themes" => Verb::Themes,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
struct Binding {
    seq: &'static str,
    verb: &'static str,
    local: bool,
}

const BINDINGS: &[Binding] = &[
    Binding { seq: "j", verb: "down", local: true },
    Binding { seq: "Down", verb: "down", local: true },
    Binding { seq: "k", verb: "up", local: true },
    Binding { seq: "Up", verb: "up", local: true },
    Binding { seq: "gg", verb: "top", local: true },
    Binding { seq: "G", verb: "bottom", local: true },
    Binding { seq: "C-d", verb: "half-page-down", local: true },
    Binding { seq: "C-u", verb: "half-page-up", local: true },
    Binding { seq: "C-f", verb: "page-down", local: true },
    Binding { seq: "C-b", verb: "page-up", local: true },
    Binding { seq: "h", verb: "pane-tree", local: true },
    Binding { seq: "Left", verb: "pane-tree", local: true },
    Binding { seq: "l", verb: "pane-items", local: true },
    Binding { seq: "Right", verb: "pane-items", local: true },
    Binding { seq: "Tab", verb: "swap-pane", local: true },
    Binding { seq: "Enter", verb: "open-read", local: true },
    Binding { seq: " ", verb: "toggle-read", local: false },
    Binding { seq: "u", verb: "unread", local: false },
    Binding { seq: "dd", verb: "eat", local: false },
    Binding { seq: "/", verb: "search", local: true },
    Binding { seq: "n", verb: "search-next", local: true },
    Binding { seq: "N", verb: "search-prev", local: true },
    Binding { seq: "Esc", verb: "escape", local: true },
    Binding { seq: "q", verb: "quit", local: true },
    Binding { seq: "?", verb: "help", local: true },
    Binding { seq: "gt", verb: "next-inbox", local: true },
    Binding { seq: "gT", verb: "prev-inbox", local: true },
    Binding { seq: ":", verb: "command", local: true },
    Binding { seq: "L", verb: "label-prompt", local: true },
    Binding { seq: "c", verb: "compose", local: false },
    Binding { seq: "R", verb: "reply", local: false },
];

#[derive(Clone, Copy)]
enum ColonSpec {
    Unit(&'static str),
    Theme,
    New,
    Send,
}

const COLONS: &[(&str, ColonSpec)] = &[
    ("q", ColonSpec::Unit("quit")),
    ("quit", ColonSpec::Unit("quit")),
    ("pull", ColonSpec::Unit("pull")),
    ("help", ColonSpec::Unit("help")),
    ("theme", ColonSpec::Theme),
    ("themes", ColonSpec::Unit("themes")),
    ("why", ColonSpec::Unit("why")),
    ("again", ColonSpec::Unit("again")),
    ("eat", ColonSpec::Unit("eat")),
    ("bury", ColonSpec::Unit("bury")),
    ("todo", ColonSpec::Unit("todo")),
    ("yank", ColonSpec::Unit("yank")),
    ("open", ColonSpec::Unit("open")),
    ("new", ColonSpec::New),
    ("compose", ColonSpec::Unit("compose")),
    ("reply", ColonSpec::Unit("reply")),
    ("send", ColonSpec::Send),
    ("w", ColonSpec::Send),
    ("which", ColonSpec::Unit("which")),
    ("db", ColonSpec::Unit("db")),
    ("only", ColonSpec::Unit("only")),
    ("spill", ColonSpec::Unit("spill")),
];

pub const UNKNOWN_PREFIX: &str = "not an editor command: ";

pub const HELP: &str = "\
j/k  ↑↓        move
gg / G         top / bottom
C-d / C-u      half page
C-f / C-b      page
h / l  ←→      tree / items
tab            swap pane
gt / gT        next / prev inbox
enter          read
space          toggle read
u              unread
dd             eat (mark read)
/  n N         search
L              label (toggle + classify)
:              command
?              help
c              compose
R              reply
esc            back
q              quit

:q :quit  :pull  :help
:theme NAME  :themes
:why  :again  :eat  :bury  :todo
:yank  :open  :new TITLE
:compose  :reply  :send  :w
:which  :db  :only  :spill
";

#[derive(Debug, Default)]
pub struct KeySeq {
    pub pending: String,
}

impl KeySeq {
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

#[derive(Debug)]
pub enum Feed {
    Verb(Verb),
    Pending,
    None,
}

pub fn feed(seq: &mut KeySeq, part: &str) -> Feed {
    let candidate = format!("{}{part}", seq.pending);
    let exact = BINDINGS.iter().find(|b| b.seq == candidate);
    let prefix = BINDINGS
        .iter()
        .any(|b| b.seq.len() > candidate.len() && b.seq.starts_with(&candidate));
    if prefix {
        seq.pending = candidate;
        return Feed::Pending;
    }
    if let Some(b) = exact {
        seq.pending.clear();
        return Feed::Verb(Verb::from_id(b.verb, None).expect("binding"));
    }
    if !seq.pending.is_empty() {
        seq.pending.clear();
        return feed(seq, part);
    }
    Feed::None
}

pub fn parse_colon(s: &str) -> Result<Verb, String> {
    let s = s.trim().trim_start_matches(':').trim();
    let (word, rest) = split_word(s);
    if word.is_empty() {
        return Err(format!("{UNKNOWN_PREFIX}"));
    }
    for (name, spec) in COLONS {
        if *name == word {
            return Ok(match spec {
                ColonSpec::Unit(id) => Verb::from_id(id, None).expect("colon unit"),
                ColonSpec::Theme => Verb::Theme {
                    name: if rest.is_empty() {
                        None
                    } else {
                        Some(rest.to_string())
                    },
                },
                ColonSpec::New => Verb::New {
                    title: rest.to_string(),
                },
                ColonSpec::Send => Verb::Send {
                    title: rest.to_string(),
                    body: String::new(),
                    reply_to: None,
                },
            });
        }
    }
    Err(format!("{UNKNOWN_PREFIX}{word}"))
}

fn split_word(s: &str) -> (&str, &str) {
    match s.split_once(char::is_whitespace) {
        Some((w, rest)) => (w, rest.trim()),
        None => (s, ""),
    }
}

pub fn bindings_json() -> String {
    let mut keys = String::from("[");
    for (i, b) in BINDINGS.iter().enumerate() {
        if i > 0 {
            keys.push(',');
        }
        keys.push_str(&format!(
            "{{\"seq\":{},\"verb\":{},\"local\":{}}}",
            jstr(b.seq),
            jstr(b.verb),
            if b.local { "true" } else { "false" }
        ));
    }
    keys.push(']');
    let mut cmds = String::from("[");
    for (i, (name, spec)) in COLONS.iter().enumerate() {
        if i > 0 {
            cmds.push(',');
        }
        let (verb, arg) = match spec {
            ColonSpec::Unit(id) => (*id, false),
            ColonSpec::Theme => ("theme", true),
            ColonSpec::New => ("new", true),
            ColonSpec::Send => ("send", true),
        };
        cmds.push_str(&format!(
            "{{\"name\":{},\"verb\":{},\"arg\":{}}}",
            jstr(name),
            jstr(verb),
            if arg { "true" } else { "false" }
        ));
    }
    cmds.push(']');
    format!(
        "{{\"keys\":{keys},\"commands\":{cmds},\"unknown_prefix\":{},\"help\":{}}}",
        jstr(UNKNOWN_PREFIX),
        jstr(HELP)
    )
}

fn jstr(s: &str) -> String {
    let mut o = String::from('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c.is_control() => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gg_needs_two() {
        let mut s = KeySeq::default();
        assert!(matches!(feed(&mut s, "g"), Feed::Pending));
        assert!(matches!(feed(&mut s, "g"), Feed::Verb(Verb::Top)));
    }

    #[test]
    fn g_then_j_is_down() {
        let mut s = KeySeq::default();
        assert!(matches!(feed(&mut s, "g"), Feed::Pending));
        assert!(matches!(feed(&mut s, "j"), Feed::Verb(Verb::Down)));
    }

    #[test]
    fn dd_eat() {
        let mut s = KeySeq::default();
        assert!(matches!(feed(&mut s, "d"), Feed::Pending));
        assert!(matches!(feed(&mut s, "d"), Feed::Verb(Verb::Eat)));
    }

    #[test]
    fn gt_gt() {
        let mut s = KeySeq::default();
        feed(&mut s, "g");
        assert!(matches!(feed(&mut s, "t"), Feed::Verb(Verb::NextInbox)));
        feed(&mut s, "g");
        assert!(matches!(feed(&mut s, "T"), Feed::Verb(Verb::PrevInbox)));
    }

    #[test]
    fn colon_table() {
        assert_eq!(parse_colon("todo"), Ok(Verb::Todo));
        assert_eq!(parse_colon(":q"), Ok(Verb::Quit));
        assert_eq!(parse_colon("quit"), Ok(Verb::Quit));
        assert_eq!(
            parse_colon("theme phosphor"),
            Ok(Verb::Theme {
                name: Some("phosphor".into())
            })
        );
        assert_eq!(
            parse_colon("new Hello World"),
            Ok(Verb::New {
                title: "Hello World".into()
            })
        );
        assert_eq!(parse_colon("compose"), Ok(Verb::Compose));
        assert_eq!(parse_colon("reply"), Ok(Verb::Reply));
        assert_eq!(
            parse_colon("send Hello"),
            Ok(Verb::Send {
                title: "Hello".into(),
                body: String::new(),
                reply_to: None,
            })
        );
        assert_eq!(
            parse_colon("w"),
            Ok(Verb::Send {
                title: String::new(),
                body: String::new(),
                reply_to: None,
            })
        );
        assert_eq!(
            parse_colon("nope"),
            Err("not an editor command: nope".into())
        );
    }

    #[test]
    fn json_has_shared_tables() {
        let j = bindings_json();
        assert!(j.contains("\"seq\":\"gg\""));
        assert!(j.contains("\"verb\":\"todo\""));
        assert!(j.contains("\"name\":\"bury\""));
        assert!(j.contains("\"verb\":\"compose\""));
        assert!(j.contains("\"seq\":\"R\""));
        assert!(j.contains("unknown_prefix"));
    }
}
