//! The CI headless smoke test the design spec's §11 makes non-optional:
//! drives `hop-gtk` far enough, headless, to capture at least the empty
//! state and a results state — acceptance criterion 8 — using
//! `hop-gtk --screenshot <path>` itself, acceptance criterion 7's
//! implementation, against a real `hopd` built from this workspace,
//! acceptance criterion 6.
//!
//! Issue #184 adds a third capture, of an exclusive route (`"=1+1"`), to the
//! same test — its own criterion 7 asks for a headless capture covering both
//! an exclusive and a non-exclusive route, and `"2+2"`'s existing capture
//! already is the non-exclusive half. The check on that third capture is a
//! byte-diff against `"2+2"`'s own PNG, which proves the two captures were
//! produced and differ — not that the mode label or marker highlight
//! actually appear on screen, since the two captures' query text differs
//! too and would make the bytes differ regardless. See that assertion's own
//! comment, further down in this file, for exactly what it does and does
//! not establish, and for where the stronger, widget-level proof lives.
//!
//! Issue #228 points a real pixel decoder at the claims only decoded pixels
//! can defend: every capture's dimensions are checked against the window
//! size the token system declares, from the PNG's IHDR header bytes alone,
//! and the results-state capture is decoded (via a dev-only `gdk-pixbuf`
//! dependency — see `Cargo.toml`'s own comment on it for why it adds no new
//! compiled code) so the selected row's composited fill can be sampled and
//! asserted against the composite `--hop-accent-subdued` documents.
//! Deliberately *not* added: pixel assertions for flat token colours such as
//! the row ground or the hint-chip background — those are already pinned at
//! the declaration level by the token-resolution tests — or for the
//! mode-label/marker-highlight visibility gap described above, which stays
//! owned by the widget-level tests.
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
// The new assertions below read every expected value live out of the token
// system — the window size, the row height, and the two colours whose
// composite the selected row's fill documents — never as literals duplicated
// beside them, for the same drift reason `src/tokens.rs`'s own tests state:
// a future edit to `assets/tokens.css` should fail here loudly, not pass
// against a stale copy of its old values.
use hop_gtk::tokens::{self, Palette};

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

/// Reads a PNG's width and height straight out of its IHDR header bytes —
/// no decode. Every PNG opens with the 8-byte signature `assert_is_a_png`
/// already checks, then chunks laid out as a 4-byte big-endian length, a
/// 4-byte type, and the data; the IHDR is required to be the first chunk,
/// so its data — big-endian width, then height, 4 bytes each — sits at
/// bytes 16..24 of the file, per the PNG specification's layout. Issue
/// #228's geometry assertion answers "does this capture measure the window
/// the token system declares" from these eight bytes alone, which is why it
/// needs no image-decoding dependency at all.
fn png_header_dimensions(png: &[u8]) -> (u32, u32) {
    const IHDR_DIMENSIONS: std::ops::Range<usize> = 16..24;
    assert!(
        png.len() > IHDR_DIMENSIONS.end,
        "file is too small to carry a PNG IHDR ({} bytes)",
        png.len()
    );
    let be_u32 =
        |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().expect("4 bytes per u32"));
    (
        be_u32(IHDR_DIMENSIONS.start),
        be_u32(IHDR_DIMENSIONS.start + 4),
    )
}

/// Asserts the PNG at `path` measures exactly the window size the token
/// system declares — `WINDOW_SIZE_PX`, read live out of `assets/tokens.css`
/// the same way `src/tokens.rs`'s own `window_size_matches_tokens_css` reads
/// it, never a `400`/`500` literal duplicated here that an edit to the
/// tokens could silently drift away from. Dimensions come from the PNG
/// header alone (see [`png_header_dimensions`]), so this half of issue
/// #228's pixel coverage needs no decode.
fn assert_capture_is_window_sized(path: &Path) {
    let png = std::fs::read(path).unwrap_or_else(|err| panic!("reading {path:?}: {err}"));
    let (width, height) = png_header_dimensions(&png);
    let (expected_w, expected_h) = *tokens::WINDOW_SIZE_PX;
    assert_eq!(
        (width, height),
        (expected_w as u32, expected_h as u32),
        "{path:?} measures {width}x{height}, but the token system declares a \
         {expected_w}x{expected_h} window"
    );
}

/// Parses `#rrggbb` — the shape every opaque colour token in
/// `assets/tokens.css` is authored in — into float channels, panicking on
/// anything else (a reshaped token is a programming error to fail on
/// immediately, the same stance `src/tokens.rs`'s own parsers take).
fn hex_channels(value: &str) -> (f64, f64, f64) {
    let hex = value
        .strip_prefix('#')
        .unwrap_or_else(|| panic!("expected a `#rrggbb` colour, got {value:?}"));
    assert!(hex.len() == 6, "expected a `#rrggbb` colour, got {value:?}");
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex channel") as f64;
    (channel(0), channel(2), channel(4))
}

/// Parses `rgba(r, g, b, a)` — the shape `--hop-accent-subdued` is authored
/// in — into float channels plus the alpha, panicking on anything else for
/// the same reason [`hex_channels`] does.
fn rgba_channels(value: &str) -> ((f64, f64, f64), f64) {
    let body = value
        .strip_prefix("rgba(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("expected an `rgba(r, g, b, a)` colour, got {value:?}"));
    let mut parts = body.split(',');
    let mut next = || {
        parts
            .next()
            .unwrap_or_else(|| panic!("too few channels in {value:?}"))
            .trim()
            .parse::<f64>()
            .unwrap_or_else(|err| panic!("channel in {value:?} is not a number: {err}"))
    };
    let colour = (next(), next(), next());
    let alpha = next();
    assert!(
        parts.next().is_none() && (0.0..=1.0).contains(&alpha),
        "malformed or out-of-range alpha in {value:?}"
    );
    (colour, alpha)
}

/// The colour `--hop-accent-subdued`'s own comment in `assets/tokens.css`
/// documents its selected-row fill compositing to, computed from the
/// committed token values rather than restated here as a second hardcoded
/// literal: both inputs — the translucent accent wash and the window ground
/// it composites over (`--hop-bg`) — are resolved live through
/// `tokens::resolve`, the same resolver the stylesheet build uses, then
/// combined with the standard source-over alpha formula and rounded to the
/// u8 channels a PNG stores. If either token moves, this moves with it and
/// the assertion below keeps telling the truth; if the *rendering* stops
/// matching the tokens, the assertion fails.
fn documented_selection_fill() -> [u8; 3] {
    let (fg, alpha) = rgba_channels(&tokens::resolve("hop-accent-subdued", Palette::Dark));
    let bg = hex_channels(&tokens::resolve("hop-bg", Palette::Dark));
    let over = |f: f64, b: f64| (f * alpha + b * (1.0 - alpha)).round() as u8;
    [over(fg.0, bg.0), over(fg.1, bg.1), over(fg.2, bg.2)]
}

/// Decodes the PNG at `path` with `gdk-pixbuf` and asserts the selected
/// row's composited fill really renders the documented composite — issue
/// #228's whole point: the one colour claim in the HIG conformance
/// checklist that only decoded pixels can defend, promoted from a one-off
/// manual sample recorded in prose to a committed regression.
/// The vertical position of the results list depends on the query entry's
/// allocated height, which GTK derives from theme metrics no token commits —
/// so the row cannot be found from geometry alone. What *is* committed is
/// the fill's colour and its size: `.hop-selection-indicator` is the only
/// surface in the capture painted `--hop-sel-fill` (the composite this
/// function expects), it spans essentially the full window width, and its
/// height is `ROW_HEIGHT_PX` by `ui::window.rs`'s own `set_height_request`.
/// So the scan finds every scanline where the expected colour matches
/// across a substantial run of pixels (a threshold low enough that the row's
/// own title text and action-hint chips drawn over the fill cannot break a
/// scanline's count, high enough that no other surface could plausibly
/// reach it), groups those scanlines into contiguous vertical bands, and
/// demands exactly one band of the committed row height. Text glyphs are
/// avoided by sampling the middle of the longest *uninterrupted* horizontal
/// run of the expected colour inside the band — a run by definition contains
/// nothing drawn over the fill. If the fill's colour, place, or size breaks,
/// the band disappears or the sample mismatches and this fails.
fn assert_selected_row_fill_is_the_documented_composite(path: &Path, expected: [u8; 3]) {
    let pixbuf = gdk_pixbuf::Pixbuf::from_file(path)
        .unwrap_or_else(|err| panic!("decoding {path:?}: {err}"));
    assert_eq!(
        pixbuf.colorspace(),
        gdk_pixbuf::Colorspace::Rgb,
        "{path:?} decoded to an unexpected colourspace"
    );
    let width = pixbuf.width() as usize;
    let height = pixbuf.height() as usize;
    let channels = pixbuf.n_channels() as usize;
    let rowstride = pixbuf.rowstride() as usize;
    let pixels = pixbuf
        .pixel_bytes()
        .expect("pixbuf exposes its pixel bytes");
    // Rowstride, not width * channels: gdk-pixbuf pads each row, so the
    // pixel at (x, y) lives at y * rowstride + x * channels, never at
    // y * width * channels + ....
    let pixel = |x: usize, y: usize| -> [u8; 3] {
        let at = y * rowstride + x * channels;
        pixels[at..at + 3]
            .try_into()
            .expect("3 bytes per RGB pixel")
    };

    let row_h = *tokens::ROW_HEIGHT_PX as usize;
    // A scanline belongs to the fill band when at least this many of its
    // pixels are exactly the expected composite — see this function's doc
    // comment for why the threshold sits where it does.
    let scanline_threshold = width / 8;

    let matching_scanlines: Vec<usize> = (0..height)
        .filter(|&y| (0..width).filter(|&x| pixel(x, y) == expected).count() >= scanline_threshold)
        .collect();

    // Group the matching scanlines into contiguous vertical bands.
    let mut bands: Vec<(usize, usize)> = Vec::new();
    for &y in &matching_scanlines {
        match bands.last_mut() {
            Some((_, end)) if *end + 1 == y => *end = y,
            _ => bands.push((y, y)),
        }
    }
    assert_eq!(
        bands.len(),
        1,
        "{path:?} should show exactly one composited-selection-fill band \
         (one deterministically-selected row), found {}: {:?}",
        bands.len(),
        bands
    );
    let (band_top, band_bottom) = bands[0];
    let band_height = band_bottom - band_top + 1;
    // The indicator's height is `ROW_HEIGHT_PX` by construction; up to a
    // scanline at each edge may blend the fill into the ground behind it
    // and so miss the exact-match count, hence the small slack — but only
    // downward: a band *taller* than a row would mean some other surface
    // joined in.
    assert!(
        band_height + 2 >= row_h && band_height <= row_h,
        "the composited-fill band in {path:?} spans {band_height} scanlines, \
         but a selected row is {row_h}px tall"
    );

    // Sample the middle scanline of the band, along its longest
    // uninterrupted run of the expected colour — inside the fill, clear of
    // every glyph and chip drawn over it (see the doc comment).
    let sample_y = (band_top + band_bottom) / 2;
    let mut best: (usize, usize) = (0, 0);
    let mut run_start: Option<usize> = None;
    for x in 0..=width {
        let matching = x < width && pixel(x, sample_y) == expected;
        match (run_start, matching) {
            (None, true) => run_start = Some(x),
            (Some(start), false) => {
                if x - start > best.1 - best.0 {
                    best = (start, x);
                }
                run_start = None;
            }
            _ => {}
        }
    }
    let (run_start, run_end) = best;
    assert!(
        run_end - run_start >= width / 2,
        "the fill band's longest unobstructed run in {path:?} is only \
         {}px wide at y={sample_y}",
        run_end - run_start
    );
    let sample_x = (run_start + run_end) / 2;
    assert_eq!(
        pixel(sample_x, sample_y),
        expected,
        "the selected row's composited fill at ({sample_x}, {sample_y}) in \
         {path:?} does not match the composite `--hop-accent-subdued` \
         documents"
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
    let exclusive_route_png = out_dir.path().join("exclusive-route.png");

    // Empty-query state: nothing typed, whatever the freshly connected
    // window shows.
    run_screenshot(&daemon, &broadway, &empty_state_png, None);
    assert_is_a_png(&empty_state_png);
    // Issue #228: every capture must measure the window the token system
    // declares — checked from the PNG's IHDR header bytes alone, no decode.
    assert_capture_is_window_sized(&empty_state_png);

    // Results state: "2+2" is the same deterministic calculator query
    // `crates/hopd/tests/calculator.rs` drives against this same real
    // `build_host()` registry — no external state, no network, the same
    // answer on every run. This is a *non-exclusive* route
    // (`hop_core::router`'s own test names it
    // `an_inferred_math_query_reports_calculator_without_exclusivity`): a
    // bare mathematical expression is a shape `route()` infers Calculator
    // from, not a marker it consumed, so issue #184's mode label must stay
    // absent on this capture — see the `"=1+1"` capture further down in this
    // same test for the exclusive-route case that shows it.
    run_screenshot(&daemon, &broadway, &results_state_png, Some("2+2"));
    assert_is_a_png(&results_state_png);
    assert_capture_is_window_sized(&results_state_png);
    // Issue #228: the selected row's composited fill — the one colour claim
    // only decoded pixels can defend (flat token colours stay pinned at the
    // declaration level by the token-resolution tests, so they get no pixel
    // assertion here) — is sampled from the decoded capture and asserted
    // against the composite `--hop-accent-subdued`'s own comment documents,
    // computed live from the committed token values by
    // `documented_selection_fill`, never restated as a literal here.
    assert_selected_row_fill_is_the_documented_composite(
        &results_state_png,
        documented_selection_fill(),
    );

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

    // Issue #184, criterion 7: a headless capture of an *exclusive* route,
    // alongside the non-exclusive `"2+2"` one above — criterion 7's literal
    // ask, "a headless capture ... covers both an exclusive route and a
    // non-exclusive one". `"=1+1"` routes through the `=` sigil
    // (`hop_core::router::route`'s `Mode::Calculator` exclusive branch)
    // rather than being inferred from shape — same deterministic,
    // network-free arithmetic as `"2+2"` above, but `exclusive: true` and a
    // real `marker_span` over the leading `=`.
    run_screenshot(&daemon, &broadway, &exclusive_route_png, Some("=1+1"));
    assert_is_a_png(&exclusive_route_png);
    assert_capture_is_window_sized(&exclusive_route_png);

    // What the assertion below does and does not establish, stated
    // precisely rather than left to be over-read: `"=1+1"` and `"2+2"`
    // differ in the query text itself, so a byte-diff between their two PNGs
    // is guaranteed by the entry's own rendered text alone — it would still
    // pass with `mode_label::apply` and `marker_highlight::apply` both
    // stubbed out to no-ops. It proves the two captures were produced and
    // are not byte-identical (satisfying criterion 7's literal ask, and
    // ruling out `--screenshot` silently writing the same frame twice), not
    // that the mode label or the marker highlight actually render on
    // screen. That stronger proof — real visibility, real text, a real
    // Pango attribute range over the reported span — lives in the
    // widget-level, broadway-gated tests in `ui::window`'s own test module:
    // `assert_mode_label_mirrors_exclusive_and_nothing_else` and
    // `assert_marker_highlight_covers_exactly_the_reported_span`. The
    // pixel-decoding dependency whose absence the previous wording of this
    // paragraph named as the reason the gap stayed open now exists — issue
    // #228 added it, dev-only, and pointed it at the claims only decoded
    // pixels can defend: the composited selection fill (asserted above) and
    // every capture's geometry (asserted from the PNG header). This gap is a
    // different claim — whether two *widget-level* effects are visible in
    // captures whose query text differs — and it stays unclosed here,
    // deliberately: a pixel scan cannot separate "mode label absent" from
    // "mode label drawn where the differing text already changed the bytes"
    // any better than the byte-diff can, so the stronger, widget-level proof
    // remains the right owner, and this test still names the gap rather than
    // implying it closed.
    let exclusive_bytes = std::fs::read(&exclusive_route_png).unwrap();
    assert_ne!(
        exclusive_bytes, results_bytes,
        "the exclusive-route and non-exclusive-route screenshots must not be \
         byte-identical — see the comment above for what this does and does \
         not prove"
    );
}
