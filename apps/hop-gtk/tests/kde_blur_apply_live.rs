//! Issue #259's integration-level proof that [`hop_gtk::kde_blur::apply_blur`]
//! (`src/kde_blur.rs`) runs its *full* Wayland bind path — not just the
//! X11/`Other` guard `tests/kde_blur_apply.rs`'s broadway test already
//! covers, where the very first downcast fails and nothing past it ever
//! executes. This machine's ambient session is real GNOME/Mutter Wayland
//! (`tests/kde_blur_probe.rs`'s own module doc gives the identical
//! reasoning for connecting to it rather than spawning a compositor), so a
//! window mapped here drives `apply_blur`'s closure all the way through
//! the downcast, `WaylandDisplay` → `wl_display()` → `backend().upgrade()`
//! → its own dedicated `EventQueue` → registry roundtrip → `ManagerAbsent`
//! (Mutter never advertises `org_kde_kwin_blur_manager` — the same fact
//! `kde_blur_probe.rs` proves directly against `probe`) → its private
//! `demote_to_opaque` helper (`src/kde_blur.rs`) — the one path nothing
//! else in this crate's test suite exercises at all.
//!
//! # Why this simulates `Mode::Blur` first, rather than calling
//! `material::resolve`
//!
//! On this machine `material::resolve` itself already answers `Mode::Opaque`
//! (Mutter, no KDE manager) — so wiring the real `resolve` → `apply` →
//! `apply_blur` pipeline the way `tests/material_mode.rs` does for its own
//! subject would never call `apply_blur` at all (`ui::window`'s own call
//! site only calls it when `mode == Mode::Blur`), and the demotion path
//! this file exists to exercise would go completely untested. So this test
//! calls `material::apply(&window, Mode::Blur)` directly first — standing
//! in for what `decide` would have answered on a real KDE Wayland session —
//! then calls `apply_blur` and checks that it corrects that starting
//! `Mode::Blur` back to `Mode::Opaque` once its own roundtrip finds no
//! manager, exactly as `kde_blur.rs`'s own doc comment for `apply_blur`
//! promises under "The honesty invariant enforced on every bind-failure
//! path".
//!
//! # What this file does not, and cannot, prove here
//!
//! There is no KWin on this machine, so
//! [`hop_gtk::kde_blur::KdeBlurProbe::ManagerPresent`] never happens here and the `create`/`set_region`/`commit`/successful-
//! flush arm of `apply_blur` stays unexercised by any test that runs on
//! this machine — the same gap `tests/kde_blur_probe.rs`'s own module doc
//! names for `probe`'s `ManagerPresent` arm, deferred to a real-KWin-session
//! follow-up. Also out of reach without a fake compositor: the
//! release-on-remap invariant (`BlurSession`'s swap-and-release on a
//! *second* successful bind) that Finding B's fix touched — proving it
//! needs a compositor that accepts `create` so a `BlurSession` actually
//! gets built at all, then a second real map to release it, neither of
//! which Mutter's `ManagerAbsent` answer here ever reaches. Naming that gap
//! here rather than papering over it: this file proves the demotion path is
//! reached and correct, not that the release path is free of bugs.
//!
//! # Re-exec under a forced Wayland backend
//!
//! Identical shape and identical reasoning to `tests/kde_blur_probe.rs`'s
//! own module doc, "Re-exec, like every other GTK-touching test here":
//! GTK is not safely re-initializable within one process, so this test
//! re-execs itself as a child with `GDK_BACKEND=wayland` forced (this
//! machine also has a live X11 `$DISPLAY`, so leaving the backend to
//! auto-detect could silently hand this test the wrong display), and
//! `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR` are left untouched so the child
//! inherits this process's own ambient session rather than an isolated one.

use std::process::Command;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use hop_gtk::kde_blur;
use hop_gtk::material::{self, Mode};

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_KDE_BLUR_APPLY_LIVE_TEST_CHILD";

/// Printed by the child, on its own line, when it discovers — only once
/// actually inside the child, past `gtk::init()` — that this machine's
/// ambient session is not, after all, one this test can exercise (GTK
/// failed to initialize under a forced Wayland backend, or the resolved
/// session was not [`hop_gtk::session::SessionKind::Wayland`]). Identical
/// role to `tests/kde_blur_probe.rs`'s own marker of the same shape.
const CHILD_SKIP_MARKER: &str = "HOP_GTK_KDE_BLUR_APPLY_LIVE_SKIP";

#[test]
fn apply_blur_demotes_to_opaque_when_this_machines_live_session_has_no_blur_manager() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_assertions();
        return;
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!(
            "skipping: no WAYLAND_DISPLAY in this process's environment — no live Wayland \
             session to bind against (see this file's module doc, 'Re-exec under a forced \
             Wayland backend')"
        );
        return;
    }

    let current_exe = std::env::current_exe()
        .expect("failed to resolve this test binary's own path to re-exec it");
    let output = Command::new(current_exe)
        .env("GDK_BACKEND", "wayland")
        .env(CHILD_MARKER, "1")
        .arg("--exact")
        .arg("apply_blur_demotes_to_opaque_when_this_machines_live_session_has_no_blur_manager")
        .arg("--nocapture")
        .output()
        .expect("failed to re-exec this test binary under this process's own Wayland session");

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains(CHILD_SKIP_MARKER) {
        eprintln!("skipping: {stderr}");
        return;
    }
    assert!(
        output.status.success(),
        "the re-exec'd child failed:\nstdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&output.stdout),
    );
}

/// The real assertions, run inside the re-exec'd child process described in
/// this file's module doc, once `GDK_BACKEND=wayland` is already set in its
/// environment (and `WAYLAND_DISPLAY`, inherited from the parent, names a
/// real ambient session).
fn run_assertions() {
    if gtk::init().is_err() {
        eprintln!(
            "{CHILD_SKIP_MARKER}: gtk::init() failed under GDK_BACKEND=wayland — no reachable \
             Wayland session despite WAYLAND_DISPLAY being set in this process's environment"
        );
        return;
    }

    let Some(display) = gtk::gdk::Display::default() else {
        eprintln!(
            "{CHILD_SKIP_MARKER}: gtk::init() succeeded but no default gdk::Display was \
             available"
        );
        return;
    };

    let kind = hop_gtk::session::SessionKind::detect(&display);
    if display
        .downcast_ref::<gdkwayland::WaylandDisplay>()
        .is_none()
    {
        eprintln!(
            "{CHILD_SKIP_MARKER}: GDK_BACKEND=wayland was forced, but the resolved session was \
             {kind:?}, not Wayland — this machine's ambient session is not actually Wayland"
        );
        return;
    }
    assert_eq!(kind, hop_gtk::session::SessionKind::Wayland);

    // `adw::Application::new` + `register`, not `app.run()` — same pattern
    // `tests/kde_blur_apply.rs`'s own child uses and documents: `register`
    // emits `GApplication::startup` synchronously, which is all
    // `adw::ApplicationWindow::builder().application(...)` needs, with no
    // main loop required to get there.
    let app = adw::Application::new(
        Some("dev.hop.test.KdeBlurApplyLive"),
        gio::ApplicationFlags::NON_UNIQUE,
    );
    app.register(gio::Cancellable::NONE)
        .expect("registering a NON_UNIQUE test application must not fail");
    let window = adw::ApplicationWindow::builder()
        .application(&app)
        .default_width(200)
        .default_height(200)
        .build();

    // Stands in for what `material::decide` would have answered on a real
    // KDE Wayland session — see this file's module doc, "Why this
    // simulates Mode::Blur first". Without this, `apply_blur`'s own
    // demotion never has a `Mode::Blur` to demote away from, since this
    // machine's own `material::resolve` already answers `Mode::Opaque`.
    material::apply(&window, Mode::Blur);
    kde_blur::apply_blur(&window);

    window.present();

    // Wait for both the surface to actually map *and* `apply_blur`'s own
    // `connect_map` handler to have run to completion and settled the
    // window on one material class — the handler's registry roundtrip is
    // synchronous, so by the time it returns the class is already
    // corrected, but nothing guarantees GTK fires `map` before or after the
    // surface reports a real size, so this waits for both rather than
    // assuming an order between them.
    assert!(
        wait_until(
            || {
                let mapped = window
                    .surface()
                    .is_some_and(|surface| surface.width() > 0 && surface.height() > 0);
                let settled = window.has_css_class(material::OPAQUE_CSS_CLASS)
                    || window.has_css_class(material::BLUR_CSS_CLASS);
                mapped && settled
            },
            Duration::from_secs(5),
        ),
        "the window never reported both a real mapped surface and a settled material class"
    );

    assert!(
        window.has_css_class(material::OPAQUE_CSS_CLASS),
        "Mutter never advertises org_kde_kwin_blur_manager (kde_blur_probe.rs proves this \
         directly), so apply_blur's own roundtrip must find no manager and demote the window \
         it was handed already wearing Mode::Blur back to Mode::Opaque"
    );
    assert!(
        !window.has_css_class(material::BLUR_CSS_CLASS),
        "a window apply_blur has demoted must not still wear the translucent blur class — \
         that is exactly the dishonest state issue #259 exists to close"
    );
}

/// Pumps the real GLib main context, sleeping briefly between checks, until
/// `condition` returns `true` or `timeout` elapses — returns whether it
/// succeeded. Copied from `tests/kde_blur_apply.rs`'s identical helper
/// (itself copied from `tests/view_tree_renderer.rs`'s: that file's own doc
/// comment explains why a non-blocking spin alone was rejected).
fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
    let ctx = glib::MainContext::default();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        while ctx.iteration(false) {}
        if condition() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
