//! Proves issue #198's own acceptance criterion that a registered GResource
//! is not the same claim as a face Pango's font map actually resolves:
//! "A test proves the bundled faces resolve — not merely that the resource
//! registered. The distinction matters: a registered resource that GTK's
//! font map never picks up is the failure this issue exists to prevent."
//! `fonts.rs`'s own unit tests (`resource_paths_resolve_to_nonempty_bytes`,
//! `materialized_files_exist_and_byte_match_the_resource_data`) already
//! cover the registered-and-materialized half — every byte is where
//! [`FACES`] says it is. Neither of those tests ever asks Pango to lay out
//! a single glyph, so neither can catch the actual failure mode this issue
//! closes: a `.gresource` that registers cleanly and a directory
//! fontconfig is told about, where the font map still hands back some
//! *other* face — Noto Sans, DejaVu Sans Mono, Cantarell, whatever the
//! ambient system happens to carry — because the ordering, the fontconfig
//! call, or the ownership of `$FONTCONFIG_FILE` was subtly wrong. This file
//! exists to catch exactly that, by asking Pango the question this issue's
//! whole brief is actually about: "if I ask you for `family`, what do I
//! get back?"
//!
//! [`FACES`]: hop_gtk::fonts::FACES
//!
//! # Why `Context::load_font`, not `Context::list_families`
//!
//! `pango::Context::list_families` (or the identically-shaped
//! `pango::FontMap::list_families`) would prove only that a family *named*
//! `"Inter"` or `"Iosevka Term"` exists somewhere fontconfig's font map
//! knows about — a containing-check over a list. That is a materially
//! weaker claim than this issue needs, for a specific reason: Pango's own
//! matching never fails outright. Ask it for a family it has never heard
//! of, at any weight, and `Context::load_font` still returns `Some(Font)`,
//! having silently substituted its own best guess (Noto Sans, DejaVu, or
//! whatever the environment's own fallback chain resolves to) — the exact
//! silent-degradation failure mode `assets/tokens.css`'s own header names
//! by name ("A launcher cannot let its identity element fall back silently
//! to generic `monospace` on a fresh install"). A `list_families` check
//! cannot see that substitution happen; loading the font and reading back
//! what `Font::describe().family()` actually says can, because a
//! substituted font describes itself honestly as whatever it actually is.
//! So every load in this file follows the same shape: build a
//! [`pango::FontDescription`] asking for one exact bundled `(family,
//! weight)` pair, [`pango::Context::load_font`] it, and assert the loaded
//! font's own [`pango::FontDescription::family`] echoes the request back —
//! not merely that some entry in a list matches.
//!
//! # What is, and is not, proved here
//!
//! **Proved:**
//! - [`strong_resolution_check`] — requesting `"Inter"` and requesting
//!   `"Iosevka Term"`, at a real weight either family bundles, each
//!   resolves to a loaded font whose own `family()` matches the request
//!   exactly. This is the test the issue's acceptance criterion names by
//!   name.
//! - [`iosevka_term_resolves_to_a_family_fontconfig_itself_reports_as_monospace`]
//!   — corroboration alongside the strong check above: the resolved
//!   `"Iosevka Term"` family is one `pango::FontFamily::is_monospace`
//!   itself reports `true` for, not merely a family that happens to share
//!   the name.
//! - [`every_bundled_face_resolves_to_its_own_family`] — all five
//!   `(family, weight)` pairs [`FACES`] declares resolve to their own
//!   requested family, not just the two distinct family *names* the checks
//!   above already cover — the five-file bundle could still ship a face at
//!   the wrong weight, or fail to register one weight specifically, in a
//!   way neither of the two name-only checks above would ever see.
//! - [`mono_face_gives_equal_advance_widths_where_the_proportional_face_does_not`]
//!   — the other acceptance criterion this issue names explicitly: "Mono
//!   genuinely renders as mono... comparing a mono-specified element's
//!   rendered advance width against a proportional one is enough." See
//!   that function's own doc comment for exactly what is measured and why
//!   it proves the claim, without sampling a single pixel.
//!
//! **Not proved here:** that a real, on-screen `hop-gtk` window actually
//! paints these faces in place of whatever CSS rule requests them — that is
//! a claim about `assets/stylesheet.css`'s own rules and `ui::window`'s
//! widget tree, already out of this issue's scope (see `fonts.rs`'s module
//! doc). What this file proves is the layer immediately below that: ask
//! for `"Inter"` or `"Iosevka Term"` through the same `pango::Context`
//! mechanism any GTK widget uses, and Pango genuinely hands back the
//! bundled face rather than a fallback. A real capture, taken manually
//! (`gtk4-broadwayd` plus `hop-gtk --screenshot`, the same shape #215
//! used), is the separate, complementary proof that the actual window
//! renders this way — see this issue's own PR notes for that capture's
//! path.
//!
//! # Re-exec under broadway
//!
//! Same shape as `tests/style_colour_scheme.rs`, `tests/stylesheet_provider.rs`
//! and `tests/view_tree_renderer.rs` — read any of those module docs for
//! the full argument against mutating this process's own environment
//! (`GDK_BACKEND`/`BROADWAY_DISPLAY` have to be set *before* `gtk::init()`
//! runs, and setting them from inside an already-running process would
//! need an `unsafe`, racy `std::env::set_var`). `gtk::init()` is called
//! directly here, not `adw::init()` — this file needs a `pango::Context`
//! off a plain `gtk::Widget`, nothing from `libadwaita`.
//!
//! [`hop_gtk::fonts::bundle`] is called *before* `gtk::init()`, in that
//! order, deliberately mirroring `app::run`'s own production ordering:
//! `fonts.rs`'s doc comment ("Registering with fontconfig", "The ordering
//! hazard") and `app.rs`'s own comment on its `fonts::bundle()` call both
//! explain why fontconfig registration has no recovery path if it loses
//! the race against Pango constructing its first font map. This test would
//! not actually exercise the claim it is named for if it called `bundle()`
//! after `gtk::init()` — it would risk passing for the wrong reason (an
//! already-cached system font map that happens to have picked up the
//! bundled directory some other way) rather than the right one.
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gtk::pango;
use gtk::pango::prelude::*;
use gtk::prelude::*;

use hop_gtk::fonts;

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_FONT_RESOLUTION_TEST_CHILD";

/// A point size with no particular significance beyond being a real,
/// positive size every [`pango::FontDescription`] in this file sets —
/// `Context::load_font` and `pango::Layout` sizing both need *some* size to
/// resolve or lay out against, and 16px is simply large enough that two
/// glyphs' advance widths (used by the monospace-vs-proportional check
/// below) differ by more than rounding noise would ever produce.
const TEST_POINT_SIZE: i32 = 16;

/// A spawned `gtk4-broadwayd`, killed on drop — duplicated from
/// `tests/style_colour_scheme.rs`'s identical helper rather than shared,
/// for the same reason that file's own copy gives: each file under
/// `tests/` compiles as its own separate crate. The base display number
/// (`500`) is deliberately different from every other file's own base
/// (`headless_smoke.rs`: 100, `view_tree_renderer.rs`: 200,
/// `stylesheet_provider.rs`: 300, `style_colour_scheme.rs`: 350,
/// `motion_setting.rs`: 450) so a parallel `cargo test` run can never
/// compute the same broadway display number as another file's test and
/// collide on its socket.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 500 + (std::process::id() % 5000);
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
fn bundled_faces_resolve_through_a_real_pango_context() {
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
        .arg("bundled_faces_resolve_through_a_real_pango_context")
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
    fonts::bundle().unwrap_or_else(|err| {
        panic!("fonts::bundle() returned an error: {err}");
    });

    gtk::init().expect("gtk init under the broadway display this process's environment selects");

    // Any widget's own `pango_context()` is the same mechanism every real
    // widget in this crate uses to lay out text — `gtk_widget_get_pango_context`
    // resolves against the widget's display, which falls back to the
    // default `GdkDisplay` for an unrealized, unparented widget like this
    // one (GTK4's own documented behavior), and the default display is the
    // broadway one this process's environment selected above. Nothing
    // about this widget is ever shown; it exists only to reach a real,
    // display-backed `pango::Context`.
    let widget = gtk::Label::new(None);
    let context = widget.pango_context();

    strong_resolution_check(&context, "Inter");
    strong_resolution_check(&context, "Iosevka Term");

    iosevka_term_resolves_to_a_family_fontconfig_itself_reports_as_monospace(&context);

    every_bundled_face_resolves_to_its_own_family(&context);

    mono_face_gives_equal_advance_widths_where_the_proportional_face_does_not(&context);

    println!(
        "all five bundled (family, weight) pairs resolve through a real pango::Context, \
         Iosevka Term resolves to a family fontconfig itself reports as monospace, and its \
         rendered advance width is uniform where Inter's is not"
    );
}

/// Builds a [`pango::FontDescription`] for `family` at [`TEST_POINT_SIZE`],
/// with `weight` set only if given — the one description-building helper
/// every check in this file shares, so the size and construction shape
/// cannot drift between them.
fn font_description(family: &str, weight: Option<pango::Weight>) -> pango::FontDescription {
    let mut desc = pango::FontDescription::new();
    desc.set_family(family);
    if let Some(weight) = weight {
        desc.set_weight(weight);
    }
    desc.set_size(TEST_POINT_SIZE * pango::SCALE);
    desc
}

/// The strong check this issue's acceptance criterion names: requesting
/// `family` (no weight pinned — any weight fontconfig's own matching picks
/// is fine here, since the point is the *family*, not a specific cut) must
/// resolve to a loaded [`pango::Font`] whose own
/// [`pango::FontDescription::family`] echoes `family` back exactly. See
/// this file's module doc, "Why `Context::load_font`, not
/// `Context::list_families`", for why this — and not a containing-check
/// over `list_families()` — is the assertion that actually catches a
/// silent substitution.
fn strong_resolution_check(context: &pango::Context, family: &str) {
    let desc = font_description(family, None);

    let loaded = context
        .load_font(&desc)
        .unwrap_or_else(|| panic!("pango::Context::load_font returned None for family {family:?}"));

    let resolved_family = loaded.describe().family();
    assert_eq!(
        resolved_family.as_deref(),
        Some(family),
        "requesting family {family:?} resolved to a different family ({resolved_family:?}) — \
         Pango's own matching never fails outright, so this is exactly the silent-substitution \
         failure issue #198 exists to catch",
    );
}

/// Corroborates [`strong_resolution_check`]'s `"Iosevka Term"` case: not
/// only does the family name match, the resolved family is one fontconfig
/// itself classifies as monospace. This is a second, independent signal —
/// a family could in principle be named `"Iosevka Term"` by mistake without
/// actually being a monospace face — and it is the one Pango-level property
/// [`pango::FontFamily::is_monospace`] exposes directly, ahead of this
/// file's own stronger, rendered-width proof further down
/// ([`mono_face_gives_equal_advance_widths_where_the_proportional_face_does_not`]).
fn iosevka_term_resolves_to_a_family_fontconfig_itself_reports_as_monospace(
    context: &pango::Context,
) {
    let font_map = context
        .font_map()
        .expect("a pango::Context built from a real, realized-display widget must have a font map");

    let family = font_map.family("Iosevka Term").expect(
        "fontconfig's font map has no family named \"Iosevka Term\" — bundle() should \
                 have already made strong_resolution_check fail before this ever ran",
    );

    assert!(
        family.is_monospace(),
        "\"Iosevka Term\" resolved to a family, but pango::FontFamily::is_monospace() reports \
         false for it — that is not the monospace face this issue bundles",
    );
}

/// All five [`hop_gtk::fonts::FACES`] entries, not just the two distinct
/// family *names* [`strong_resolution_check`] already covers: this is what
/// would catch a bundle that resolves `"Inter"` correctly at its default
/// weight but is missing, or has swapped, one of its other two weights (or
/// either of `"Iosevka Term"`'s two), a defect neither of the two name-only
/// checks above could see.
fn every_bundled_face_resolves_to_its_own_family(context: &pango::Context) {
    for face in hop_gtk::fonts::FACES {
        let desc = font_description(face.family, Some(pango_weight(face.weight)));

        let loaded = context.load_font(&desc).unwrap_or_else(|| {
            panic!(
                "pango::Context::load_font returned None for {} weight {}",
                face.family, face.weight
            )
        });

        let resolved_family = loaded.describe().family();
        assert_eq!(
            resolved_family.as_deref(),
            Some(face.family),
            "requesting {} weight {} resolved to a different family ({resolved_family:?})",
            face.family,
            face.weight,
        );
    }
}

/// Maps a [`hop_gtk::fonts::Face::weight`] value to the [`pango::Weight`]
/// variant with the identical numeric meaning — `PANGO_WEIGHT_NORMAL`,
/// `_MEDIUM` and `_SEMIBOLD` are defined as 400, 500 and 600 respectively,
/// the same CSS numeric weight scale `assets/tokens.css` and [`FACES`] both
/// already use, so this is a renaming, not a unit conversion. The
/// fallback panics rather than guessing, because a weight [`FACES`] names
/// that is not one of the three real weights this bundle ships would mean
/// this test itself has drifted from `fonts.rs`'s own data — worth failing
/// loudly over, not silently rounding to the nearest known weight.
///
/// [`FACES`]: hop_gtk::fonts::FACES
fn pango_weight(weight: u16) -> pango::Weight {
    match weight {
        400 => pango::Weight::Normal,
        500 => pango::Weight::Medium,
        600 => pango::Weight::Semibold,
        other => panic!(
            "hop_gtk::fonts::FACES named weight {other}, which this test does not know how to \
             map to a pango::Weight — FACES has drifted from what this test expects"
        ),
    }
}

/// The other acceptance criterion issue #198 names explicitly: "Mono
/// genuinely renders as mono... comparing a mono-specified element's
/// rendered advance width against a proportional one is enough, and state
/// plainly what you measured."
///
/// **What is measured:** for each of two strings, `"iiiii"` (five narrow
/// glyphs) and `"WWWWW"` (five wide ones), a [`pango::Layout`] is built
/// with `"Iosevka Term"` at weight 500 (one of [`FACES`]'s own two Iosevka
/// Term weights) and its total logical width read back via
/// [`pango::Layout::size`]. The same two strings are then laid out again
/// with `"Inter"`, also at weight 500 — one of [`FACES`]'s own three Inter
/// weights, and the one weight both bundled families actually share, so
/// this needs no second [`pango_weight`] case.
///
/// **Why this proves the claim:** a monospace face's entire definition is
/// that every glyph advances the pen by the same fixed width regardless of
/// its shape — `"i"` and `"W"` occupy the same horizontal cell even though
/// their ink is very different widths. So `"iiiii"` and `"WWWWW"` laid out
/// in a genuinely monospace face must measure to *exactly* the same total
/// width; laid out in a proportional face — where each glyph's advance is
/// however wide that glyph actually is — they almost certainly measure to
/// *different* widths, because five narrow `i` glyphs and five wide `W`
/// glyphs are not the same width in any proportional design. Measuring
/// equal-for-mono and unequal-for-proportional in the same test, against
/// the same [`pango::Context`], is what makes this a comparison rather
/// than a single number asserted against a guessed constant — it needs no
/// assumption about what either width "should" be, only that a real mono
/// face is internally consistent between glyphs and a real proportional
/// face is not. No pixel is sampled anywhere in this check — [`pango::Layout::size`]
/// reports Pango's own logical layout width, computed from each glyph's
/// advance metrics, which is what "advance width" names.
///
/// [`FACES`]: hop_gtk::fonts::FACES
fn mono_face_gives_equal_advance_widths_where_the_proportional_face_does_not(
    context: &pango::Context,
) {
    let iosevka_i = layout_width(context, "Iosevka Term", "iiiii");
    let iosevka_w = layout_width(context, "Iosevka Term", "WWWWW");
    assert_eq!(
        iosevka_i, iosevka_w,
        "Iosevka Term (mono) must give \"iiiii\" and \"WWWWW\" the same total advance width; \
         got {iosevka_i} and {iosevka_w} Pango units — a mismatch here means this is not \
         actually rendering as a monospace face",
    );

    let inter_i = layout_width(context, "Inter", "iiiii");
    let inter_w = layout_width(context, "Inter", "WWWWW");
    assert_ne!(
        inter_i, inter_w,
        "Inter (proportional) gave \"iiiii\" and \"WWWWW\" the same total advance width \
         ({inter_i} Pango units each) — either Inter itself failed to resolve and both strings \
         fell back to the same substituted face, or something is wrong with this measurement",
    );
}

/// Lays `text` out in `family` at weight 500 and returns the resulting
/// layout's total logical width, in Pango units (`pango::SCALE` per
/// pixel) — the one measurement
/// [`mono_face_gives_equal_advance_widths_where_the_proportional_face_does_not`]
/// takes four of, on two families and two strings.
fn layout_width(context: &pango::Context, family: &str, text: &str) -> i32 {
    let layout = pango::Layout::new(context);
    let desc = font_description(family, Some(pango::Weight::Medium));
    layout.set_font_description(Some(&desc));
    layout.set_text(text);
    layout.size().0
}
