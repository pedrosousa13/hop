//! Issue #253's integration-level proof that `material::resolve`/
//! `material::apply` (`src/material.rs`) actually reach a real, live GTK
//! widget under a real display — not just the pure decision matrix that
//! module's own unit tests already cover exhaustively. A window built
//! under a headless `gtk4-broadwayd` display and run through the real
//! pipeline must wear exactly one of `assets/stylesheet.css`'s two
//! MATERIAL MODES classes, and never both — the invariant `material::apply`
//! is written to guarantee (it clears the other class before adding its
//! own) but that no unit test can check, since none of them touch a real
//! `gtk::Widget`.
//!
//! Broadway is [`hop_gtk::session::SessionKind::Other`] — `session.rs`'s
//! own module doc names it explicitly as "not a real user session" and the
//! backend the headless smoke tests already run under — so
//! `material::decide`'s degrade matrix (proven exhaustively in
//! `src/material.rs`'s own unit tests: see
//! `other_sessions_are_always_opaque_regardless_of_the_irrelevant_x11_probe`)
//! guarantees this resolves to [`Mode::Opaque`] deterministically, with no
//! X11 compositor probe and no flakiness. This file's job is narrower than
//! that matrix's: not "which mode is correct" but "does `apply` really
//! only ever leave one class on a real widget, wired through the real
//! `resolve` → `apply` pipeline `ui::window::HopWindow::build` itself
//! calls" — which is exactly what [`run_assertions`] below checks.
//!
//! # Re-exec under broadway
//!
//! Identical shape and identical reasoning to every other file under
//! `tests/` that needs a real GTK display — see
//! `tests/view_tree_renderer.rs`'s module doc for the full argument against
//! mutating this process's own environment in place (this crate's
//! `unsafe_code = "deny"` lint forbids the `std::env::set_var` that would
//! otherwise be needed). This file re-execs itself as a child process with
//! `GDK_BACKEND`/`BROADWAY_DISPLAY` set via `Command::env` (a *child's*
//! environment, needing no `unsafe`) and a marker variable telling the
//! child to run [`run_assertions`] directly instead of re-execing a second
//! time.
//!
//! Display base `800`, chosen distinct from every other `tests/*.rs`
//! file's own base (`headless_smoke.rs`/`x11_smoke.rs`: 100,
//! `view_tree_renderer.rs`: 200, `stylesheet_provider.rs`: 300,
//! `style_colour_scheme.rs`: 350, `motion_setting.rs`: 450,
//! `font_resolution.rs`: 500, `honesty_locked_provider.rs`: 600+) so a
//! parallel `cargo test` run — which runs every `#[test]` in every one of
//! this crate's integration test binaries concurrently by default — can
//! never compute the same broadway display number as another file's test
//! and collide on its socket.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gtk::prelude::*;

use hop_gtk::material::{self, Mode};

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_MATERIAL_MODE_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — duplicated from every other
/// `tests/*.rs` file's identical helper rather than shared, since each file
/// under `tests/` compiles as its own separate crate (the same reasoning
/// `motion_setting.rs`'s own copy of this struct gives).
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 800 + (std::process::id() % 5000);
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
fn a_live_window_run_through_the_real_pipeline_wears_exactly_one_material_mode_class() {
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
        .arg("a_live_window_run_through_the_real_pipeline_wears_exactly_one_material_mode_class")
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

    let window = gtk::Window::new();

    // The real pipeline, not a hand-picked `Mode`: the same two calls
    // `ui::window::HopWindow::build` makes, against the same kind of real,
    // live widget a real presentation ends up wearing them on.
    let mode = material::resolve();
    material::apply(&window, mode);

    // Broadway is `session::SessionKind::Other` — never X11, never
    // Wayland — so `material::decide`'s own degrade matrix (see
    // `src/material.rs`'s unit tests) guarantees this deterministically:
    // no compositor probe runs, and nothing here can flake.
    assert_eq!(
        mode,
        Mode::Opaque,
        "broadway must degrade to opaque — see src/material.rs's module doc"
    );

    assert!(
        window.has_css_class(material::OPAQUE_CSS_CLASS),
        "a live window run through the real material pipeline must wear the opaque class"
    );
    assert!(
        !window.has_css_class(material::BLUR_CSS_CLASS),
        "a live window run through the real material pipeline must never wear both classes"
    );
}
