//! The X11 half of the headless proof: under a real Xvfb server, with
//! `GDK_BACKEND=x11`, `hop-gtk` maps its window centered at the overlay
//! size, dismisses it on focus loss, captures it with the existing
//! `--screenshot` harness, and round-trips a query → results → default
//! action over the socket — issue #232's acceptance criteria, exercised
//! against the one backend broadway cannot stand in for. Broadway is a
//! different display protocol entirely; only an X server can verify what
//! the X11 backend does with one.
//!
//! # Why Xvfb, and why this skips rather than fails without it
//!
//! `Xvfb` is not installed on every machine this suite runs on (issue
//! #179's environment note records the build machine as one of them — sudo
//! needs a password there, so it cannot be provisioned mid-test-run). Each
//! test below checks for the binaries it needs first and **returns early
//! with a printed reason** when they are absent: a skip that says why, not
//! a silent pass and not a red build on a machine that was never promised
//! an X server. CI (`.github/workflows/ci.yml`) installs `xvfb` (and
//! `dbus`, for the interactive arm's session bus) and makes the same tests
//! a hard requirement there.
//!
//! # No window manager on purpose
//!
//! Xvfb is run bare — no WM — which is exactly the harder of §2's two X11
//! environments: a WM would place (or at least frame) the default toplevel,
//! but with none, an un-positioned window sits at (0, 0) forever. Asserting
//! the window ends up centered *here* proves hop's own positioning works
//! rather than some WM's placement policy. The focus-loss arm likewise
//! cannot hide behind WM focus management: the test moves the X input focus
//! itself (`SetInputFocus`), which is the same primitive a WM would use.
//!
//! # Why subprocesses, like `headless_smoke.rs`
//!
//! Same reasoning, same precedent: GTK is not safely re-initializable
//! within one process, and `cargo test` runs every test in one process —
//! so `hop-gtk` is driven exactly as a user or CI would drive it, as a
//! real subprocess per state. The helpers this file needs (`hopd` spawn
//! with an isolated XDG tree, PNG magic checks) are duplicated from
//! `headless_smoke.rs` rather than shared for the reason that file's own
//! doc comments already recorded for `crates/hopd/tests/socket.rs`:
//! integration-test helpers are private to their own test crate, and the
//! duplication is the established, documented pattern here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hop_protocol::ExecOutcome;

use hop_gtk::ipc::{self, IpcCommand, IpcEvent};
use hop_gtk::tokens;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, InputFocus, MapState};
use x11rb::rust_connection::RustConnection;

/// How long any single "wait for the X server to reflect reality" poll may
/// run before the test gives up and fails with context. Generous on
/// purpose: CI runners are slow, and a flaky red here reads as a positioning
/// regression when it is really a scheduling hiccup.
const POLL_TIMEOUT: Duration = Duration::from_secs(15);

/// A spawned `Xvfb`, killed on drop — the same child-ownership shape
/// `headless_smoke.rs`'s `BroadwayServer` and `DaemonProcess` use, so a
/// failing assertion cannot leak a server into the rest of the run.
struct XvfbServer {
    child: Child,
    display: u32,
}

impl XvfbServer {
    /// Spawns Xvfb on the first free display number tried, or `None` when
    /// the `Xvfb` binary is not installed — the documented skip condition.
    ///
    /// Display numbers derive from this process's pid (the same trick
    /// `BroadwayServer::start` uses) so parallel test invocations do not
    /// collide; a stale lock file from a previous crashed run is skipped
    /// over rather than removed, since deleting another live server's lock
    /// file would be worse than trying the next number.
    fn start() -> Option<Self> {
        let xvfb = find_in_path("Xvfb")?;
        let base = 100 + (std::process::id() % 5000);
        for offset in 0..8 {
            let display = base + offset;
            let lock = PathBuf::from(format!("/tmp/.X11-unix/X{display}-lock"));
            if lock.exists() {
                continue;
            }
            let mut child = Command::new(&xvfb)
                .arg(format!(":{display}"))
                // An explicit screen: 24-bit depth, a size comfortably larger
                // than the overlay, and no TCP listener — the test talks to
                // the server over its Unix socket only.
                .args(["-screen", "0", "1280x1024x24", "-nolisten", "tcp"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("Xvfb was found on $PATH but could not be spawned");
            // Xvfb exits immediately when its display is taken; give it a
            // moment, then either trust the socket or move to the next
            // number.
            std::thread::sleep(Duration::from_millis(400));
            if child.try_wait().expect("polling Xvfb").is_some() {
                let _ = child.kill();
                let _ = child.wait();
                continue;
            }
            let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
            let deadline = Instant::now() + POLL_TIMEOUT;
            while Instant::now() < deadline {
                if socket.exists() {
                    return Some(XvfbServer { child, display });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("Xvfb :{display} started but never created its socket");
        }
        None
    }

    /// The `DISPLAY` value every client of this server needs.
    fn display_string(&self) -> String {
        format!(":{}", self.display)
    }
}

impl Drop for XvfbServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A spawned `hopd`, killed on drop — duplicated verbatim from
/// `headless_smoke.rs` (which duplicated it from `crates/hopd/tests/
/// socket.rs`); see this file's top doc comment for why the duplication is
/// the established pattern rather than an oversight.
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

/// `hopd`'s executable path, located as `hop-gtk`'s sibling — same reasoning
/// as `headless_smoke.rs`'s identical helper.
fn hopd_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_hop-gtk"));
    path.set_file_name(if cfg!(windows) { "hopd.exe" } else { "hopd" });
    path
}

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

    // A created socket file does not mean a listening one: bind(2) trails
    // create(2) by an unbounded moment on a loaded runner, and hop-gtk's
    // IPC client *drops* a query sent during its not-yet-connected window
    // rather than queueing it — the exact shape of the CI flake this
    // closed (screenshot child connected to nothing and timed out). Poll
    // for a successful connection, which is what every later step
    // already assumes.
    for _ in 0..50 {
        if std::os::unix::net::UnixStream::connect(&process.socket_path).is_ok() {
            return process;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("hopd never accepted a connection on its socket");
}

/// The environment every `hop-gtk` subprocess of these tests runs under:
/// the Xvfb display, the X11 backend forced (GDK's auto-probe would find
/// the X server anyway, but naming it is the point of an X11-specific
/// test), and the daemon's isolated XDG tree. `GSK_RENDERER=cairo` keeps
/// the runner's lack of a GPU out of the picture — what this file verifies
/// (window geometry, focus, socket traffic) is renderer-independent, and
/// the software renderer removes the one flake source a CI runner's
/// missing GL stack could otherwise contribute.
fn hop_gtk_env(xvfb: &XvfbServer, daemon: &DaemonProcess) -> Vec<(&'static str, String)> {
    vec![
        ("DISPLAY", xvfb.display_string()),
        ("GDK_BACKEND", "x11".to_string()),
        ("GSK_RENDERER", "cairo".to_string()),
        ("XDG_RUNTIME_DIR", daemon.runtime_dir.display().to_string()),
    ]
}

/// Finds `name` on `$PATH`, or `None` — the skip signal every test here
/// starts with.
fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// An X connection to the test's own Xvfb, plus the root window and screen
/// geometry every assertion below reads.
struct XConnection {
    conn: RustConnection,
    root: u32,
    screen_w: i32,
    screen_h: i32,
}

impl XConnection {
    fn connect(display: &str) -> Self {
        let (conn, screen_num) = RustConnection::connect(Some(display))
            .expect("connecting to the test's own Xvfb must not fail");
        let screen = &conn.setup().roots[screen_num];
        XConnection {
            root: screen.root,
            screen_w: i32::from(screen.width_in_pixels),
            screen_h: i32::from(screen.height_in_pixels),
            conn,
        }
    }

    /// The hop-gtk overlay window: the one client toplevel on this server
    /// measuring exactly the overlay size the token system declares.
    /// Returns its XID and current geometry, or `None` while it has not
    /// mapped yet.
    fn find_hop_window(&self) -> Option<(u32, (i32, i32, u16, u16))> {
        let Ok(tree_cookie) = self.conn.query_tree(self.root) else {
            return None;
        };
        let tree = match tree_cookie.reply() {
            Ok(tree) => tree,
            Err(_) => return None,
        };
        for &child in &tree.children {
            let Ok(geo_cookie) = self.conn.get_geometry(child) else {
                continue;
            };
            let Ok(geo) = geo_cookie.reply() else {
                continue;
            };
            if u32::from(geo.width) == tokens::WINDOW_SIZE_PX.0 as u32
                && u32::from(geo.height) == tokens::WINDOW_SIZE_PX.1 as u32
            {
                return Some((
                    child,
                    (i32::from(geo.x), i32::from(geo.y), geo.width, geo.height),
                ));
            }
        }
        None
    }

    /// Whether the overlay window is currently gone from the screen —
    /// either absent from the tree entirely or mapped no more.
    ///
    /// # Why "absent from the tree" is not enough on X11
    ///
    /// Dismissal is `close()` on a `hide_on_close` window: GTK *hides* the
    /// surface rather than destroying it (the pre-built window must survive
    /// for the next toggle). Under broadway a hidden surface disappears
    /// from the window tree, which is what this file's original wording
    /// assumed; a real X server keeps an unmapped window in `query_tree`
    /// forever as an `IsUnMapped` child — measured against Xvfb, where the
    /// dismissed overlay stays listed at its last geometry. So "gone"
    /// here means the X server reports the window unmapped (or unreachable,
    /// which for a live server means the same user-visible thing).
    fn hop_window_gone(&self) -> bool {
        let Some((xid, _)) = self.find_hop_window() else {
            return true;
        };
        match self.conn.get_window_attributes(xid) {
            Ok(cookie) => cookie
                .reply()
                .map(|attr| attr.map_state == MapState::UNMAPPED)
                .unwrap_or(true),
            Err(_) => true,
        }
    }
}

/// Polls `f` until it returns `Some`, or panics with `context` once
/// [`POLL_TIMEOUT`] elapses.
fn poll_until<T>(context: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        if let Some(value) = f() {
            return value;
        }
        if Instant::now() >= deadline {
            panic!("{context} (waited {}s)", POLL_TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Acceptance criteria 1 + 2: under Xvfb with no WM, hop-gtk's window maps
/// centered at the overlay size rather than at (0, 0) where a default
/// toplevel would sit — and focus loss dismisses it.
#[test]
fn interactive_window_maps_centered_and_dismisses_on_focus_loss() {
    let Some(xvfb) = XvfbServer::start() else {
        eprintln!("skipping: Xvfb not found on $PATH — install the `xvfb` package (CI does)");
        return;
    };

    // A unique GApplication registers on the session bus, so the interactive
    // run needs one; `dbus-run-session` provides a private bus for exactly
    // this process tree. Absent locally it is the same documented skip as
    // Xvfb itself; CI installs `dbus` so the arm is a hard requirement
    // there.
    let Some(dbus_run_session) = find_in_path("dbus-run-session") else {
        eprintln!(
            "skipping: dbus-run-session not found on $PATH — install the `dbus` package (CI does)"
        );
        return;
    };

    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());

    let mut command = Command::new(dbus_run_session);
    command
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_hop-gtk"))
        .arg("--socket")
        .arg(&daemon.socket_path);
    for (key, value) in hop_gtk_env(&xvfb, &daemon) {
        command.env(key, value);
    }
    command.env("HOME", runtime_dir.path().join("isolated-home"));
    let mut hop = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run hop-gtk under dbus-run-session");

    let x = XConnection::connect(&xvfb.display_string());

    // Criterion 1: the window exists, is overlay-sized, and is centered —
    // the expected origin computed by the very function the app positions
    // with, so the assertion and the implementation cannot drift apart.
    poll_until("hop-gtk's overlay window never mapped", || {
        x.find_hop_window()
    });
    let (_, (win_x, win_y, _, _)) = poll_until(
        "hop-gtk's window never reached its centered position",
        || {
            x.find_hop_window().filter(|(_, (win_x, win_y, w, h))| {
                (*win_x, *win_y)
                    == hop_gtk::x11::centered_origin(
                        x.screen_w,
                        x.screen_h,
                        i32::from(*w),
                        i32::from(*h),
                    )
            })
        },
    );
    let expected = hop_gtk::x11::centered_origin(
        x.screen_w,
        x.screen_h,
        tokens::WINDOW_SIZE_PX.0,
        tokens::WINDOW_SIZE_PX.1,
    );
    assert_eq!(
        (win_x, win_y),
        expected,
        "the overlay must map centered, not wherever a default toplevel lands"
    );

    // Criterion 2: focus loss dismisses. The test moves the X input focus
    // itself — first onto the overlay (FocusIn: GTK reports the window
    // active), then to `None` (FocusOut: keyboard events discarded, the
    // window has lost input focus) — the same primitive a WM drives when
    // the user clicks away. Dismissal is `close()` on a `hide_on_close`
    // window: the window unmaps and disappears from the tree.
    let (xid, _) = x.find_hop_window().expect("overlay present just above");
    x.conn
        .set_input_focus(InputFocus::NONE, xid, x11rb::CURRENT_TIME)
        .expect("SetInputFocus onto the overlay");
    std::thread::sleep(Duration::from_millis(500));
    x.conn
        .set_input_focus(InputFocus::NONE, x11rb::NONE, x11rb::CURRENT_TIME)
        .expect("SetInputFocus away from the overlay");
    poll_until("the overlay never dismissed on focus loss", || {
        x.hop_window_gone().then_some(())
    });

    // Killing `dbus-run-session` with SIGKILL orphans the hop-gtk beneath
    // it, so the Xvfb drop is what actually reaps it: a dead X server is a
    // fatal IO error for GDK's X11 backend, which exits the client. The
    // daemon has its own Drop. Nothing this test spawns outlives it.
    let _ = hop.kill();
    let _ = hop.wait();
}

/// Runs `hop-gtk --screenshot <path>` under Xvfb against `daemon`,
/// asserted to exit successfully; the full output is returned because the
/// stderr carries the startup capability report the log criterion asserts
/// against.
fn run_screenshot(
    xvfb: &XvfbServer,
    daemon: &DaemonProcess,
    out_path: &Path,
    query: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hop-gtk"));
    for (key, value) in hop_gtk_env(xvfb, daemon) {
        command.env(key, value);
    }
    command
        .env("HOME", daemon.runtime_dir.join("isolated-home"))
        .arg("--socket")
        .arg(&daemon.socket_path)
        .arg("--screenshot")
        .arg(out_path);
    if let Some(query) = query {
        command.arg("--query").arg(query);
    }
    let output = command
        .output()
        .expect("failed to run hop-gtk --screenshot under Xvfb");
    assert!(
        output.status.success(),
        "hop-gtk --screenshot exited with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

/// Asserts `path` is a real PNG — duplicated from `headless_smoke.rs`; see
/// this file's top doc comment.
fn assert_is_a_png(path: &Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("reading {path:?}: {err}"));
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    assert!(
        bytes.len() > PNG_MAGIC.len(),
        "{path:?} is too small to be a PNG"
    );
    assert_eq!(
        &bytes[..PNG_MAGIC.len()],
        &PNG_MAGIC,
        "{path:?} does not start with the PNG magic bytes"
    );
}

/// Reads a PNG's width and height straight out of its IHDR header bytes —
/// byte-for-byte the same layout `headless_smoke.rs`'s
/// `png_header_dimensions` reads (see that function's doc comment for the
/// chunk arithmetic); duplicated here rather than shared, per this file's
/// top doc comment.
fn png_header_dimensions(png: &[u8]) -> (u32, u32) {
    let be_u32 = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (be_u32(16), be_u32(20))
}

/// How many pixels of CSD drop-shadow margin the default Adwaita theme
/// draws inside each side of a GTK4 toplevel's X surface under X11 — the
/// inset between the window's X geometry (which does measure
/// `tokens::WINDOW_SIZE_PX`; see `find_hop_window`) and the widget area
/// `--screenshot` actually captures. See the size assertion in
/// [`screenshot_captures_the_x11_session_and_the_socket_round_trips`] for
/// why this is measured GTK4/X11 fact rather than a tolerance fudge.
const CSD_SHADOW_INSET_PX: i32 = 5;

/// Acceptance criteria 3, 4, and 5: `--screenshot` captures the positioned
/// window under Xvfb; a query drives results over the socket in that same
/// session (the capture only happens after `QueryDone` — a zero-exit run
/// *is* the completed round trip); and the startup log names the detected
/// session type and the chosen overlay strategy. The default-action half of
/// criterion 4 is exercised against the same daemon through the same
/// production `ipc` client `ui::window` uses.
#[test]
fn screenshot_captures_the_x11_session_and_the_socket_round_trips() {
    let Some(xvfb) = XvfbServer::start() else {
        eprintln!("skipping: Xvfb not found on $PATH — install the `xvfb` package (CI does)");
        return;
    };

    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());
    let out_dir = runtime_dir.path().join("captures");
    std::fs::create_dir(&out_dir).unwrap();

    let empty = out_dir.join("empty.png");
    let results = out_dir.join("results.png");

    // The empty state: the window presented, captured, exited cleanly.
    run_screenshot(&xvfb, &daemon, &empty, None);
    assert_is_a_png(&empty);

    // The results state: `--query "2+2"` is typed into the real entry once
    // `ipc` reports Connected, and `drive_to_state` waits for that query's
    // QueryDone before capturing — so a successful exit proves the query
    // reached `hopd` over the socket and its results came back, inside
    // this X11 session.
    let results_run = run_screenshot(&xvfb, &daemon, &results, Some("2+2"));
    assert_is_a_png(&results);

    // Both captures must measure what an X11 window of the declared token
    // size actually renders, and the results capture must differ from the
    // empty one — identical bytes would mean the query never changed what
    // was on screen.
    //
    // What an X11 window of the declared size renders is *not* the declared
    // size itself, and that is GTK4 client-side-decoration truth, not a hop
    // bug: under X11, GDK draws every CSD toplevel's drop shadow INSIDE the
    // window's own X surface (there is no compositor-side frame to hang it
    // on, and GTK4 sets no `_GTK_FRAME_EXTENTS` for anyone to read back).
    // The widget the screenshot harness captures — the very thing a user
    // sees as "the window" — sits inset within that surface by the default
    // Adwaita theme's shadow margin, 5px per side, so a 400×500 surface
    // paints a 390×490 content area. Measured against Ubuntu noble's
    // libadwaita (what CI runs), not assumed: mapping this exact binary
    // under Xvfb shows a GetGeometry of exactly WINDOW_SIZE_PX at the X
    // level (the interactive arm below asserts precisely that through
    // `find_hop_window`) while the PNG comes out (W−10, H−10). If a future
    // theme widens the shadow, both numbers move together and this
    // assertion fails loudly rather than drifting silently.
    let empty_bytes = std::fs::read(&empty).unwrap();
    let results_bytes = std::fs::read(&results).unwrap();
    let expected = (
        (tokens::WINDOW_SIZE_PX.0 - 2 * CSD_SHADOW_INSET_PX) as u32,
        (tokens::WINDOW_SIZE_PX.1 - 2 * CSD_SHADOW_INSET_PX) as u32,
    );
    assert_eq!(
        png_header_dimensions(&results_bytes),
        expected,
        "the capture must measure the rendered content area of the \
         declared overlay size (declared surface minus the CSD shadow \
         inset GTK4 carves out on X11)"
    );
    assert_ne!(
        empty_bytes, results_bytes,
        "the results capture must differ from the empty capture — identical \
         bytes would mean the query never changed what was on screen"
    );

    // Criterion 5: the capability report. `startup_report`'s own unit test
    // pins the exact wording; here the words must actually have been
    // printed by a real X11 run.
    let stderr = String::from_utf8_lossy(&results_run.stderr);
    assert!(
        stderr.contains("display session: X11"),
        "startup log must name the X11 session; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("overlay strategy: override-positioned"),
        "startup log must name the overlay strategy; stderr was:\n{stderr}"
    );

    // Criterion 4's default-action half: the exact `Query` → `Results` →
    // `Execute { item_id, action_id: default_action }` sequence
    // `ui::window::activate_selected` sends on Enter, through the
    // production `ipc` client, against the daemon this session used —
    // the same shape `tests/exec_round_trip.rs` proves protocol-wide,
    // re-proven inside the X11 session's own daemon.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (cmd_tx, evt_rx) = ipc::spawn(daemon.socket_path.clone());
    let outcome = runtime.block_on(async {
        loop {
            match evt_rx.recv().await.expect("ipc thread exited early") {
                IpcEvent::Connected => break,
                IpcEvent::ConnectFailed(reason) => panic!("connect failed: {reason}"),
                _ => {}
            }
        }
        cmd_tx.send(IpcCommand::Query("2+2".to_string()));
        let mut items = Vec::new();
        loop {
            match evt_rx.recv().await.expect("ipc thread exited early") {
                IpcEvent::Results(new_items) => items = new_items,
                IpcEvent::QueryDone => break,
                IpcEvent::Error(msg) => panic!("query failed: {msg}"),
                _ => {}
            }
        }
        let item = items
            .into_iter()
            .next()
            .expect("the calculator provider must answer \"2+2\" with one item");
        cmd_tx.send(IpcCommand::Execute {
            item_id: item.id.clone(),
            action_id: item.default_action.clone(),
        });
        loop {
            match evt_rx.recv().await.expect("ipc thread exited early") {
                IpcEvent::Executed(outcome) => return outcome,
                IpcEvent::Error(msg) => panic!("execute failed: {msg}"),
                _ => {}
            }
        }
    });
    match outcome {
        ExecOutcome::CopyText(text) => assert_eq!(text.as_str(), "4"),
        other => panic!("expected CopyText(\"4\"), got {other:?}"),
    }
}
