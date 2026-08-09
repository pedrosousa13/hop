//! Parsing systemd's socket-activation environment — sd_listen_fds(3) — as
//! a pure function, kept apart from [`crate::server::acquire_listener`],
//! which is the only place in this crate (and, after this module lands,
//! this workspace's production code) that contains `unsafe`.
//!
//! systemd hands an activated process an already-bound, already-listening
//! socket as an inherited file descriptor, named by two environment
//! variables: `LISTEN_PID` (this process's own pid, so a descendant that
//! merely *inherits* the variable through `fork` does not mistake its
//! parent's activation for its own) and `LISTEN_FDS` (how many descriptors,
//! starting at [`SD_LISTEN_FDS_START`], were passed). Both must check out
//! for activation to apply. Anything else — either variable absent,
//! unparseable, or a pid that does not match this process — is not an
//! error, it is simply "no inherited listener," the same refuse-to-guess
//! footing [`crate::runtime_dir`] already takes with `XDG_RUNTIME_DIR`:
//! environment is input from a user-controlled process tree
//! (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "The
//! boundary"), not a fact this module can verify, so a value that does not
//! check out is treated as absence rather than corruption. See this crate's
//! implementation plan
//! (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
//! Design decision 2) for the full reasoning, including what happens when
//! `LISTEN_FDS` declares more than this daemon's one socket.

use std::os::fd::RawFd;

/// The first (and, for this daemon, only) inherited descriptor's number,
/// fixed by the sd_listen_fds(3) protocol itself — not configurable, and
/// not this daemon's choice.
// This module lands a commit ahead of its only production caller
// (`server::acquire_listener`, Task 2 of this crate's implementation plan),
// so outside this module's own tests nothing constructs or reads these items
// yet. `cfg_attr(not(test), expect(dead_code, ...))` — not a bare
// `#[expect]` — because this module's own tests (below) already use every
// item, so an unconditional `#[expect]` would go unfulfilled under `cargo
// test` itself. Per-item, not module-wide, matching this crate's own
// precedent (`apps.rs`'s `ParsedEntry`) and, before that,
// `hop-protocol`'s single-statement `#[expect(unsafe_code)]`: the moment
// Task 2 gives an item a real caller, that item's own expectation goes
// unfulfilled and `-D warnings` fails the build, so the attribute deletes
// itself rather than outliving its reason.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "wired into server::acquire_listener by Task 2 of this crate's \
                  socket-activation plan; until then only this module's own tests use it"
    )
)]
pub(crate) const SD_LISTEN_FDS_START: RawFd = 3;

/// What [`inherited_fd`] found.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "wired into server::acquire_listener by Task 2 of this crate's \
                  socket-activation plan; until then only this module's own tests use it"
    )
)]
pub(crate) struct InheritedFd {
    /// Always [`SD_LISTEN_FDS_START`] — hopd listens on exactly one socket,
    /// so this is the only descriptor it ever reads.
    pub(crate) fd: RawFd,
    /// The value `LISTEN_FDS` parsed to. `1` is what this daemon's own
    /// `.socket` unit (`contrib/systemd/hopd.socket`) produces; anything
    /// higher means the unit file declared more sockets than this daemon
    /// consumes. [`Self::fd`] is still valid and still used either way —
    /// see [`crate::server::acquire_listener`]'s caller for what it does
    /// with a `declared` above `1`.
    pub(crate) declared: usize,
}

/// Checks whether `lookup` (a stand-in for [`std::env::var`], taken as a
/// parameter so this function stays pure and testable with a fake, rather
/// than mutating real process environment — the same reason
/// [`crate::state_dir::resolve_from_env`] takes its inputs as parameters)
/// describes systemd socket activation for a process whose own pid is
/// `self_pid`.
///
/// Returns `Some` only when **both** hold: `LISTEN_PID` parses as a `u32`
/// and equals `self_pid`, **and** `LISTEN_FDS` parses as a `usize` that is
/// at least `1`. Every other case returns `None`, meaning "bind
/// standalone" — never an error. See this module's own doc comment for why
/// a value that fails to check out is treated as absence rather than
/// something worth reporting.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "wired into server::acquire_listener by Task 2 of this crate's \
                  socket-activation plan; until then only this module's own tests use it"
    )
)]
pub(crate) fn inherited_fd(
    lookup: impl Fn(&str) -> Option<String>,
    self_pid: u32,
) -> Option<InheritedFd> {
    let listen_pid: u32 = lookup("LISTEN_PID")?.parse().ok()?;
    if listen_pid != self_pid {
        return None;
    }
    let listen_fds: usize = lookup("LISTEN_FDS")?.parse().ok()?;
    if listen_fds == 0 {
        return None;
    }
    Some(InheritedFd {
        fd: SD_LISTEN_FDS_START,
        declared: listen_fds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn matching_pid_and_a_positive_count_is_activation() {
        let found = inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "1")]), 42)
            .expect("this is exactly the activated case");
        assert_eq!(found.fd, SD_LISTEN_FDS_START);
        assert_eq!(found.declared, 1);
    }

    #[test]
    fn a_count_above_one_is_still_activation_with_the_full_count_reported() {
        let found = inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "3")]), 42)
            .expect("still activation; the caller decides what to do about the extra fds");
        assert_eq!(
            found.fd, SD_LISTEN_FDS_START,
            "only the first fd is ever named"
        );
        assert_eq!(found.declared, 3, "the full count is reported, not clamped");
    }

    #[test]
    fn no_listen_pid_is_not_activation() {
        assert!(inherited_fd(env(&[("LISTEN_FDS", "1")]), 42).is_none());
    }

    #[test]
    fn no_listen_fds_is_not_activation() {
        assert!(inherited_fd(env(&[("LISTEN_PID", "42")]), 42).is_none());
    }

    #[test]
    fn a_mismatched_pid_is_not_activation() {
        // The case that matters most in practice: a descendant of a truly
        // activated hopd that inherited both variables through fork(),
        // with no fd of its own at SD_LISTEN_FDS_START.
        assert!(inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "1")]), 43).is_none());
    }

    #[test]
    fn an_unparseable_pid_is_not_activation() {
        assert!(
            inherited_fd(
                env(&[("LISTEN_PID", "not-a-number"), ("LISTEN_FDS", "1")]),
                42
            )
            .is_none()
        );
    }

    #[test]
    fn an_unparseable_count_is_not_activation() {
        assert!(
            inherited_fd(
                env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "not-a-number")]),
                42
            )
            .is_none()
        );
    }

    #[test]
    fn a_zero_count_is_not_activation() {
        assert!(inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "0")]), 42).is_none());
    }

    #[test]
    fn a_negative_count_is_not_activation() {
        // LISTEN_FDS is documented as a non-negative count; a negative or
        // otherwise garbage string must fail usize::parse the same way an
        // unparseable one does, not be accepted as some other meaning.
        assert!(inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "-1")]), 42).is_none());
    }
}
