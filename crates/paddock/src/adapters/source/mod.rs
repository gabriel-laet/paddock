//! Sources: where items come from and where drafts go. fs is a directory;
//! exec is any program speaking the protocol, which is how every format of
//! the world (mail, feeds, chat) reaches the kernel: as a plugin.

pub mod exec;
pub mod filename;
pub mod fs;

pub use exec::Exec;
pub use fs::{item_from_file, Fs};

pub(crate) fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

pub(crate) fn opt(s: Option<String>) -> Option<String> {
    nonempty(s.as_deref()).map(str::to_string)
}
