//! Everything that touches the world, one directory per port:
//!
//! - `store`: where items live (sqlite)
//! - `source`: where items come from and drafts go (fs, rss, exec)
//! - `classifier`: what stamps labels beyond regex (script, exec, http, llm)
//! - `host`: the machine (paths, the TOML config, the standard wiring)

pub mod classifier;
pub mod host;
pub mod source;
pub mod store;

pub use host::{default_config_toml, expand_path, init, kernel, load, load_config, Paths, Std};
pub use source::item_from_file;
pub use store::Sqlite;
