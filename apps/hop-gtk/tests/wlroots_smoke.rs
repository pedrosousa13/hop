//! The wlroots half of the headless proof (issue #233): under a real headless
//! wlroots compositor (sway), with `GDK_BACKEND=wayland`, a `layer-shell`-
//! feature build of `hop-gtk` maps its window as a **layer surface** —
//! overlay layer, exclusive keyboard, unanchored (which is what makes the
//! compositor center it, per design spec §2's wlroots/KDE rows) — captures it
//! with the existing `--screenshot` harness, and its startup log names the
//! layer-shell path and why it was taken. The two independent "unsupported"
//! answers the probe can give both stay observable here too: the feature-off
//! build under the *same* kind of session still maps the ordinary window,
//! and a feature-on build under a compositor that implements no
//! `zwlr_layer_shell_v1` (headless Weston) falls back just as cleanly.
//!
//! # How "mapped as a layer surface" is proven from outside the process
//!
//! The test cannot reach into hop-gtk's GTK objects, so it proves the claim
//! one layer down, where there is no room for a no-op to hide: hop-gtk runs
//! with `WAYLAND_DEBUG=1`, libwayland's wire logger, and the test asserts on
//! the raw protocol traffic. A real layer-surface run must contain the
//! `zwlr_layer_shell_v1` requests `layer_shell::apply_or_fallback` issues —
//! `set_layer(3)` (overlay), `set_keyboard_mode(1)` (exclusive) — and must
//! not contain any nonzero `set_anchor` (unanchored is how the surface ends
//! up centered). The feature-off and Weston runs must contain *no*
//! `zwlr_layer_shell_v1` traffic at all. This is stronger than a screenshot:
//! the `--screenshot` capture is hop-gtk's own rendering, which would look
//! identical either way, so the wire log is what distinguishes the layer-
//! surface path from the ordinary-window path.
//!
//! # Why sway, and why Weston
//!
//! sway is a wlroots compositor, the exact family design spec §2's
//! layer-shell rows name, and wlroots has a first-class headless backend:
//! `WLR_BACKENDS=headless` gives a compositor with a real Wayland socket and
//! a real output, no GPU and no input devices required (`WLR_LIBINPUT_NO_DEVICES=1`,
//! `WLR_RENDERER=pixman` keep the runner's hardware out of the picture).
//! For the unsupported-compositor arm, sway cannot stand in — it *does*
//! implement the protocol — so the same harness runs against headless
//! Weston, which implements no `zwlr_layer_shell_v1` (the same reason GNOME's
//! Mutter falls back, per `layer_shell`'s module doc): the one compositor
//! that is both headless-friendly and layer-shell-less in distro packages.
//!
//! # Why this skips rather than fails without its prerequisites
//!
//! Same reasoning and precedent as `x11_smoke.rs`: `sway`, `weston`, and a
//! `libgtk4-layer-shell` the `layer-shell` feature can link are not
//! installed on every machine this suite runs on (issue #179's environment
//! note: the build machine cannot provision system packages mid-run). Each
//! test checks for what it needs first and **returns early with a printed
//! reason** when it is absent. CI (`.github/workflows/ci.yml`'s
//! `layer-shell-gate` job) installs all three and makes the same tests a
//! hard requirement there — both with the feature on (sway + Weston arms)
//! and with it off (the feature-off arm, which only compiles in a default
//! build).
//!
//! # Why subprocesses, like `x11_smoke.rs`
//!
//! Same reasoning, same precedent: GTK is not safely re-initializable
//! within one process, and `cargo test` runs every test in one process —
//! so `hop-gtk` is driven exactly as a user or CI would drive it, as a real
//! subprocess per capture. The helpers this file needs are duplicated from
//! `x11_smoke.rs` (which duplicated them from `headless_smoke.rs`) rather
//! than shared: integration-test helpers are private to their own test
//! crate, and the duplication is the established, documented pattern here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hop_gtk::tokens;

/// How long any single "wait for the compositor to come up" poll may run
/// before the test gives up with context — generous on purpose, because a
/// flaky red here reads as a layer-shell regression when it is really a
/// scheduling hiccup on a loaded runner.
const POLL_TIMEOUT: Duration = Duration::from_secs(15);

/// A spawned headless Wayland compositor (sway or Weston), killed on drop —
/// the same child-ownership shape `x11_smoke.rs`'s `XvfbServer` uses, so a
/// failing assertion cannot leak a compositor into the rest of the run.
struct WaylandServer {
    child: Child,
    /// The `WAYLAND_DISPLAY` socket name the compositor created inside the
    /// test's runtime dir — what every client of this session connects
    /// through.
    socket_name: String,
}

impl WaylandServer {
    /// Spawns headless **sway** in `runtime_dir`, or `None` when the `sway`
    /// binary is not installed — the documented skip condition.
    ///
    /// The Wayland socket name derives from this process's pid (the same
    /// trick `x11_smoke.rs`'s display-number derivation uses) so parallel
    /// test invocations do not collide; each test also gets its own
    /// `runtime_dir`, so identical names in different directories stay
    /// independent.
    fn start_sway(runtime_dir: &Path) -> Option<Self> {
        let sway = find_in_path("sway")?;
        let socket_name = format!("hop-wl-{}", std::process::id());

        // sway insists on a config file; this one is minimal on purpose —
        // a single headless output at a size comfortably larger than the
        // overlay, no bar, nothing else. `output *` rather than the
        // backend-specific `HEADLESS-1` keeps this immune to wlroots
        // renaming its headless outputs.
        let config = runtime_dir.join("sway-config");
        std::fs::write(&config, "output * resolution 1280x800 position 0,0\n")
            .expect("writing the minimal sway config");

        let mut child = Command::new(&sway)
            .arg("-c")
            .arg(&config)
            .env("WLR_BACKENDS", "headless")
            // No input devices exist under the headless backend; this tells
            // wlroots that is expected rather than an error.
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            // CI runners have no GPU: force wlroots' software renderer the
            // same way the client side forces GSK_RENDERER=cairo below.
            .env("WLR_RENDERER", "pixman")
            .env("XDG_RUNTIME_DIR", runtime_dir)
            // wlroots names its listening socket after this variable when
            // set — which is what makes `socket_name` deterministic.
            .env("WAYLAND_DISPLAY", &socket_name)
            .env("HOME", runtime_dir.join("isolated-home"))
            .env(
                "XDG_CONFIG_HOME",
                runtime_dir.join("isolated-xdg-config-home"),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sway was found on $PATH but could not be spawned");
        Self::await_socket(runtime_dir, &socket_name, &mut child, "sway");
        Some(WaylandServer { child, socket_name })
    }

    /// Compiled only with the `layer-shell` feature: Weston is the
    /// unsupported-compositor arm's stand-in, and that arm only exists in a
    /// feature-on build (a feature-off build has no layer-shell probe to
    /// answer "unsupported" in the first place). Spawns headless **Weston**
    /// in `runtime_dir`, or `None` when the `weston` binary is not
    /// installed — the documented skip condition for that arm. Weston
    /// implements no `zwlr_layer_shell_v1`, which is exactly the point:
    /// this is the compositor-without-layer-shell fallback, headlessly.
    #[cfg(feature = "layer-shell")]
    fn start_weston(runtime_dir: &Path) -> Option<Self> {
        let weston = find_in_path("weston")?;
        let socket_name = format!("hop-wl-{}", std::process::id());

        let mut child = Command::new(&weston)
            .arg("--backend=headless-backend.so")
            // Like sway's WLR_RENDERER above: no GPU on the runner, and
            // Weston does not fall back to its software renderer on its own.
            .arg("--renderer=pixman")
            .arg(format!("--socket={socket_name}"))
            .args(["--width", "1280", "--height", "800"])
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .env("HOME", runtime_dir.join("isolated-home"))
            .env(
                "XDG_CONFIG_HOME",
                runtime_dir.join("isolated-xdg-config-home"),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("weston was found on $PATH but could not be spawned");
        Self::await_socket(runtime_dir, &socket_name, &mut child, "weston");
        Some(WaylandServer { child, socket_name })
    }

    /// Polls for the compositor's socket, failing with context if the
    /// compositor exits first (a config error, a missing renderer — anything
    /// that would otherwise surface as an opaque timeout).
    fn await_socket(runtime_dir: &Path, socket_name: &str, child: &mut Child, compositor: &str) {
        let socket = runtime_dir.join(socket_name);
        let deadline = Instant::now() + POLL_TIMEOUT;
        loop {
            if socket.exists() {
                return;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("{compositor} exited before creating its socket (status {status})");
            }
            if Instant::now() >= deadline {
                panic!("{compositor} started but never created {socket:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for WaylandServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A spawned `hopd`, killed on drop — duplicated from `x11_smoke.rs` (which
/// duplicated it from `headless_smoke.rs`); see this file's top doc comment.
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
/// as `x11_smoke.rs`'s identical helper.
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

    for _ in 0..50 {
        if process.socket_path.exists() {
            return process;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("hopd did not create its socket in time");
}

/// The environment every `hop-gtk` subprocess of these tests runs under:
/// the compositor's Wayland socket, the Wayland backend forced (GDK's
/// auto-probe would find the socket anyway, but naming it is the point of a
/// Wayland-specific test), and the daemon's isolated XDG tree.
/// `GSK_RENDERER=cairo` keeps the runner's lack of a GPU out of the picture,
/// exactly as in `x11_smoke.rs`.
fn hop_gtk_env(server: &WaylandServer, daemon: &DaemonProcess) -> Vec<(&'static str, String)> {
    vec![
        ("WAYLAND_DISPLAY", server.socket_name.clone()),
        ("GDK_BACKEND", "wayland".to_string()),
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

/// Runs `hop-gtk --screenshot <path>` under `server` against `daemon` with
/// `WAYLAND_DEBUG=1`, asserted to exit successfully. The full output is
/// returned because both halves of the proof live in its stderr: the
/// startup capability report, and the Wayland wire log the layer-surface
/// assertions read.
fn run_screenshot(
    server: &WaylandServer,
    daemon: &DaemonProcess,
    out_path: &Path,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hop-gtk"));
    for (key, value) in hop_gtk_env(server, daemon) {
        command.env(key, value);
    }
    command
        .env("HOME", daemon.runtime_dir.join("isolated-home"))
        // The wire logger. Everything this file asserts about *how* the
        // window mapped — layer surface vs ordinary toplevel — is read from
        // this log; see the module doc comment.
        .env("WAYLAND_DEBUG", "1")
        .arg("--socket")
        .arg(&daemon.socket_path)
        .arg("--screenshot")
        .arg(out_path);
    let output = command
        .output()
        .expect("failed to run hop-gtk --screenshot under the headless compositor");
    assert!(
        output.status.success(),
        "hop-gtk --screenshot exited with {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

/// Asserts `path` is a real PNG — duplicated from `x11_smoke.rs`; see this
/// file's top doc comment.
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
/// duplicated from `x11_smoke.rs`; see this file's top doc comment.
fn png_header_dimensions(png: &[u8]) -> (u32, u32) {
    let be_u32 = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (be_u32(16), be_u32(20))
}

/// The layer-surface half of the wire-log proof: the exact requests
/// `layer_shell::apply_or_fallback` must have issued, asserted against
/// libwayland's log lines (each request prints as its interface and object
/// id, then the method name and bare integer arguments — the wlroots
/// `layer` enum's overlay is 3, the `keyboard_interactivity` enum's
/// exclusive is 1). The unanchored half is negative: no nonzero
/// `set_anchor` (the four anchor bits are 1/2/4/8) may appear, because an
/// unanchored layer surface is what the compositor centers. Compiled only
/// with the feature, for the same reason `start_weston` is.
#[cfg(feature = "layer-shell")]
fn assert_wire_shows_layer_surface(stderr: &str) {
    assert!(
        stderr.contains("zwlr_layer_shell_v1"),
        "the compositor's layer-shell protocol must have been bound and used"
    );
    assert!(
        stderr.contains(".set_layer(3)"),
        "the surface must be requested on the overlay layer (3)"
    );
    assert!(
        stderr.contains(".set_keyboard_mode(1)"),
        "the surface must request exclusive keyboard interactivity (1)"
    );
    for anchor in 1..=15u32 {
        assert!(
            !stderr.contains(&format!(".set_anchor({anchor})")),
            "the surface must stay unanchored (compositor-centered), \
             but the wire log anchors bit {anchor}"
        );
    }
}

/// The negative half: not one byte of layer-shell traffic, for whichever
/// unsupported arm `context` names.
fn assert_wire_shows_no_layer_shell(stderr: &str, context: &str) {
    assert!(
        !stderr.contains("zwlr_layer_shell_v1"),
        "{context}: no layer-shell protocol traffic may appear, \
         but the wire log mentions zwlr_layer_shell_v1"
    );
}

/// The startup-report half of every arm: the session must be named Wayland
/// and the report must say which overlay path was chosen *and why* — the
/// probe's answer, not just the strategy (issue #233's criterion 5).
fn assert_report(stderr: &str, support: &str, strategy: &str) {
    let report_line = stderr
        .lines()
        .find(|line| line.contains("display session:"))
        .unwrap_or_else(|| panic!("no startup capability report on stderr; stderr was:\n{stderr}"));
    assert!(
        report_line.contains("display session: Wayland"),
        "the Wayland session must be detected as Wayland: {report_line}"
    );
    assert!(
        report_line.contains(&format!("layer-shell support: {support}")),
        "the report must record the probe's answer ({support}): {report_line}"
    );
    assert!(
        report_line.contains(&format!("overlay strategy: {strategy}")),
        "the report must name the chosen strategy ({strategy}): {report_line}"
    );
}

/// No error spam: the fallback arms must leave the ordinary window working
/// *quietly* (issue #233's criterion 4). GTK criticals, protocol errors, and
/// layer-shell's own complaints are the three shapes a mis-wired fallback
/// takes; none may appear. (GTK's informational `Gtk-` messages and the
/// wire logger's own chatter are expected, not spam.)
fn assert_no_error_spam(stderr: &str, context: &str) {
    for line in stderr.lines() {
        assert!(
            !line.contains("CRITICAL"),
            "{context}: GTK critical on stderr: {line}"
        );
        assert!(
            !line.contains("Protocol error"),
            "{context}: Wayland protocol error: {line}"
        );
        assert!(
            !line.contains("gtk4-layer-shell ERROR"),
            "{context}: layer-shell error: {line}"
        );
    }
}

/// Criterion 1 + 2 + 5: a `layer-shell`-feature build under headless sway
/// probes Supported, maps the window as a real layer surface (overlay layer,
/// exclusive keyboard, unanchored — proven on the wire), captures it with
/// `--screenshot`, and the startup log names the layer-shell path and why.
#[cfg(feature = "layer-shell")]
#[test]
fn layer_shell_window_maps_as_a_layer_surface_under_headless_sway() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let Some(server) = WaylandServer::start_sway(runtime_dir.path()) else {
        eprintln!("skipping: sway not found on $PATH — install the `sway` package (CI does)");
        return;
    };
    let daemon = spawn_daemon(runtime_dir.path());

    let capture = runtime_dir.path().join("layer-shell.png");
    let output = run_screenshot(&server, &daemon, &capture);

    // The capture: the window presented, rendered, and exited cleanly
    // inside the layer-shell session, at exactly the overlay size the
    // token system declares.
    assert_is_a_png(&capture);
    let bytes = std::fs::read(&capture).unwrap();
    assert_eq!(
        png_header_dimensions(&bytes),
        (
            tokens::WINDOW_SIZE_PX.0 as u32,
            tokens::WINDOW_SIZE_PX.1 as u32
        ),
        "the capture must measure the overlay size the token system declares"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_report(&stderr, "Supported", "layer-shell overlay");
    assert_wire_shows_layer_surface(&stderr);
    assert_no_error_spam(&stderr, "layer-shell arm");
}

/// Criterion 3 (and half of 4): the feature-off build — this file's own
/// compilation mode when `hop-gtk` is built with default features — under
/// the *same* kind of headless sway session still maps the ordinary window,
/// exits cleanly, and never touches the layer-shell protocol. This is the
/// arm every machine without `libgtk4-layer-shell` runs by default.
#[cfg(not(feature = "layer-shell"))]
#[test]
fn feature_off_build_maps_the_ordinary_window_under_headless_sway() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let Some(server) = WaylandServer::start_sway(runtime_dir.path()) else {
        eprintln!("skipping: sway not found on $PATH — install the `sway` package (CI does)");
        return;
    };
    let daemon = spawn_daemon(runtime_dir.path());

    let capture = runtime_dir.path().join("feature-off.png");
    let output = run_screenshot(&server, &daemon, &capture);

    assert_is_a_png(&capture);
    let bytes = std::fs::read(&capture).unwrap();
    assert_eq!(
        png_header_dimensions(&bytes),
        (
            tokens::WINDOW_SIZE_PX.0 as u32,
            tokens::WINDOW_SIZE_PX.1 as u32
        ),
        "the ordinary window must still capture at the overlay size"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Wayland session, probe says not-compiled-in, so the strategy is the
    // GNOME row's ordinary compositor-placed window — sway places it, and
    // close-on-focus-loss is that row's documented behavior.
    assert_report(
        &stderr,
        "NotCompiledIn",
        "compositor-placed window (close-on-focus-loss)",
    );
    assert_wire_shows_no_layer_shell(&stderr, "feature-off build");
    assert_no_error_spam(&stderr, "feature-off arm");
}

/// The other half of criterion 4: a `layer-shell`-feature build under a
/// compositor that implements no `zwlr_layer_shell_v1` (headless Weston)
/// falls back to the ordinary window cleanly — the probe answers
/// UnsupportedByCompositor, no layer-shell traffic touches the wire, the
/// window still maps and captures, and nothing complains.
#[cfg(feature = "layer-shell")]
#[test]
fn compositor_without_layer_shell_falls_back_cleanly_under_headless_weston() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let Some(server) = WaylandServer::start_weston(runtime_dir.path()) else {
        eprintln!("skipping: weston not found on $PATH — install the `weston` package (CI does)");
        return;
    };
    let daemon = spawn_daemon(runtime_dir.path());

    let capture = runtime_dir.path().join("weston-fallback.png");
    let output = run_screenshot(&server, &daemon, &capture);

    assert_is_a_png(&capture);
    let bytes = std::fs::read(&capture).unwrap();
    assert_eq!(
        png_header_dimensions(&bytes),
        (
            tokens::WINDOW_SIZE_PX.0 as u32,
            tokens::WINDOW_SIZE_PX.1 as u32
        ),
        "the fallback window must still capture at the overlay size"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_report(
        &stderr,
        "UnsupportedByCompositor",
        "compositor-placed window (close-on-focus-loss)",
    );
    assert_wire_shows_no_layer_shell(&stderr, "Weston (no zwlr_layer_shell_v1)");
    assert_no_error_spam(&stderr, "Weston fallback arm");
}
