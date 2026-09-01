//! Issue #259's integration-level proof that [`hop_gtk::kde_blur::probe`]
//! (`src/kde_blur.rs`) reaches a live Wayland compositor and gets a real
//! answer back — not the pure `KdeBlurProbe` enum sanity check
//! `src/kde_blur.rs`'s own unit test module already covers, and not
//! `src/material.rs`'s own exhaustive `decide` matrix, which never opens a
//! display connection at all (both of those already prove everything they
//! can prove without one).
//!
//! # Why this connects to the ambient session rather than spawning one
//!
//! Every other display-dependent integration test in this crate
//! (`headless_smoke.rs`'s `BroadwayServer`, `x11_smoke.rs`'s `XvfbServer`,
//! `wlroots_smoke.rs`'s `WaylandServer`) spawns its own throwaway
//! compositor because what those files need to prove — window placement,
//! layer-shell wiring, capture geometry — has to hold against a compositor
//! whose exact behavior is under this project's control. This file needs
//! the opposite: a *real* GNOME/Mutter session, because the fact under
//! test — "Mutter never advertises `org_kde_kwin_blur_manager`" — is a
//! claim about a compositor nobody in this repo builds or configures, and
//! no throwaway headless compositor this crate already knows how to spawn
//! (sway, Weston) is Mutter. So this test does not spawn a compositor at
//! all: it connects to whatever live Wayland session this process is
//! already running under, and skips — never fails — when there is none.
//! That is the documented pattern every other display-dependent test in
//! this crate already uses for its own missing prerequisite (see
//! `x11_smoke.rs`'s module doc, "Why Xvfb, and why this skips rather than
//! fails without it", and `tests/wlroots_smoke.rs`'s identical section):
//! each test checks for what it needs first and returns early with a
//! printed reason when it is absent, so a machine that never had the
//! resource in the first place gets a quiet skip, not a red build.
//!
//! # Re-exec, like every other GTK-touching test here
//!
//! Same reasoning as `tests/material_mode.rs`'s module doc: GTK is not
//! safely re-initializable within one process, and `cargo test` runs every
//! `#[test]` in one process — so this test re-execs itself as a child with
//! `GDK_BACKEND=wayland` forced. Forcing it matters here in a way it does
//! not for `material_mode.rs`'s broadway child: this machine also has a
//! live X11 `$DISPLAY` (`session.rs`'s own module doc explains why GDK's
//! auto-probe is never trusted anywhere in this crate), so leaving the
//! backend to auto-detect could silently hand this test an X11 display
//! instead of the Wayland one it exists to probe. Unlike
//! `material_mode.rs`'s broadway child, `WAYLAND_DISPLAY` and
//! `XDG_RUNTIME_DIR` are left untouched in the child's environment
//! (`Command::env` only ever *adds* to an inherited environment, never
//! clears it): this test wants the child to inherit this process's own
//! ambient session, not a spawned, isolated one.

use std::process::Command;

use gtk::prelude::*;

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc, "Re-exec, like every other GTK-touching test here".
const CHILD_MARKER: &str = "HOP_GTK_KDE_BLUR_PROBE_TEST_CHILD";

/// Printed by the child, on its own line, when it discovers — only once
/// actually inside the child, past `gtk::init()` — that this machine's
/// ambient session is not, after all, one this test can probe (GTK failed
/// to initialize under a forced Wayland backend, or the resolved session
/// was not [`hop_gtk::session::SessionKind::Wayland`]). The parent process
/// greps the child's stderr for this exact marker to tell "skip" apart
/// from "the assertion actually failed", the same two-outcome shape
/// `x11_smoke.rs`/`wlroots_smoke.rs` establish with their own pre-flight
/// binary checks — except here the check cannot run until GTK itself has
/// tried and failed, so it has to happen inside the child rather than
/// before spawning it.
const CHILD_SKIP_MARKER: &str = "HOP_GTK_KDE_BLUR_PROBE_SKIP";

#[test]
fn mutter_never_advertises_the_kde_blur_manager_on_this_machines_live_session() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_assertions();
        return;
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!(
            "skipping: no WAYLAND_DISPLAY in this process's environment — no live Wayland \
             session to probe (see this file's module doc, 'Why this connects to the ambient \
             session rather than spawning one')"
        );
        return;
    }

    let current_exe = std::env::current_exe()
        .expect("failed to resolve this test binary's own path to re-exec it");
    let output = Command::new(current_exe)
        .env("GDK_BACKEND", "wayland")
        .env(CHILD_MARKER, "1")
        .arg("--exact")
        .arg("mutter_never_advertises_the_kde_blur_manager_on_this_machines_live_session")
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

/// The real assertion, run inside the re-exec'd child process described in
/// this file's module doc, once `GDK_BACKEND=wayland` is already set in
/// its environment (and `WAYLAND_DISPLAY`, inherited from the parent,
/// names a real ambient session).
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
    let Some(wayland_display) = display.downcast_ref::<gdkwayland::WaylandDisplay>() else {
        eprintln!(
            "{CHILD_SKIP_MARKER}: GDK_BACKEND=wayland was forced, but the resolved session was \
             {kind:?}, not Wayland — this machine's ambient session is not actually Wayland"
        );
        return;
    };

    // The one thing this test exists to prove is downstream of this
    // holding — belt-and-braces alongside the downcast above, which is
    // what `session::SessionKind::detect` itself is built on (`session.rs`'s
    // own module doc).
    assert_eq!(kind, hop_gtk::session::SessionKind::Wayland);

    let result = hop_gtk::kde_blur::probe(wayland_display);
    assert_eq!(
        result,
        hop_gtk::kde_blur::KdeBlurProbe::ManagerAbsent,
        "GNOME/Mutter never advertises org_kde_kwin_blur_manager — see src/material.rs's \
         module doc, 'Wayland: KDE's org_kde_kwin_blur_manager, detected but not yet applied' \
         — but the probe returned {result:?}. If this machine's live session is not actually \
         GNOME/Mutter, this assertion needs deliberately revisiting, not silencing.",
    );
}
