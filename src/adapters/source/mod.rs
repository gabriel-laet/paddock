//! Sources: where items come from and where drafts go.
//! fs is a directory, rss is a feed, exec is any program.

pub mod exec;
pub mod filename;
pub mod fs;
pub mod rss;

pub use exec::Exec;
pub use fs::{item_from_file, Fs};
pub use rss::Rss;

pub(crate) fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

pub(crate) fn opt(s: Option<String>) -> Option<String> {
    nonempty(s.as_deref()).map(str::to_string)
}
