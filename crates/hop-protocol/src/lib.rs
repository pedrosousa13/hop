//! The typed IPC contract for hop: every type that crosses a process boundary.

pub mod item;
pub mod wire;

pub use item::*;
pub use wire::*;

/// The version of the wire protocol implemented by this crate.
pub const API_VERSION: u32 = 1;
