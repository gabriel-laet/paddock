//! paddock — an inbox kernel.
//!
//! `kernel` is pure: the nouns, the questions inboxes ask, and the verbs.
//! `adapters` is everything that touches the world. `main.rs` is a thin CLI.

pub mod adapters;
pub mod kernel;

pub use adapters::*;
pub use kernel::*;
