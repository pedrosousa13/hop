//! Binding the socket and accepting connections onto it.
//!
//! What happens *on* a connection — the handshake gate, the query lifecycle,
//! the framing calls that make both safe against a hostile peer — belongs to
//! the crate-private `connection` module. This module's whole job is the
//! listener: where the socket file lives, what mode it is born at, and one
//! spawned task per peer that turns up.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use hop_core::host::ProviderHost;
use tokio::net::UnixListener;

use crate::connection::handle_connection;
use crate::source::{HostSource, ResultSource, SkeletonProvider, StderrLog};

/// The socket's file name inside the runtime directory
/// [`crate::runtime_dir::resolve`] returns.
const SOCKET_FILE_NAME: &str = "hopd.sock";

/// Binds `<runtime_dir>/hopd.sock` and serves connections until an error
/// stops the accept loop or the process is killed — whichever comes first.
///
/// `runtime_dir` is assumed already created at 0700 by
/// [`crate::runtime_dir::resolve`]; this function does not create it, only
/// the socket file inside it.
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
/// The v1 spec fixes the runtime directory's mode at 0700 (which grants or
/// withholds *traverse*) and says nothing about the socket file's own mode,
/// which is what grants or withholds *connect* once traverse is granted —
/// left unstated, that mode would be whatever the process's umask happens to
/// produce. The threat model
/// (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "The
/// boundary") calls that out as a decision this slice must make rather than
/// inherit, so the socket file is explicitly narrowed to 0600 with
/// `set_permissions` right after `bind`. Between `bind` returning and that
/// call completing there is a brief window where the socket's own mode is
/// whatever the umask left it at — but the *directory* is already 0700 by
/// the time this function runs, and reaching a path inside it requires
/// traverse on every component, so the parent directory's mode is what
/// carries the access control during that window, not the socket file's.
pub async fn serve(runtime_dir: &Path) -> io::Result<()> {
    serve_with(runtime_dir, HostSource::new(Arc::new(build_host()))).await
}

/// Builds the daemon's provider host: the registry every query runs through.
///
/// Registration failures are a programming error rather than an operating
/// condition — the only ids registered here are literals in this function, so
/// a duplicate means two lines in this file chose the same one. It is reported
/// and the provider skipped rather than panicking, because a daemon that
/// refuses to start over one misconfigured provider is worse than one that
/// serves the rest: spec §9's per-provider isolation rule applied to startup.
fn build_host() -> ProviderHost {
    let mut host = ProviderHost::with_log(Arc::new(StderrLog));
    if let Err(err) = host.register(SkeletonProvider) {
        eprintln!("hopd: could not register the skeleton provider: {err}");
    }
    if let Err(err) = host.register(crate::apps::build_apps_provider()) {
        eprintln!("hopd: could not register the apps provider: {err}");
    }
    host
}

/// [`serve`], generic over what answers the queries.
///
/// This is the integration seam. A test injects a scripted [`ResultSource`]
/// here — one that streams several batches, or stalls, or floods past
/// [`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY) — and
/// then drives it over a real socket with a real client, so what the suite
/// pins is the daemon's actual wire behaviour rather than a mock agreeing
/// with itself. Everything else about the connection is the production path,
/// unchanged: the only thing a test gets to choose is where the items come
/// from.
///
/// [`serve`] passes a [`HostSource`] over the daemon's real provider host and
/// is what the binary runs; every behaviour documented on [`serve`] is
/// documented about this function too.
pub async fn serve_with<S: ResultSource>(runtime_dir: &Path, source: S) -> io::Result<()> {
    let socket_path = runtime_dir.join(SOCKET_FILE_NAME);

    match std::fs::remove_file(&socket_path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }

    let listener = UnixListener::bind(&socket_path)?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;

    // No accept-loop exit beyond an unrecoverable startup error above: a
    // per-connection failure is logged and the loop keeps accepting, so the
    // only way out of this loop is the process being killed. Signal handling
    // and any orderly shutdown belong to issue #62 (socket activation and
    // lifecycle), not this slice.
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                // One task per connection, per the brief's acceptance
                // criterion that the runtime be multi-threaded: unbounded
                // today, since a per-connection or per-daemon cap on
                // concurrent connections is issue #98's, not this slice's.
                //
                // Every connection gets its own handle on the source rather
                // than sharing one, which is why [`ResultSource`] is `Clone`:
                // an implementation is expected to be a cheap handle over
                // whatever shared state it has, not the state itself.
                let source = source.clone();
                tokio::spawn(async move {
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

#[cfg(test)]
mod build_host_tests {
    use super::*;

    #[test]
    fn build_host_registers_both_the_skeleton_and_apps_providers() {
        // Not a behavior test of AppsProvider itself (Task 5 already covers
        // that) — this pins that `build_host` actually calls the wiring
        // function this task adds, so a future edit that adds the function
        // but forgets to call it fails here rather than silently shipping a
        // daemon with no apps provider registered.
        let host = build_host();
        let ids: Vec<_> = host.manifests().iter().map(|m| m.id).collect();
        assert!(ids.contains(&"skeleton"));
        assert!(ids.contains(&hop_core::provider::APPS_PROVIDER_ID));
    }
}
