//! paddock — the `paddock` binary's library: every adapter, and the kernel
//! re-exported so callers see one crate.
//!
//! The kernel lives in `paddock-kernel` and is pure. The exec protocol lives
//! in `paddock-protocol`, which plugins depend on alone. This crate is
//! everything that touches the world, plus the CLI in `main.rs`.

pub mod adapters;

pub use paddock_kernel as kernel;
pub use paddock_protocol as protocol;

pub use adapters::*;
pub use kernel::*;
