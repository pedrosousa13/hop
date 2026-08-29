//! The typed IPC contract for hop: every type that crosses a process
//! boundary. That is the crate's core and the rule for what belongs here —
//! a type two or more of `hopd`, `hop-cli` and `hop-gtk` need to agree on
//! the shape of, on either side of the socket. `socket` and `config_file`
//! are the two modules that do not fit that rule (nothing in either
//! crosses the wire); they live here anyway because this is the one crate
//! all three binaries already depend on, so `socket` is where deriving and
//! resolving `hopd`'s socket path is shared rather than copied three times
//! (issue #180), and `config_file` is where the bounded, hazard-aware read
//! of a config file is shared rather than copied a second time (issue
//! #182): `hopd::config::Config::from_path` calls it for `hopd`'s own two
//! scalar keys, and `hop_gtk::keymap::Keymap::from_path` calls the same
//! function for `hop-gtk`'s `[keymap]` table — two readers of the identical
//! attacker-influenceable path today, not a promise about a reader still to
//! come. See each module's own doc comment for its full case.

#[cfg(unix)]
pub mod config_file;
pub mod content;
pub mod framing;
pub mod item;
pub mod limits;
pub mod marker_span;
pub mod mode;
pub mod redaction;
pub mod socket;
pub mod wire;

pub use content::*;
pub use framing::*;
pub use item::*;
pub use limits::*;
pub use marker_span::*;
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
///
/// **[2026-08-19] Bumped again, though not for quite the same reason.** Issue
/// #184 added a `marker_span` field to
/// [`DaemonMsg::QueryRouted`](crate::DaemonMsg), so a conforming daemon now
/// sends a field it did not before. Unlike #127's bump, this specific change
/// is not one that would otherwise break a mismatched peer at the JSON level:
/// this crate's frames already tolerate an unknown field — pinned by
/// `wire::tests::unknown_fields_tolerated_for_forward_compat` — so a client
/// built before this commit reads a `QueryRouted` frame from a new daemon by
/// silently ignoring `marker_span`, not by failing to parse it, and
/// `marker_span`'s own type is `Option`-shaped and takes serde's ordinary
/// missing-field default, so a client built after this commit reads a
/// `QueryRouted` frame from an old daemon as `marker_span: None`, not as a
/// parse error either — pinned by
/// `wire::tests::a_query_routed_frame_missing_marker_span_parses_as_none`.
/// Both directions already degrade cleanly without this bump, which is a
/// real difference from #127, where an unrecognized `type` value was a hard
/// parse failure with no tolerant path around it.
///
/// The bump happens anyway, on a narrower rationale than #127's: every
/// wire-visible change to this contract earns a version bump as a matter of
/// policy, rather than each one being individually adjudicated for whether
/// its particular failure mode happens to degrade gracefully. The
/// alternative — bump only the changes proven unsafe to skip — turns every
/// future change into a fresh argument for why *this one* is the safe kind,
/// decided by whoever is making it and rarely revisited by whoever reads the
/// diff later. `API_VERSION` still costs nothing to bump for the reason the
/// paragraph above gives: no release exists yet, and a stale binary in
/// `~/.cargo/bin` is ordinary regardless of whether the specific field it is
/// missing would have degraded gracefully or not.
/// **[2026-08-29] Bumped for issue #258's additive `RecentItems` response
/// frame.** Old binaries fail at the existing handshake version check with a
/// clear mismatch rather than attempting to consume a frame they cannot
/// render; peers built from this workspace move together on the new contract.
pub const API_VERSION: u32 = 4;
