//! Everything that touches the world, one directory per port:
//!
//! - `store`: where items live (sqlite)
//! - `source`: where items come from and drafts go (fs, rss, exec)
//! - `classifier`: what stamps labels beyond regex (script, exec, http, llm)
//! - `model`: a chat model (exec, ollama, openai)
//! - `embedder`: text to vector (exec, http, ollama, openai)
//! - `mirror`: a copy of the store elsewhere (s3, any command)
//! - `host`: the machine (paths, the TOML config, the standard wiring)
//!
//! `transport` is the shared plumbing: a child process or an HTTP POST.

pub mod classifier;
pub mod embedder;
pub mod host;
pub mod mirror;
pub mod model;
pub mod source;
pub mod store;
pub(crate) mod transport;

pub use host::{
    default_config_toml, expand_path, init, kernel, kernel_at, load, load_config, mirror,
    mirror_after_pull, store_key, Paths,
};
pub use mirror::Mirror;
pub use source::item_from_file;
pub use store::Sqlite;
