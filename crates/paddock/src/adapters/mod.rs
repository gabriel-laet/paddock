//! Everything that touches the world, one directory per port:
//!
//! - `store`: where items live (sqlite)
//! - `source`: where items come from and drafts go (fs, rss, exec)
//! - `classifier`: what stamps labels beyond regex (script, exec, http, llm)
//! - `model`: a chat model (exec, ollama, openai)
//! - `embedder`: text to vector (exec, http, ollama, openai)
//! - `mirror`: a copy of the store elsewhere (s3, any command)
//! - `agent`: an agent CLI that sets the host up on request; the notice command
//! - `skills`: named fragments of classifiers and inboxes a config grafts in with `use`
//! - `host`: the machine (paths, the TOML config, the standard wiring)
//!
//! `transport` is the shared plumbing: a child process or an HTTP POST.

pub mod agent;
pub mod classifier;
pub mod embedder;
pub mod host;
pub mod mirror;
pub mod model;
pub mod skills;
pub mod source;
pub mod store;
pub(crate) mod transport;

pub use agent::{agents_on_path, briefing, notify, plugins_on_path, setup, AgentSpec};
pub use host::{
    default_config_toml, expand_path, init, kernel, kernel_at, load, load_config, mirror,
    mirror_after_pull, store_key, Paths,
};
pub use host::{push_mirror, resolve_secrets};
pub use mirror::Mirror;
pub use skills::{skill, skills, Skill};
pub use source::item_from_file;
pub use store::Sqlite;
