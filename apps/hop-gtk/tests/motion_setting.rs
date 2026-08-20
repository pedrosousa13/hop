//! Proves the runtime half of issue #207's motion axis: a live change to
//! GTK's own `gtk-enable-animations` setting reloads the already-installed
//! [`gtk::CssProvider`] in place, with no restart — the exact shape
//! `tests/style_colour_scheme.rs` already proves for the palette axis,
//! transferred to the motion one, per this issue's own brief ("the palette
//! axis already has an equivalent proof — find it and transfer its shape,
//! driving the setting through its own public setter and diffing resolved
//! output").
//!
//! `stylesheet.rs`'s own unit tests
//! (`hint_fade_uses_the_token_resolved_duration_easing_and_delay`) only
//! prove `stylesheet::resolve` is motion-aware in isolation; they never
//! call [`style::install`], never touch `gtk::Settings`, and never prove
//! the `connect_gtk_enable_animations_notify` handler actually fires or
//! that it reloads the *live*, already-installed provider rather than some
//! fresh one. This file closes that gap: it installs the real provider
//! through the real production entry point, drives a real
//! `gtk-enable-animations` change through `gtk::Settings`'s own setter, and
//! reads the installed provider's own serialized content back with
//! [`gtk::CssProvider::to_str`] to confirm it changed to the other motion
//! state's values.
//!
//! # What this test does and does not prove
//!
//! **Proves:** [`style::install`]'s `connect_gtk_enable_animations_notify`
//! closure is live and correctly wired — flipping
//! `Gtk.Settings:gtk-enable-animations` through its own public setter
//! causes the *same* `gtk::CssProvider` object `install` returned to hold
//! different, motion-correct CSS text afterward, repeatably in both
//! directions (full → reduced → full again), without ever calling
//! `style::install` a second time or constructing a second provider. That
//! is the entire runtime mechanism this issue's brief names: a live
//! restyle with no restart.
//!
//! **Does not prove:** that a real, on-screen hint actually plays a
//! visible fade in response — no window and no row widget is built here,
//! only the stylesheet provider `style::install` returns. That is a
//! separate claim: `tests/view_tree_renderer.rs`'s own recycling section
//! proves `ui::row::bind` drives [`hop_gtk::ui::row::HINT_SHOWN_CLASS`]
//! correctly at the level of observable widget state, and
//! `docs/hig-conformance-checklist.md`'s reduced-motion item already
//! records — for every motion-table row, not only this one — that a
//! transition's path and timing are not capture-verifiable, only its
//! endpoints. This file's own assertions accordingly check the *values*
//! GTK's CSS engine will animate with (`transition-duration`,
//! `transition-delay`, `transition-timing-function`), never that an
//! animation frame was actually painted.
//!
//! # Re-exec under broadway
//!
//! Same shape as `tests/style_colour_scheme.rs` and every other file under
//! `tests/` that needs a real GTK display — read that file's own module doc
//! for the full argument against mutating this process's own environment.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_gtk::style;

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_MOTION_SETTING_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — duplicated from every other
/// `tests/*.rs` file's identical helper rather than shared, for the same
/// reason each of those own copies gives: each file under `tests/` compiles
/// as its own separate crate. The base display number (`450`) is
/// deliberately different from every other file's own base
/// (`headless_smoke.rs`: 100, `view_tree_renderer.rs`: 200,
/// `stylesheet_provider.rs`: 300, `style_colour_scheme.rs`: 350) so a
/// parallel `cargo test` run — which runs every `#[test]` in every one of
/// this crate's integration test *binaries* concurrently by default — can
/// never compute the same broadway display number as another file's test
/// and collide on its socket.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 450 + (std::process::id() % 5000);
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
fn motion_setting_change_reloads_the_installed_provider_with_the_other_motion_state() {
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
        .arg("motion_setting_change_reloads_the_installed_provider_with_the_other_motion_state")
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
/// dispatch, then returns — draining any `notify::gtk-enable-animations`
/// delivery `gtk::Settings` might schedule rather than emit synchronously
/// from inside `set_gtk_enable_animations` itself. Cheap and correct
/// either way, mirroring `style_colour_scheme.rs`'s identically-named
/// helper for the identical reason: if the signal already fired
/// synchronously, this finds no pending source and returns immediately
/// after one non-blocking check.
fn drain_pending_glib_events() {
    let ctx = glib::MainContext::default();
    while ctx.iteration(false) {}
}

/// The substring that identifies the hint-fade rule's own selector in
/// [`gtk::CssProvider::to_str`]'s serialized dump — `gtk::CssProvider`
/// re-serializes a compound class selector with its classes reordered
/// (confirmed directly against a real, installed GTK 4.14 while writing
/// this test: `assets/stylesheet.css`'s source order,
/// `.hop-row-hint.hop-row-hint-shown`, comes back as
/// `.hop-row-hint-shown.hop-row-hint`), so this searches for the one
/// class name unique to this rule rather than the exact selector string.
/// `opacity: 1;` alone would *not* be unique — `assets/stylesheet.css`'s
/// inert `.hop-honesty { opacity: 1; }` rule (issue #200's future work,
/// authored but not yet applied to any widget) declares the identical
/// property/value pair.
const HINT_SHOWN_RULE_MARKER: &str = "hop-row-hint-shown";

/// The real assertions, run inside the re-exec'd child process described in
/// this file's module doc, once `GDK_BACKEND=broadway` and
/// `BROADWAY_DISPLAY` are already set in its environment.
fn run_assertions() {
    gtk::init().expect("gtk init under the broadway display this process's environment selects");

    let Some(display) = gtk::gdk::Display::default() else {
        panic!("no gdk::Display available under the broadway backend this test selected");
    };

    let Some(settings) = gtk::Settings::default() else {
        panic!("no gtk::Settings available under the broadway backend this test selected");
    };

    // Force a known starting motion state *before* installing, so this
    // test's outcome does not depend on whatever the ambient, display-less
    // headless environment happens to report as its own default — the same
    // reasoning `style_colour_scheme.rs`'s own `run_assertions` gives for
    // forcing a starting palette.
    settings.set_gtk_enable_animations(true);
    drain_pending_glib_events();
    assert!(
        settings.is_gtk_enable_animations(),
        "gtk-enable-animations=true must read back true before this test can trust its own \
         starting point"
    );

    let provider = style::install(&display);
    let full_css = provider.to_str();
    let full_rule = extract_rule(&full_css, HINT_SHOWN_RULE_MARKER);
    assert!(
        full_rule.contains("transition-delay: 40ms;"),
        "expected the full-motion 40ms hint-fade delay right after style::install, got:\n{full_rule}"
    );
    assert!(
        full_rule.contains("transition-duration: 80ms;"),
        "expected the token-resolved 80ms hint-fade duration, got:\n{full_rule}"
    );

    // The actual runtime exercise: flip the setting through
    // `gtk::Settings`'s own public setter — the same `notify::
    // gtk-enable-animations` signal path a real desktop's reduced-motion
    // toggle drives — and confirm the *same* provider object
    // `style::install` returned now holds different content, proving
    // `style.rs`'s live `connect_gtk_enable_animations_notify` handler
    // actually fired and reloaded it, not merely that
    // `stylesheet::resolve` differs in isolation (already pinned by
    // `stylesheet.rs`'s own unit tests).
    settings.set_gtk_enable_animations(false);
    drain_pending_glib_events();
    assert!(
        !settings.is_gtk_enable_animations(),
        "gtk-enable-animations=false must read back false"
    );

    let reduced_css = provider.to_str();
    let reduced_rule = extract_rule(&reduced_css, HINT_SHOWN_RULE_MARKER);
    assert!(
        reduced_rule.contains("transition-delay: 0;"),
        "expected the live-reloaded provider to hold reduced motion's 0-delay hint fade after \
         the setting change, got:\n{reduced_rule}"
    );
    assert!(
        reduced_rule.contains("transition-duration: 80ms;"),
        "the 80ms duration must survive unchanged under reduced motion — \
         --hop-duration-fast has no @media override — got:\n{reduced_rule}"
    );
    assert!(
        !reduced_rule.contains("transition-delay: 40ms;"),
        "the full-motion 40ms delay must not survive the reload, got:\n{reduced_rule}"
    );

    // Flip back: a live, repeatable restyle, not a one-shot transition this
    // test could pass by accident on a handler that only ever runs once.
    settings.set_gtk_enable_animations(true);
    drain_pending_glib_events();
    let full_css_again = provider.to_str();
    let full_rule_again = extract_rule(&full_css_again, HINT_SHOWN_RULE_MARKER);
    assert!(
        full_rule_again.contains("transition-delay: 40ms;"),
        "expected switching back to full motion to reload the same provider with the 40ms \
         delay again, got:\n{full_rule_again}"
    );

    println!(
        "the installed gtk::CssProvider reloads live on a gtk::Settings \
         gtk-enable-animations change, in both directions"
    );
}

/// Finds the `{ ... }` block whose *selector* contains `marker` in `css` (a
/// serialized `gtk::CssProvider::to_str` dump), inclusive of the braces —
/// the slice this file's own assertions read `transition-*` properties out
/// of. `marker` is expected to name a class unique to one selector (see
/// [`HINT_SHOWN_RULE_MARKER`]'s own doc comment for why a declaration like
/// `opacity: 1;` cannot serve this role here), so this looks for the `{`
/// *after* `marker`'s position (the selector precedes its own block), not
/// before it.
fn extract_rule<'a>(css: &'a str, marker: &str) -> &'a str {
    let marker_pos = css
        .find(marker)
        .unwrap_or_else(|| panic!("marker {marker:?} not found in serialized provider CSS"));
    let open = css[marker_pos..]
        .find('{')
        .map(|i| marker_pos + i)
        .unwrap_or_else(|| panic!("no opening `{{` found after marker {marker:?}"));
    let close = css[open..]
        .find('}')
        .map(|i| open + i)
        .unwrap_or_else(|| panic!("no closing `}}` found after marker {marker:?}"));
    &css[open..=close]
}
