//! Resolves `assets/stylesheet.css` — hop's real GTK stylesheet, authored in
//! the CSS subset GTK 4.14 actually implements — against
//! [`crate::tokens`]'s palette-aware token table, producing the concrete CSS
//! text a [`gtk::CssProvider`] can load.
//!
//! # Why a second file and a second `include_str!`, not a second parser
//!
//! `assets/tokens.css` (parsed by [`crate::tokens`]) and `assets/stylesheet.css`
//! (parsed by this module) answer two different questions. `tokens.css` is a
//! *palette*: it declares design values, never how a window or a row look.
//! `stylesheet.css` is the missing stylesheet — real component rules,
//! written as real GTK CSS, that *reference* those values by name instead of
//! repeating them. Bundling it with [`include_str!`] follows `tokens.rs`'s
//! own precedent (see that module's top doc comment, "Why parsing, not a GTK
//! `CssProvider` load") for the identical reason: a launcher's own
//! stylesheet should not depend on the working directory or an install
//! layout finding the source tree.
//!
//! This module's own job is narrow on purpose: find every `{{...}}`
//! placeholder in the template and substitute it, via [`crate::tokens::resolve`],
//! for a palette-resolved value. It does not re-parse `tokens.css`, re-walk
//! `var()` chains, or re-implement any of the fail-loudly behaviour Task 1's
//! [`crate::tokens`] module already has — every placeholder resolves through
//! that module's one real `resolve` function, so a missing token or a `var()`
//! cycle referenced from this file panics exactly the way it already does for
//! every other caller.
//!
//! See `assets/stylesheet.css`'s own top doc comment for the placeholder
//! syntax itself (`{{name}}` and `{{font:name}}`) and why it was chosen —
//! that account belongs next to the file it describes, not duplicated here.
//!
//! # The public surface
//!
//! [`resolve`] is the one function this module exports: give it a
//! [`crate::tokens::Palette`], get back the *full*, concrete stylesheet text
//! for that palette, with every placeholder substituted. Nothing here reads
//! from or writes to a `gtk::Display`, constructs a `gtk::CssProvider`, or
//! knows anything about libadwaita's colour-scheme signal — installing the
//! provider, re-resolving on a colour-scheme change, and guarding parse
//! errors are a later issue's job (issue #193's own plan, Task 3), which
//! needs exactly this one call — "the full stylesheet text, resolved for
//! palette P" — and nothing else from this module to do it.

use crate::tokens::{self, Palette};

/// The full contents of the repo's `assets/stylesheet.css`, bundled into the
/// binary at compile time — see this module's top doc comment for why that
/// matches `tokens.rs`'s own precedent.
const STYLESHEET_TEMPLATE: &str = include_str!("../../../assets/stylesheet.css");

/// Resolves hop's real stylesheet for `palette`: every `{{name}}` and
/// `{{font:name}}` placeholder in `assets/stylesheet.css` substituted for its
/// concrete, palette-resolved value. The result is ready to hand to a
/// `gtk::CssProvider::load_from_string` — see this module's top doc comment
/// for what installing that provider is (deliberately) not this module's job.
pub fn resolve(palette: Palette) -> String {
    resolve_template(STYLESHEET_TEMPLATE, palette)
}

/// [`resolve`] against an arbitrary `template` string rather than the real,
/// bundled file — the seam the unit tests below use to exercise the missing-
/// token and unterminated-placeholder failure paths without needing a
/// specially-broken copy of `assets/stylesheet.css` on disk.
///
/// Strips every `/* ... */` comment before scanning for `{{...}}`
/// placeholders, for the exact reason `tokens.rs::strip_comments`'s own doc
/// comment gives for doing the same thing to `tokens.css`: this file's own
/// top-of-file prose documents the placeholder syntax *by example* —
/// `` `{{name}}` `` appears literally inside a `/* ... */` block describing
/// what it means — and a scan that did not skip comments would try to
/// resolve those illustrative examples as if they were real placeholders
/// (confirmed directly while writing this function: an earlier version
/// panicked on `assets/stylesheet.css`'s own header, having found `{{...}}`
/// inside a sentence and gone looking for a token literally named `...`).
/// Unlike `tokens.rs::strip_comments`, whose stripped text is only ever
/// scanned and never itself returned to a caller, the comment-free text
/// *is* this function's output — `gtk::CssProvider` has no use for hop's own
/// authorial comments at runtime, and stripping them here means the
/// resolved sheet this module hands out is exactly what a leftover-
/// placeholder check (or a human comparing dark against light) needs to
/// look at, nothing extra to filter back out.
fn resolve_template(template: &str, palette: Palette) -> String {
    let code_only = strip_comments(template);
    let mut out = String::with_capacity(code_only.len());
    let mut rest: &str = &code_only;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{{".len()..];
        let end = after.find("}}").unwrap_or_else(|| {
            let snippet: String = after.chars().take(40).collect();
            panic!(
                "assets/stylesheet.css has a `{{{{` with no matching `}}}}` \
                 near: {snippet:?}"
            )
        });
        let inner = after[..end].trim();
        out.push_str(&resolve_placeholder(inner, palette));
        rest = &after[end + "}}".len()..];
    }
    out.push_str(rest);
    out
}

/// Removes every `/* ... */` comment from `css`, replacing each with nothing
/// — the same operation `tokens.rs::strip_comments` performs on
/// `tokens.css`, duplicated here rather than shared because the two operate
/// on different files with no real coupling between them (`tokens.rs`'s
/// version is a private implementation detail of that module, not a
/// reusable utility this crate exposes anywhere). Panics naming this file on
/// an unterminated comment, the same failure shape `tokens.rs`'s version
/// uses for the identical situation in `tokens.css`.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("*/").unwrap_or_else(|| {
            panic!("assets/stylesheet.css has an unterminated `/* ... */` comment")
        });
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Resolves one placeholder's inner text (`name`, or `font:name`) to its
/// concrete value. Panics naming the token — via [`tokens::resolve`], which
/// already does this — if `name` has no declaration in `assets/tokens.css`
/// under `palette`, or if it is a `var()` cycle.
fn resolve_placeholder(inner: &str, palette: Palette) -> String {
    match inner.strip_prefix("font:") {
        Some(name) => font_shorthand_no_line_height(&tokens::resolve(name.trim(), palette)),
        None => tokens::resolve(inner, palette),
    }
}

/// Reshapes a resolved `--hop-text-*` value — `<weight> <size>px/<line>px
/// <family-list>`, e.g. `500 13.5px/20px "Inter", -apple-system, sans-serif`
/// — into the 3-field `<weight> <size> <family-list>` form GTK's `font:`
/// shorthand actually parses.
///
/// This exists because of one concrete, empirically-confirmed fact: GTK
/// 4.14's CSS parser rejects the 4-field CSS-Fonts form `assets/tokens.css`'s
/// own `--hop-text-*` tokens are authored in. Loading
/// `.x { font: 500 13.5px/20px "Inter", sans-serif; }` through a real
/// `gtk::CssProvider` (checked directly, under `gtk4-broadwayd`, with the
/// provider's `parsing-error` signal connected, before this function was
/// written) produces a parse error — "Expected a string" — rather than the
/// weight/size/family GTK accepts happily once the `/<line-height>` segment
/// is gone. This function is the fix: not a second token parser (it takes
/// `raw`, an *already-resolved* string — everything token-lookup and
/// `var()`-chain-following about it happened in [`tokens::resolve`], once,
/// before this function ever runs), just the one bounded text transform GTK's
/// narrower shorthand needs.
///
/// Splits `raw` on ASCII whitespace into exactly three pieces
/// (`splitn(3, ...)`, matching every `--hop-text-*` token's own shape: one
/// weight, one `<size>/<line>` pair with no internal whitespace, then a
/// family list that legitimately contains its own internal whitespace after
/// each comma and must not be re-split) and drops the `/<line-height>` half
/// of the middle piece. Falls back to returning `raw` unchanged, piece by
/// piece, if a piece is missing — this function is only ever called on a
/// `{{font:...}}` placeholder's already-resolved value, which is always one
/// of `tokens.css`'s own `--hop-text-*` tokens, so a malformed input here
/// would be a bug in `tokens.css` itself, not something this function should
/// paper over by inventing a value; the caller (`resolve_placeholder`) does
/// no validation of its own; a shape this function cannot make sense of
/// simply best-effort passes each recognisable piece through, keeping any
/// resulting CSS parse error visible rather than silently dropping the whole
/// placeholder.
fn font_shorthand_no_line_height(raw: &str) -> String {
    let mut parts = raw.splitn(3, char::is_whitespace);
    let weight = parts.next().unwrap_or_default();
    let size_and_line = parts.next().unwrap_or_default();
    let family = parts.next().unwrap_or_default();
    let size = size_and_line.split('/').next().unwrap_or(size_and_line);
    format!("{weight} {size} {family}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hard requirement this whole issue exists to satisfy: the real,
    /// shipped stylesheet, once resolved, must contain no leftover `{{`/`}}`
    /// marker anywhere — every placeholder [`resolve`] finds in the real
    /// file must actually have been substituted, not merely attempted.
    #[test]
    fn resolved_real_stylesheet_has_no_leftover_placeholder() {
        let dark = resolve(Palette::Dark);
        assert!(
            !dark.contains("{{") && !dark.contains("}}"),
            "the dark-resolved stylesheet still contains a `{{{{`/`}}}}` marker"
        );

        let light = resolve(Palette::Light);
        assert!(
            !light.contains("{{") && !light.contains("}}"),
            "the light-resolved stylesheet still contains a `{{{{`/`}}}}` marker"
        );
    }

    /// A placeholder naming a token `assets/tokens.css` does not declare
    /// must fail exactly the way [`tokens::resolve`] already fails for every
    /// other caller — this is what makes "silently emit an unsubstituted
    /// placeholder" impossible rather than merely unlikely.
    #[test]
    #[should_panic(expected = "hop-this-token-does-not-exist")]
    fn placeholder_naming_a_missing_token_panics() {
        resolve_template(
            ".x { color: {{hop-this-token-does-not-exist}}; }",
            Palette::Dark,
        );
    }

    /// The `{{font:...}}` form must fail exactly the same way — it still
    /// routes through [`tokens::resolve`] before doing anything else.
    #[test]
    #[should_panic(expected = "hop-this-token-does-not-exist")]
    fn font_placeholder_naming_a_missing_token_panics() {
        resolve_template(
            ".x { font: {{font:hop-this-token-does-not-exist}}; }",
            Palette::Dark,
        );
    }

    /// An unterminated `{{` (no matching `}}`) is the second way a
    /// placeholder can go wrong, distinct from a missing token name — this
    /// pins that this module catches it too, rather than silently emitting
    /// the dangling `{{` text into the resolved sheet.
    #[test]
    #[should_panic(expected = "no matching")]
    fn unterminated_placeholder_panics() {
        resolve_template(".x { color: {{hop-bg; }", Palette::Dark);
    }

    /// The same real stylesheet, resolved under each palette, must actually
    /// differ — proving `palette` is threaded all the way through to every
    /// placeholder, not dropped somewhere between this module and
    /// `tokens::resolve`. `--hop-bg` alone (window ground) is enough to tell
    /// dark and light apart, per `tokens.rs`'s own
    /// `light_palette_resolves_a_semantic_token_to_the_light_ramp` test.
    #[test]
    fn resolved_real_stylesheet_differs_between_palettes() {
        let dark = resolve(Palette::Dark);
        let light = resolve(Palette::Light);
        assert_ne!(
            dark, light,
            "the light-resolved stylesheet must differ from the dark one \
             somewhere `.hop-theme-light` overrides"
        );

        // Pin exactly *where* they differ, not just that they do: the window
        // ground's background-color line, the one place both `resolve` calls
        // above are guaranteed to touch `--hop-bg`.
        assert!(
            dark.contains("background-color: #121214;"),
            "dark window ground must resolve --hop-bg to the dark ramp's value"
        );
        assert!(
            light.contains("background-color: #faf9f6;"),
            "light window ground must resolve --hop-bg to the light ramp's value"
        );
    }

    /// Issue #214's own regression test: `.hop-row-hint-key`'s `color:`
    /// must actually vary by palette, not silently pin to the dark accent
    /// under both. Before the fix, `{{hop-accent}}` named a raw ramp entry
    /// `.hop-theme-light` never overlays, so both `resolve` calls below
    /// produced the identical `.hop-row-hint-key` rule — this test fails
    /// against that bug (`dark_rule == light_rule`, and the light rule
    /// still carrying the dark hex) and passes once the rule is repointed
    /// at a semantic-layer alias with a light-palette entry.
    #[test]
    fn hint_key_glyph_colour_differs_between_palettes() {
        let dark = resolve(Palette::Dark);
        let light = resolve(Palette::Light);

        let dark_rule = extract_rule(&dark, ".hop-row-hint-key");
        let light_rule = extract_rule(&light, ".hop-row-hint-key");

        assert_ne!(
            dark_rule, light_rule,
            "the key glyph's colour must differ between palettes — got the \
             same rule under both: {dark_rule}"
        );
        assert!(
            dark_rule.contains("color: #e3a83b;"),
            "dark key glyph should resolve to the dark accent, got: {dark_rule}"
        );
        assert!(
            light_rule.contains("color: #875c0f;"),
            "light key glyph should resolve to the light accent, got: {light_rule}"
        );
    }

    /// Issue #214's audit turned up a second instance of the same defect
    /// shape: `.hop-mode-label`'s `color:` named `{{hop-neutral-400}}`, a
    /// raw ramp entry `.hop-theme-light` never overlays, so both `resolve`
    /// calls below produced the identical `.hop-mode-label` rule — the mode
    /// label rendered the dark ramp's grey on light paper regardless of
    /// theme. This test fails against that bug (`dark_rule == light_rule`)
    /// and passes once the rule is repointed at `--hop-fg-3`, the existing
    /// semantic alias for this exact ramp tier.
    #[test]
    fn mode_label_colour_differs_between_palettes() {
        let dark = resolve(Palette::Dark);
        let light = resolve(Palette::Light);

        let dark_rule = extract_rule(&dark, ".hop-mode-label");
        let light_rule = extract_rule(&light, ".hop-mode-label");

        assert_ne!(
            dark_rule, light_rule,
            "the mode label's colour must differ between palettes — got the \
             same rule under both: {dark_rule}"
        );
        assert!(
            dark_rule.contains("color: #8f8e95;"),
            "dark mode label should resolve to the dark ramp's grey, got: {dark_rule}"
        );
        assert!(
            light_rule.contains("color: #6a6559;"),
            "light mode label should resolve to the light ramp's grey, got: {light_rule}"
        );
    }

    /// Finds `selector`'s first `{ ... }` block in `sheet` (a resolved
    /// stylesheet, comments already stripped by `resolve`), inclusive of
    /// the braces — the slice `hint_key_glyph_colour_differs_between_palettes`
    /// compares, so a match elsewhere in the file can never satisfy it.
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

    /// [`font_shorthand_no_line_height`]'s own unit coverage, isolated from
    /// [`tokens::resolve`] entirely — proves the text transform itself,
    /// independent of which real token happened to produce its input.
    #[test]
    fn font_shorthand_strips_the_line_height_segment() {
        assert_eq!(
            font_shorthand_no_line_height(
                "500 13.5px/20px \"Inter\", -apple-system, \"Cantarell\", sans-serif"
            ),
            "500 13.5px \"Inter\", -apple-system, \"Cantarell\", sans-serif"
        );
    }

    /// A single `{{font:...}}` placeholder, resolved end to end against the
    /// real token table, must already be the exact 3-field form GTK's `font:`
    /// shorthand accepts — confirming the whole pipeline (`tokens::resolve`
    /// then the transform) for one concrete, real token rather than only the
    /// synthetic string the test above uses.
    #[test]
    fn font_placeholder_resolves_to_the_gtk_accepted_shorthand_form() {
        let resolved = resolve_template(".x { font: {{font:hop-text-error}}; }", Palette::Dark);
        assert!(
            resolved.contains("font: 500 13.5px \"Inter\""),
            "expected the 3-field shorthand with no `/<line-height>` segment, got: {resolved}"
        );
    }
}
