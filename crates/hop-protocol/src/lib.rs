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
///
/// # Why issue #103's replace-frame change did not bump this
///
/// Issue #103 changed what a `results` frame *means* — a client now replaces
/// its held list on every frame rather than appending to it, per
/// [`DaemonMsg::Results`](wire::DaemonMsg::Results)'s docs — without changing
/// any frame's shape: the JSON on the wire is unchanged, only the meaning a
/// peer is expected to give it. A semantics change under an unchanged version
/// number is normally exactly what a handshake exists to catch, so leaving
/// this constant at `1` is an exception, not the norm, and it is sound here
/// for one reason that will not be true later: nothing has shipped. This repo
/// carries no git tags and no releases, `API_VERSION` has never left this
/// tree, and both peers that speak it — `hopd` and `hop-cli` — are built from
/// this same workspace and move together in this same commit. Bumping would
/// gate this tree against a client that does not exist.
///
/// That reasoning expires the moment a release exists. The next semantics
/// change to land after one does not get to point back at this paragraph —
/// once a peer built from a different commit can be on the other end of the
/// socket, a meaning change with no version change is the exact failure a
/// handshake is for, and this paragraph is a record of why one prior
/// exception was safe, not a policy that the next one will be too.
pub const API_VERSION: u32 = 1;
