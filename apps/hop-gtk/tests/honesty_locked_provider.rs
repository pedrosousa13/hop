//! Proves issue #200's actual enforcement claim: `style::install_locked`'s
//! second, above-`gtk::STYLE_PROVIDER_PRIORITY_USER` provider really does
//! out-rank a hostile user theme for the honesty-critical locked
//! categories — opacity, dimensions, and contrast — and, just as important,
//! that a user theme is still free to restyle the *ordinary*, non-locked
//! surface, **and** the one property the contract carves out on the
//! honesty-critical selectors themselves. Three directions, not one: a
//! provider that locked *everything* would still pass a one-directional
//! "locked things stay locked" test, so
//! `ordinary_non_locked_surface_is_still_user_overridable` exists
//! specifically to catch that bug, per this issue's own brief ("A
//! one-directional test would pass if you simply locked everything, which
//! would be a bug") — and a provider that correctly left the *ordinary*
//! surface alone could still, independently, over-lock a property *on*
//! `.hop-honesty` itself that the contract says stays overridable, which
//! is what `hostile_user_theme_can_override_the_locked_font_family` exists
//! to catch (a code-review fix to this issue, after `assets/stylesheet.css`'s
//! locked block was found doing exactly that to `font-family` via the
//! `font:` shorthand). None of the three would be caught by either of the
//! other two.
//!
//! # How the assertions actually read the CSS cascade, not just one
//! provider's own text
//!
//! `tests/style_colour_scheme.rs` and `tests/motion_setting.rs` both read
//! [`gtk::CssProvider::to_str`] back — enough to prove a *single* provider's
//! own content changed, but not enough to prove anything about *priority*:
//! serializing one provider's text says nothing about what wins once a
//! second provider, at a different priority, contests the same selector.
//! This file instead builds real widgets, attaches every provider under
//! test to the same real [`gtk::gdk::Display`], and reads back the
//! *resolved* style through the widget itself —
//! [`gtk::Widget::opacity`] (does the CSS `opacity:` declaration actually
//! win), [`gtk::Widget::color`] (does the CSS `color:` declaration actually
//! win — the mechanism a "contrast" claim rides on, since a locked
//! `color:`/`font-weight:`/`font-size:` set is exactly what keeps text
//! legible), [`gtk::Widget::measure`] (does `min-width`/`min-height`
//! actually win), and — added for the `font-family` overridability test, a
//! code-review fix to this issue — [`gtk::Widget::pango_context`] paired
//! with [`pango::Context::font_description`] (does a *user* theme's
//! `font-family:` actually win, the one property
//! `docs/theme-token-contract.md:18-20` says the locked provider must
//! never contest). Confirmed directly, before writing the real assertions
//! below, that the first three getters really do reflect the live CSS
//! cascade rather than only an app-set widget property that CSS happens to
//! share a name with — a throwaway probe under `gtk4-broadwayd` (built,
//! run, and discarded while writing this file) attached an ordinary, a
//! locked, and a hostile provider to one display in that exact priority
//! order and confirmed `opacity()`/`color()`/`measure()` each reported the
//! *locked* provider's values, not the hostile one's — the same
//! empirical-first discipline `style.rs`'s own doc comment describes for
//! confirming GTK's `font:` shorthand behavior before trusting it. The
//! fourth getter's own doc comment, on
//! `hostile_user_theme_can_override_the_locked_font_family` below, records
//! the equivalent confirmation for `pango_context`.
//!
//! # Why a probe widget, not the real `ui::offline_indicator::OfflineIndicator`, for
//! the dimension lock
//!
//! `.hop-honesty .hop-skeleton`'s `min-width`/`min-height` rule has no real
//! production widget wearing `.hop-skeleton` yet — this issue's own brief
//! marks the skeleton bars explicitly out of scope ("one widget is enough
//! to make the lock real and testable"). Proving the *dimension* category
//! of the lock therefore needs a widget built purely for this test, the
//! same way `assets/stylesheet.css`'s own "HONESTY-CRITICAL SELECTORS"
//! comment already treats that rule as "authored now, inert until [a
//! consumer]" — this test is not that consumer, it is a probe confirming
//! the *rule* the locked provider carries actually wins the cascade,
//! independent of which widget eventually wears the class in production.
//! The opacity and contrast assertions, by contrast, run against the real
//! [`hop_gtk::ui::offline_indicator::OfflineIndicator`] — no probe needed there, since
//! issue #200 does ship a real consumer for those two categories.
//!
//! # Re-exec under broadway
//!
//! Same shape as every other file under `tests/` that needs a real GTK
//! display — see `tests/stylesheet_provider.rs`'s own module doc for the
//! full argument against mutating this process's own environment. Base
//! display number `600`, the first unclaimed one: `headless_smoke.rs`: 100,
//! `view_tree_renderer.rs`: 200, `stylesheet_provider.rs`: 300,
//! `style_colour_scheme.rs`: 350, `ui::window`'s own tests: 400,
//! `motion_setting.rs`: 450, `font_resolution.rs`: 500.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use hop_gtk::stylesheet;
use hop_gtk::tokens::{Motion, Palette};
use hop_gtk::ui::offline_indicator::OfflineIndicator;

/// Set on the re-exec'd child so it knows to run the real assertions
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_HONESTY_LOCKED_PROVIDER_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — duplicated from every other
/// `tests/*.rs` file's identical helper rather than shared, for the same
/// reason each of those own copies gives: each file under `tests/` compiles
/// as its own separate crate.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        // Unlike every other `tests/*.rs` broadway file, which carries
        // exactly one `#[test]`, this file carries three — and Rust runs a
        // test binary's tests on parallel threads by default, all sharing
        // one process id. A plain `600 + (process::id() % 5000)`, copied
        // from those single-test files, therefore hands all three the *same*
        // display and races three `gtk4-broadwayd` instances onto it; the
        // losers fail with a bare "Failed to initialize GTK". That is a
        // race, so it survived local runs and only surfaced in CI, where
        // fewer cores and slower spawns lose it reliably. The per-server
        // counter is what makes the number unique per *test*, not merely
        // per process — the pid term still separates concurrent `cargo
        // test` runs from each other.
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let display =
            600 + (std::process::id() % 5000) + NEXT.fetch_add(1, Ordering::Relaxed) * 5001;
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

/// Re-execs this test binary under a headless `broadway` display and
/// asserts the child's real-assertion run succeeded.
fn run_under_broadway(test_name: &str) {
    if std::env::var_os(CHILD_MARKER).is_some() {
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
        .arg(test_name)
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

/// Installs `sheet` into a fresh [`gtk::CssProvider`] at `priority` on
/// `display` — the one call every provider this file attaches (ordinary,
/// locked, and hostile alike) goes through, so every one of them is guarded
/// against a parse error the identical way `style.rs`'s own
/// `guard_parse_errors` is (panicking, since a parse error in any CSS text
/// this file itself wrote is this test's own bug, not a condition to
/// silently tolerate).
fn install(display: &gtk::gdk::Display, sheet: &str, priority: u32) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_provider, section, error| {
        panic!("test-authored CSS failed to parse at {section:?}: {error}");
    });
    provider.load_from_string(sheet);
    gtk::style_context_add_provider_for_display(display, &provider, priority);
    provider
}

/// Runs the GLib main context until it has nothing left to dispatch —
/// `tests/style_colour_scheme.rs`'s identical `drain_pending_glib_events`
/// helper, duplicated for the same reason every other small helper in this
/// file is: no shared `tests/common` module exists for this crate to route
/// through, and one function is not worth inventing one for.
fn drain_pending_glib_events() {
    let ctx = glib::MainContext::default();
    while ctx.iteration(false) {}
}

#[test]
fn hostile_user_theme_cannot_override_the_locked_categories() {
    run_under_broadway("hostile_user_theme_cannot_override_the_locked_categories");
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    gtk::init().expect("gtk init under the broadway display this process's environment selects");
    use gtk::prelude::*;

    let Some(display) = gtk::gdk::Display::default() else {
        panic!("no gdk::Display available under the broadway backend this test selected");
    };

    // The two real providers `style::install`/`style::install_locked`
    // install in production, at their real priorities — resolved for one
    // fixed palette/motion pair, since this test is about priority, not
    // about re-proving palette/motion resolution (the tests below cover
    // that axis).
    install(
        &display,
        &stylesheet::resolve(Palette::Dark, Motion::Full),
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    install(
        &display,
        &stylesheet::resolve_locked_block(Palette::Dark, Motion::Full),
        hop_gtk::style::STYLE_PROVIDER_PRIORITY_LOCKED,
    );

    // The hostile theme: loaded at `STYLE_PROVIDER_PRIORITY_USER` — the
    // exact priority a real `~/.config/gtk-4.0/gtk.css` loads at — and
    // shaped exactly as this issue's brief describes: `opacity: 0`,
    // shrinking dimensions, and colour washed toward the window ground
    // (`#121214`, the dark palette's own `--hop-bg`, per
    // `stylesheet.rs`'s own `resolved_real_stylesheet_differs_between_palettes`
    // test) rather than toward the locked foreground.
    install(
        &display,
        ".hop-honesty { opacity: 0; }\n\
         .hop-honesty .hop-honesty-text { color: rgb(18, 18, 20); }\n\
         .hop-honesty .hop-honesty-stamp { color: rgb(18, 18, 20); }\n\
         .hop-honesty .hop-skeleton { min-width: 0px; min-height: 0px; }",
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    // Real production widget for the opacity and contrast categories:
    // `ui::offline_indicator::OfflineIndicator`, the one widget issue #200 actually
    // ships wearing `.hop-honesty`/`.hop-honesty-text`/`.hop-honesty-stamp`.
    let offline_indicator = OfflineIndicator::build();
    let window = gtk::Window::new();
    window.set_child(Some(&offline_indicator.widget));
    // `OfflineIndicator::build` starts hidden — `apply(Some(..))` is what a real
    // `IpcEvent::Disconnected` drives, and is also what this test needs:
    // an invisible widget's opacity/color still reads back correctly under
    // GTK4 (confirmed directly against the same throwaway probe this file's
    // module doc names), but showing it is what a real capture of this
    // scenario would do, so the test matches that rather than relying on
    // an incidental GTK behavior this test does not actually need.
    offline_indicator.apply(Some("14:32"));
    window.present();

    // Probe widget for the dimension category — see this file's module
    // doc, "Why a probe widget, not the real `OfflineIndicator`", for why
    // `.hop-skeleton` needs one of its own.
    let skeleton_probe = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    skeleton_probe.add_css_class("hop-skeleton");
    let honesty_probe = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    honesty_probe.add_css_class("hop-honesty");
    honesty_probe.append(&skeleton_probe);
    let probe_window = gtk::Window::new();
    probe_window.set_child(Some(&honesty_probe));
    probe_window.present();

    for _ in 0..50 {
        drain_pending_glib_events();
    }

    let text_widget = offline_indicator
        .widget
        .first_child()
        .expect("OfflineIndicator's container must have its text label as a first child");
    let stamp_widget = text_widget
        .next_sibling()
        .expect("OfflineIndicator's container must have its stamp label as a second child");

    assert_eq!(
        offline_indicator.widget.opacity(),
        1.0,
        "the locked provider's `.hop-honesty {{ opacity: 1; }}` must survive the hostile \
         theme's `opacity: 0`, but the offline indicator's own resolved opacity is \
         {actual}",
        actual = offline_indicator.widget.opacity(),
    );

    // `--hop-fg` under the dark palette is `#f2f1ee`, an almost-white — this
    // checks the resolved colour is nowhere near the hostile theme's
    // near-black `rgb(18, 18, 20)` wash target, rather than pinning the
    // exact literal (which `stylesheet.rs`'s own unit tests already do for
    // the token resolution itself, and which would make this test brittle
    // against an unrelated future token-value change this issue's own
    // constraints already forbid making here anyway).
    let text_color = text_widget.color();
    assert!(
        !((text_color.red() - 0.07).abs() < 0.02
            && (text_color.green() - 0.07).abs() < 0.02
            && (text_color.blue() - 0.078).abs() < 0.02),
        "the locked provider's contrast rule must survive the hostile theme's colour wash \
         toward the window ground, but the offline indicator's text resolved to {text_color:?}, \
         near the hostile theme's rgb(18, 18, 20) target"
    );

    let stamp_color = stamp_widget.color();
    assert!(
        !((stamp_color.red() - 0.07).abs() < 0.02
            && (stamp_color.green() - 0.07).abs() < 0.02
            && (stamp_color.blue() - 0.078).abs() < 0.02),
        "the locked provider's contrast rule must survive the hostile theme's colour wash \
         for the stamp label too, but it resolved to {stamp_color:?}, near the hostile \
         theme's rgb(18, 18, 20) target"
    );

    let (min_w, _, _, _) = skeleton_probe.measure(gtk::Orientation::Horizontal, -1);
    let (min_h, _, _, _) = skeleton_probe.measure(gtk::Orientation::Vertical, -1);
    assert_eq!(
        (min_w, min_h),
        (24, 9),
        "the locked provider's `.hop-honesty .hop-skeleton {{ min-width: 24px; \
         min-height: 9px; }}` must survive the hostile theme's 0×0 override, but the \
         probe measured {min_w}x{min_h}"
    );

    println!("opacity, contrast, and dimension locks all survived the hostile user-priority theme");
}

/// # The direction the two tests above never check — a code-review fix to
/// issue #200
///
/// `hostile_user_theme_cannot_override_the_locked_categories` above proves
/// the locked provider wins on opacity, dimensions, and contrast.
/// `ordinary_non_locked_surface_is_still_user_overridable` below proves a
/// user theme still wins on `.hop-offline-indicator`'s own, entirely
/// separate, non-honesty `padding`. Neither one ever restyles a property
/// *on the honesty-critical selectors themselves* and expects the user
/// theme to win — which is exactly what a review of this issue found
/// missing, and exactly what `docs/theme-token-contract.md:18-20` requires
/// to exist: "the boundary is narrow. On honesty-critical elements, a user
/// theme may still restyle the font family and accent, provided the
/// element remains present and legible." Before this fix,
/// `assets/stylesheet.css`'s locked block declared the full `font:`
/// shorthand on `.hop-honesty .hop-honesty-text`/`.hop-honesty-stamp` —
/// which also sets `font-family` — so the above-user-priority provider was
/// silently locking family too, a real over-lock this test exists to catch
/// a regression of.
///
/// # Why `pango_context().font_description()`, not
/// [`gtk::CssProvider::to_str`] or [`gtk::Widget::color`]-style getters
///
/// This file's module doc already explains why the other two tests read
/// back through the *widget*, not a provider's own serialized text — the
/// identical argument applies here, once more. But `gtk::Widget` has no
/// `font()`/`font_family()` getter the way it has [`gtk::Widget::opacity`]
/// and [`gtk::Widget::color`] (confirmed against the pinned `gtk4-0.11.4`
/// crate's own `auto/widget.rs`: no such method exists) — CSS's `color`
/// and `opacity` each happen to have a first-class GTK widget-property
/// mirror, but `font-family` does not. [`gtk::Widget::pango_context`] is
/// what does: GTK's own documentation for
/// `gtk_widget_get_pango_context` says the returned context "is already
/// configured using the appropriate font, font options... for rendering
/// text for this widget", i.e. it reflects the live, CSS-cascade-resolved
/// font, not a static default — the same "reads the resolved style back
/// through the widget, not through any one provider's own text" property
/// this file's other assertions already rely on.
/// [`pango::Context::font_description`] then hands back a
/// [`pango::FontDescription`] whose [`pango::FontDescription::family`]
/// names exactly the resolved `font-family` value — the family string this
/// test asserts on.
#[test]
fn hostile_user_theme_can_override_the_locked_font_family() {
    run_under_broadway("hostile_user_theme_can_override_the_locked_font_family");
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    gtk::init().expect("gtk init under the broadway display this process's environment selects");
    use gtk::prelude::*;

    let Some(display) = gtk::gdk::Display::default() else {
        panic!("no gdk::Display available under the broadway backend this test selected");
    };

    // The same two real hop-owned providers the other two tests in this
    // file install, at their real priorities.
    install(
        &display,
        &stylesheet::resolve(Palette::Dark, Motion::Full),
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    install(
        &display,
        &stylesheet::resolve_locked_block(Palette::Dark, Motion::Full),
        hop_gtk::style::STYLE_PROVIDER_PRIORITY_LOCKED,
    );

    // Not a hostile theme this time — a real, positive user theme,
    // restyling exactly the one property
    // `docs/theme-token-contract.md:18-20` names as still theirs to
    // restyle on a honesty-critical element: `font-family`. `monospace`
    // is deliberately nothing like `assets/tokens.css`'s own
    // `--hop-font-sans` (`"Inter", -apple-system, "Cantarell", sans-serif`)
    // so a resolved family of `"monospace"` can only mean this declaration
    // actually won the cascade, never an accidental match against the
    // ordinary sheet's own default.
    install(
        &display,
        ".hop-honesty .hop-honesty-text { font-family: monospace; }",
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    let offline_indicator = OfflineIndicator::build();
    let window = gtk::Window::new();
    window.set_child(Some(&offline_indicator.widget));
    offline_indicator.apply(Some("14:32"));
    window.present();

    for _ in 0..50 {
        drain_pending_glib_events();
    }

    let text_widget = offline_indicator
        .widget
        .first_child()
        .expect("OfflineIndicator's container must have its text label as a first child");

    let resolved_family = text_widget
        .pango_context()
        .font_description()
        .expect(
            "a widget's own pango context must have a resolved font description once its CSS \
             has actually been applied to a presented window",
        )
        .family()
        .expect("the resolved font description must name a family");

    assert_eq!(
        resolved_family.as_str(),
        "monospace",
        "a user-priority theme's `font-family` on `.hop-honesty .hop-honesty-text` must win \
         the cascade — docs/theme-token-contract.md:18-20's own carve-out — but the offline \
         indicator's text resolved to family {resolved_family:?}, not the user theme's \
         \"monospace\""
    );

    println!(
        "a user theme's font-family on the honesty-critical text still wins, exactly as the \
         contract's own carve-out requires"
    );
}

/// # Why `padding`, not `opacity` or `color`, is the property this test
/// restyles
///
/// The property this test picks has to satisfy one hard constraint the rest
/// of this comment exists to justify: `assets/stylesheet.css` must actually
/// declare it on `.hop-offline-indicator` in the *ordinary* sheet, so the ordinary
/// provider genuinely contests the user theme's declaration for it. If it
/// did not — restyling a property the ordinary sheet is silent on for this
/// selector — no rule at any priority would contest it, so the user theme's
/// declaration would win no matter what, correct implementation or buggy
/// one alike. That is not a hypothetical: an earlier version of this test
/// restyled `color`, and `assets/stylesheet.css`'s `.hop-offline-indicator` rule
/// declares only `padding` there, never `color` — so that version passed
/// vacuously, insensitive to the exact mistake it exists to catch. Caught
/// by, and fixed after, the control described below.
///
/// `opacity` was ruled out earlier still, for an unrelated reason worth
/// keeping on record: an even earlier version restyled `.hop-offline-indicator {
/// opacity: 0.5; }` — the same property the hostile-theme test above locks
/// — and it failed against a *correct* implementation, confirmed directly
/// while writing this file: `.hop-honesty`'s own locked `opacity: 1` and
/// `.hop-offline-indicator`'s hostile `opacity: 0.5` are two *different
/// selectors* that both happen to match the *same widget* (the offline
/// row's container carries both classes), and GTK's own CSS cascade orders
/// by provider priority *before* selector specificity — confirmed against
/// the same real, installed GTK 4.14 this crate's other empirical findings
/// are checked against. So the locked provider's `opacity: 1` always won
/// regardless of which selector the user theme's own `opacity` rule named,
/// because *any* higher-priority provider's declaration for a property beats
/// *any* lower-priority provider's declaration for that same property on
/// that widget, independent of specificity. That is not a bug in this
/// test's target implementation; it is `STYLE_PROVIDER_PRIORITY_LOCKED`
/// working as intended — but it does mean opacity specifically can never be
/// the property this *overridability* test restyles, because `.hop-honesty`
/// claims it on every widget that also carries `.hop-offline-indicator`.
///
/// `padding` has neither problem. `assets/stylesheet.css:461` declares
/// `.hop-offline-indicator { padding: {{hop-space-2}} {{hop-space-3}}; }` — `8px
/// 12px` once `stylesheet::resolve` fills the tokens in — so the ordinary
/// provider really does carry a competing declaration for this exact
/// (selector, property) pair, and the locked provider's own rules (which
/// only ever touch `.hop-honesty` and `.hop-honesty .hop-skeleton`, never
/// `.hop-offline-indicator` itself) never claim it either, so there is no
/// opacity-style collision to rule it out. `gtk::Widget::measure` reads it
/// back the same way the hostile-theme test above already does for
/// `.hop-skeleton`'s `min-width`/`min-height`: CSS padding is added to a
/// widget's own measured minimum size by GTK's box model regardless of its
/// children's content, so an oversized user-theme padding must inflate the
/// offline indicator's measured minimum size if, and only if, the user theme's
/// declaration is the one actually winning the cascade.
///
/// # The control that proves this version is sensitive
///
/// Confirmed directly, the same way this file's own module doc describes
/// for its other empirical claims. This function below installs the locked
/// block itself — `&stylesheet::resolve_locked_block(..)` at
/// `hop_gtk::style::STYLE_PROVIDER_PRIORITY_LOCKED` — rather than calling
/// [`hop_gtk::style::install_locked`] (this file's providers are built by
/// hand throughout, mirroring what `install`/`install_locked` each load at
/// their real priority, without going through the production functions
/// themselves — see this file's own module doc, "How the assertions
/// actually read the CSS cascade", for why: those functions also wire up
/// live GSettings subscriptions this synthetic-display test has no need
/// for). So the control that actually exercises this test's code path is
/// swapping *that* line's `stylesheet::resolve_locked_block` for
/// `stylesheet::resolve` (the *full* sheet, including this same
/// `.hop-offline-indicator { padding: 8px 12px; }` rule) — the direct, in-test
/// equivalent of the mistake `style.rs`'s own module doc warns
/// `install_locked` must never make. With that one-line swap in place, this
/// test's assertion below fails: the "locked" provider now also carries the
/// ordinary 8px/12px padding declaration at `STYLE_PROVIDER_PRIORITY_LOCKED`,
/// which outranks `STYLE_PROVIDER_PRIORITY_USER`, so the user theme's
/// oversized override loses and the measured size stays at the ordinary
/// default. Reverting the swap makes it pass again. (Editing
/// `style::install_locked` itself, rather than this line, was tried first
/// and confirmed to have no effect on this test either way — this test
/// never calls `install_locked`, for the reason above, so a bug isolated to
/// that function's own body cannot show up here; the swap above is what
/// actually stands in for it.) The exact numbers observed while running
/// this control, for the same `padding: 300px` override this test installs
/// below: ordinary default (buggy swap in place) measures 149×34; the user
/// theme's override, correctly winning (swap reverted), measures 725×618 —
/// a margin far wider than any plausible font-metric variance across
/// environments, which is why the assertion below uses a wide separating
/// threshold rather than pinning either exact value.
#[test]
fn ordinary_non_locked_surface_is_still_user_overridable() {
    run_under_broadway("ordinary_non_locked_surface_is_still_user_overridable");
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    gtk::init().expect("gtk init under the broadway display this process's environment selects");
    use gtk::prelude::*;

    let Some(display) = gtk::gdk::Display::default() else {
        panic!("no gdk::Display available under the broadway backend this test selected");
    };

    // The same two real hop-owned providers as the test above — this test's
    // whole point is that installing *both* still leaves the ordinary,
    // non-locked surface user-overridable, so a reviewer cannot mistake
    // the previous test's pass for "the locked provider happens to win
    // because nothing else was installed".
    install(
        &display,
        &stylesheet::resolve(Palette::Dark, Motion::Full),
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    install(
        &display,
        &stylesheet::resolve_locked_block(Palette::Dark, Motion::Full),
        hop_gtk::style::STYLE_PROVIDER_PRIORITY_LOCKED,
    );

    // A real, positive user theme, restyling ordinary surface exactly as
    // `docs/theme-token-contract.md`'s "Ordinary user-theme surface" section
    // says it may: `.hop-offline-indicator`, the offline indicator's own *layout*
    // class, deliberately kept separate from `.hop-honesty`
    // (`assets/stylesheet.css`'s own comment on that rule), and never
    // mentioned anywhere in the locked block. `padding`, not `color` or
    // `opacity`, is the property this restyle targets — see this function's
    // own doc comment above for why. `300px` is deliberately far larger
    // than any real theme would ever use — the point is only to make the
    // ordinary sheet's `8px 12px` default and the user theme's override
    // trivially distinguishable by measured size, per the control in this
    // function's own doc comment.
    install(
        &display,
        ".hop-offline-indicator { padding: 300px; }",
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );

    let offline_indicator = OfflineIndicator::build();
    let window = gtk::Window::new();
    window.set_child(Some(&offline_indicator.widget));
    offline_indicator.apply(Some("14:32"));
    window.present();

    for _ in 0..50 {
        drain_pending_glib_events();
    }

    // GTK adds a widget's own CSS padding to its measured minimum size
    // regardless of its children's content (the same box-model mechanism
    // the hostile-theme test above relies on for `.hop-skeleton`'s
    // `min-width`/`min-height`), so a 300px padding actually winning must
    // push the offline indicator's measured minimum size far past what the
    // ordinary sheet's own `8px 12px` default could ever produce for this
    // row's two short labels — 149×34, measured directly as part of the
    // control described in this function's own doc comment. 400 sits
    // comfortably above that ordinary-default ceiling and comfortably
    // below the 725×618 the user theme's override actually produces when
    // it wins, so crossing it can only mean the override won the cascade.
    let (min_w, _, _, _) = offline_indicator
        .widget
        .measure(gtk::Orientation::Horizontal, -1);
    let (min_h, _, _, _) = offline_indicator
        .widget
        .measure(gtk::Orientation::Vertical, -1);
    assert!(
        min_w > 400 && min_h > 400,
        "a user theme restyling `.hop-offline-indicator`'s padding — ordinary, non-locked surface \
         — must still win against hop's own providers, but the offline indicator's own resolved \
         minimum size is {min_w}x{min_h}, too close to the ordinary sheet's own 149x34 \
         default and nowhere near the user theme's 300px override; the carve-out must not \
         have quietly narrowed to exclude this declaration"
    );

    println!("a user theme still wins on the offline indicator's own non-locked padding");
}
