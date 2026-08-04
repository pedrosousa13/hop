//! Integration tests that drive a real `hopd` process over its real Unix
//! socket. Nothing here mocks the transport: every test spawns
//! `env!("CARGO_BIN_EXE_hopd")` and talks to it with a blocking
//! `std::os::unix::net::UnixStream`, the same client shape `hop-cli` (Task 3)
//! will use, so a passing suite here is evidence the wire contract actually
//! works end to end rather than evidence a mock agreed with itself.
#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::limits::MAX_FRAME_BYTES;
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, ErrorCode, Kind, QueryText};

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
fn spawn_daemon(runtime_dir: &Path) -> DaemonProcess {
    let child = Command::new(env!("CARGO_BIN_EXE_hopd"))
        .env("XDG_RUNTIME_DIR", runtime_dir)
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

/// Sends `msg` as one length-prefixed frame, through the same
/// [`hop_protocol::framing`] functions the daemon itself uses to decode —
/// so a test failure here means the wire contract broke, not that this
/// helper drifted from it.
fn send(stream: &mut UnixStream, msg: &ClientMsg) {
    let frame = encode_frame(msg).expect("test message must encode");
    stream
        .write_all(&frame)
        .expect("write to hopd must succeed");
}

/// Reads exactly one length-prefixed frame and decodes it as a [`DaemonMsg`].
fn recv(stream: &mut UnixStream) -> DaemonMsg {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream
        .read_exact(&mut prefix)
        .expect("hopd must reply with a frame");
    let len = payload_len(prefix).expect("hopd's own prefix must be in-cap");
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .expect("hopd's declared payload length must be honored");
    decode_payload(&payload).expect("hopd's reply must decode as a DaemonMsg")
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

fn hello(stream: &mut UnixStream) {
    send(
        stream,
        &ClientMsg::Hello {
            api_version: API_VERSION,
        },
    );
    let reply = recv(stream);
    assert_eq!(
        reply,
        DaemonMsg::HelloAck {
            api_version: API_VERSION
        }
    );
}

#[test]
fn the_round_trip_returns_one_item_end_to_end() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 7,
            text: QueryText::new("hello").unwrap(),
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
    assert!(!partial);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Hello from hopd");
    assert_eq!(items[0].kind, Kind::Action);

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
