//! The typed IPC contract for hop: every type that crosses a process boundary.

pub mod content;
pub mod framing;
pub mod item;
pub mod limits;
pub mod redaction;
pub mod wire;

pub use content::*;
pub use framing::*;
pub use item::*;
pub use limits::*;
pub use redaction::*;
pub use wire::*;

/// The version of the wire protocol implemented by this crate.
pub const API_VERSION: u32 = 1;
