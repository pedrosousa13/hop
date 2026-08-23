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
//! placeholder in the template and substitute it, via [`crate::tokens::resolve`]
//! or — issue #207 — [`crate::tokens::resolve_motion`], for a palette- or
//! motion-resolved value. It does not re-parse `tokens.css`, re-walk `var()`
//! chains, or re-implement any of the fail-loudly behaviour Task 1's
//! [`crate::tokens`] module already has — every placeholder resolves through
//! that module's own `resolve`/`resolve_motion` functions, so a missing
//! token or a `var()` cycle referenced from this file panics exactly the way
//! it already does for every other caller.
//!
//! See `assets/stylesheet.css`'s own top doc comment for the placeholder
//! syntax itself (`{{name}}`, `{{font:name}}`, and `{{motion:name}}`) and why
//! it was chosen — that account belongs next to the file it describes, not
//! duplicated here.
//!
//! # The public surface
//!
//! [`resolve`] is the one function this module exports: give it a
//! [`crate::tokens::Palette`] and a [`crate::tokens::Motion`], get back the
//! *full*, concrete stylesheet text for that palette and motion state, with
//! every placeholder substituted. Nothing here reads from or writes to a
//! `gtk::Display`, constructs a `gtk::CssProvider`, or knows anything about
//! libadwaita's colour-scheme signal or GTK's `gtk-enable-animations` setting
//! — installing the provider, re-resolving on a live change to either axis,
//! and guarding parse errors are `style.rs`'s job (issue #193's own plan,
//! Task 3, and issue #207's own Task 2), which needs exactly this one call —
//! "the full stylesheet text, resolved for palette P and motion M" — and
//! nothing else from this module to do it.

use crate::tokens::{self, Motion, Palette};

/// The full contents of the repo's `assets/stylesheet.css`, bundled into the
/// binary at compile time — see this module's top doc comment for why that
/// matches `tokens.rs`'s own precedent.
const STYLESHEET_TEMPLATE: &str = include_str!("../../../assets/stylesheet.css");

/// Resolves hop's real stylesheet for `palette` and `motion`: every
/// `{{name}}`, `{{font:name}}`, and — issue #207 — `{{motion:name}}`
/// placeholder in `assets/stylesheet.css` substituted for its concrete,
/// palette- or motion-resolved value. The result is ready to hand to a
/// `gtk::CssProvider::load_from_string` — see this module's top doc comment
/// for what installing that provider is (deliberately) not this module's job.
///
/// Two independent axes, not fused into one: `palette` and `motion` are
/// [`crate::tokens::Palette`] and [`crate::tokens::Motion`], the same pair
/// [`tokens::resolve`]/[`tokens::resolve_motion`] keep separate — see
/// [`Motion`]'s own doc comment for why. A `{{name}}` placeholder only ever
/// consults `palette` (through [`tokens::resolve`]), and a `{{motion:name}}`
/// placeholder only ever consults `motion` (through
/// [`tokens::resolve_motion`]); nothing in this file's placeholder syntax can
/// ask for both at once, because no single token in `assets/tokens.css` is
/// declared on both axes.
pub fn resolve(palette: Palette, motion: Motion) -> String {
    resolve_template(STYLESHEET_TEMPLATE, palette, motion)
}

/// The literal markers bracketing the honesty-critical block in
/// `assets/stylesheet.css` — see that file's own "HONESTY-CRITICAL
/// SELECTORS" section and its `HOP-HONESTY-LOCKED-BLOCK-START`/`-END`
/// comment pair. Two `const`s, not one shared prefix a caller derives both
/// from, because the two are searched for independently by
/// [`locked_block_slice`] and giving each its own name makes a failure
/// message ("missing `{START}`" vs "missing `{END}`") name the actual
/// marker that went missing rather than a computed half of a shared string.
///
/// The two are not symmetric strings, and that asymmetry is deliberate.
/// `assets/stylesheet.css`'s real `-START` sentinel is a long comment,
/// explaining itself to a reader who lands on it directly in that file —
/// this constant only needs to name it *uniquely*, not spell it in full, so
/// it stops right after the text that makes it unique and leaves the rest
/// (including that comment's own closing `*/`) for [`locked_block_slice`]
/// to find separately. The `-END` sentinel carries no such prose — it is
/// one short, self-closed comment — so this constant is simply that whole
/// comment, `*/` included, and needs no second search to find where it ends.
const LOCKED_BLOCK_START: &str = "/* HOP-HONESTY-LOCKED-BLOCK-START";
const LOCKED_BLOCK_END: &str = "/* HOP-HONESTY-LOCKED-BLOCK-END */";

/// Resolves *only* the honesty-critical block of hop's real stylesheet for
/// `palette` and `motion` — `style.rs`'s second [`gtk::CssProvider`], the
/// one it installs above `gtk::STYLE_PROVIDER_PRIORITY_USER`, loads exactly
/// this text and nothing else.
///
/// [`gtk::CssProvider`]: https://docs.gtk.org/gtk4/class.CssProvider.html
///
/// # One source of text, not two
///
/// The obvious-looking alternative — author the locked declarations a
/// second time, in a second file or a second Rust string literal, and load
/// that into the second provider — was rejected before it was written:
/// `assets/stylesheet.css`'s own header already forbids exactly this shape
/// ("no design value appears as a literal in both the stylesheet and
/// `tokens.css`"), and a hand-duplicated locked block is the identical
/// hazard one file over — two texts that must agree forever, with nothing
/// but a human's discipline keeping them in lockstep, the same drift risk
/// that rule exists to close for every *other* value in this file. Worse
/// here than most: the two literal dimensions on `.hop-honesty .hop-skeleton`
/// (`24px`/`9px`) are already, deliberately, un-tokenized (per the contract's
/// own "fixed declarations, not overridable custom properties" requirement
/// — that rule's own comment in `assets/stylesheet.css` explains why), so
/// there is no shared `--name` either copy could point at instead of
/// repeating the literal outright. A second copy would have meant a third
/// place carrying `24px`/`9px` by hand, on top of the two
/// `assets/stylesheet.css`'s own comment already tracks against
/// `assets/tokens.css`.
///
/// This function instead treats `assets/stylesheet.css` as the *only*
/// place the locked block is ever written down: [`locked_block_slice`]
/// finds the exact substring of the raw, unresolved template between the
/// `HOP-HONESTY-LOCKED-BLOCK-START`/`-END` sentinel comments — the same
/// four rules a reader sees inline in the "HONESTY-CRITICAL SELECTORS"
/// section — and [`resolve_template`] resolves *that slice*, through the
/// identical placeholder pipeline [`resolve`] uses for the whole file. If
/// the ordinary sheet's honesty-critical rules ever change, this function's
/// output changes with them automatically, because there is only the one
/// piece of source text for both to read.
pub fn resolve_locked_block(palette: Palette, motion: Motion) -> String {
    resolve_template(locked_block_slice(STYLESHEET_TEMPLATE), palette, motion)
}

/// Finds the exact substring of `template` between the
/// `HOP-HONESTY-LOCKED-BLOCK-START`/`-END` sentinel comments — the slice
/// [`resolve_locked_block`] resolves — panicking naming whichever sentinel
/// is missing, the same fail-loudly posture [`resolve_template`] already
/// takes for a dangling `{{` (this is a build-time-checkable defect in
/// `assets/stylesheet.css` itself, not a runtime condition to degrade
/// around).
///
/// The returned slice starts *after* the `-START` marker's own closing
/// `*/` is reached inside it (the marker text itself, `LOCKED_BLOCK_START`,
/// stops short of `*/` on purpose — see that constant's own value — so this
/// function finds the marker's own comment-closing `*/` itself, not
/// `assets/stylesheet.css`'s next unrelated one) and ends *before* the
/// `-END` marker begins, so neither sentinel comment is itself part of what
/// gets resolved — only the four rules between them are. A caller comparing
/// this slice's resolved output against [`resolve`]'s full-file output
/// (see this module's `#[cfg(test)]` below) would otherwise see the two
/// sentinel comments as a spurious difference having nothing to do with the
/// declarations either actually cares about.
fn locked_block_slice(template: &str) -> &str {
    let after_start_marker = template.find(LOCKED_BLOCK_START).unwrap_or_else(|| {
        panic!("assets/stylesheet.css is missing its {LOCKED_BLOCK_START:?} sentinel comment")
    }) + LOCKED_BLOCK_START.len();
    let start = after_start_marker
        + template[after_start_marker..]
            .find("*/")
            .unwrap_or_else(|| {
                panic!(
                    "assets/stylesheet.css's {LOCKED_BLOCK_START:?} sentinel comment is never \
                     closed with `*/`"
                )
            })
        + "*/".len();
    let end = template.find(LOCKED_BLOCK_END).unwrap_or_else(|| {
        panic!("assets/stylesheet.css is missing its {LOCKED_BLOCK_END:?} sentinel comment")
    });
    assert!(
        start <= end,
        "assets/stylesheet.css's {LOCKED_BLOCK_START:?} sentinel must appear before its \
         {LOCKED_BLOCK_END:?} pair, found the reverse"
    );
    &template[start..end]
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
fn resolve_template(template: &str, palette: Palette, motion: Motion) -> String {
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
        out.push_str(&resolve_placeholder(inner, palette, motion));
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

/// Resolves one placeholder's inner text (`name`, `font:name`,
/// `font-weight:name`, `font-size:name`, `font-family:name`, or —
/// issue #207 — `motion:name`) to its concrete value. Panics naming the
/// token — via [`tokens::resolve`]/[`tokens::resolve_motion`], which already
/// does this — if `name` has no declaration in `assets/tokens.css` under
/// `palette`/`motion`, or if it is a `var()` cycle.
///
/// # Why `font-weight:`/`font-size:`/`font-family:`, alongside `font:` —
/// issue #200's code-review fix
///
/// `font:` alone was the whole placeholder vocabulary until a review of
/// issue #200 found it over-locking `assets/stylesheet.css`'s honesty-
/// critical block: [`resolve_locked_block`] extracts that block's rules
/// verbatim and loads them into the second, above-user-priority provider,
/// and a `font:` shorthand's very definition sets `font-family` alongside
/// weight and size — GTK's own CSS spec, matching ordinary CSS-Fonts
/// shorthand semantics here, gives `font:` no way to name weight and size
/// without also naming (or resetting) family. Loading that shorthand into
/// the locked provider therefore locked family too, which
/// `docs/theme-token-contract.md:18-20` explicitly forbids: "the boundary
/// is narrow. On honesty-critical elements, a user theme may still restyle
/// the font family and accent, provided the element remains present and
/// legible." Splitting the shorthand into three longhands — each resolved
/// independently, through this same placeholder pipeline — is what lets
/// `assets/stylesheet.css` put `font-weight:`/`font-size:` inside the
/// locked block (contrast) and `font-family:` in the ordinary sheet only
/// (never contested by the locked provider), rather than the whole
/// three-in-one shorthand living only in the one place either the lock or
/// the override would have to lose. See `assets/stylesheet.css`'s own
/// comment on the `.hop-honesty-text`/`.hop-honesty-stamp` rules, right
/// where the split actually lives, for the full account — including why
/// the contract's own "as implemented today" paragraph (lines 69-71) still
/// describes the pre-split, single-`font:` form and was deliberately left
/// unedited (editing `docs/theme-token-contract.md`'s normative text is
/// out of scope for this fix).
///
/// `motion:` is checked after the four `font`-prefixed arms and before the
/// bare, no-prefix arm (the same relative position `font:` already held)
/// so a name that happens to start with none of them falls through to the
/// plain `{{name}}` form exactly as before either issue — adding more
/// prefixes never changes what an un-prefixed placeholder means.
fn resolve_placeholder(inner: &str, palette: Palette, motion: Motion) -> String {
    if let Some(name) = inner.strip_prefix("font:") {
        return font_shorthand_no_line_height(&tokens::resolve(name.trim(), palette));
    }
    if let Some(name) = inner.strip_prefix("font-weight:") {
        return font_weight_only(&tokens::resolve(name.trim(), palette));
    }
    if let Some(name) = inner.strip_prefix("font-size:") {
        return font_size_only(&tokens::resolve(name.trim(), palette));
    }
    if let Some(name) = inner.strip_prefix("font-family:") {
        return font_family_only(&tokens::resolve(name.trim(), palette));
    }
    if let Some(name) = inner.strip_prefix("motion:") {
        return tokens::resolve_motion(name.trim(), motion);
    }
    tokens::resolve(inner, palette)
}

/// Reshapes a resolved `--hop-text-*` value — `<weight> <size>px/<line>px
/// <family-list>`, e.g. `500 13.5px/20px "Geist", -apple-system, sans-serif`
/// — into the 3-field `<weight> <size> <family-list>` form GTK's `font:`
/// shorthand actually parses.
///
/// This exists because of one concrete, empirically-confirmed fact: GTK
/// 4.14's CSS parser rejects the 4-field CSS-Fonts form `assets/tokens.css`'s
/// own `--hop-text-*` tokens are authored in. Loading
/// `.x { font: 500 13.5px/20px "Geist", sans-serif; }` through a real
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
/// [`split_font_token`] does the actual splitting — on ASCII whitespace,
/// into exactly three pieces (`splitn(3, ...)`, matching every
/// `--hop-text-*` token's own shape: one weight, one `<size>/<line>` pair
/// with no internal whitespace, then a family list that legitimately
/// contains its own internal whitespace after each comma and must not be
/// re-split), already dropping the `/<line-height>` half of the middle
/// piece — this function just rejoins the three fields `font:` actually
/// wants. Falls back to an empty piece, not a panic, if one is missing —
/// this function is only ever called on a `{{font:...}}` placeholder's
/// already-resolved value, which is always one of `tokens.css`'s own
/// `--hop-text-*` tokens, so a malformed input here would be a bug in
/// `tokens.css` itself, not something this function should paper over by
/// inventing a value; the caller (`resolve_placeholder`) does no
/// validation of its own; a shape this function cannot make sense of
/// simply best-effort passes each recognisable piece through, keeping any
/// resulting CSS parse error visible rather than silently dropping the
/// whole placeholder.
fn font_shorthand_no_line_height(raw: &str) -> String {
    let (weight, size, family) = split_font_token(raw);
    format!("{weight} {size} {family}")
}

/// Splits an already-resolved `--hop-text-*` value into its three
/// meaningful fields — weight, `<size>` (the `/<line-height>` segment
/// already dropped), and family-list — the one parse
/// [`font_shorthand_no_line_height`], [`font_weight_only`],
/// [`font_size_only`], and [`font_family_only`] all build on. Extracted
/// once issue #200's code review added the latter three functions, so the
/// four `{{font...}}` placeholder forms share one splitting rule and can
/// never disagree about where one field ends and the next begins — the
/// same "one source of truth, not four near-duplicates" reasoning this
/// module's own doc comment already applies to `resolve_locked_block`
/// versus a hand-duplicated second copy of the locked block's text.
///
/// Same whitespace-splitting shape [`font_shorthand_no_line_height`]
/// always used, and the same deliberately permissive fallback: a piece
/// this function cannot find is `""`, not a panic — see
/// [`font_shorthand_no_line_height`]'s own doc comment for why a malformed
/// input here would be a bug in `tokens.css` itself, not a condition this
/// text transform should paper over.
fn split_font_token(raw: &str) -> (&str, &str, &str) {
    let mut parts = raw.splitn(3, char::is_whitespace);
    let weight = parts.next().unwrap_or_default();
    let size_and_line = parts.next().unwrap_or_default();
    let family = parts.next().unwrap_or_default();
    let size = size_and_line.split('/').next().unwrap_or(size_and_line);
    (weight, size, family)
}

/// Resolves a `{{font-weight:name}}` placeholder — issue #200's code-review
/// fix, [`resolve_placeholder`]'s own doc comment for the full "why a
/// longhand, not the `font:` shorthand" account. Just the first of
/// [`split_font_token`]'s three fields, as a `String` a caller can own the
/// same way every other placeholder resolver in this file returns one.
fn font_weight_only(raw: &str) -> String {
    split_font_token(raw).0.to_string()
}

/// Resolves a `{{font-size:name}}` placeholder — the second of
/// [`split_font_token`]'s three fields (the `/<line-height>` segment
/// already stripped by that shared helper), for the identical reason
/// [`font_weight_only`] exists.
fn font_size_only(raw: &str) -> String {
    split_font_token(raw).1.to_string()
}

/// Resolves a `{{font-family:name}}` placeholder — the third of
/// [`split_font_token`]'s three fields. `assets/stylesheet.css`'s ordinary,
/// application-priority `.hop-honesty-text`/`.hop-honesty-stamp` rules are
/// this function's one call site: family is deliberately never resolved
/// inside the `HOP-HONESTY-LOCKED-BLOCK-START`/`-END` sentinels, which is
/// the entire point of splitting the shorthand in the first place — see
/// [`resolve_placeholder`]'s own doc comment.
fn font_family_only(raw: &str) -> String {
    split_font_token(raw).2.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hard requirement this whole issue exists to satisfy: the real,
    /// shipped stylesheet, once resolved, must contain no leftover `{{`/`}}`
    /// marker anywhere — every placeholder [`resolve`] finds in the real
    /// file must actually have been substituted, not merely attempted.
    ///
    /// Issue #207 extends this from the two palettes alone to the full
    /// 2×2 palette-by-motion matrix, since `{{motion:name}}` is a second,
    /// independent placeholder prefix this same scan has to resolve
    /// cleanly under both [`Motion`] states — extending the existing guard
    /// rather than adding a separate, narrower one, per this issue's own
    /// brief.
    #[test]
    fn resolved_real_stylesheet_has_no_leftover_placeholder() {
        for palette in [Palette::Dark, Palette::Light] {
            for motion in [Motion::Full, Motion::Reduced] {
                let resolved = resolve(palette, motion);
                assert!(
                    !resolved.contains("{{") && !resolved.contains("}}"),
                    "the {palette:?}/{motion:?}-resolved stylesheet still contains a \
                     `{{{{`/`}}}}` marker"
                );
            }
        }
    }

    /// Issue #200's own version of the guard above, narrowed to
    /// `resolve_locked_block`'s output rather than the whole file — the
    /// locked block goes through the identical `{{name}}`/`{{font:name}}`
    /// placeholder pipeline [`resolve`] uses, so it needs the identical
    /// proof that nothing substitutes cleanly on paper but leaves a marker
    /// behind.
    #[test]
    fn resolved_locked_block_has_no_leftover_placeholder() {
        for palette in [Palette::Dark, Palette::Light] {
            for motion in [Motion::Full, Motion::Reduced] {
                let resolved = resolve_locked_block(palette, motion);
                assert!(
                    !resolved.contains("{{") && !resolved.contains("}}"),
                    "the {palette:?}/{motion:?}-resolved locked block still contains a \
                     `{{{{`/`}}}}` marker"
                );
            }
        }
    }

    /// [`resolve_locked_block`]'s whole reason to exist: its output must
    /// carry exactly the four honesty-critical rules — `.hop-honesty`
    /// itself, `.hop-honesty-text`, `.hop-honesty-stamp`, and
    /// `.hop-honesty .hop-skeleton` — and *nothing* from the rest of the
    /// file. The first half alone would pass for a function that
    /// (incorrectly) returned the entire resolved sheet; the second half is
    /// what actually proves this is the narrow, above-user-priority slice
    /// `style.rs`'s second provider is allowed to carry, not the whole
    /// sheet by another name — a hostile theme could otherwise contest
    /// anything the *ordinary* provider styles, not just the locked
    /// categories, exactly the failure this issue's brief calls out
    /// ("raising the ordinary sheet's priority would silently revoke" the
    /// contract's "everywhere outside the honesty-critical class"
    /// guarantee).
    #[test]
    fn resolved_locked_block_carries_exactly_the_four_honesty_rules_and_nothing_else() {
        let locked = resolve_locked_block(Palette::Dark, Motion::Full);

        for must_contain in [
            ".hop-honesty {",
            ".hop-honesty .hop-honesty-text {",
            ".hop-honesty .hop-honesty-stamp {",
            ".hop-honesty .hop-skeleton {",
            "opacity: 1;",
            "min-width: 24px;",
            "min-height: 9px;",
        ] {
            assert!(
                locked.contains(must_contain),
                "the locked block must contain {must_contain:?}, got:\n{locked}"
            );
        }

        for must_not_contain in [
            // A selector from well outside the honesty-critical section —
            // proves the slice does not run past its own `-END` sentinel
            // into the rest of the file.
            ".hop-status",
            ".hop-row-hint-label",
            "window.background",
            // The sentinel comments themselves must not survive into the
            // resolved text — `resolve_template`'s comment-stripping
            // already guarantees this for every `/* ... */` span, but this
            // pins it for these two specifically, since a caller diffing
            // this output against a hand-written expectation would
            // otherwise see them as a spurious difference (this module's
            // own doc comment on `locked_block_slice` makes the same
            // point).
            "HOP-HONESTY-LOCKED-BLOCK",
        ] {
            assert!(
                !locked.contains(must_not_contain),
                "the locked block must not contain {must_not_contain:?}, got:\n{locked}"
            );
        }
    }

    /// [`locked_block_slice`]'s own failure path: a template missing the
    /// `-START` sentinel entirely must panic naming it, rather than
    /// silently returning some other, wrong slice (or the whole string).
    #[test]
    #[should_panic(expected = "HOP-HONESTY-LOCKED-BLOCK-START")]
    fn locked_block_slice_panics_when_the_start_sentinel_is_missing() {
        locked_block_slice(".hop-honesty { opacity: 1; }\n/* HOP-HONESTY-LOCKED-BLOCK-END */");
    }

    /// The `-END` sentinel's identical failure path.
    #[test]
    #[should_panic(expected = "HOP-HONESTY-LOCKED-BLOCK-END")]
    fn locked_block_slice_panics_when_the_end_sentinel_is_missing() {
        locked_block_slice(
            "/* HOP-HONESTY-LOCKED-BLOCK-START trailing prose */\n.hop-honesty { opacity: 1; }",
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
            Motion::Full,
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
            Motion::Full,
        );
    }

    /// The `{{motion:...}}` form — issue #207 — must fail exactly the same
    /// way, via [`tokens::resolve_motion`] rather than [`tokens::resolve`].
    #[test]
    #[should_panic(expected = "hop-this-token-does-not-exist")]
    fn motion_placeholder_naming_a_missing_token_panics() {
        resolve_template(
            ".x { transition-duration: {{motion:hop-this-token-does-not-exist}}; }",
            Palette::Dark,
            Motion::Full,
        );
    }

    /// An unterminated `{{` (no matching `}}`) is the second way a
    /// placeholder can go wrong, distinct from a missing token name — this
    /// pins that this module catches it too, rather than silently emitting
    /// the dangling `{{` text into the resolved sheet.
    #[test]
    #[should_panic(expected = "no matching")]
    fn unterminated_placeholder_panics() {
        resolve_template(".x { color: {{hop-bg; }", Palette::Dark, Motion::Full);
    }

    /// The same real stylesheet, resolved under each palette, must actually
    /// differ — proving `palette` is threaded all the way through to every
    /// placeholder, not dropped somewhere between this module and
    /// `tokens::resolve`. `--hop-bg` alone (window ground) is enough to tell
    /// dark and light apart, per `tokens.rs`'s own
    /// `light_palette_resolves_a_semantic_token_to_the_light_ramp` test.
    #[test]
    fn resolved_real_stylesheet_differs_between_palettes() {
        let dark = resolve(Palette::Dark, Motion::Full);
        let light = resolve(Palette::Light, Motion::Full);
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

    /// Issue #215's own regression test: `GtkListView`'s own bare
    /// `listview` node (not `listview > row`, which was already styled)
    /// must paint the window-ground token — before the fix, no selector
    /// named the bare node at all, so any area the list view owns but no
    /// realized row covers fell through to libadwaita's stock background.
    /// This test fails against that bug (no `listview {` rule exists to
    /// extract) and passes once the rule is added, resolving `--hop-bg`
    /// under both palettes exactly as
    /// `resolved_real_stylesheet_differs_between_palettes` above already
    /// pins for `window.background` itself.
    #[test]
    fn listview_own_node_paints_the_window_ground_token() {
        let dark = resolve(Palette::Dark, Motion::Full);
        let light = resolve(Palette::Light, Motion::Full);

        let dark_rule = extract_rule(&dark, "listview {");
        let light_rule = extract_rule(&light, "listview {");

        assert!(
            dark_rule.contains("background-color: #121214;"),
            "the listview node should resolve --hop-bg to the dark ramp's value, got: {dark_rule}"
        );
        assert!(
            light_rule.contains("background-color: #faf9f6;"),
            "the listview node should resolve --hop-bg to the light ramp's value, got: {light_rule}"
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
        let dark = resolve(Palette::Dark, Motion::Full);
        let light = resolve(Palette::Light, Motion::Full);

        let dark_rule = extract_rule(&dark, ".hop-row-hint-key");
        let light_rule = extract_rule(&light, ".hop-row-hint-key");

        assert_ne!(
            dark_rule, light_rule,
            "the key glyph's colour must differ between palettes — got the \
             same rule under both: {dark_rule}"
        );
        assert!(
            dark_rule.contains("color: #5AA9E6;"),
            "dark key glyph should resolve to the dark accent, got: {dark_rule}"
        );
        assert!(
            light_rule.contains("color: #3A6E96;"),
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
        let dark = resolve(Palette::Dark, Motion::Full);
        let light = resolve(Palette::Light, Motion::Full);

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

    /// Issue #207's own consumer-level proof that the motion axis actually
    /// threads all the way from [`tokens::resolve_motion`] through this
    /// module's `{{motion:name}}` placeholder into real, resolved CSS text
    /// — `tokens.rs`'s own tests already prove `resolve_motion` itself is
    /// motion-aware in isolation; this proves this module does not drop
    /// that awareness on the way to a stylesheet.
    ///
    /// Pins every acceptance-criterion detail at once: the fade's duration
    /// (80ms, `--hop-duration-fast`, untouched by the `@media` block) and
    /// easing curve (`--hop-ease-out`) are identical under both motion
    /// states, while only the delay (`--hop-duration-hint`) collapses to
    /// `0ms` under [`Motion::Reduced`] — "the delay disappears, the fade
    /// does not," per this issue's own brief.
    #[test]
    fn hint_fade_uses_the_token_resolved_duration_easing_and_delay() {
        let full = resolve(Palette::Dark, Motion::Full);
        let reduced = resolve(Palette::Dark, Motion::Reduced);

        let full_rule = extract_rule(&full, ".hop-row-hint.hop-row-hint-shown");
        let reduced_rule = extract_rule(&reduced, ".hop-row-hint.hop-row-hint-shown");

        assert_ne!(
            full_rule, reduced_rule,
            "the hint fade's rule must differ between motion states — got the same rule \
             under both: {full_rule}"
        );

        assert!(
            full_rule.contains("transition: opacity 80ms cubic-bezier(0.16, 1, 0.3, 1) 40ms;"),
            "under full motion the fade should carry the token-resolved 80ms duration, \
             ease-out curve, and 40ms delay, got: {full_rule}"
        );
        assert!(
            reduced_rule.contains("transition: opacity 80ms cubic-bezier(0.16, 1, 0.3, 1) 0ms;"),
            "under reduced motion the same 80ms duration and ease-out curve must survive \
             unchanged (--hop-duration-fast has no @media override), with only the delay \
             collapsing to 0ms (one of the six overrides), got: {reduced_rule}"
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
                "500 13.5px/20px \"Geist\", -apple-system, \"Cantarell\", sans-serif"
            ),
            "500 13.5px \"Geist\", -apple-system, \"Cantarell\", sans-serif"
        );
    }

    /// A single `{{font:...}}` placeholder, resolved end to end against the
    /// real token table, must already be the exact 3-field form GTK's `font:`
    /// shorthand accepts — confirming the whole pipeline (`tokens::resolve`
    /// then the transform) for one concrete, real token rather than only the
    /// synthetic string the test above uses.
    #[test]
    fn font_placeholder_resolves_to_the_gtk_accepted_shorthand_form() {
        let resolved = resolve_template(
            ".x { font: {{font:hop-text-error}}; }",
            Palette::Dark,
            Motion::Full,
        );
        assert!(
            resolved.contains("font: 500 13.5px \"Geist\""),
            "expected the 3-field shorthand with no `/<line-height>` segment, got: {resolved}"
        );
    }

    /// [`font_weight_only`]/[`font_size_only`]/[`font_family_only`]'s own
    /// unit coverage — issue #200's code-review fix — the same isolated-
    /// from-`tokens::resolve` shape `font_shorthand_strips_the_line_height_segment`
    /// above uses, proving each longhand extracts exactly its own field and
    /// nothing else from the identical raw token text that test's shorthand
    /// case already covers.
    #[test]
    fn font_longhand_helpers_each_extract_their_own_field() {
        let raw = "500 13.5px/20px \"Geist\", -apple-system, \"Cantarell\", sans-serif";
        assert_eq!(font_weight_only(raw), "500");
        assert_eq!(font_size_only(raw), "13.5px");
        assert_eq!(
            font_family_only(raw),
            "\"Geist\", -apple-system, \"Cantarell\", sans-serif"
        );
    }

    /// The locked-block half of
    /// `font_placeholder_resolves_to_the_gtk_accepted_shorthand_form` above:
    /// `{{font-weight:name}}`/`{{font-size:name}}`, resolved end to end
    /// against the real token table, must produce bare weight/size values
    /// with no family and no `/<line-height>` segment — the exact shape
    /// `assets/stylesheet.css`'s locked `.hop-honesty-text`/
    /// `.hop-honesty-stamp` rules need so the above-user-priority provider
    /// never contests `font-family`.
    #[test]
    fn font_weight_and_size_placeholders_resolve_with_no_family_or_line_height() {
        let resolved = resolve_template(
            ".x { font-weight: {{font-weight:hop-text-error}}; \
             font-size: {{font-size:hop-text-error}}; }",
            Palette::Dark,
            Motion::Full,
        );
        assert!(
            resolved.contains("font-weight: 500;"),
            "expected the bare weight with no other field, got: {resolved}"
        );
        assert!(
            resolved.contains("font-size: 13.5px;"),
            "expected the bare size with the `/<line-height>` segment dropped, got: {resolved}"
        );
        assert!(
            !resolved.contains("Geist"),
            "a `{{{{font-weight:...}}}}`/`{{{{font-size:...}}}}` placeholder must never leak \
             the family list into the resolved sheet, got: {resolved}"
        );
    }
}
