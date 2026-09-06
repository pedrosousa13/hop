//! Issue #259's integration-level proof that [`hop_gtk::kde_blur::apply_blur`]
//! (`src/kde_blur.rs`) is safe to wire onto a real, live window that maps
//! under a non-KDE-Wayland surface — the half of #259's acceptance criteria
//! this machine can actually exercise. This machine has no KWin (this
//! file's module doc, same as `kde_blur_probe.rs`'s and `material.rs`'s own
//! module docs record), so the one thing nothing here can prove is that a
//! bound `org_kde_kwin_blur` object actually makes KWin blur anything — that
//! is real-session follow-up work, the same deferral
//! `tests/kde_blur_probe.rs`'s own module doc names for its own probe half.
//! What this file *can* prove, deterministically, on any machine: that
//! wiring `apply_blur` onto a window's `connect_map` never panics, and the
//! window still reaches a real mapped surface exactly as it would without
//! `apply_blur` wired in at all.
//!
//! # Why broadway, not this machine's live session
//!
//! `tests/kde_blur_probe.rs` deliberately connects to the ambient live
//! session because *that* file's fact under test — "Mutter never advertises
//! `org_kde_kwin_blur_manager`" — is a claim about a real compositor this
//! repo does not build. This file's fact under test is different: "the
//! function does not panic and the window still maps", which holds on
//! *any* non-Wayland-KDE surface and needs no real compositor at all — the
//! same reasoning `tests/material_mode.rs` already gives for choosing
//! broadway over a real session for its own live-widget proof. Broadway is
//! [`hop_gtk::session::SessionKind::Other`]: `apply_blur`'s first downcast
//! (the mapped surface to `gdkwayland::WaylandSurface`) fails immediately
//! under it, the identical "not a Wayland surface" guard that already
//! covers X11 in production (`ui::window`'s own comment on the call site).
//! Deterministic, needs nothing installed beyond `gtk4-broadwayd` (already
//! this crate's test dependency — see `headless_smoke.rs`'s own top
//! comment), and portable to CI.
//!
//! # Re-exec under broadway, like every other file here
//!
//! Identical shape and identical reasoning to `tests/material_mode.rs`'s
//! own module doc: GTK is not safely re-initializable within one process,
//! so this file re-execs itself as a child with `GDK_BACKEND=broadway` /
//! `BROADWAY_DISPLAY` forced via `Command::env`.
//!
//! Display base `900`, distinct from every other `tests/*.rs` file's own
//! base (see `material_mode.rs`'s module doc for the running list this
//! extends) so a parallel `cargo test` run cannot collide on a broadway
//! socket with another file's test.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use hop_gtk::kde_blur;

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_KDE_BLUR_APPLY_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — duplicated from
/// `material_mode.rs`'s identical helper rather than shared, since each
/// file under `tests/` compiles as its own separate crate (that file's own
/// doc comment gives the same reasoning for its own copy).
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 900 + (std::process::id() % 5000);
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
        // Asynchronous socket creation, same fixed sleep every other copy
        // of this helper uses (the socket lives in the abstract namespace,
        // so it cannot be polled for by `Path::exists`).
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
fn apply_blur_is_a_safe_no_op_that_still_lets_a_non_kde_surface_map() {
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
        .arg("apply_blur_is_a_safe_no_op_that_still_lets_a_non_kde_surface_map")
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

/// The real assertions, run inside the re-exec'd child process described in
/// this file's module doc, once `GDK_BACKEND=broadway` and
/// `BROADWAY_DISPLAY` are already set in its environment.
fn run_assertions() {
    gtk::init().expect("gtk init under the broadway display this process's environment selects");

    // `adw::Application::new` + `register`, not `app.run()` — the same
    // pattern `ui::window`'s own `build_configured_window` test helper uses
    // and documents: GTK asserts "New application windows must be added
    // after the GApplication::startup signal has been emitted" the moment
    // an `adw::ApplicationWindow` is built with `.application(app)` set,
    // and `register` emits that signal synchronously with no main loop
    // needed, standing in for the `startup` emission `app.run_with_args`
    // would otherwise give it.
    let app = adw::Application::new(
        Some("dev.hop.test.KdeBlurApply"),
        gio::ApplicationFlags::NON_UNIQUE,
    );
    app.register(gio::Cancellable::NONE)
        .expect("registering a NON_UNIQUE test application must not fail");
    let window = adw::ApplicationWindow::builder()
        .application(&app)
        .default_width(200)
        .default_height(200)
        .build();

    // The call under test — wired exactly the way `ui::window`'s own call
    // site wires it, before the window is ever presented.
    kde_blur::apply_blur(&window);

    window.present();

    assert!(
        wait_until(
            || {
                window
                    .surface()
                    .is_some_and(|surface| surface.width() > 0 && surface.height() > 0)
            },
            Duration::from_secs(5),
        ),
        "the window never reported a real, mapped surface — apply_blur must never prevent a \
         non-KDE surface from mapping normally"
    );
}

/// Pumps the real GLib main context, sleeping briefly between checks, until
/// `condition` returns `true` or `timeout` elapses — returns whether it
/// succeeded. Copied from `tests/view_tree_renderer.rs`'s identical helper
/// (that file's own doc comment on it explains why a non-blocking spin
/// alone was rejected: a headless backend maps and configures a surface
/// asynchronously, on the main loop's own schedule).
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
