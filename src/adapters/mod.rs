//! Everything that touches the world: SQLite, files, feeds, programs, chat
//! models, and the TOML config. Each one implements a port from the kernel.

pub mod host;
pub mod llm;
pub mod script;
pub mod sources;
pub mod sqlite;

pub use host::{default_config_toml, expand_path, init, kernel, load, load_config, Paths, Std};
pub use sources::item_from_file;
pub use sqlite::Sqlite;
