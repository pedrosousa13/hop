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
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, ErrorCode, Kind, Mode, QueryText};

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
/// `HOME`, `XDG_DATA_HOME`, `XDG_DATA_DIRS`, and — since issue #60 made
/// `hopd` resolve a config and state dir at startup — `XDG_CONFIG_HOME` and
/// `XDG_STATE_HOME`, are pinned to paths under `runtime_dir` that this test
/// never populates, rather than left to whatever the developer or CI box
/// running this suite happens to have set.
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
/// Builds a `Command` for `hopd`, pinned to the same five isolated
/// environment roots under `runtime_dir` both process-spawning helpers in
/// this file need: `HOME`, `XDG_DATA_HOME`, `XDG_DATA_DIRS`,
/// `XDG_CONFIG_HOME` and `XDG_STATE_HOME`, alongside `XDG_RUNTIME_DIR`
/// itself — see [`spawn_daemon`]'s own doc comment for why each is pinned
/// rather than left to whatever the developer or CI box running this suite
/// happens to have set.
///
/// [`spawn_daemon`] and [`run_second_daemon_to_completion`] used to each
/// build this same five-`env()` `Command` verbatim; a sixth variable added
/// to one and not the other is exactly the drift this file's own
/// `tests/common/mod.rs` sibling exists to prevent across files, and there
/// is no reason to tolerate it within one. What genuinely differs between
/// the two callers — stdio handling, and how each waits for the process —
/// stays with each of them: only the command construction itself was ever
/// duplicated, so only that is shared here.
fn hopd_command(runtime_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hopd"));
    command
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
        );
    command
}

fn spawn_daemon(runtime_dir: &Path) -> DaemonProcess {
    // `state_dir::resolve` treats a missing parent *base* directory as an
    // error — it creates only the `hop` dir inside it, not recursively — so
    // the isolated state-home root must already exist, or the daemon would
    // refuse to start before it ever binds a socket. `config::load` has no
    // such requirement: `fs::read_to_string` returns `NotFound` just the same
    // whether the leaf file or an ancestor directory is missing, and
    // `config::load` maps any `NotFound` to `Ok(Config::default())` — so
    // pre-creating the config root here is not load-bearing for it. It is
    // done anyway to keep the two isolated roots symmetric.
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-config-home")).unwrap();

    let child = hopd_command(runtime_dir)
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

/// Spawns a second `hopd` against `runtime_dir` — reusing the exact
/// isolated env roots [`spawn_daemon`] already created there for a first
/// daemon — and waits for it to exit, panicking after 5s if it never does.
///
/// `Command::output()` is not used here: it waits for the child
/// unconditionally, and before issue #158's fix this second daemon would
/// unlink the first daemon's socket and then bind and serve forever, so
/// `output()` would hang the test suite rather than fail it. Polling
/// `try_wait` at 100ms up to 50 times mirrors [`spawn_daemon`]'s own bound
/// on the opposite wait (a socket that should appear); a refused daemon's
/// connect probe is local and near-instant, so 5s is generous, not tight.
/// The command itself is [`hopd_command`], shared with [`spawn_daemon`];
/// only this wait shape — polling for exit instead of for a socket — is
/// this helper's own.
fn run_second_daemon_to_completion(runtime_dir: &Path) -> std::process::Output {
    let mut child = hopd_command(runtime_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn the second hopd");

    for _ in 0..50 {
        if let Some(_status) = child.try_wait().expect("failed to poll the second hopd") {
            return child
                .wait_with_output()
                .expect("failed to collect the second hopd's output after it exited");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "the second hopd did not exit within 5s against a live socket under {runtime_dir:?} — \
         a refused standalone daemon must exit almost immediately, not bind and serve"
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

    // #127: the routed frame opens every exchange, ahead of results. This is
    // the walking skeleton's end-to-end round trip, so pinning it here means
    // the very first test anyone reads about this protocol shows the real frame
    // order. "walking skeleton" names no mode, so it reaches the `All`
    // fallback, which is never exclusive.
    assert_eq!(
        recv(&mut stream),
        DaemonMsg::QueryRouted {
            query_id: 7,
            mode: Mode::All,
            exclusive: false,
        }
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
            .any(|item| item.title.as_str() == "Hello from hopd" && item.kind == Kind::Action),
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
fn a_65th_connection_waits_until_one_of_64_slots_is_released() {
    const EXPECTED_CONNECTION_LIMIT: usize = 64;

    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());

    let mut admitted = Vec::with_capacity(EXPECTED_CONNECTION_LIMIT);
    for _ in 0..EXPECTED_CONNECTION_LIMIT {
        let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
        hello(&mut stream);
        admitted.push(stream);
    }

    let mut waiting = UnixStream::connect(&daemon.socket_path).unwrap();
    send(
        &mut waiting,
        &ClientMsg::Hello {
            api_version: API_VERSION,
        },
    );
    waiting
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let mut prefix = [0_u8; 4];
    let blocked = waiting.read_exact(&mut prefix).unwrap_err();
    assert!(matches!(
        blocked.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));

    drop(admitted.pop());
    waiting
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(
        recv(&mut waiting),
        DaemonMsg::HelloAck {
            api_version: API_VERSION,
        }
    );
}

#[test]
fn an_inbound_frame_one_byte_over_64_kib_is_refused_from_prefix_alone() {
    const EXPECTED_MAX_INBOUND_FRAME_BYTES: usize = 65_536;

    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    // The handshake gate would also refuse a technically-valid frame sent
    // before `Hello`, so completing it first is what isolates the frame-size
    // cap as the thing under test.
    hello(&mut stream);

    let over_cap_len = (EXPECTED_MAX_INBOUND_FRAME_BYTES as u32) + 1;
    stream
        .write_all(&over_cap_len.to_be_bytes())
        .expect("writing the oversize prefix must succeed");
    // Deliberately nothing else is written: the daemon must refuse on the
    // prefix alone, never asking for the payload that would follow it. The
    // shared `payload_len` gate runs first, and the hopd-only inbound gate
    // then refuses this 65,537-byte value before allocation; the former's
    // 256 MiB ceiling alone would admit this prefix. The allocation itself is
    // a property this test cannot observe directly; what it can and does
    // observe is that the connection is refused and closed without hanging on
    // a payload read.

    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let reply = recv(&mut stream);
    let DaemonMsg::Error { error, .. } = reply else {
        panic!("expected an error frame, got {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::FrameTooLarge);

    assert_eof(&mut stream);
}

#[test]
fn an_inbound_frame_exactly_64_kib_is_read_then_refused_as_malformed() {
    const EXPECTED_MAX_INBOUND_FRAME_BYTES: usize = 65_536;

    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    hello(&mut stream);

    let payload = vec![b'x'; EXPECTED_MAX_INBOUND_FRAME_BYTES];
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .expect("writing the exact-boundary prefix must succeed");
    stream
        .write_all(&payload)
        .expect("writing the exact-boundary payload must succeed");

    let reply = recv(&mut stream);
    let DaemonMsg::Error { error, .. } = reply else {
        panic!("expected an error frame, got {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::MalformedFrame);

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
    // prefix-only refusal `an_inbound_frame_one_byte_over_64_kib_is_refused_from_prefix_alone`
    // covers,
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

/// Issue #84's flagged hazard: `decode_payload`'s failure path used to build
/// `message` from `err.to_string()`, and a `serde_json` "unknown variant"
/// error echoes back the exact text a peer sent for the tag it did not
/// recognize — verified directly against `decode_payload` while designing
/// this test: `{"type":"XYZZY..."}"` produces `unknown variant`
/// `XYZZY...`, expected one of ...`. That distinctive text is peer input
/// here, not a daemon secret, but it stands for the whole class of thing
/// this issue closes: whatever a parse failure's `Display` happens to
/// contain no longer has any path into a client-facing frame, because
/// `message` is now derived from a fixed [`hop_protocol::ErrorDetail`]
/// rather than from the error itself.
#[test]
fn a_distinctive_value_in_a_malformed_payload_does_not_survive_into_the_error_message() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();

    hello(&mut stream);

    let marker = "XYZZY_DISTINCTIVE_MARKER_998877";
    let payload = format!(r#"{{"type":"{marker}"}}"#).into_bytes();
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .expect("writing the prefix must succeed");
    stream
        .write_all(&payload)
        .expect("writing the malformed payload must succeed");

    let reply = recv(&mut stream);
    let DaemonMsg::Error { error, .. } = reply else {
        panic!("expected an error frame, got {reply:?}");
    };
    assert_eq!(error.code, ErrorCode::MalformedFrame);
    assert!(
        !error.message().contains(marker),
        "the malformed-frame message must not echo the peer's bytes, got: {:?}",
        error.message()
    );

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
    // This test does not use `spawn_daemon`: it wants the daemon's real
    // stderr (`spawn_daemon` discards both streams), and it does not wait for
    // a socket that this daemon must never bind. But since issue #60, `run()`
    // resolves `config::load()` and `state_dir::resolve()` *before* it ever
    // checks `XDG_RUNTIME_DIR` — so, exactly like `spawn_daemon`, this process
    // must pin `HOME`/`XDG_CONFIG_HOME`/`XDG_STATE_HOME` to an isolated temp
    // dir rather than inherit the real test process's environment. Left
    // unpinned, this test would read the developer's real `~/.config/hop` and
    // create `~/.local/state/hop` as a side effect, and — if that real config
    // happened to be malformed — would fail on the wrong assertion entirely
    // (a config-parse error, not a missing-`XDG_RUNTIME_DIR` one).
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(temp.path().join("isolated-xdg-config-home")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_hopd"))
        .env_remove("XDG_RUNTIME_DIR")
        .env("HOME", temp.path().join("isolated-home"))
        .env(
            "XDG_CONFIG_HOME",
            temp.path().join("isolated-xdg-config-home"),
        )
        .env(
            "XDG_STATE_HOME",
            temp.path().join("isolated-xdg-state-home"),
        )
        .env("XDG_DATA_DIRS", "")
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

#[test]
fn a_malformed_config_is_a_startup_error() {
    // Issue #60 criterion 2: a config that exists but does not parse must
    // refuse to start the daemon loudly, never fall back to defaults. Config
    // resolves ahead of the runtime dir in `run()`, so this daemon must exit
    // before binding a socket at all — the socket-path assertion below is
    // the direct proof of that, and the stderr naming the offending file is
    // the proof it got as far as reading it.
    let temp = tempfile::tempdir().unwrap();
    let config_root = temp.path().join("isolated-xdg-config-home");
    let config_dir = config_root.join("hop");
    std::fs::create_dir_all(&config_dir).unwrap();
    // `max_results = =` is not valid TOML: an `=` where a value is expected.
    std::fs::write(config_dir.join("config.toml"), "max_results = =\n").unwrap();

    let runtime_dir = temp.path().join("runtime");
    let output = Command::new(env!("CARGO_BIN_EXE_hopd"))
        .env("XDG_CONFIG_HOME", &config_root)
        .env(
            "XDG_STATE_HOME",
            temp.path().join("isolated-xdg-state-home"),
        )
        .env("HOME", temp.path().join("isolated-home"))
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_DATA_DIRS", "")
        .output()
        .expect("failed to run hopd");

    assert!(
        !output.status.success(),
        "hopd must exit non-zero on a malformed config"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("config.toml") && stderr.contains("not valid TOML"),
        "stderr must name the malformed config and say it did not parse, got: {stderr}"
    );

    // `output()` already waited for the process to exit, so this is not a
    // race against a still-running daemon: a malformed config must never
    // reach the point of binding a socket at all, in `runtime_dir/hop/`
    // (the same layout `spawn_daemon` and every other test here expects).
    let socket_path = runtime_dir.join("hop").join("hopd.sock");
    assert!(
        !socket_path.exists(),
        "a malformed config must be refused before any socket is bound, found {socket_path:?}"
    );
}

/// Issue #158's central acceptance criterion, proven end to end with two
/// real `hopd` processes rather than by asserting on `acquire_listener`'s
/// return value alone: a second standalone daemon started against a
/// still-live socket must not be able to take over its reachable name,
/// existing clients of the first daemon must keep working straight through
/// the second daemon's failed startup attempt, and a brand new client must
/// still land on the first daemon rather than silently switching to a
/// second one that never actually bound anything.
#[test]
fn a_second_standalone_daemon_against_a_live_socket_is_refused_and_the_first_keeps_serving() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());

    // A connection opened *before* the second daemon's startup attempt.
    // If this stopped working afterward, that alone would prove the first
    // listener's identity was disturbed — a stronger claim than a bare
    // "bind failed" assertion could make.
    let mut existing_client = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut existing_client);

    let original_inode = std::fs::metadata(&daemon.socket_path).unwrap().ino();

    // A second standalone hopd, started against the exact same
    // `XDG_RUNTIME_DIR` — and so the exact same socket path — as the first,
    // which is still live. `spawn_daemon` itself is not used here: it polls
    // for a socket to appear and panics if one never does, but a correctly
    // refused daemon must never bind one; `run_second_daemon_to_completion`
    // is this test's mirror-image wait, on the process exiting instead. The
    // isolated env roots it reuses (`isolated-home`, `isolated-xdg-*`) are
    // the exact ones `spawn_daemon` already created for the first daemon
    // under this same `runtime_dir` — nothing about this test needs the
    // second process's state or config roots to differ from the first's.
    let output = run_second_daemon_to_completion(runtime_dir.path());

    assert!(
        !output.status.success(),
        "a second standalone hopd against a live socket must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already listening"),
        "stderr must diagnose the refusal as a daemon already listening, got: {stderr}"
    );
    assert!(
        !stderr.to_lowercase().contains("permission"),
        "the refusal must read as its own diagnosis, not a permission or I/O failure, got: {stderr}"
    );

    // The first listener's socket path must be untouched by the refused
    // second daemon: same inode as before the attempt.
    let inode_after_attempt = std::fs::metadata(&daemon.socket_path).unwrap().ino();
    assert_eq!(
        original_inode, inode_after_attempt,
        "the first listener's socket path must not have been unlinked or rebound"
    );

    // The connection opened before the second daemon's attempt must still
    // work — proof the *first* daemon, not a replacement, is still serving.
    send(
        &mut existing_client,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("walking skeleton").unwrap(),
        },
    );
    let routed = recv(&mut existing_client);
    assert!(
        matches!(routed, DaemonMsg::QueryRouted { .. }),
        "the pre-existing connection must still be answered by the original daemon, got {routed:?}"
    );

    // A brand new client, connecting only after the second daemon's failed
    // attempt, must also reach the first daemon — not silently be routed to
    // a second one that never actually bound anything. The inode check above
    // already proves the listener at this path was never replaced; sending a
    // real query here and asserting it is answered proves the further thing
    // that check cannot: that a client reaching that unchanged listener is
    // actually served by it, the same way the pre-existing client is checked
    // just above, rather than a connection that completes a handshake and
    // then goes nowhere.
    let mut new_client = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut new_client);
    send(
        &mut new_client,
        &ClientMsg::Query {
            id: 2,
            text: QueryText::new("walking skeleton").unwrap(),
        },
    );
    let routed = recv(&mut new_client);
    assert!(
        matches!(routed, DaemonMsg::QueryRouted { .. }),
        "a brand new connection must also be answered by the original daemon, got {routed:?}"
    );
}
