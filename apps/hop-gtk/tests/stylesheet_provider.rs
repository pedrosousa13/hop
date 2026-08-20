//! Proves the real, shipped `assets/stylesheet.css` — resolved for every
//! combination of palette and motion state — loads into a real
//! [`gtk::CssProvider`] with **zero** parse errors. This is the single most
//! important artifact of issue #193: GTK's CSS parser drops anything it
//! cannot parse *silently* (confirmed during this issue's own triage —
//! handing `assets/tokens.css` to a raw provider produced 20 silent parse
//! errors, none of them visible anywhere without a `parsing-error` signal
//! connected), which is exactly the failure mode that let hop ship with no
//! working stylesheet for as long as it did. A test that only checks the
//! *text* `stylesheet::resolve` produces — no leftover `{{`/`}}` marker,
//! which `stylesheet.rs`'s own unit tests already pin — can never catch a
//! placeholder that substituted cleanly into CSS GTK's parser still rejects
//! for some other reason. Only handing the resolved text to a real
//! `gtk::CssProvider` and watching its own `parsing-error` signal can catch
//! that, which is what this file does.
//!
//! Issue #207 extended the matrix this covers from the two palettes alone
//! to the full 2×2 palette-by-motion combination, since that issue's own
//! `{{motion:name}}` placeholder (`.hop-row-hint-shown`'s `transition:`
//! declaration, in particular — real, GTK-parsed
//! `transition-property`/`transition-duration`/`transition-timing-function`/
//! `transition-delay` syntax, not something the leftover-placeholder check
//! alone could ever validate) is exactly the kind of "substituted cleanly
//! but still rejected by GTK's real parser" risk this file's whole reason
//! to exist already names. This is the acceptance criterion's own
//! "extended, not bypassed" — the existing two-palette assertion function
//! now takes a [`Motion`] too, rather than a second, parallel function
//! being added alongside it.
//!
//! # Re-exec under broadway
//!
//! Same shape, for the same reasons, as `tests/view_tree_renderer.rs`'s own
//! module doc comment (read that one first for the full argument): GDK's
//! backend/display auto-probe that `gtk::init()` depends on reads
//! `GDK_BACKEND`/`BROADWAY_DISPLAY` only from *this process's own*
//! environment, and mutating that in place needs `std::env::set_var` — an
//! `unsafe fn` this crate's `unsafe_code = "deny"` lint forbids, including
//! in tests. So this file re-execs itself as a child process with those two
//! variables set via `Command::env` (which sets a *child's* environment,
//! needing no `unsafe`) and a marker variable telling the child to run
//! [`run_assertions`] directly instead of re-execing a second time.
//!
//! # How this test was confirmed to actually catch a bad sheet
//!
//! Written down here rather than merely claimed, per this issue's own
//! brief. Verified directly while writing this file: `assets/stylesheet.css`
//! was temporarily edited to replace `.hop-mode-label`'s `color:
//! {{hop-neutral-400}};` declaration with the deliberately malformed
//! `color: ;` (a property with no value — real, GTK-rejected CSS, not a
//! placeholder problem `stylesheet::resolve` itself would already catch).
//! Running `cargo test -p hop-gtk --test stylesheet_provider` against that
//! edit failed both assertions below (dark and light both resolve
//! `.hop-mode-label`'s rule, so both hit the same malformed declaration),
//! printing a parser diagnostic naming the empty `color:` declaration to
//! the re-exec'd child's stderr, which this test's own failure message
//! surfaces via the parent's `output.status.success()` check. Before that:
//! confirmed the *unmodified* file first, and that it passes. The edit was
//! reverted immediately after with `git checkout -- assets/stylesheet.css`,
//! confirmed clean with `git diff --stat`. Both outcomes (the corrupted
//! failure and the restored pass) are reported verbatim in this issue's
//! task-3 report. This file's own `#[test]` always runs against the real,
//! unmodified file — the corruption above was never committed.

use std::cell::{Cell, RefCell};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::Duration;

use hop_gtk::stylesheet;
use hop_gtk::tokens::{Motion, Palette};

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_STYLESHEET_PROVIDER_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop. Duplicated from
/// `tests/view_tree_renderer.rs`'s identical helper rather than shared —
/// see that file's own copy of this struct for why: each file under
/// `tests/` compiles as its own separate crate, with no shared module
/// unless routed through `tests/common`, and this is the only piece either
/// file needs from it.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 300 + (std::process::id() % 5000);
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
fn resolved_stylesheet_loads_with_zero_parse_errors_on_both_palettes_and_both_motion_states() {
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
        .arg("resolved_stylesheet_loads_with_zero_parse_errors_on_both_palettes_and_both_motion_states")
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

    for palette in [Palette::Dark, Palette::Light] {
        for motion in [Motion::Full, Motion::Reduced] {
            assert_zero_parse_errors(palette, motion);
        }
    }

    println!("resolved stylesheet parses cleanly under both palettes and both motion states");
}

/// Resolves `assets/stylesheet.css` for `palette` and `motion`, loads it
/// into a fresh `gtk::CssProvider`, and fails with every collected parser
/// diagnostic if its `parsing-error` signal fired even once. A `Vec` of
/// messages, not just a count, is what this collects — a failure here
/// should tell a developer exactly what GTK rejected and where, not just
/// that something, somewhere, did not parse.
fn assert_zero_parse_errors(palette: Palette, motion: Motion) {
    let provider = gtk::CssProvider::new();
    let messages: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let error_count = Rc::new(Cell::new(0u32));

    provider.connect_parsing_error({
        let messages = messages.clone();
        let error_count = error_count.clone();
        move |_provider, section, error| {
            error_count.set(error_count.get() + 1);
            messages.borrow_mut().push(format!("{section:?}: {error}"));
        }
    });

    provider.load_from_string(&stylesheet::resolve(palette, motion));

    assert_eq!(
        error_count.get(),
        0,
        "expected zero gtk::CssProvider parse errors resolving assets/stylesheet.css under \
         {palette:?}/{motion:?}, got {}: {:#?}",
        error_count.get(),
        messages.borrow(),
    );
}
