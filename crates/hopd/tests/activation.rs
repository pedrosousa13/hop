//! An integration test proving hopd actually uses a listener inherited via
//! systemd's socket-activation protocol (sd_listen_fds(3)) — acceptance
//! criteria 2, 3 and 5 on issue #62 — not merely that
//! `hopd::activation::inherited_fd` parses the right environment variables
//! in isolation (that module's own unit tests) and not merely that a
//! listener built from an arbitrary raw fd works in-process
//! (`server.rs`'s own `acquire_listener_tests`). This spawns the real
//! `hopd` binary as a separate process with a real, already-bound-and-
//! listening `UnixListener` handed to it at file descriptor 3,
//! `LISTEN_FDS=1` and `LISTEN_PID` set to the daemon's own post-exec pid —
//! see this crate's implementation plan
//! (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
//! Design decision 5) for what this does and does not prove, and why this
//! is the mechanism used rather than a real systemd user session (this
//! crate's CI has none).

#![allow(clippy::unwrap_used)]

mod common;

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{recv, send};
use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, Mode, QueryText};

/// The same protocol constant `hopd::activation::SD_LISTEN_FDS_START`
/// names — duplicated here rather than imported, because that module is
/// `pub(crate)` inside `hopd` and this file is a separate crate. Fixed by
/// sd_listen_fds(3) itself, not a choice either side makes.
const SD_LISTEN_FDS_START: RawFd = 3;

/// A `hopd` started via a real inherited descriptor, deliberately not
/// `tests/socket.rs`'s `spawn_daemon` (which lets hopd bind its own
/// socket) — this helper's whole point is that hopd must *not* do that.
struct ActivatedDaemon {
    child: Child,
    socket_path: PathBuf,
}

impl Drop for ActivatedDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Binds the socket **in this test process** — the way a `.socket` unit's
/// own `ListenStream=`/`SocketMode=`/`DirectoryMode=` would have — then
/// spawns `hopd` with that descriptor duped onto fd 3 and
/// `LISTEN_FDS=1`/`LISTEN_PID=<hopd's own pid>` set.
///
/// # Why a shell wrapper, not `Command::new(hopd_path)` directly
///
/// `LISTEN_PID` must equal the pid `hopd` itself reads back from
/// `std::process::id()` — but `std::process::Command` fixes its child's
/// environment before `spawn()` returns, and the child's own pid is not
/// knowable until `spawn()` has already forked; there is no seam a caller
/// of `Command` can hook between "the fork happened" and "the child
/// execs" to inject a pid it just learned.
///
/// The fix used here: spawn `sh -c "export LISTEN_FDS=1; export
/// LISTEN_PID=$$; exec <hopd>"`. `$$` in a shell is the shell's own pid,
/// and `exec` replaces the shell's process image with `hopd`'s **without
/// changing the pid** — so the value `$$` resolves to immediately before
/// `exec` is exactly what `hopd` reads back from `std::process::id()`
/// afterward. Verified directly with a throwaway probe before writing this
/// into this crate's implementation plan: the exec'd process's own
/// `getpid()` matched `$$`, and a file descriptor `dup2`'d onto fd 3
/// before the outer `sh` starts survived the shell's own `exec` unchanged.
///
/// # The one `unsafe` in this file
///
/// `pre_exec`'s closure runs between `fork` and `exec` in the child, so it
/// must stick to async-signal-safe calls — `dup2`/`fcntl` are.
/// `CommandExt::pre_exec` is itself an `unsafe fn` for exactly this reason.
/// Test-only, the same footing as the workspace's other test-only
/// `unsafe` (`hop-protocol`'s `content.rs`, a `libc::mkfifo` call): neither
/// is production code, and both need a narrow `#[expect(unsafe_code)]` to
/// build at all under this workspace's `unsafe_code = "deny"` lint.
fn spawn_activated_daemon(runtime_dir: &Path) -> ActivatedDaemon {
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-config-home")).unwrap();

    let hop_dir = runtime_dir.join("hop");
    // Mirrors what the .socket unit's DirectoryMode=0700 would have
    // produced before hopd ever runs.
    std::fs::create_dir(&hop_dir).unwrap();
    std::fs::set_permissions(&hop_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let socket_path = hop_dir.join("hopd.sock");
    // Mirrors the .socket unit's own ListenStream=/SocketMode=0600: the
    // socket exists, bound and listening, before hopd ever starts.
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let raw_fd: RawFd = listener.as_raw_fd();

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "export LISTEN_FDS=1; export LISTEN_PID=$$; exec {:?}",
            env!("CARGO_BIN_EXE_hopd")
        ))
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", runtime_dir.join("isolated-home"))
        .env("XDG_DATA_HOME", runtime_dir.join("isolated-xdg-data-home"))
        .env("XDG_DATA_DIRS", "")
        .env(
            "XDG_CONFIG_HOME",
            runtime_dir.join("isolated-xdg-config-home"),
        )
        .env(
            "XDG_STATE_HOME",
            runtime_dir.join("isolated-xdg-state-home"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: the closure calls only dup2 and fcntl, both async-signal-safe
    // per signal-safety(7), between this process's fork and its exec — the
    // one window pre_exec exists for. It captures a plain integer
    // (`raw_fd`), no allocation or heap state.
    #[expect(
        unsafe_code,
        reason = "pre_exec is how a test hands a spawned process a pre-bound fd at a fixed \
                  number, reproducing sd_listen_fds(3) without a real systemd session; \
                  test-only, matching the precedent already set by hop-protocol's mkfifo test"
    )]
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(raw_fd, SD_LISTEN_FDS_START) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(SD_LISTEN_FDS_START, libc::F_GETFD);
            if flags < 0
                || libc::fcntl(
                    SD_LISTEN_FDS_START,
                    libc::F_SETFD,
                    flags & !libc::FD_CLOEXEC,
                ) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().expect("failed to spawn hopd via sh");
    // The parent's own copy of the listener is no longer needed once
    // spawn() has forked; the child's dup2'd copy at fd 3 is independent.
    drop(listener);

    ActivatedDaemon { child, socket_path }
}

/// Attempts one handshake without panicking, so [`connect_when_ready`] can
/// retry instead of failing on the first attempt that catches hopd
/// mid-startup. `common::hello` cannot be reused here — it `.expect()`s a
/// reply, which would panic on exactly the timeout this function needs to
/// treat as "not ready yet."
fn try_hello(stream: &mut UnixStream) -> bool {
    let Ok(frame) = encode_frame(&ClientMsg::Hello {
        api_version: API_VERSION,
    }) else {
        return false;
    };
    if stream.write_all(&frame).is_err() {
        return false;
    }
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    if stream.read_exact(&mut prefix).is_err() {
        return false;
    }
    let Ok(len) = payload_len(prefix) else {
        return false;
    };
    let mut payload = vec![0u8; len];
    if stream.read_exact(&mut payload).is_err() {
        return false;
    }
    matches!(decode_payload(&payload), Ok(DaemonMsg::HelloAck { .. }))
}

/// Connects and completes the handshake, retrying until it succeeds or the
/// budget runs out.
///
/// `socket_path.exists()` — `tests/socket.rs`'s own readiness check — is
/// not usable here: `UnixListener::bind`'s backlog accepts a `connect()`
/// the instant it is bound, which in this test happens **before hopd is
/// even spawned**, since this test does the binding itself. A completed
/// `hello`/`hello_ack` round trip is the earliest observable proof hopd's
/// accept loop is actually running over the inherited fd.
fn connect_when_ready(daemon: &ActivatedDaemon) -> UnixStream {
    for _ in 0..50 {
        if let Ok(mut stream) = UnixStream::connect(&daemon.socket_path) {
            stream
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            if try_hello(&mut stream) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                return stream;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("hopd (over the inherited listener) did not answer a handshake within 5s");
}

#[test]
fn a_query_over_an_inherited_listener_is_served_without_hopd_rebinding_the_socket() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_activated_daemon(runtime_dir.path());
    let ino_before = std::fs::metadata(&daemon.socket_path).unwrap().ino();

    let mut stream = connect_when_ready(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("walking skeleton").unwrap(),
        },
    );
    // `QueryRouted` is the first frame of any accepted query as of #127, ahead
    // of results and ahead of `QueryDone`. Asserted rather than skipped: this
    // test reads frames in order, so it is a free place to pin the ordering
    // rule that a mode label can be rendered before the first item arrives.
    // "walking skeleton" names no mode, so it routes to the `All` fallback,
    // non-exclusive.
    assert_eq!(
        recv(&mut stream),
        DaemonMsg::QueryRouted {
            query_id: 1,
            mode: Mode::All,
            exclusive: false,
            marker_span: None,
        }
    );

    let results = recv(&mut stream);
    let DaemonMsg::Results {
        query_id, items, ..
    } = results
    else {
        panic!("expected a results frame, got {results:?}");
    };
    assert_eq!(query_id, 1);
    assert!(
        items
            .iter()
            .any(|item| item.title.as_str() == "Hello from hopd"),
        "expected the skeleton item among the results, got {items:?}"
    );
    assert_eq!(recv(&mut stream), DaemonMsg::QueryDone { query_id: 1 });

    // Criterion 3, made specific: hopd used the fd this test handed it,
    // rather than falling back to standalone and coincidentally still
    // working at the same path. serve_with's standalone path always
    // removes and rebinds the socket file first (server.rs, unchanged by
    // this plan) — which would mint a *new* inode at the same path. An
    // unchanged inode is only possible if that removal never ran, i.e.
    // activation was genuinely taken.
    let ino_after = std::fs::metadata(&daemon.socket_path).unwrap().ino();
    assert_eq!(
        ino_before, ino_after,
        "the socket file must be the exact one this test bound, never rebound by hopd"
    );

    // Criterion 5, under activation specifically: this test process is the
    // one that bound the socket at 0600 and made the runtime directory at
    // 0700, so these assertions cannot prove activation *applies* those
    // modes — only that hopd's activation path leaves them as the service
    // manager set them, neither chmod'ing nor rebinding. In production
    // it's the shipped `.socket` unit's `SocketMode=0600` and
    // `DirectoryMode=0700` that enforce the modes, pinned by server.rs's
    // `the_socket_unit_declares_the_modes_activation_must_carry` and
    // applied by systemd itself — which nothing in this repository's
    // automated tests exercises.
    let socket_mode = std::fs::metadata(&daemon.socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600);
    let dir_mode = std::fs::metadata(runtime_dir.path().join("hop"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
}
