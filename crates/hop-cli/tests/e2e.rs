//! End-to-end tests that drive the real `hop` binary against a real `hopd`
//! process over a real Unix socket. Nothing here mocks the daemon or the
//! transport: `the_cli_query_round_trips_and_exits_zero` spawns
//! `env!("CARGO_BIN_EXE_hop")` as a subprocess and inspects the exit code
//! and stdout it produces, the same way a shell script driving both
//! binaries would.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_protocol::Item;

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
