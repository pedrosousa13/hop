//! Proves the ctrl-K action panel's material — `assets/stylesheet.css`'s
//! `.hop-action-panel` family plus its `popover.background` chrome overrides
//! — implements issue #254's styling half for SPEC decisions 1, 2, and 5.
//! `apps/hop-gtk/src/ui/action_panel.rs` (a different, concurrently-written
//! task) owns the widget that sets the five fixed classes this file styles
//! against (`.hop-action-panel`, `.hop-action-panel-entry`, `.hop-action-row`,
//! `.hop-action-row-label`, `.hop-action-row-kind`); this file never imports
//! or exercises that module — it proves the *stylesheet*, resolved the same
//! way `tests/row_hover_lift.rs` proves the row-lift rule, independently of
//! whatever widget eventually wears these classes.
//!
//! Three claims, matching this issue's own "at minimum" test list:
//!
//! 1. [`panel_surface_and_row_states_resolve_differently_under_dark_and_light_palettes`]
//!    — the palette axis: `.hop-action-panel`'s `background-color` (routed
//!    through the existing `--hop-bg` semantic alias, not a bare ramp name)
//!    and `.hop-action-panel`'s `box-shadow` (routed through `--hop-elev` and
//!    `--hop-line`, the two tokens this issue's brief names to reuse rather
//!    than invent parallel ones) each resolve to a different literal under
//!    `Palette::Light` than under `Palette::Dark` — proving semantic-layer
//!    routing, not the bare-ramp-name trap `assets/stylesheet.css`'s own ROW
//!    SURFACE comment documents (a bare `--hop-neutral-950`/`--hop-elev-1`
//!    would silently resolve to the same dark literal under both palettes,
//!    because `.hop-theme-light` only overlays the semantic layer's own
//!    aliases, never a ramp name directly).
//! 2. [`panel_open_transition_resolves_to_the_full_duration_under_full_motion_and_the_shortened_value_under_reduced`]
//!    — the motion axis: `.hop-action-panel.hop-action-panel-shown`'s
//!    open-fade `transition:` (moved here from an inert declaration on
//!    `popover.background` — see that rule's own corrected comment in
//!    `assets/stylesheet.css` for the verified-false claim that motivated
//!    the move, and this file's own module doc history for why the
//!    original version of this very test passed throughout, proving only
//!    that the rule parsed, never that it animated anything) carries the
//!    real `--hop-duration-panel-open` (220ms, this issue's own ≤220ms
//!    ceiling for the panel-open transition class — distinct from SPEC
//!    decision 2's ≤140ms ceiling, which that decision's own text scopes to
//!    "hover/selection transitions") under `Motion::Full`, and the
//!    shortened `85ms` `@media (prefers-reduced-motion: reduce)` override
//!    under `Motion::Reduced` — proving the rule reads
//!    `{{motion:hop-duration-panel-open}}` (which reaches
//!    [`hop_gtk::tokens::resolve_motion`]) and not the palette-only
//!    `{{hop-duration-panel-open}}` spelling that would always resolve the
//!    unconditional `:root` value regardless of motion state, the identical
//!    trap `tests/row_hover_lift.rs`'s own lift-transform test guards
//!    against for a different token.
//! 3. [`panel_base_rule_is_transparent_and_the_shown_class_resolves_to_full_opacity`]
//!    — the mechanism itself, not just the timing figure: `.hop-action-panel`
//!    (the base, un-shown rule) and `.hop-action-panel.hop-action-panel-shown`
//!    must resolve to two genuinely different `opacity` values (`0` and `1`).
//!    This is the assertion claim 2 above cannot make on its own — a rule
//!    could carry a real, motion-correct `transition:` duration and still be
//!    inert if the property it names never actually changes value between
//!    the base and entered states (exactly what `popover.background`'s own
//!    removed declaration was: a correctly-timed transition on a property
//!    that never changed, because nothing in that node's own resolved style
//!    ever set `opacity` to two different values in the first place). Only
//!    together do claims 2 and 3 rule out an inert fade: a real value change
//!    (claim 3) timed by a real, motion-aware duration (claim 2).
//! 4. [`broadway_guard::resolved_action_panel_rules_carry_real_declarations_in_a_real_gtk_provider`]
//!    — the GTK-parses-it guard: hands the exact resolved
//!    `popover.background`, `.hop-action-panel`, `.hop-action-row`,
//!    `.hop-action-row:hover`, `.hop-action-row:selected`, and
//!    `.hop-action-panel-entry` rules to a real `gtk::CssProvider` (the same
//!    re-exec-under-broadway shape `tests/row_hover_lift.rs` and
//!    `tests/stylesheet_provider.rs` already use, for the identical reason
//!    given there — GTK's parser drops what it cannot parse *silently*) and
//!    reads back its own serialized `to_str()`, not the placeholder-resolved
//!    source text, to confirm GTK's parser kept the multi-layer `box-shadow`
//!    (`--hop-elev`'s own two shadows plus this rule's own inset hairline
//!    ring, three comma-separated shadow layers in one declaration — never
//!    combined this way anywhere else in this file before this issue),
//!    the hover lift's `transform`/`box-shadow`, the selected fill, the
//!    entry's `caret-color`, and the popover's own `transition-*` longhands
//!    as real, non-empty declarations rather than silently discarding a
//!    property or a value shape GTK's CSS subset does not actually support.
//!    `tests/stylesheet_provider.rs`'s whole-file zero-parse-error check
//!    already covers this rule incidentally (it resolves and parses the
//!    *entire* file under all four palette/motion combinations, this rule
//!    included) — this file's third test is narrower and more direct on
//!    purpose, the same relationship `tests/row_hover_lift.rs`'s own module
//!    doc draws to that file for its own fourth test.

use hop_gtk::stylesheet;
use hop_gtk::tokens::{Motion, Palette};

/// Finds `selector`'s first `{ ... }` block in `sheet` (a resolved
/// stylesheet, comments already stripped by [`stylesheet::resolve`]),
/// inclusive of the braces. Duplicated from `tests/row_hover_lift.rs`'s own
/// copy of this helper (itself duplicated from `hop_gtk::stylesheet`'s
/// private `tests::extract_rule`) rather than shared — this crate's
/// `tests/` binaries have no access to a `src/` module's private test
/// helpers or to each other's modules, the same constraint every other
/// `tests/*.rs` file's own duplicated helper documents for its own
/// copy-not-share choice.
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
fn panel_surface_and_row_states_resolve_differently_under_dark_and_light_palettes() {
    let dark = stylesheet::resolve(Palette::Dark, Motion::Full);
    let light = stylesheet::resolve(Palette::Light, Motion::Full);

    let dark_panel = extract_rule(&dark, ".hop-action-panel {");
    let light_panel = extract_rule(&light, ".hop-action-panel {");

    let normalize = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let dark_panel_n = normalize(dark_panel);
    let light_panel_n = normalize(light_panel);

    assert!(
        dark_panel_n.contains("background-color: #121214;"),
        "the dark palette's panel rule should carry --hop-bg's own dark literal, got: {dark_panel}"
    );
    assert!(
        light_panel_n.contains("background-color: #faf9f6;"),
        "the light palette's panel rule should carry --hop-bg's own light literal via \
         .hop-theme-light's override, got: {light_panel}"
    );
    assert_ne!(
        dark_panel_n, light_panel_n,
        "a bare ramp literal (or a bare --hop-elev-1 in the box-shadow) would resolve \
         identically under both palettes — the panel rule must route background-color and \
         box-shadow through the semantic aliases --hop-bg/--hop-elev/--hop-line instead, which \
         these two rules must therefore differ to prove; got the same rule under both: \
         {dark_panel}"
    );

    // The two action-row states this issue's brief names for reuse
    // (--hop-bg-hover, --hop-sel-fill) must also differ from the panel's own
    // resting surface and from each other, under a single palette — proving
    // hover and selection remain visually distinct affordances inside the
    // panel, the same three-way distinction `listview > row`/`:hover` and
    // `.hop-selection-indicator` already keep for the main list.
    let hover = extract_rule(&dark, ".hop-action-row:hover {");
    let selected = extract_rule(&dark, ".hop-action-row:selected {");
    assert!(
        hover.contains("background-color: #202024;"),
        "the action row's hover state should carry --hop-bg-hover's own dark literal — the \
         exact token `listview > row:hover` already uses — got: {hover}"
    );
    assert!(
        selected.contains("background-color: rgba(90, 169, 230, 0.14);"),
        "the action row's selected state should carry --hop-sel-fill's own dark literal — the \
         exact token `.hop-selection-indicator` already uses — got: {selected}"
    );
    assert_ne!(
        hover, selected,
        "hover and selection must read as distinct affordances, got the same rule for both: \
         {hover}"
    );
}

#[test]
fn panel_open_transition_resolves_to_the_full_duration_under_full_motion_and_the_shortened_value_under_reduced()
 {
    let full = stylesheet::resolve(Palette::Dark, Motion::Full);
    let reduced = stylesheet::resolve(Palette::Dark, Motion::Reduced);

    let full_shown = extract_rule(&full, ".hop-action-panel.hop-action-panel-shown {");
    let reduced_shown = extract_rule(&reduced, ".hop-action-panel.hop-action-panel-shown {");

    assert!(
        full_shown.contains("transition: opacity 220ms"),
        "under full motion the panel's open transition should carry \
         --hop-duration-panel-open's real 220ms value — this issue's own ≤220ms ceiling for \
         the panel-open transition class, distinct from SPEC decision 2's ≤140ms ceiling for \
         hover/selection transitions — got: {full_shown}"
    );
    assert!(
        reduced_shown.contains("transition: opacity 85ms"),
        "under reduced motion --hop-duration-panel-open's own @media override must shorten the \
         open fade to 85ms — kept, not zeroed, matching --hop-duration-open/-close's own \
         precedent that an opacity-only fade carries no vestibular trigger and so is shortened \
         rather than eliminated — got: {reduced_shown}"
    );
    assert_ne!(
        full_shown, reduced_shown,
        "the panel's shown-state rule must differ between motion states — got the same rule \
         under both: {full_shown}"
    );
}

/// See this file's own module doc, claim 3, for why this test exists
/// alongside the motion-duration test above rather than instead of it: a
/// correctly-timed `transition:` on a property that never actually changes
/// value is exactly as inert as the wrong duration would be. This is the
/// test that would have failed throughout the original, broken version of
/// this fix — the one where `.hop-action-panel` carried no `opacity` at all
/// and `popover.background` carried the (inert) transition instead: there
/// would have been no `.hop-action-panel.hop-action-panel-shown` rule for
/// `extract_rule` to find, and this test would panic on the missing
/// selector rather than quietly passing.
#[test]
fn panel_base_rule_is_transparent_and_the_shown_class_resolves_to_full_opacity() {
    let sheet = stylesheet::resolve(Palette::Dark, Motion::Full);

    let base = extract_rule(&sheet, ".hop-action-panel {");
    let shown = extract_rule(&sheet, ".hop-action-panel.hop-action-panel-shown {");

    assert!(
        base.contains("opacity: 0;"),
        "the base, un-shown .hop-action-panel rule must resolve opacity to 0 — the fade's own \
         starting value — got: {base}"
    );
    assert!(
        shown.contains("opacity: 1;"),
        "the entered .hop-action-panel.hop-action-panel-shown rule must resolve opacity to 1 — \
         the fade's own ending value — got: {shown}"
    );
    assert_ne!(
        base, shown,
        "the base and shown rules must differ in more than just their selector — got the same \
         declarations under both: {base}"
    );
}

/// The real assertions for
/// [`broadway_guard::resolved_action_panel_rules_carry_real_declarations_in_a_real_gtk_provider`],
/// run inside the re-exec'd child process — same shape as
/// `tests/row_hover_lift.rs`'s own `broadway_guard` module, duplicated
/// rather than shared for the reason every other `tests/*.rs` file in this
/// crate already gives for its own copy of this pattern: each file under
/// `tests/` compiles as its own separate crate.
mod broadway_guard {
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use hop_gtk::stylesheet;
    use hop_gtk::tokens::{Motion, Palette};

    const CHILD_MARKER: &str = "HOP_GTK_ACTION_PANEL_MATERIAL_TEST_CHILD";

    /// Base display number `650` — distinct from every other `tests/*.rs`
    /// file's own base (100/200/300/350/450/550, per
    /// `tests/row_hover_lift.rs`'s own comment enumerating them) so a
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
    fn resolved_action_panel_rules_carry_real_declarations_in_a_real_gtk_provider() {
        if std::env::var_os(CHILD_MARKER).is_some() {
            run_assertions();
            return;
        }

        let display = 650 + (std::process::id() % 5000);
        let broadway = BroadwayServer::start(display);

        let current_exe = std::env::current_exe()
            .expect("failed to resolve this test binary's own path to re-exec it");
        let output = Command::new(current_exe)
            .env("GDK_BACKEND", "broadway")
            .env("BROADWAY_DISPLAY", format!(":{display}"))
            .env(CHILD_MARKER, "1")
            .arg("--exact")
            .arg(
                "broadway_guard::resolved_action_panel_rules_carry_real_declarations_in_a_real_gtk_provider",
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
        // property, or an unusual value shape (three comma-separated
        // box-shadow layers combining two non-inset shadows from
        // --hop-elev with one inset hairline ring, never done elsewhere in
        // this file before this issue), was not silently
        // understood-and-discarded, the exact failure mode this test
        // exists to rule out (see this file's own module doc).
        let serialized = provider.to_str();

        let panel_rule = extract_serialized_rule(&serialized, ".hop-action-panel {");
        assert!(
            panel_rule.contains("box-shadow:") && !panel_rule.contains("box-shadow: none;"),
            "GTK's own serialized provider dump should carry a real, non-`none` box-shadow \
             combining --hop-elev's two shadows with the inset hairline ring on the panel \
             surface, got: {panel_rule}"
        );

        // Issue #254 review, finding 4: this used to read
        // `":hover.hop-action-row {"` — GTK's serializer reordering a
        // single compound selector's pseudo-class ahead of its style
        // class, the same reordering `tests/motion_setting.rs`'s own
        // `HINT_SHOWN_RULE_MARKER` comment documents for a *different*
        // compound (two style classes, not a style class plus a
        // pseudo-class). Re-verified directly against this crate's real,
        // installed GTK 4.14 while adding the overflow chevron's own
        // `.hop-row-action-icon:hover, .hop-row-overflow-icon:hover { ... }`
        // rule earlier in this same stylesheet: GTK's canonical print order
        // for a style-class-plus-pseudo-class compound is evidently not a
        // fixed rule this crate can rely on independent of what else the
        // stylesheet declares — adding an unrelated rule earlier in the
        // file changed this one compound's serialized order from
        // `:hover.hop-action-row` back to source order,
        // `.hop-action-row:hover`, with no change to `.hop-action-row`'s
        // own declarations at all. This assertion is therefore pinned to
        // whatever GTK's real parser produces *today*, exactly like every
        // other `extract_serialized_rule` call in this file — a future
        // stylesheet edit is free to shift this order again, and should
        // fix this literal to match rather than treat a mismatch here as a
        // sign anything is actually broken.
        let hover_rule = extract_serialized_rule(&serialized, ".hop-action-row:hover {");
        assert!(
            hover_rule.contains("transform:") && !hover_rule.contains("transform: none;"),
            "GTK's own serialized provider dump should carry a real, non-`none` transform on \
             the action row's hover rule, got: {hover_rule}"
        );
        assert!(
            hover_rule.contains("box-shadow:") && !hover_rule.contains("box-shadow: none;"),
            "GTK's own serialized provider dump should carry a real, non-`none` box-shadow on \
             the action row's hover rule, got: {hover_rule}"
        );

        let selected_rule = extract_serialized_rule(&serialized, ":selected.hop-action-row {");
        assert!(
            selected_rule.contains("background-color:"),
            "GTK's own serialized provider dump should carry a real background-color on the \
             action row's selected rule, got: {selected_rule}"
        );

        let entry_rule = extract_serialized_rule(&serialized, ".hop-action-panel-entry {");
        assert!(
            entry_rule.contains("caret-color:"),
            "GTK's own serialized provider dump should carry a real caret-color on the \
             filter entry, got: {entry_rule}"
        );

        // The open-fade transition now lives on `.hop-action-panel`'s own
        // `-shown` state, not on `popover.background` — see
        // `assets/stylesheet.css`'s `popover.background` rule for the
        // verified-false claim (GTK 4.14 does not read a popover node's own
        // `transition-*` to time its present/dismiss) that moved it here,
        // and this file's own module doc, claim 3, for why proving the
        // *value actually changes* (the string test above) is not enough on
        // its own without also proving GTK's real parser, not just this
        // crate's own placeholder resolver, kept the longhands.
        // GTK's serializer reorders this compound selector's two classes to
        // `.hop-action-panel-shown.hop-action-panel`, source order reversed
        // — confirmed directly against this crate's real, installed GTK
        // 4.14 while writing this test, the same class-reordering behaviour
        // `hover_rule`/`selected_rule` above already document for a
        // pseudo-class-plus-style-class compound, recurring here for a
        // plain two-style-class compound instead.
        let shown_rule =
            extract_serialized_rule(&serialized, ".hop-action-panel-shown.hop-action-panel {");
        assert!(
            shown_rule.contains("transition-property:")
                && shown_rule.contains("transition-duration:"),
            "GTK's own serialized provider dump should carry real transition-* longhands on \
             the panel's own shown-state open-fade rule (the shorthand `transition:` this file \
             authors is always re-serialized to its longhands — the same shape \
             tests/row_hover_lift.rs's own resting-row assertion already documents), got: \
             {shown_rule}"
        );

        let popover_rule = extract_serialized_rule(&serialized, "popover.background {");
        assert!(
            popover_rule.contains("background-color: rgba(0,0,0,0);")
                && !popover_rule.contains("transition-property:"),
            "the popover node itself should still carry the transparent background-color that \
             strips its own stock chrome, and — now that the (inert) transition declaration has \
             been removed from it — no transition-property longhand at all; a reappearing \
             transition-property here would mean the removed, false-comment declaration this \
             issue's fix deleted has silently come back, got: {popover_rule}"
        );

        println!(
            "resolved action-panel rules parse with zero errors in a real gtk::CssProvider, \
             and the panel's combined box-shadow, the hover lift's transform/box-shadow, the \
             selected fill, the entry's caret-color, and the shown-state's transition-* longhands \
             all survive into its own serialized dump"
        );
    }

    /// Finds the `{ ... }` block whose *selector* is exactly `selector` in
    /// `css` (a serialized `gtk::CssProvider::to_str()` dump), inclusive of
    /// the braces. Duplicated from `tests/row_hover_lift.rs`'s own copy —
    /// see that file's own comment on `extract_serialized_rule` for why
    /// matching `selector` verbatim (rather than re-parsing GTK's own
    /// reordering of a single compound selector's classes) is sufficient
    /// here: none of this file's own selectors can be confused with one
    /// another as substrings in either direction.
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
