//! Binding the socket and accepting connections onto it.
//!
//! What happens *on* a connection — the handshake gate, the query lifecycle,
//! the framing calls that make both safe against a hostile peer — belongs to
//! the crate-private `connection` module. This module's whole job is the
//! listener: where the socket file lives, what mode it is born at, and one
//! spawned task per peer that turns up.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use hop_core::host::ProviderHost;
use tokio::net::UnixListener;
use tokio::sync::Semaphore;

use crate::activation;
use crate::connection::handle_connection;
use crate::source::{ResultSource, SkeletonProvider, StderrLog};

/// The socket's file name inside the runtime directory
/// [`crate::runtime_dir::resolve`] returns.
const SOCKET_FILE_NAME: &str = "hopd.sock";

/// Maximum number of connections whose tasks may run at once.
///
/// One permit is acquired before `accept`, so the 65th local peer waits in
/// the listener backlog rather than allocating a connection task. The permit
/// stays owned by that task until [`handle_connection`] returns. This bounds
/// same-uid robustness exposure from buggy or runaway local clients: across
/// 64 admitted connections, their one 64 KiB inbound payload buffer each sum
/// to at most 4 MiB, alongside at most 64,000 retained bounded items. It is
/// not a security boundary against a hostile peer, and it is the intentional
/// connection backpressure; no accept-rate limiter is needed.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// Builds the daemon's provider host: the registry every query runs through.
///
/// Registration failures are a programming error rather than an operating
/// condition — the only ids registered here are literals in this function, so
/// a duplicate means two lines in this file chose the same one. It is reported
/// and the provider skipped rather than panicking, because a daemon that
/// refuses to start over one misconfigured provider is worse than one that
/// serves the rest: spec §9's per-provider isolation rule applied to startup.
pub(crate) fn build_host() -> ProviderHost {
    let mut host = ProviderHost::with_log(Arc::new(StderrLog));
    if let Err(err) = host.register(SkeletonProvider) {
        eprintln!("hopd: could not register the skeleton provider: {err}");
    }
    if let Err(err) = host.register(crate::apps::build_apps_provider()) {
        eprintln!("hopd: could not register the apps provider: {err}");
    }
    if let Err(err) = host.register(crate::calculator::CalculatorProvider) {
        eprintln!("hopd: could not register the calculator provider: {err}");
    }
    host
}

/// Binds `<runtime_dir>/hopd.sock` and serves connections until an error
/// stops the accept loop or the process is killed — whichever comes first.
///
/// `runtime_dir` is assumed already created at 0700 by
/// [`crate::runtime_dir::resolve`]; this function does not create it, only
/// the socket file inside it.
///
/// [`crate::run`] is this function's production caller: it builds a
/// config-aware [`HostSource`](crate::source::HostSource) over
/// [`build_host`]'s provider registry and calls this function directly, so
/// everything documented below is exactly what the binary does.
///
/// # Stale-socket removal is provisional
///
/// Whatever sits at the socket path is removed before binding, unconditionally
/// — not gated on an `exists()` check first. This is what makes restarting
/// hopd after a crash work at all — `bind` otherwise fails with `AddrInUse`
/// against a leftover socket file, live or not — but it is not a
/// single-instance guard: nothing here checks whether another `hopd` is still
/// listening on that path before unlinking it out from under it. That check
/// is a later M2 slice's job, not this daemon's.
///
/// The removal is unconditional rather than `if socket_path.exists() {
/// remove_file }` for two reasons. First, that shape is a TOCTOU: whatever
/// might be true between the `exists()` check and the `remove_file` call is
/// exactly the kind of race this process cannot rule out just by checking
/// first. Second, `exists()` follows symlinks and reports `false` for a
/// dangling one — a socket path left behind as a symlink to a since-deleted
/// target would make `exists()` say "nothing here" and then `bind` fail with
/// `AddrInUse` anyway, since the kernel still finds a directory entry there.
/// `remove_file` alone, tolerating only `NotFound`, handles both: the common
/// case (nothing there) and the dangling-symlink case (something there that
/// isn't a live socket) the same way, and still surfaces a genuine permission
/// or I/O error instead of swallowing it.
///
/// # The socket's mode is decided, not inherited
///
/// This section describes the standalone branch only — see
/// "# Socket activation" below for the other one. The v1 spec fixes the
/// runtime directory's mode at 0700 (which grants or withholds *traverse*)
/// and says nothing about the socket file's own mode, which is what grants
/// or withholds *connect* once traverse is granted — left unstated, that
/// mode would be whatever the process's umask happens to produce. The threat
/// model (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`,
/// "The boundary") calls that out as a decision this slice must make rather
/// than inherit, so the socket file is explicitly narrowed to 0600 with
/// `set_permissions` right after `bind`. Between `bind` returning and that
/// call completing there is a brief window where the socket's own mode is
/// whatever the umask left it at — but the *directory* is already 0700 by
/// the time this function runs, and reaching a path inside it requires
/// traverse on every component, so the parent directory's mode is what
/// carries the access control during that window, not the socket file's.
///
/// # Socket activation
///
/// When `LISTEN_PID`/`LISTEN_FDS` describe activation for this exact
/// process ([`activation::inherited_fd`]), the standalone bind above does
/// not run at all: `acquire_listener` turns the inherited descriptor
/// directly into a listener and never removes, binds, or `chmod`s anything
/// at `runtime_dir`. The socket's mode is still 0600 and its directory
/// still 0700 in this case too — carried by `contrib/systemd/hopd.socket`'s
/// own `SocketMode=`/`DirectoryMode=` instead of by this function. See this
/// crate's implementation plan
/// (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
/// Design decision 3) for why ownership of the socket file itself switches
/// entirely to the service manager under activation, rather than this
/// function reconciling two owners.
///
/// # The integration seam
///
/// This function is also generic over what answers the queries, which is
/// what makes it the seam tests use: a test injects a scripted
/// [`ResultSource`] here — one that streams several batches, or stalls, or
/// floods past
/// [`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY) — and
/// then drives it over a real socket with a real client, so what the suite
/// pins is the daemon's actual wire behaviour rather than a mock agreeing
/// with itself. Everything else about the connection is the production path,
/// unchanged: the only thing a test gets to choose is where the items come
/// from.
pub async fn serve_with<S: ResultSource>(runtime_dir: &Path, source: S) -> io::Result<()> {
    let activation = activation::inherited_fd(|k| std::env::var(k).ok(), std::process::id());
    let listener = acquire_listener(runtime_dir, activation)?;

    // The permit is acquired before every accept, so the connection cap is
    // backpressure in the listener rather than a post-accept task limit. The
    // 50 ms sleep below remains only a hot-spin floor for accept errors; it is
    // not a rate policy. These bounds are robustness against buggy or runaway
    // same-uid local clients, not a security boundary against hostile peers.
    let connection_slots = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    // No accept-loop exit beyond an unrecoverable startup error above: a
    // per-connection failure is logged and the loop keeps accepting, so the
    // only way out of this loop is the process being killed. Signal handling
    // and any orderly shutdown remain unowned by any filed issue (see
    // `crate::run`'s own "# Shutdown" section) — issue #62 added
    // *activation* only, not lifecycle.
    loop {
        let permit = Arc::clone(&connection_slots)
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("connection limiter closed"))?;

        match listener.accept().await {
            Ok((stream, _addr)) => {
                // One owned permit and one task per connection. Binding the
                // permit for the entire task makes the cap cover the full
                // connection lifecycle, not only the accept operation.
                //
                // Every connection gets its own handle on the source rather
                // than sharing one, which is why [`ResultSource`] is `Clone`:
                // an implementation is expected to be a cheap handle over
                // whatever shared state it has, not the state itself.
                let source = source.clone();
                tokio::spawn(async move {
                    let _connection_slot = permit;
                    if let Err(err) = handle_connection(stream, source).await {
                        // Issue #34's logging seam is `ProviderLog`
                        // (`hop_core::host::ProviderLog`), landed in this
                        // branch and implemented in this crate as
                        // `StderrLog` (`source.rs`) — not blocked on a later
                        // slice. Nor is this `eprintln!` the only place this
                        // crate reports an error: `build_host` above (this
                        // file) and `StderrLog::record`'s three logging arms
                        // (`source.rs`) do too. What is still true, and what
                        // the brief's behavior spec actually asks for, is
                        // narrower: this remains the only place a
                        // *connection-level* I/O error is reported.
                        eprintln!("hopd: connection error: {err}");
                    }
                });
            }
            Err(err) => {
                drop(permit);
                eprintln!("hopd: accept error: {err}");
                // A floor, not a policy: this sleep exists only so a
                // persistent accept error (EMFILE, exhausted file
                // descriptors) cannot hot-spin the loop and pin a core
                // logging the same line as fast as it can. It is not a
                // backoff strategy and not a connection-rate limit — the
                // real accept-rate and connection-cap policy is issue #98's,
                // not this daemon's.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

/// Turns either an inherited descriptor or `runtime_dir` into a working
/// listener. See this crate's implementation plan
/// (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
/// Design decisions 1 and 3) for the full reasoning behind both branches.
fn acquire_listener(
    runtime_dir: &Path,
    activation: Option<activation::InheritedFd>,
) -> io::Result<UnixListener> {
    match activation {
        Some(found) => {
            if found.declared > 1 {
                eprintln!(
                    "hopd: LISTEN_FDS declared {} descriptors; hopd listens on one \
                     socket, so only the first (fd {}) is used",
                    found.declared, found.fd
                );
            }

            // SAFETY: `found.fd` came from `activation::inherited_fd`, this
            // crate's own re-implementation of the sd_listen_fds(3)
            // protocol, which only returns `Some` once `LISTEN_PID` has
            // matched this process's own pid — systemd's anti-spoofing
            // check. That match is what makes the descriptor trustworthy:
            // it relies on a service manager honoring the same protocol
            // having already bound and listened on it, and handed over sole
            // ownership. This is the only `unsafe` in this crate,
            // and the only one in this workspace's production code
            // (root `Cargo.toml`'s `[workspace.lints.rust] unsafe_code`
            // doc comment; the tree's one prior `unsafe` is test-only, in
            // `hop-protocol`'s `content.rs`). See this crate's
            // implementation plan, Design decision 1, for why this is taken
            // directly rather than through a crate that hides the same call.
            #[expect(
                unsafe_code,
                reason = "sd_listen_fds(3) hands the daemon a raw fd; OwnedFd::from_raw_fd \
                          is the only way to take ownership of it, and every step after \
                          this one is safe"
            )]
            let owned = unsafe { OwnedFd::from_raw_fd(found.fd) };

            let std_listener = std::os::unix::net::UnixListener::from(owned);
            // tokio::net::UnixListener::from_std requires non-blocking mode
            // (tokio 1.53.1's own doc comment on that function: passing a
            // listener in blocking mode is erroneous and its
            // `check_socket_for_blocking` helper `debug_assert`s on exactly
            // this in a debug build, which is what `cargo test` runs under).
            // A descriptor inherited from systemd is not guaranteed to
            // already be non-blocking, so this is set explicitly rather
            // than assumed.
            std_listener.set_nonblocking(true)?;
            tokio::net::UnixListener::from_std(std_listener)
        }
        None => {
            // Exactly today's standalone path, unchanged: see
            // `serve_with`'s own doc comment ("The socket's mode is
            // decided, not inherited") for the stale-removal and chmod
            // reasoning.
            let socket_path = runtime_dir.join(SOCKET_FILE_NAME);
            match std::fs::remove_file(&socket_path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            let listener = UnixListener::bind(&socket_path)?;
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
            Ok(listener)
        }
    }
}

#[cfg(test)]
mod build_host_tests {
    use super::*;

    #[test]
    fn build_host_registers_the_skeleton_apps_and_calculator_providers() {
        // Not a behavior test of any one provider (each has its own suite
        // already) — this pins that `build_host` actually calls every
        // wiring function this crate has, so a future edit that adds a
        // provider but forgets to register it fails here rather than
        // silently shipping a daemon with a gap.
        let host = build_host();
        let ids: Vec<_> = host.manifests().iter().map(|m| m.id).collect();
        assert!(ids.contains(&"skeleton"));
        assert!(ids.contains(&hop_core::provider::APPS_PROVIDER_ID));
        assert!(ids.contains(&hop_core::provider::CALCULATOR_PROVIDER_ID));
    }
}

#[cfg(test)]
mod acquire_listener_tests {
    #![allow(clippy::unwrap_used)]

    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::activation::InheritedFd;

    #[tokio::test]
    async fn with_no_activation_it_binds_and_chmods_the_socket_path_exactly_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let _listener = acquire_listener(dir.path(), None).unwrap();

        let socket_path = dir.path().join(SOCKET_FILE_NAME);
        assert!(
            socket_path.exists(),
            "the standalone path must still bind the socket file"
        );
        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the standalone path must still narrow the mode"
        );
    }

    #[tokio::test]
    async fn with_an_inherited_fd_it_never_touches_the_runtime_dir_path() {
        let backing = tempfile::tempdir().unwrap();
        let std_listener =
            std::os::unix::net::UnixListener::bind(backing.path().join("preexisting.sock"))
                .unwrap();
        let fd = std_listener.into_raw_fd();

        let unrelated_dir = tempfile::tempdir().unwrap();
        let never_created = unrelated_dir.path().join("never-created-subdir");

        let result = acquire_listener(&never_created, Some(InheritedFd { fd, declared: 1 }));
        assert!(
            result.is_ok(),
            "acquire_listener must accept the inherited fd: {:?}",
            result.err()
        );
        assert!(
            !never_created.exists(),
            "activation must never create, bind inside, or otherwise touch the runtime dir path"
        );
    }

    #[tokio::test]
    async fn a_listener_built_from_an_inherited_fd_actually_accepts_connections() {
        let backing = tempfile::tempdir().unwrap();
        let path = backing.path().join("real.sock");
        let std_listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let fd = std_listener.into_raw_fd();

        let listener =
            acquire_listener(backing.path(), Some(InheritedFd { fd, declared: 1 })).unwrap();

        let accept_task = tokio::spawn(async move { listener.accept().await });
        let _client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let accepted = accept_task.await.unwrap();
        assert!(
            accepted.is_ok(),
            "a listener rebuilt from an inherited fd must actually accept: {:?}",
            accepted.err()
        );
    }
}

#[cfg(test)]
mod systemd_unit_tests {
    use super::*;

    const SOCKET_UNIT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contrib/systemd/hopd.socket"
    ));
    const SERVICE_UNIT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contrib/systemd/hopd.service"
    ));

    #[test]
    fn the_socket_unit_names_the_same_path_this_module_binds_to_standalone() {
        // A cross-check, not a duplicate literal: if SOCKET_FILE_NAME ever
        // changes, this fails instead of the unit file silently drifting
        // from what a standalone-started hopd actually binds to.
        assert!(
            SOCKET_UNIT.contains(&format!("ListenStream=%t/hop/{SOCKET_FILE_NAME}")),
            "the socket unit's ListenStream= must name %t/hop/{SOCKET_FILE_NAME}"
        );
    }

    #[test]
    fn the_socket_unit_declares_the_modes_activation_must_carry() {
        // Design decision 3: under activation hopd itself sets neither
        // mode, so the unit file is the only place these are enforced.
        assert!(SOCKET_UNIT.contains("SocketMode=0600"));
        assert!(SOCKET_UNIT.contains("DirectoryMode=0700"));
    }

    #[test]
    fn the_socket_unit_is_enablable_on_its_own() {
        assert!(
            SOCKET_UNIT.contains("WantedBy=sockets.target"),
            "without an [Install] target, `systemctl --user enable hopd.socket` has nothing to link"
        );
    }

    #[test]
    fn the_service_unit_has_an_exec_start() {
        assert!(SERVICE_UNIT.contains("ExecStart="));
    }
}
