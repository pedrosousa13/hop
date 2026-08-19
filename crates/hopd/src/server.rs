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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hop_core::host::ProviderHost;
use thiserror::Error;
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
/// # A live listener's pathname is never replaced (#158)
///
/// This section used to be called "Stale-socket removal is provisional" and
/// said plainly that nothing here checked whether another `hopd` was still
/// listening before unlinking its pathname — the check was left to "a later
/// M2 slice." #158 is that slice, and this section documents what actually
/// landed rather than what was deferred.
///
/// **The problem the old unconditional removal had.** A Unix listener stays
/// open after its pathname is unlinked — the primary Linux
/// [`unlink(2)`](https://man7.org/linux/man-pages/man2/unlink.2.html)
/// documentation says as much: removing a name does not invalidate the
/// object behind it for anyone already holding it open. So a second
/// standalone `hopd`, started while a first is still live, could unlink the
/// first's socket file and bind its own at the freed path. The first
/// daemon kept serving the connections it already had; every *new* client
/// reached the second. Nothing signaled either daemon that this had
/// happened.
///
/// **Why the removal was unconditional in the first place, and why that
/// reasoning still holds.** `if socket_path.exists() { remove_file }` was
/// rejected, not merely not yet written, for two reasons that #158 does not
/// get to undo: first, that shape is a TOCTOU — whatever might become true
/// between the `exists()` check and the `remove_file` call is exactly the
/// kind of race a stat-then-unlink sequence cannot rule out by checking
/// first. Second, `exists()` follows symlinks and reports `false` for a
/// dangling one — a socket path left behind as a symlink to a
/// since-deleted target would make `exists()` say "nothing here" and then
/// `bind` fail with `AddrInUse` anyway, since the kernel still finds a
/// directory entry there. Any fix had to keep clearing both of those, not
/// just the live-listener gap.
///
/// **What changed.** The standalone branch now asks a different question
/// before it touches the path at all: it connects to `socket_path` the way
/// a real client would.
///
/// - A successful connect means a live `hopd` is answering. `acquire_listener`
///   returns [`ListenerError::AlreadyListening`] immediately — no
///   `remove_file`, no `bind` — and drops the probe connection. The live
///   daemon sees one connection open and close, indistinguishable in its
///   logs from a client that connected and went away; that is the accepted
///   cost of asking the question this way rather than not asking it at all.
/// - `ECONNREFUSED` means a socket file is present but nothing is listening
///   — exactly what a crashed or hard-killed `hopd` leaves behind, and
///   exactly the case the old unconditional removal existed to recover.
///   `acquire_listener` now reaches `remove_file` deliberately, on this
///   outcome, rather than unconditionally.
/// - `ENOENT` means nothing is at the path — including a dangling symlink,
///   whose target lookup fails the same way a real client's connect would.
///   Nothing to remove; falling through to the `remove_file` call below,
///   which still tolerates `NotFound` on its own, covers a symlink that
///   really is there and the benign race where something removed it
///   between the probe and this call.
/// - Any other connect error (permission denied, the path exists but is not
///   a socket at all) is surfaced as [`ListenerError::Io`] rather than
///   folded into either case above — a genuine I/O problem should read as
///   one, not get silently treated as "stale" or "safe to start anyway."
///
/// **Why a connect probe rather than an advisory lock.** An `flock`/`O_EXCL`
/// lockfile would close the residual race below entirely: its ownership is
/// arbitrated by the kernel across processes, not inferred from watching a
/// connect attempt's outcome. It was rejected for this slice because it
/// costs a second file with its own lifecycle to design and test — creation
/// mode, who cleans up a lock left by a killed process, whether *it* now
/// needs the same "restart must still work" unconditional-removal treatment
/// the socket itself needed — none of which buys anything a live-hopd
/// probe does not already answer for the one question this issue is
/// actually about: is something accepting connections at this path right
/// now. A connect probe asks the kernel that exact question, the same way a
/// real client already does, with no new file and no new dependency. #158
/// is explicitly a same-uid lifecycle and availability control inside the
/// trust boundary the threat model already declares
/// (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "The
/// boundary"), not a new authentication mechanism — so the cheaper
/// mechanism that answers the actual question wins, and the stronger one
/// documented here is not free to fall back on if this posture ever turns
/// out to be wrong.
///
/// **The residual race, and why it is acceptable.** Between the probe
/// reporting "not live" and the `remove_file` + `bind` that follow, a
/// second process running this same function concurrently could reach
/// `bind` too — the probe adds no lock, so that interleaving is not made
/// impossible. What it does rule out is the specific failure #158 was filed
/// for: a daemon that is already established and serving connections
/// having its name pulled out from under it by a second, later start. Two
/// `hopd`s racing to start against a path neither has bound yet is a
/// starting-order coin flip with no established victim — ordinary
/// `AddrInUse`-shaped contention, the same class every `bind` on a shared
/// path already has, and a different situation from silently displacing a
/// daemon clients already depend on. A live listener is never unlinked on
/// this path: it either answers the probe, and the standalone bind refuses
/// outright, or it does not, and there was nothing live to unlink in the
/// first place.
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
///
/// # The return type
///
/// [`ListenerError`], not a bare `io::Error`: this function's only error
/// path before the accept loop starts is [`acquire_listener`]'s, and #158
/// made that path distinguish "a daemon is already listening" from a
/// generic I/O failure. Widening `serve_with`'s own signature to match is
/// what lets that distinction survive all the way to [`crate::run`]'s
/// `eprintln!("hopd: {err}")` and to the user — see
/// [`ListenerError`]'s own doc comment.
pub async fn serve_with<S: ResultSource>(
    runtime_dir: &Path,
    source: S,
) -> Result<(), ListenerError> {
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

/// Why [`acquire_listener`] (and therefore [`serve_with`]) could not
/// produce a listener.
///
/// Before #158 this was a bare `io::Result`, which meant a live-listener
/// refusal would have reached a user as text indistinguishable from a
/// permission or disk error — two failures with completely different
/// correct responses ("something is already answering here; leave it
/// alone, or stop it first" versus "check this daemon's file permissions").
/// [`AlreadyListening`](ListenerError::AlreadyListening) is what makes the
/// refusal its own diagnosis: `#[error(...)]` below is exactly the text
/// [`crate::run`]'s `eprintln!("hopd: {err}")` shows a user, so this is not
/// merely an internal distinction — it is the wording the acceptance
/// criterion asks for.
#[derive(Debug, Error)]
pub enum ListenerError {
    /// A connect probe against the socket path succeeded: a live `hopd` is
    /// already answering there. See `acquire_listener`'s "# A live
    /// listener's pathname is never replaced (#158)" doc section for how
    /// this is decided and why a connect probe rather than a lockfile.
    #[error(
        "a daemon is already listening at {}; refusing to replace it — stop the \
         running hopd first, or point XDG_RUNTIME_DIR somewhere else",
        .path.display()
    )]
    AlreadyListening {
        /// The socket path a live listener already answers on.
        path: PathBuf,
    },

    /// Any other failure acquiring the listener: permission denied on the
    /// runtime directory or socket, a `remove_file` that failed for a
    /// reason other than `NotFound`, `bind` itself failing, and so on —
    /// unchanged in substance from what a bare `io::Error` said before
    /// #158, just now a named variant instead of the only shape this type
    /// could take.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Whether a connect attempt against the socket path found a live
/// listener, a socket file nothing is behind, or nothing at all. See
/// [`probe_socket_liveness`].
enum SocketLiveness {
    /// A live listener accepted the probe connection.
    Live,
    /// The path exists but nothing is listening — `ECONNREFUSED`, what a
    /// crashed or hard-killed `hopd` leaves behind.
    Stale,
    /// Nothing is at the path at all — `ENOENT`, including a dangling
    /// symlink, whose target lookup fails the same way.
    Absent,
}

/// Decides [`SocketLiveness`] for `socket_path` by attempting to connect to
/// it exactly as a real client would, rather than by `stat`-ing it first.
///
/// See `acquire_listener`'s "# A live listener's pathname is never replaced
/// (#158)" doc section for the full reasoning: briefly, connecting is what
/// lets this function ask "is something accepting connections here right
/// now" without the TOCTOU an `exists()`-then-`remove_file` shape would
/// introduce, and without `exists()`'s own blind spot for a dangling
/// symlink. Any connect failure other than the two that name "not live"
/// (`ConnectionRefused`, `NotFound`) is returned as a genuine I/O error
/// rather than folded into either case — a permission failure or a path
/// that is not a socket at all should read as its own problem, not as
/// "stale" or "safe to start anyway."
fn probe_socket_liveness(socket_path: &Path) -> io::Result<SocketLiveness> {
    match std::os::unix::net::UnixStream::connect(socket_path) {
        Ok(_probe_connection) => Ok(SocketLiveness::Live),
        Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => Ok(SocketLiveness::Stale),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(SocketLiveness::Absent),
        Err(err) => Err(err),
    }
}

/// Turns either an inherited descriptor or `runtime_dir` into a working
/// listener. See this crate's implementation plan
/// (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
/// Design decisions 1 and 3) for the full reasoning behind both branches,
/// and `serve_with`'s "# A live listener's pathname is never replaced
/// (#158)" doc section for the standalone branch's liveness check.
fn acquire_listener(
    runtime_dir: &Path,
    activation: Option<activation::InheritedFd>,
) -> Result<UnixListener, ListenerError> {
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
            // ownership. This is the only `unsafe` in this workspace's
            // production code — not the only one in this crate, which also
            // carries three test-only blocks (root `Cargo.toml`'s
            // `[workspace.lints.rust] unsafe_code` doc comment counts five
            // in the tree as of issue #161: this one, and four test-only
            // `libc::mkfifo`/`pre_exec` calls, in `hop-protocol`'s
            // `content.rs`, this crate's `config.rs`, this crate's own
            // `tests/activation.rs`, and this crate's own `apps.rs`). See
            // this crate's implementation plan, Design decision 1, for why
            // this is taken directly rather than through a crate that hides
            // the same call.
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
            Ok(tokio::net::UnixListener::from_std(std_listener)?)
        }
        None => {
            // #158: unlike the rest of this branch, the liveness check
            // below is new — see `serve_with`'s doc comment ("A live
            // listener's pathname is never replaced") for the stale-vs-live
            // decision and the mode/chmod reasoning that follows it.
            let socket_path = runtime_dir.join(SOCKET_FILE_NAME);
            match probe_socket_liveness(&socket_path)? {
                SocketLiveness::Live => {
                    return Err(ListenerError::AlreadyListening { path: socket_path });
                }
                SocketLiveness::Stale | SocketLiveness::Absent => {
                    match std::fs::remove_file(&socket_path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                    }
                }
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;
    use crate::activation::InheritedFd;

    /// The live-listener case (#158). A first listener is bound and left
    /// running — nothing here ever calls `accept` on it, because the point
    /// is proving the *second* `acquire_listener` call refuses to disturb
    /// it before either side accepts anything. `acquire_listener` must
    /// refuse rather than unlink-and-rebind, and the first listener must
    /// still be reachable at the exact same path afterward — checked here
    /// by inode, and separately by proving it still accepts below.
    #[tokio::test]
    async fn a_live_listener_is_refused_without_being_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join(SOCKET_FILE_NAME);
        let first = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let original_inode = std::fs::metadata(&socket_path).unwrap().ino();

        let result = acquire_listener(dir.path(), None);
        let err = result.expect_err("a live listener at the path must be refused");
        assert!(
            matches!(&err, ListenerError::AlreadyListening { path } if path == &socket_path),
            "expected AlreadyListening naming {socket_path:?}, got {err:?}"
        );

        let inode_after_refusal = std::fs::metadata(&socket_path).unwrap().ino();
        assert_eq!(
            original_inode, inode_after_refusal,
            "the refusal must not unlink or replace the live listener's socket path"
        );

        // The first listener must still be the one answering at this path —
        // not merely present on disk, but actually able to accept.
        let accept_task = tokio::spawn(async move { first.accept().await });
        let _client = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let accepted = accept_task.await.unwrap();
        assert!(
            accepted.is_ok(),
            "the first listener must still accept after the refusal: {:?}",
            accepted.err()
        );
    }

    /// The stale-path case (#158). Binding and dropping a listener without
    /// ever accepting anything leaves exactly what a hard-killed or crashed
    /// `hopd` would: a socket inode on disk with nothing behind it to
    /// accept a connection (`std`'s `UnixListener` does not unlink its own
    /// path on drop). `acquire_listener` must still recover this path per
    /// the documented policy, producing a listener that actually works.
    #[tokio::test]
    async fn a_stale_socket_file_is_recovered_and_a_fresh_listener_binds() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join(SOCKET_FILE_NAME);
        {
            let _abandoned = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        }

        let listener = acquire_listener(dir.path(), None)
            .expect("a stale socket file must still be recoverable");

        let accept_task = tokio::spawn(async move { listener.accept().await });
        let _client = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let accepted = accept_task.await.unwrap();
        assert!(
            accepted.is_ok(),
            "the recovered listener must actually accept: {:?}",
            accepted.err()
        );
    }

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

    /// #158's own pin of the brief's activation criterion ("does not unlink
    /// or rebind the activated path"), distinct from the test just above.
    /// That test predates #158 and only proves the runtime dir path is
    /// never *created* by activation; it never puts anything live there, so
    /// it cannot exercise the case #158 actually made interesting: a
    /// runtime-dir path that already has a *live* listener on it — exactly
    /// what the standalone branch's `probe_socket_liveness` exists to find
    /// and refuse to disturb. Activation must never reach that probe at
    /// all — `acquire_listener`'s `match activation` takes the `Some` arm
    /// unconditionally and returns before `probe_socket_liveness` or
    /// `remove_file` are ever called — so a live socket sitting at the
    /// runtime path during activation must come out the other side
    /// completely undisturbed, checked here the same two ways
    /// `a_live_listener_is_refused_without_being_unlinked` checks the
    /// standalone branch's own live-listener case: by inode, and by proving
    /// it still accepts.
    #[tokio::test]
    async fn with_an_inherited_fd_a_live_socket_at_the_runtime_path_is_left_completely_alone() {
        let backing = tempfile::tempdir().unwrap();
        let std_listener =
            std::os::unix::net::UnixListener::bind(backing.path().join("activated.sock")).unwrap();
        let fd = std_listener.into_raw_fd();

        // The runtime dir this activation call is handed also has a live,
        // already-bound listener sitting at exactly the path the standalone
        // branch would probe — and, finding it live, refuse to unlink.
        // Nothing here ever calls `accept` on it before `acquire_listener`
        // returns, because the point is proving the activation branch never
        // reaches for it in the first place.
        let runtime_dir = tempfile::tempdir().unwrap();
        let live_path = runtime_dir.path().join(SOCKET_FILE_NAME);
        let live_listener = tokio::net::UnixListener::bind(&live_path).unwrap();
        let original_inode = std::fs::metadata(&live_path).unwrap().ino();

        let result = acquire_listener(runtime_dir.path(), Some(InheritedFd { fd, declared: 1 }));
        assert!(
            result.is_ok(),
            "activation must succeed regardless of what sits at the runtime-dir path: {:?}",
            result.err()
        );

        // The live socket must be untouched: same inode, and still able to
        // accept — not merely present on disk, but actually the thing
        // answering there, exactly as `probe_socket_liveness` would have
        // found had the standalone branch run instead.
        let inode_after = std::fs::metadata(&live_path).unwrap().ino();
        assert_eq!(
            original_inode, inode_after,
            "activation must not unlink or rebind the live socket at the runtime-dir path"
        );
        let accept_task = tokio::spawn(async move { live_listener.accept().await });
        let _client = tokio::net::UnixStream::connect(&live_path).await.unwrap();
        let accepted = accept_task.await.unwrap();
        assert!(
            accepted.is_ok(),
            "the live listener at the runtime-dir path must still accept, untouched by \
             activation: {:?}",
            accepted.err()
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
