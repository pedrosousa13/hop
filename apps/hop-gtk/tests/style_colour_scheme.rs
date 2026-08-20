//! Proves the runtime half of issue #193's acceptance criterion: "Switching
//! the system colour scheme at runtime restyles the window without
//! restarting it, using the light palette's own values." `style.rs`
//! subscribes to `adw::StyleManager`'s `notify::dark` signal and reloads
//! [`style::install`]'s [`gtk::CssProvider`] in place — but nothing before
//! this file exercised that subscription. `stylesheet.rs`'s own unit tests
//! (`resolved_real_stylesheet_differs_between_palettes`) only prove
//! `stylesheet::resolve` is palette-aware in isolation; they never call
//! [`style::install`], never touch `adw::StyleManager`, and never prove the
//! `connect_dark_notify` handler actually fires or that it reloads the
//! *live*, already-installed provider rather than some fresh one. This file
//! closes that gap: it installs the real provider through the real
//! production entry point, drives a real colour-scheme change through
//! `adw::StyleManager`'s own setter (the same signal a real desktop's
//! dark/light toggle drives), and reads the installed provider's own
//! serialized content back with [`gtk::CssProvider::to_str`] to confirm it
//! changed to the other palette's values.
//!
//! Issue #200 extends this file's `run_assertions` to also install
//! [`style::install_locked`] and drive the identical colour-scheme flip
//! against it — this issue's own acceptance criterion, "the locked block
//! survives a palette change", needs the identical runtime proof
//! `style_colour_scheme.rs` already builds for the ordinary provider, not a
//! second file re-deriving the same `BroadwayServer`/re-exec scaffolding
//! for one more provider on the same display.
//!
//! # What this test does and does not prove
//!
//! **Proves:** [`style::install`]'s `connect_dark_notify` closure is live
//! and correctly wired — flipping `AdwStyleManager`'s colour scheme through
//! its own public setter causes the *same* `gtk::CssProvider` object
//! `install` returned to hold different, palette-correct CSS text
//! afterward, repeatably in both directions (dark → light → dark again),
//! without ever calling `style::install` a second time or constructing a
//! second provider. That is the entire runtime mechanism the acceptance
//! criterion names: a live restyle with no restart.
//!
//! **Does not prove:** that a real, on-screen window actually repaints in
//! response. No window is built here — this test never calls
//! `ui::window::HopWindow::build`, so it cannot show pixels changing. That
//! is a separate claim (a `gtk::CssProvider` installed via
//! `style_context_add_provider_for_display` is documented GTK behavior to
//! automatically restyle every widget on `display`, which this test does
//! not re-verify) already covered indirectly by the widget/screenshot tests
//! in `headless_smoke.rs` and `view_tree_renderer.rs`. It also does not
//! prove behavior under a *real* desktop's system dark-mode toggle
//! specifically — headless `broadway` has no desktop settings daemon to
//! toggle. What it drives instead is `AdwColorScheme::ForceDark`/
//! `ForceLight` through `set_color_scheme`, which changes `is-dark` and
//! fires `notify::dark` through the exact same GObject signal path a real
//! desktop toggle would — libadwaita does not distinguish the two at the
//! signal layer — so this is the closest a headless test can honestly get
//! to "the system colour scheme changed at runtime" without a running
//! desktop session.
//!
//! # Re-exec under broadway
//!
//! Same shape as `tests/stylesheet_provider.rs` and `tests/view_tree_renderer.rs`
//! — read either module doc for the full argument against mutating this
//! process's own environment. `gtk::init()` is not called directly here;
//! [`adw::init`] is used instead, since this test needs `adw::StyleManager`
//! (`adw::init` calls `gtk::init` internally — see its own doc comment in
//! the `libadwaita` crate).

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_gtk::style;

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_STYLE_COLOUR_SCHEME_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — duplicated from
/// `tests/stylesheet_provider.rs`'s identical helper rather than shared, for
/// the same reason that file's own copy gives: each file under `tests/`
/// compiles as its own separate crate. The base display number (`350`) is
/// deliberately different from every other file's own base
/// (`headless_smoke.rs`: 100, `view_tree_renderer.rs`: 200,
/// `stylesheet_provider.rs`: 300) so a parallel `cargo test` run — which
/// runs every `#[test]` in every one of this crate's integration test
/// *binaries* concurrently by default — can never compute the same
/// broadway display number as another file's test and collide on its
/// socket.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 350 + (std::process::id() % 5000);
        let child = Command::new("gtk4-broadwayd")
            .arg(format!(":{display}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect(
                "failed to spawn gtk4-broadwayd — it ships in libgtk-4-bin \
                 (NOT `broadwayd` on $PATH, which on Debian/Ubuntu is \
                 libgtk-3-bin's incompatible GTK3 server; see \
                 headless_smoke.rs's top doc comment for how this was \
                 diagnosed)",
            );
        // Asynchronous socket creation — see `headless_smoke.rs`'s
        // `BroadwayServer::start` for why this is a fixed sleep rather than
        // a `Path::exists` poll (the socket lives in the abstract
        // namespace).
        std::thread::sleep(Duration::from_millis(300));
        BroadwayServer { child, display }
    }
}

impl Drop for BroadwayServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn colour_scheme_change_reloads_the_installed_provider_with_the_other_palette() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_assertions();
        return;
    }

    let broadway = BroadwayServer::start();

    let current_exe = std::env::current_exe()
        .expect("failed to resolve this test binary's own path to re-exec it");
    let output = Command::new(current_exe)
        .env("GDK_BACKEND", "broadway")
        .env("BROADWAY_DISPLAY", format!(":{}", broadway.display))
        .env(CHILD_MARKER, "1")
        .arg("--exact")
        .arg("colour_scheme_change_reloads_the_installed_provider_with_the_other_palette")
        .arg("--nocapture")
        .output()
        .expect("failed to re-exec this test binary under the headless broadway display");

    assert!(
        output.status.success(),
        "the headless child process failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Runs the main-context event loop until it has no more pending sources to
/// dispatch, then returns — draining any `notify::dark` delivery that
/// `AdwStyleManager` might schedule rather than emit synchronously from
/// inside `set_color_scheme` itself. Cheap and correct either way: if the
/// signal already fired synchronously (the common GObject property-setter
/// shape, and what was actually observed while writing this test — see this
/// file's own verification notes in the issue's task-3 report), this finds
/// no pending source and returns immediately after one non-blocking check.
fn drain_pending_glib_events() {
    let ctx = glib::MainContext::default();
    while ctx.iteration(false) {}
}

/// The real assertions, run inside the re-exec'd child process described in
/// this file's module doc, once `GDK_BACKEND=broadway` and
/// `BROADWAY_DISPLAY` are already set in its environment.
fn run_assertions() {
    adw::init().expect("adw init under the broadway display this process's environment selects");

    let Some(display) = gtk::gdk::Display::default() else {
        panic!("no gdk::Display available under the broadway backend this test selected");
    };

    let style_manager = adw::StyleManager::default();

    // Force a known starting palette *before* installing, so this test's
    // outcome does not depend on whatever the ambient, display-less
    // headless environment happens to report as its own default colour
    // scheme.
    style_manager.set_color_scheme(adw::ColorScheme::ForceDark);
    drain_pending_glib_events();
    assert!(
        style_manager.is_dark(),
        "AdwColorScheme::ForceDark must resolve StyleManager::is_dark() to true \
         before this test can trust its own starting point"
    );

    let provider = style::install(&display);
    let dark_css = provider.to_str();
    assert!(
        dark_css.contains("background-color: rgb(18,18,20);"),
        "expected the dark palette's window-ground colour right after \
         style::install, got:\n{dark_css}"
    );
    assert!(
        !dark_css.contains("background-color: rgb(250,249,246);"),
        "the light palette's window-ground colour must not appear while dark \
         is the active palette, got:\n{dark_css}"
    );

    // Issue #200's own second provider — the identical exercise, on the
    // identical signal, for the honesty-critical locked block: "the locked
    // block survives a palette change" (this issue's own acceptance
    // criterion) means the live `connect_dark_notify` handler
    // `style::install_locked` wires up must reload *this* provider too, not
    // just the ordinary one, and must reload it with the *other* palette's
    // contrast colour — `--hop-fg`, resolved on `.hop-honesty-text` — the
    // same way `dark_css`/`light_css` above pin `--hop-bg` for the ordinary
    // sheet. `#f4f3f1` (dark `--hop-fg`) and `#211f1a` (light `--hop-fg`)
    // were read directly off `stylesheet::resolve_locked_block`'s own
    // output while writing this test, the same empirical-first approach
    // `stylesheet.rs`'s own tests already use for every other palette-pinned
    // literal.
    let locked_provider = style::install_locked(&display);
    let locked_dark_css = locked_provider.to_str();
    assert!(
        locked_dark_css.contains("color: rgb(244,243,241);"),
        "expected the dark palette's --hop-fg contrast colour in the locked provider right \
         after style::install_locked, got:\n{locked_dark_css}"
    );
    assert!(
        !locked_dark_css.contains("color: rgb(33,31,26);"),
        "the light palette's --hop-fg contrast colour must not appear while dark is the \
         active palette, got:\n{locked_dark_css}"
    );

    // The actual runtime exercise: flip the colour scheme through
    // `AdwStyleManager`'s own public setter — the same `notify::dark` signal
    // path a real desktop's dark/light toggle drives — and confirm the
    // *same* provider object `style::install` returned now holds different
    // content, proving `style.rs`'s live `connect_dark_notify` handler
    // actually fired and reloaded it, not merely that `stylesheet::resolve`
    // differs in isolation (already pinned by `stylesheet.rs`'s own unit
    // tests).
    style_manager.set_color_scheme(adw::ColorScheme::ForceLight);
    drain_pending_glib_events();
    assert!(
        !style_manager.is_dark(),
        "AdwColorScheme::ForceLight must resolve StyleManager::is_dark() to false"
    );

    let light_css = provider.to_str();
    assert!(
        light_css.contains("background-color: rgb(250,249,246);"),
        "expected the live-reloaded provider to hold the light palette's \
         window-ground colour after the colour-scheme change, got:\n{light_css}"
    );
    assert!(
        !light_css.contains("background-color: rgb(18,18,20);"),
        "the dark palette's window-ground colour must not survive the \
         reload, got:\n{light_css}"
    );

    let locked_light_css = locked_provider.to_str();
    assert!(
        locked_light_css.contains("color: rgb(33,31,26);"),
        "expected the live-reloaded locked provider to hold the light palette's --hop-fg \
         contrast colour after the colour-scheme change, got:\n{locked_light_css}"
    );
    assert!(
        !locked_light_css.contains("color: rgb(244,243,241);"),
        "the dark palette's --hop-fg contrast colour must not survive the reload, \
         got:\n{locked_light_css}"
    );

    // Flip back: "restyles the window without restarting it" means a live,
    // repeatable restyle, not a one-shot transition this test could pass by
    // accident on a handler that only ever runs once (e.g. one that
    // disconnects itself after firing).
    style_manager.set_color_scheme(adw::ColorScheme::ForceDark);
    drain_pending_glib_events();
    let dark_css_again = provider.to_str();
    assert!(
        dark_css_again.contains("background-color: rgb(18,18,20);"),
        "expected switching back to dark to reload the same provider with \
         the dark palette again, got:\n{dark_css_again}"
    );

    let locked_dark_css_again = locked_provider.to_str();
    assert!(
        locked_dark_css_again.contains("color: rgb(244,243,241);"),
        "expected switching back to dark to reload the locked provider with the dark \
         palette's --hop-fg contrast colour again, got:\n{locked_dark_css_again}"
    );

    println!(
        "both the ordinary and the locked gtk::CssProvider reload live on an \
         AdwStyleManager colour-scheme change, in both directions"
    );
}
