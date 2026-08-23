//! Proves the hovered-row lift `assets/stylesheet.css`'s `listview >
//! row`/`listview > row:hover` pair implements for SPEC decisions 2 and 6
//! (issue #253) — see that pair's own "ROW HOVER LIFT" comment for the full
//! design account this file only tests against, never re-explains.
//!
//! Four claims, four tests, matching the task brief's own enumeration:
//!
//! 1. [`lift_transform_resolves_to_the_real_value_under_full_motion_and_to_none_under_reduced`]
//!    — the motion axis: [`Motion::Full`] carries the real translate,
//!    [`Motion::Reduced`] collapses it to `none`, proving the rule reads
//!    `{{motion:hop-lift-transform}}` (which reaches
//!    [`hop_gtk::tokens::resolve_motion`]) and not the palette-only
//!    `{{hop-lift-transform}}` spelling this file's own comment warns a
//!    future edit could silently regress to.
//! 2. [`elevation_shadow_resolves_differently_under_dark_and_light_palettes`]
//!    — the palette axis: `{{hop-elev}}` resolves to the dark ramp's shadow
//!    under [`Palette::Dark`] and a *different* concrete shadow under
//!    [`Palette::Light`], proving the rule routes through the semantic
//!    alias `tokens.css`'s `:root`/`.hop-theme-light` blocks both declare,
//!    not a bare `--hop-elev-1` ramp literal that would silently resolve
//!    to the same dark value under both palettes (the exact trap
//!    `listview > row`'s own ROW SURFACE comment documents for
//!    `--hop-neutral-900`, cited by this rule's comment for the identical
//!    reason).
//! 3. [`transition_lives_on_the_resting_row_not_the_hover_rule`] — locks in
//!    the placement decision the ROW HOVER LIFT comment argues for at
//!    length (checked directly against GTK 4.14.5's own source while
//!    writing that comment, not merely asserted): the resting rule must
//!    carry `transition:`, and `:hover` must not re-declare it, or a
//!    future edit "matching the hint's pattern" would silently regress the
//!    lift back to one-directional (eases in, snaps back out).
//! 4. [`resolved_hover_rule_carries_real_unclipped_transform_and_shadow_in_a_real_gtk_provider`]
//!    — the GTK-parses-it guard: hands the *exact* resolved
//!    `listview > row`/`listview > row:hover` text to a real
//!    `gtk::CssProvider` (same re-exec-under-broadway shape
//!    `tests/stylesheet_provider.rs` already uses, for the identical
//!    reason given there) and reads back its own serialized
//!    `to_str()` — not the placeholder-resolved source text — to confirm
//!    GTK's parser kept `transform`/`box-shadow`/`transition-*` as real,
//!    non-empty declarations rather than silently dropping an unknown
//!    property the way `tests/stylesheet_provider.rs`'s own module doc
//!    describes GTK doing for `assets/tokens.css` handed to it directly.
//!
//! `tests/stylesheet_provider.rs`'s own whole-file zero-parse-error check
//! already covers this rule incidentally (it resolves and parses the
//! *entire* file, this rule included, under all four palette/motion
//! combinations) — this file's fourth test is narrower and more direct on
//! purpose: it asserts what the parsed declarations actually *contain*,
//! not merely that parsing produced zero errors, which a property GTK
//! silently drops (no error, no effect) would still pass.
//!
//! # Why this file, not new assertions folded into an existing one
//!
//! `tests/stylesheet_provider.rs` and `tests/motion_setting.rs` each own a
//! narrow, already-large claim of their own (see their own module docs);
//! adding a third, unrelated rule's worth of assertions to either would
//! blur which file is the source of truth for this rule's own behaviour.
//! This file is that source of truth instead — one rule, one file, the
//! same shape `tests/motion_setting.rs`'s own module doc gives for why it
//! exists apart from `tests/style_colour_scheme.rs`.

use hop_gtk::stylesheet;
use hop_gtk::tokens::{Motion, Palette};

/// Finds `selector`'s first `{ ... }` block in `sheet` (a resolved
/// stylesheet, comments already stripped by [`stylesheet::resolve`]),
/// inclusive of the braces. Duplicated from
/// `hop_gtk::stylesheet`'s own private `tests::extract_rule` rather than
/// shared — this crate's `tests/` binaries have no access to a `src/`
/// module's private test helpers, the same constraint every other
/// `tests/*.rs` file's own duplicated `BroadwayServer` helper documents for
/// its own copy-not-share choice.
fn extract_rule<'a>(sheet: &'a str, selector: &str) -> &'a str {
    let start = sheet
        .find(selector)
        .unwrap_or_else(|| panic!("selector {selector:?} not found in resolved sheet"));
    let open = sheet[start..]
        .find('{')
        .map(|i| start + i)
        .unwrap_or_else(|| panic!("selector {selector:?} has no opening `{{`"));
    let close = sheet[open..]
        .find('}')
        .map(|i| open + i)
        .unwrap_or_else(|| panic!("selector {selector:?}'s rule has no closing `}}`"));
    &sheet[open..=close]
}

#[test]
fn lift_transform_resolves_to_the_real_value_under_full_motion_and_to_none_under_reduced() {
    let full = stylesheet::resolve(Palette::Dark, Motion::Full);
    let reduced = stylesheet::resolve(Palette::Dark, Motion::Reduced);

    let full_hover = extract_rule(&full, "listview > row:hover {");
    let reduced_hover = extract_rule(&reduced, "listview > row:hover {");

    assert!(
        full_hover.contains("transform: translateY(-1px);"),
        "under full motion the hover rule should carry the real, token-resolved lift \
         transform, got: {full_hover}"
    );
    assert!(
        reduced_hover.contains("transform: none;"),
        "under reduced motion --hop-lift-transform's own @media override must collapse the \
         hover rule's transform to `none` — the vestibular-trigger strip SPEC decision 2 \
         requires — got: {reduced_hover}"
    );
    assert_ne!(
        full_hover, reduced_hover,
        "the hover rule must differ between motion states — got the same rule under both: \
         {full_hover}"
    );
}

#[test]
fn elevation_shadow_resolves_differently_under_dark_and_light_palettes() {
    let dark = stylesheet::resolve(Palette::Dark, Motion::Full);
    let light = stylesheet::resolve(Palette::Light, Motion::Full);

    let dark_hover = extract_rule(&dark, "listview > row:hover {");
    let light_hover = extract_rule(&light, "listview > row:hover {");

    // `--hop-elev-1`/`--hop-elev-1-light` are each authored in
    // `tokens.css` as a two-shadow value split across a wrapped
    // continuation line for readability, indentation included — that
    // literal whitespace survives substitution verbatim, so this
    // compares against a whitespace-normalized (single-space-collapsed)
    // copy of each rule rather than pinning the exact column the token's
    // own line-wrap happens to land on.
    let normalize = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let dark_hover_n = normalize(dark_hover);
    let light_hover_n = normalize(light_hover);

    assert!(
        dark_hover_n.contains(
            "box-shadow: 0 1px 2px rgba(0, 0, 0, 0.35), 0 10px 24px -14px rgba(0, 0, 0, 0.6);"
        ),
        "the dark palette's hover rule should carry --hop-elev-1's own literal shadow via the \
         --hop-elev semantic alias, got: {dark_hover}"
    );
    assert!(
        light_hover_n.contains(
            "box-shadow: 0 1px 2px rgba(33, 31, 26, 0.08), 0 10px 24px -16px rgba(33, 31, 26, 0.2);"
        ),
        "the light palette's hover rule should carry --hop-elev-1-light's own literal shadow \
         via .hop-theme-light's --hop-elev override, got: {light_hover}"
    );
    assert_ne!(
        dark_hover_n, light_hover_n,
        "a bare --hop-elev-1 ramp literal would resolve to the identical dark shadow under \
         both palettes — the rule must route through the --hop-elev semantic alias instead, \
         which these two rules must therefore differ to prove; got the same rule under both: \
         {dark_hover}"
    );
}

#[test]
fn transition_lives_on_the_resting_row_not_the_hover_rule() {
    let sheet = stylesheet::resolve(Palette::Dark, Motion::Full);

    let resting = extract_rule(&sheet, "listview > row {");
    let hover = extract_rule(&sheet, "listview > row:hover {");

    assert!(
        resting.contains("transition:"),
        "the resting `listview > row` rule must declare the transition itself — GTK always \
         reads transition-duration/-property/-timing-function off the style being \
         transitioned *into* (gtk_css_animated_style_create_css_transitions's own \
         `base_style` argument in GTK 4.14.5's gtk/gtkcssanimatedstyle.c), so only a \
         transition declared here (present in the cascade both when `:hover` matches and \
         when it does not) animates both the lift-in and the settle-back-out, got: {resting}"
    );
    assert!(
        !hover.contains("transition:"),
        "the hover rule must NOT redeclare transition: — this is the resting rule's job here, \
         deliberately the opposite of `.hop-row-hint-shown`'s own precedent (which declares \
         transition: on the *entered* rule, on purpose, for a one-directional entrance-only \
         fade); redeclaring it on :hover here would not break the lift-in animation but would \
         make this test's own proof of *where* the working declaration lives ambiguous, got: \
         {hover}"
    );
}

/// The real assertions for [`resolved_hover_rule_carries_real_unclipped_transform_and_shadow_in_a_real_gtk_provider`],
/// run inside the re-exec'd child process — same shape as
/// `tests/stylesheet_provider.rs`'s own `run_assertions`/`CHILD_MARKER`
/// dance, duplicated rather than shared for the reason every other
/// `tests/*.rs` file in this crate already gives for its own copy of this
/// pattern: each file under `tests/` compiles as its own separate crate.
mod broadway_guard {
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use hop_gtk::stylesheet;
    use hop_gtk::tokens::{Motion, Palette};

    const CHILD_MARKER: &str = "HOP_GTK_ROW_HOVER_LIFT_TEST_CHILD";

    /// Base display number `550` — deliberately distinct from every other
    /// `tests/*.rs` file's own base (100/200/300/350/450, per
    /// `tests/motion_setting.rs`'s own comment enumerating them) so a
    /// parallel `cargo test` run across this crate's several integration
    /// test binaries can never collide on the same broadway socket.
    struct BroadwayServer {
        child: Child,
    }

    impl BroadwayServer {
        fn start(display: u32) -> Self {
            let child = Command::new("gtk4-broadwayd")
                .arg(format!(":{display}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect(
                    "failed to spawn gtk4-broadwayd — it ships in libgtk-4-bin (NOT \
                     `broadwayd` on $PATH, which on Debian/Ubuntu is libgtk-3-bin's \
                     incompatible GTK3 server; see headless_smoke.rs's top doc comment for \
                     how this was diagnosed)",
                );
            std::thread::sleep(Duration::from_millis(300));
            BroadwayServer { child }
        }
    }

    impl Drop for BroadwayServer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn resolved_hover_rule_carries_real_unclipped_transform_and_shadow_in_a_real_gtk_provider() {
        if std::env::var_os(CHILD_MARKER).is_some() {
            run_assertions();
            return;
        }

        let display = 550 + (std::process::id() % 5000);
        let broadway = BroadwayServer::start(display);

        let current_exe = std::env::current_exe()
            .expect("failed to resolve this test binary's own path to re-exec it");
        let output = Command::new(current_exe)
            .env("GDK_BACKEND", "broadway")
            .env("BROADWAY_DISPLAY", format!(":{display}"))
            .env(CHILD_MARKER, "1")
            .arg("--exact")
            .arg(
                "broadway_guard::resolved_hover_rule_carries_real_unclipped_transform_and_shadow_in_a_real_gtk_provider",
            )
            .arg("--nocapture")
            .output()
            .expect("failed to re-exec this test binary under the headless broadway display");
        drop(broadway);

        assert!(
            output.status.success(),
            "the headless child process failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn run_assertions() {
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        let sheet = stylesheet::resolve(Palette::Dark, Motion::Full);

        let provider = gtk::CssProvider::new();
        let messages: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let messages = messages.clone();
            provider.connect_parsing_error(move |_provider, section, error| {
                messages.borrow_mut().push(format!("{section:?}: {error}"));
            });
        }
        provider.load_from_string(&sheet);
        assert!(
            messages.borrow().is_empty(),
            "expected zero gtk::CssProvider parse errors resolving the full stylesheet, got: \
             {:#?}",
            messages.borrow(),
        );

        // `to_str()` re-serializes what GTK's parser actually kept, not the
        // placeholder-resolved source text — the only way to prove a
        // property was not silently understood-and-discarded as an
        // unsupported value, the exact failure mode this test exists to
        // rule out (see this file's own module doc).
        let serialized = provider.to_str();

        let hover_rule = extract_serialized_rule(&serialized, "listview > row:hover");
        assert!(
            hover_rule.contains("transform:") && !hover_rule.contains("transform: none;"),
            "GTK's own serialized provider dump should carry a real, non-`none` transform on \
             the hover rule, got: {hover_rule}"
        );
        assert!(
            hover_rule.contains("box-shadow:") && !hover_rule.contains("box-shadow: none;"),
            "GTK's own serialized provider dump should carry a real, non-`none` box-shadow on \
             the hover rule, got: {hover_rule}"
        );

        let resting_rule = extract_serialized_rule(&serialized, "listview > row {");
        assert!(
            resting_rule.contains("transition-property:")
                && resting_rule.contains("transition-duration:"),
            "GTK's own serialized provider dump should carry real transition-* longhands on \
             the resting row rule (the shorthand `transition:` this file authors is always \
             re-serialized to its longhands — the identical shape \
             tests/motion_setting.rs's own HINT_SHOWN_RULE_MARKER comment already documents \
             for `.hop-row-hint-shown`), got: {resting_rule}"
        );

        println!(
            "resolved listview > row / listview > row:hover parse with zero errors in a real \
             gtk::CssProvider, and both the hover transform/shadow and the resting \
             transition-* longhands survive into its own serialized dump"
        );
    }

    /// Finds the `{ ... }` block whose *selector* is exactly `selector` in
    /// `css` (a serialized `gtk::CssProvider::to_str()` dump), inclusive of
    /// the braces. Unlike `stylesheet::resolve`'s own comment-stripped
    /// source text, GTK's serializer never re-orders or merges *distinct*
    /// selectors the way it reorders a single compound selector's classes
    /// (`tests/motion_setting.rs`'s own `HINT_SHOWN_RULE_MARKER` comment
    /// documents that narrower case), so matching `selector` verbatim is
    /// sufficient here — `"listview > row {"` and `"listview > row:hover"`
    /// cannot be confused with each other as substrings of one another in
    /// either direction.
    fn extract_serialized_rule<'a>(css: &'a str, selector: &str) -> &'a str {
        let start = css
            .find(selector)
            .unwrap_or_else(|| panic!("selector {selector:?} not found in serialized CSS"));
        let open = css[start..]
            .find('{')
            .map(|i| start + i)
            .unwrap_or_else(|| panic!("selector {selector:?} has no opening `{{`"));
        let close = css[open..]
            .find('}')
            .map(|i| open + i)
            .unwrap_or_else(|| panic!("selector {selector:?}'s rule has no closing `}}`"));
        &css[open..=close]
    }
}
