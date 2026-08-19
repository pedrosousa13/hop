//! The CI headless smoke test the design spec's §11 makes non-optional:
//! drives `hop-gtk` far enough, headless, to capture at least the empty
//! state and a results state — acceptance criterion 8 — using
//! `hop-gtk --screenshot <path>` itself, acceptance criterion 7's
//! implementation, against a real `hopd` built from this workspace,
//! acceptance criterion 6.
//!
//! # Why a subprocess per screenshot rather than driving `hop_gtk::app` in-process
//!
//! GTK is not safely re-initializable within one process — `gtk::init()` (and
//! the `adw::Application::run` this crate builds on) assumes it owns the
//! process's main loop and display connection for the program's lifetime.
//! Two states means two headless-backend runs, and `cargo test` runs every
//! test (and, within one binary, every `#[test]` function) in the same
//! process by default — spawning `hop-gtk --screenshot` as a real subprocess
//! per state sidesteps that entirely, and is also the literal shape
//! acceptance criterion 7 describes: "writes a PNG ... and exits", exercised
//! exactly as an agent or a CI job would run it, not as a function call this
//! test happens to make from inside itself.
//!
//! # Which headless backend, and why `gtk4-broadwayd` specifically
//!
//! `app::run_screenshot`'s own doc comment has the full account: this
//! issue's environment does not have GTK4's `offscreen` backend compiled in
//! (Ubuntu's `libgtk-4-1` package only builds `x11`, `wayland`, `broadway`),
//! so this test drives `broadway` instead. The one sharp edge worth
//! repeating here because it is easy to hit by accident: the `broadwayd` on
//! `$PATH` on a Debian/Ubuntu box is `libgtk-3-bin`'s server, and it speaks
//! a protocol GTK4 clients cannot connect to (a `connect()` to the wrong
//! socket shape, observed directly with `strace` while this was being
//! diagnosed). The binary that actually answers a GTK4 `broadway` client is
//! `gtk4-broadwayd`, from `libgtk-4-bin` — this is the one this file spawns.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A spawned `gtk4-broadwayd`, killed on drop. Display number is derived
/// from this process's own pid so parallel `cargo test` invocations (a
/// second workspace checkout, a second CI shard) do not collide on the same
/// display.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    /// `runtime_dir` must be the same `XDG_RUNTIME_DIR` the `hop-gtk`
    /// subprocesses in this test are given: broadway's socket resolves
    /// under `$XDG_RUNTIME_DIR` on both the server and client side, and
    /// this test already overrides that variable to an isolated tempdir for
    /// [`spawn_daemon`]'s sake (so a real `hopd` cannot collide with an
    /// unrelated one on the same machine). Starting `gtk4-broadwayd` against
    /// the *ambient* `XDG_RUNTIME_DIR` instead — the first shape this test
    /// was written with — silently fails: the server binds its socket under
    /// the real runtime dir, the `hop-gtk` client looks under the isolated
    /// one because that is the only `XDG_RUNTIME_DIR` its `Command` was
    /// given, and neither side reports a name mismatch — GDK just reports
    /// "Failed to open display", which reads exactly like the demonstrably
    /// wrong direction (backend or protocol) to be debugging in.
    fn start(runtime_dir: &Path) -> Self {
        let display = 100 + (std::process::id() % 5000);
        let child = Command::new("gtk4-broadwayd")
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .arg(format!(":{display}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect(
                "failed to spawn gtk4-broadwayd — it ships in libgtk-4-bin \
                 (NOT `broadwayd` on $PATH, which on Debian/Ubuntu is \
                 libgtk-3-bin's incompatible GTK3 server; see this file's \
                 top doc comment)",
            );
        // `gtk4-broadwayd` creates its listening socket asynchronously
        // after `spawn()` returns, the same reason
        // `crates/hopd/tests/socket.rs`'s `spawn_daemon` polls for a socket
        // file rather than assuming one exists the instant the child
        // starts — broadway's socket lives in the abstract namespace, so it
        // cannot be polled for by `Path::exists`; a short fixed wait stands
        // in for that poll instead.
        std::thread::sleep(Duration::from_millis(300));
        BroadwayServer { child, display }
    }

    fn env(&self) -> [(&'static str, String); 2] {
        [
            ("GDK_BACKEND", "broadway".to_string()),
            ("BROADWAY_DISPLAY", format!(":{}", self.display)),
        ]
    }
}

impl Drop for BroadwayServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A spawned `hopd`, killed on drop — see `crates/hopd/tests/socket.rs`'s
/// `DaemonProcess` for the identical shape and the reasoning behind it
/// (owning the child behind a `Drop` impl is what keeps a failing assertion
/// from leaking the daemon into the rest of the test run).
struct DaemonProcess {
    child: Child,
    socket_path: PathBuf,
    runtime_dir: PathBuf,
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `hopd`'s executable path, located as `hop-gtk`'s own sibling in the
/// shared workspace target directory rather than through
/// `env!("CARGO_BIN_EXE_hopd")` — Cargo only sets a `CARGO_BIN_EXE_<name>`
/// variable for a package's *own* binary targets, never a dependency's, so
/// that macro is unavailable for a binary belonging to another crate. This
/// crate's `Cargo.toml` declares `hopd` as a `dev-dependency` purely to
/// guarantee Cargo builds it before this test binary runs (so it exists at
/// this path in time), even under `cargo test -p hop-gtk` run on its own —
/// see that `Cargo.toml` entry's own comment.
fn hopd_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_hop-gtk"));
    path.set_file_name(if cfg!(windows) { "hopd.exe" } else { "hopd" });
    path
}

/// Spawns an isolated `hopd` (own `XDG_RUNTIME_DIR` and friends, exactly
/// like `crates/hopd/tests/socket.rs`'s own `spawn_daemon` — duplicated
/// rather than shared because that helper is private to `hopd`'s own test
/// crate) and polls for its socket to appear.
fn spawn_daemon(runtime_dir: &Path) -> DaemonProcess {
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-config-home")).unwrap();

    let child = Command::new(hopd_path())
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
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn hopd");

    let socket_path = runtime_dir.join("hop").join("hopd.sock");
    let process = DaemonProcess {
        child,
        socket_path,
        runtime_dir: runtime_dir.to_path_buf(),
    };

    for _ in 0..50 {
        if process.socket_path.exists() {
            return process;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("hopd did not create its socket in time");
}

/// Runs `hop-gtk --screenshot <out_path> [--query <query>]` as a real
/// subprocess against `daemon`, pointed at `broadway`'s headless display,
/// and asserts it exits successfully.
fn run_screenshot(
    daemon: &DaemonProcess,
    broadway: &BroadwayServer,
    out_path: &Path,
    query: Option<&str>,
) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hop-gtk"));
    command
        .env("XDG_RUNTIME_DIR", &daemon.runtime_dir)
        .envs(broadway.env())
        .arg("--screenshot")
        .arg(out_path);
    if let Some(query) = query {
        command.arg("--query").arg(query);
    }

    let output = command.output().expect("failed to run hop-gtk");
    assert!(
        output.status.success(),
        "hop-gtk --screenshot exited with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Asserts `path` exists and starts with the PNG magic bytes — a real,
/// non-empty PNG file, not just a file that happens to exist.
fn assert_is_a_png(path: &Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("reading {path:?}: {err}"));
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    assert!(
        bytes.len() > PNG_MAGIC.len(),
        "{path:?} is too small to be a PNG ({} bytes)",
        bytes.len()
    );
    assert_eq!(
        &bytes[..PNG_MAGIC.len()],
        &PNG_MAGIC,
        "{path:?} does not start with the PNG magic bytes"
    );
}

#[test]
fn captures_the_empty_state_and_a_results_state_headless() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let broadway = BroadwayServer::start(runtime_dir.path());

    let out_dir = tempfile::tempdir().unwrap();
    let empty_state_png = out_dir.path().join("empty-state.png");
    let results_state_png = out_dir.path().join("results-state.png");

    // Empty-query state: nothing typed, whatever the freshly connected
    // window shows.
    run_screenshot(&daemon, &broadway, &empty_state_png, None);
    assert_is_a_png(&empty_state_png);

    // Results state: "2+2" is the same deterministic calculator query
    // `crates/hopd/tests/calculator.rs` drives against this same real
    // `build_host()` registry — no external state, no network, the same
    // answer on every run.
    run_screenshot(&daemon, &broadway, &results_state_png, Some("2+2"));
    assert_is_a_png(&results_state_png);

    // The two states are visually different renders, not the same frame
    // written twice — a coarse but meaningful check that content actually
    // reflects the driven state per acceptance criterion 6 rather than
    // `--screenshot` capturing a static, query-independent window.
    let empty_bytes = std::fs::read(&empty_state_png).unwrap();
    let results_bytes = std::fs::read(&results_state_png).unwrap();
    assert_ne!(
        empty_bytes, results_bytes,
        "the empty and results screenshots must not be byte-identical"
    );
}
