//! End-to-end tests that drive the real `hop` binary against a real `hopd`
//! process over a real Unix socket. Nothing here mocks the daemon or the
//! transport: `the_cli_query_round_trips_and_exits_zero` spawns
//! `env!("CARGO_BIN_EXE_hop")` as a subprocess and inspects the exit code
//! and stdout it produces, the same way a shell script driving both
//! binaries would.
#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::limits::{MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, Item};

/// `CARGO_BIN_EXE_hopd` is not set here: Cargo only defines a
/// `CARGO_BIN_EXE_<bin>` variable for binaries the *current* package builds,
/// and `hopd` belongs to the `hopd` crate, not `hop-cli`. What Cargo does
/// give this test is `CARGO_BIN_EXE_hop`, and under any build that also
/// built `hopd` the two binaries land as siblings in the same output
/// directory — which is the case `cargo test --workspace` guarantees, since
/// it builds every workspace member's binaries before running any test.
/// `cargo test -p hop-cli` alone does not build `hopd` at all, so the
/// `assert!` below turns that corner into a named failure instead of a
/// confusing "No such file or directory" from `Command::spawn`.
fn hopd_binary_path() -> PathBuf {
    let hop_path = Path::new(env!("CARGO_BIN_EXE_hop"));
    let hopd_path = hop_path.parent().unwrap().join("hopd");
    assert!(
        hopd_path.exists(),
        "hopd binary not built — run cargo test --workspace"
    );
    hopd_path
}

/// A spawned `hopd`, killed on drop so a failing assertion never leaks a
/// daemon process into the rest of the suite.
///
/// This mirrors `crates/hopd/tests/socket.rs`'s `DaemonProcess` /
/// `spawn_daemon` almost exactly. They are duplicated rather than shared: a
/// test-only helper this small (~20 lines) does not carry its weight as a
/// third crate, and the two copies drift only if the daemon's startup
/// contract changes, which is exactly when a diff in both files is the
/// useful signal.
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

/// Spawns `hopd` at `hopd_path` with `XDG_RUNTIME_DIR` set to `runtime_dir`,
/// and polls for its socket to appear before handing the process back. Same
/// 100ms/50-attempt (5s total) shape as `hopd/tests/socket.rs`'s helper, for
/// the same reason: fast on the common case, bounded on a stuck one.
fn spawn_daemon(hopd_path: &Path, runtime_dir: &Path) -> DaemonProcess {
    let child = Command::new(hopd_path)
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

    panic!(
        "hopd socket did not appear at {:?} within 5s",
        process.socket_path
    );
}

#[test]
fn the_cli_query_round_trips_and_exits_zero() {
    let hopd_path = hopd_binary_path();
    let runtime_dir = tempfile::tempdir().unwrap();
    let _daemon = spawn_daemon(&hopd_path, runtime_dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("hello")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .expect("failed to run hop query");

    assert!(
        output.status.success(),
        "hop query must exit 0, got {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one line of stdout, got: {stdout:?}"
    );

    let item: Item = serde_json::from_str(lines[0]).expect("line must parse as an Item");
    assert_eq!(item.title, "Hello from hopd");
}

#[test]
fn the_version_subcommand_prints_both_versions() {
    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("version")
        .output()
        .expect("failed to run hop version");

    assert!(
        output.status.success(),
        "hop version must exit 0, got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout must contain the CLI's own version, got: {stdout:?}"
    );
    assert!(
        stdout.contains("protocol 1"),
        "stdout must contain the protocol version, got: {stdout:?}"
    );
}

/// A scripted daemon: binds the socket where `hop` will look, accepts one
/// connection, answers the handshake, hands the accepted stream to `script`,
/// and keeps listening so the CLI's whole exchange happens against bytes
/// this test chose. Runs on a thread; joined via the returned handle so a
/// panic inside the script fails the test instead of vanishing.
fn fake_daemon(
    runtime_dir: &Path,
    script: impl FnOnce(&mut std::os::unix::net::UnixStream, u64) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let hop_dir = runtime_dir.join("hop");
    std::fs::create_dir_all(&hop_dir).unwrap();
    let listener = UnixListener::bind(hop_dir.join("hopd.sock")).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Handshake: expect Hello, answer HelloAck.
        let hello = read_client_frame(&mut stream);
        assert!(matches!(hello, ClientMsg::Hello { .. }));
        write_daemon_frame(
            &mut stream,
            &DaemonMsg::HelloAck {
                api_version: API_VERSION,
            },
        );
        // Expect the query; its id is what the script frames must reference.
        let ClientMsg::Query { id, .. } = read_client_frame(&mut stream) else {
            panic!("expected the query frame after the handshake");
        };
        script(&mut stream, id);
    })
}

fn read_client_frame(stream: &mut std::os::unix::net::UnixStream) -> ClientMsg {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream.read_exact(&mut prefix).unwrap();
    let len = payload_len(prefix).unwrap();
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    decode_payload(&payload).unwrap()
}

fn write_daemon_frame(stream: &mut std::os::unix::net::UnixStream, msg: &DaemonMsg) {
    stream.write_all(&encode_frame(msg).unwrap()).unwrap();
}

fn tiny_item(n: usize, title: &str) -> Item {
    use hop_protocol::{Action, ActionId, ActionKind, ItemId, Kind};
    Item {
        id: ItemId::new(format!("test:{n}")).unwrap(),
        kind: Kind::Action,
        title: title.to_string(),
        subtitle: None,
        icon: None,
        actions: vec![Action {
            id: ActionId::new("open").unwrap(),
            kind: ActionKind::Open,
            label: "Open".to_string(),
        }],
        default_action: ActionId::new("open").unwrap(),
        copy_text: None,
        append_to_end: false,
        provider: "test".to_string(),
    }
}

#[test]
fn the_cli_drops_frames_whose_query_id_is_not_current() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        // A stale frame (wrong id) before, between, and after the real ones:
        // none of the "stale" titles may reach stdout.
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id + 1,
                partial: true,
                items: vec![tiny_item(1, "stale before")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(2, "current one")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id + 1,
                partial: true,
                items: vec![tiny_item(3, "stale between")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(4, "current two")],
            },
        );
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id + 1 }); // stale done: must NOT end the query
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id });
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    daemon.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("current one") && stdout.contains("current two"));
    assert!(
        !stdout.contains("stale"),
        "a stale frame's items must never be rendered, got: {stdout}"
    );
    // Assembled output: both current items, in delivery order.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn the_cli_refuses_a_daemon_that_streams_past_the_per_query_cap() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        // One item over the cap, delivered as six frames — each frame is
        // individually in-bounds; only the exchange total is not.
        let full: Vec<Item> = (0..MAX_ITEMS_PER_RESULTS_FRAME)
            .map(|n| tiny_item(n, "x"))
            .collect();
        for _ in 0..5 {
            write_daemon_frame(
                stream,
                &DaemonMsg::Results {
                    query_id: id,
                    partial: true,
                    items: full.clone(),
                },
            );
        }
        // Last frame pushes the exchange total one past the cap. The CLI is
        // expected to have already bailed by the time this write happens, so
        // it must tolerate a broken pipe rather than unwrap — unlike every
        // other write in this file, this one does not use the panicking
        // helper.
        let frame = encode_frame(&DaemonMsg::Results {
            query_id: id,
            partial: true,
            items: vec![tiny_item(9, "the straw")],
        })
        .unwrap();
        let _ = stream.write_all(&frame);
        // No QueryDone: the CLI must have bailed already; writing more would
        // hit a closed pipe.
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    let _ = daemon.join();

    assert!(
        !output.status.success(),
        "an over-cap stream must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&MAX_ITEMS_PER_QUERY.to_string()),
        "the refusal must name the cap, got: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing may be printed for a query that was refused mid-assembly"
    );
}
