//! Integration tests that drive a real `hopd` process over its real Unix
//! socket. Nothing here mocks the transport: every test spawns
//! `env!("CARGO_BIN_EXE_hopd")` and talks to it with a blocking
//! `std::os::unix::net::UnixStream`, the same client shape `hop-cli` (Task 3)
//! will use, so a passing suite here is evidence the wire contract actually
//! works end to end rather than evidence a mock agreed with itself.
#![allow(clippy::unwrap_used)]

mod common;
use common::{hello, recv, send};

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_protocol::limits::MAX_FRAME_BYTES;
use hop_protocol::{ClientMsg, DaemonMsg, ErrorCode, Kind, QueryText};

/// A spawned `hopd`, and the path its socket should appear at.
///
/// Owning the [`Child`] behind a `Drop` impl is what keeps a failing test from
/// leaking a daemon process into the rest of the suite: whether a test
/// returns normally or panics partway through an assertion, unwinding runs
/// this `Drop` and kills the process. `wait()` after `kill()` reaps the
/// zombie rather than leaving it for the test harness to notice.
struct DaemonProcess {
    child: Child,
    socket_path: PathBuf,
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns `hopd` with `XDG_RUNTIME_DIR` set to `runtime_dir`, and polls for
/// its socket to appear before handing the process back.
///
/// The daemon's own startup is not synchronous from this process's point of
/// view — binding the listener happens after this function's `spawn()`
/// returns — so a fixed sleep would be either flaky (too short, on a loaded
/// CI box) or slow (long enough to never be flaky). Polling at 100ms up to
/// 50 times (5s total) is the brief's stated shape: fast on the common case,
/// bounded on a stuck one.
///
/// stdout and stderr are both discarded here. `hopd`'s only chatter under the
/// happy paths this helper serves is the `eprintln!` accept/connection-error
/// seam (behavior spec point 6), and letting that reach the test harness's
/// own stderr would make a passing suite's output look like a failing one.
/// The one test that needs the daemon's stderr —
/// `an_unset_runtime_dir_is_a_startup_error` — does not use this helper.
///
/// `HOME`, `XDG_DATA_HOME` and `XDG_DATA_DIRS` are pinned to paths under
/// `runtime_dir` that this test never populates, rather than left to
/// whatever the developer or CI box running this suite happens to have set.
/// Since issue #57, this spawned `hopd` registers a real, environment-backed
/// apps provider (`hopd::apps::build_apps_provider`) alongside the skeleton
/// one, and that provider answers from whatever `.desktop` files actually
/// exist under those roots — `the_round_trip_returns_one_item_end_to_end`
/// queries for "hello" and asserts exactly one item comes back, which would
/// silently start failing on any machine that happens to have an installed
/// application whose name, keywords or command contain that substring. This
/// closes the two roots `build_apps_provider` reads from `std::env`, so the
/// scan it does at startup always sees no applications, regardless of the
/// host machine's real `$HOME`. It cannot close the one root
/// `flatpak_application_roots` does not parameterize —
/// `/var/lib/flatpak/exports/share/applications`, a fixed system path — so a
/// machine with a Flatpak-installed app matching "hello" remains a residual,
/// unclosable risk, accepted rather than solved by inventing a seam
/// `build_apps_provider`'s deliberately parameterless signature does not
/// offer.
fn spawn_daemon(runtime_dir: &Path) -> DaemonProcess {
    let child = Command::new(env!("CARGO_BIN_EXE_hopd"))
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", runtime_dir.join("isolated-home"))
        .env("XDG_DATA_HOME", runtime_dir.join("isolated-xdg-data-home"))
        .env("XDG_DATA_DIRS", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn hopd");

    let socket_path = runtime_dir.join("hop").join("hopd.sock");
    let process = DaemonProcess { child, socket_path };

    for _ in 0..50 {
        if process.socket_path.exists() {
            return process;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // `process` still owns the child here, so panicking drops it and kills
    // the process this attempt leaked rather than leaving it running.
    panic!(
        "hopd socket did not appear at {:?} within 5s",
        process.socket_path
    );
}

/// Asserts the next read on `stream` returns 0 bytes — the daemon closed its
/// end of the connection after the error it just sent.
fn assert_eof(stream: &mut UnixStream) {
    let mut buf = [0u8; 1];
    let n = stream
        .read(&mut buf)
        .expect("read after close must not error");
    assert_eq!(n, 0, "expected EOF after hopd's error, got {n} byte(s)");
}

#[test]
fn the_round_trip_returns_one_item_end_to_end() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    hello(&mut stream);

    // `SkeletonProvider::query` (`source.rs`) ignores its query text and
    // always answers with the same hardcoded item, but the daemon now runs
    // that item through `Pipeline::assemble` (issue #103) before it reaches a
    // client, and `Ranker::rank` drops anything whose haystack does not
    // fuzzy-match the term — so a nonsense token that used to prove the round
    // trip by returning the one hardcoded item now correctly returns nothing.
    // "walking skeleton" is chosen instead because it matches the skeleton
    // item's haystack (`Hello from hopd` + `M2.2 walking skeleton`) on both
    // atoms, while an installed application whose title and subtitle contain
    // both "walking" and "skeleton" is implausible — but not impossible, so
    // the assertion below checks the list *contains* the item rather than
    // assuming it is the only one. That is also why this test does not assert
    // an exact list length: doing so would depend on what happens to be
    // installed on whatever machine runs it, which is the one root
    // `spawn_daemon` cannot isolate (its own doc comment above names it: the
    // hardcoded, unparameterized Flatpak system export directory).
    send(
        &mut stream,
        &ClientMsg::Query {
            id: 7,
            text: QueryText::new("walking skeleton").unwrap(),
        },
    );

    let results = recv(&mut stream);
    let DaemonMsg::Results {
        query_id,
        partial,
        items,
    } = results
    else {
        panic!("expected a results frame, got {results:?}");
    };
    assert_eq!(query_id, 7);
    assert!(
        partial,
        "streamed results frames are partial; QueryDone is the terminal signal"
    );
    assert!(
        items
            .iter()
            .any(|item| item.title == "Hello from hopd" && item.kind == Kind::Action),
        "expected the skeleton item among the results, got {items:?}"
    );

    let done = recv(&mut stream);
    assert_eq!(done, DaemonMsg::QueryDone { query_id: 7 });
}

#[test]
fn a_query_before_the_handshake_is_refused() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("too early").unwrap(),
        },
    );

    let reply = recv(&mut stream);
    let DaemonMsg::Error { error, .. } = reply else {
        panic!("expected an error frame, got {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::HandshakeRequired);

    assert_eof(&mut stream);
}

#[test]
fn an_oversize_length_prefix_is_refused_without_the_payload_being_read() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    // The handshake gate would also refuse a technically-valid frame sent
    // before `Hello`, so completing it first is what isolates the frame-size
    // cap as the thing under test.
    hello(&mut stream);

    let over_cap_len = (MAX_FRAME_BYTES as u32) + 1;
    stream
        .write_all(&over_cap_len.to_be_bytes())
        .expect("writing the oversize prefix must succeed");
    // Deliberately nothing else is written: the daemon must refuse on the
    // prefix alone, never asking for the payload that would follow it. That
    // it never allocates for that payload is enforced by construction —
    // `payload_len` runs before any read of it — and is a property this test
    // cannot observe directly; what it can and does observe is that the
    // connection is refused and closed without hanging on a payload read.

    let reply = recv(&mut stream);
    let DaemonMsg::Error { error, .. } = reply else {
        panic!("expected an error frame, got {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::FrameTooLarge);

    assert_eof(&mut stream);
}

#[test]
fn a_payload_that_is_not_valid_json_is_refused_as_malformed_not_internal() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    hello(&mut stream);

    // A correct length prefix in front of a payload that is not JSON at all:
    // this is a peer-fault failure at `decode_payload`, distinct from the
    // prefix-only refusal `an_oversize_length_prefix_is_refused...` covers,
    // and it must not be reported as `Internal` — that code names a bug in
    // hopd itself, not bytes a peer sent that hopd was never obligated to
    // make sense of.
    let payload = b"not json";
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .expect("writing the prefix must succeed");
    stream
        .write_all(payload)
        .expect("writing the malformed payload must succeed");

    let reply = recv(&mut stream);
    let DaemonMsg::Error { error, .. } = reply else {
        panic!("expected an error frame, got {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::MalformedFrame);

    assert_eof(&mut stream);
}

#[test]
fn a_version_mismatch_is_an_explicit_error() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    send(&mut stream, &ClientMsg::Hello { api_version: 999 });

    let reply = recv(&mut stream);
    let DaemonMsg::Error { error, .. } = reply else {
        panic!("expected an error frame, got {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::VersionMismatch);

    assert_eof(&mut stream);
}

#[test]
fn the_runtime_dir_is_created_at_mode_0700_and_the_socket_at_0600() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());

    let hop_dir = runtime_dir.path().join("hop");
    let dir_mode = std::fs::metadata(&hop_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "runtime dir must be born at 0700");

    let socket_mode = std::fs::metadata(&daemon.socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600, "socket file must be narrowed to 0600");
}

#[test]
fn an_unset_runtime_dir_is_a_startup_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_hopd"))
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .expect("failed to run hopd");

    assert!(
        !output.status.success(),
        "hopd must exit non-zero with no XDG_RUNTIME_DIR"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("XDG_RUNTIME_DIR"),
        "stderr must name the missing variable, got: {stderr}"
    );
}
