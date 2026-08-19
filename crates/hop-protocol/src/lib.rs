//! The typed IPC contract for hop: every type that crosses a process
//! boundary. That is the crate's core and the rule for what belongs here —
//! a type two or more of `hopd`, `hop-cli` and `hop-gtk` need to agree on
//! the shape of, on either side of the socket. `socket` and `config_file`
//! are the two modules that do not fit that rule (nothing in either
//! crosses the wire); they live here anyway because this is the one crate
//! all three binaries already depend on, so `socket` is where deriving and
//! resolving `hopd`'s socket path is shared rather than copied three times
//! (issue #180), and `config_file` is where the bounded, hazard-aware read
//! of a config file — `hopd`'s config today, `hop-gtk`'s keymap tomorrow —
//! is shared rather than copied a second time (issue #182). See each
//! module's own doc comment for its full case.

#[cfg(unix)]
pub mod config_file;
pub mod content;
pub mod framing;
pub mod item;
pub mod limits;
pub mod mode;
pub mod redaction;
pub mod socket;
pub mod wire;

pub use content::*;
pub use framing::*;
pub use item::*;
pub use limits::*;
pub use mode::*;
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
/// **[2026-08-10] The exception above ended, and this is the bump that ended
/// it.** Issue #127 added [`DaemonMsg::QueryRouted`](crate::DaemonMsg), a
/// frame the daemon now sends for *every* accepted query. That is additive
/// rather than a semantics change, so the paragraph above would have
/// tolerated it — but the failure mode without a bump is the bad kind. A
/// `hop` built before this commit passes a `1 == 1` handshake, connects
/// happily, and then fails on its first query with a deserialization error
/// about an unknown variant, which reads as a corrupt daemon rather than a
/// stale client. With the bump it fails at the handshake saying exactly what
/// is wrong. Stale binaries in `~/.cargo/bin` and `~/.local/bin` are ordinary,
/// so that clean failure is worth more than the "gate against a client that
/// does not exist" cost the paragraph above weighed — the client does exist,
/// on any machine where `cargo install` ran once.
pub const API_VERSION: u32 = 2;
