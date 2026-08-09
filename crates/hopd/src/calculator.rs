//! The calculator provider: evaluates arithmetic expressions with
//! `fasteval` and offers the formatted result as a single, copyable item.
//!
//! Every function in this module is pure — no `std::fs`, no `std::process`,
//! no network client anywhere in this file (acceptance criterion 6 on
//! issue #58; pinned structurally once the whole module exists, by
//! `provider_tests::the_module_source_touches_no_disk_process_or_network`
//! in Task 4). There is no index to build, no state to watch, and no cache
//! between a `query()` and the `execute()` that follows it — the expression
//! is re-evaluated from the item id, because re-evaluating is as cheap as
//! evaluating was the first time. See this crate's implementation plan
//! (`docs/superpowers/plans/2026-08-09-issue-58-calculator-provider.md`)
//! for the full reasoning behind the item-id scheme, the formatting rule,
//! and the one deliberate reading of `%` as modulo rather than percent-of —
//! `fasteval`'s own grammar has no percent-of operator, and this module
//! does not add one.

use fasteval::EmptyNamespace;

/// Evaluates `expr` as an arithmetic expression, folding every failure
/// mode into `None`: a parse error (an unbalanced paren, a bare `%` with no
/// right-hand operand, an unknown identifier), *and* a successful
/// evaluation whose result is not finite. Acceptance criterion 4 draws no
/// distinction between "this is not an expression" and "this expression's
/// answer is not a number worth copying," so this function draws none
/// either — every caller downstream treats `None` as the single "no
/// calculator item" outcome.
///
/// # `fasteval` does not error on `1/0` or `0/0`
///
/// Verified directly against the vendored `fasteval` 0.2.4 source: division
/// is plain `f64` division with no special-casing, so `1/0` evaluates to
/// `Ok(f64::INFINITY)`, `0/0` to `Ok(f64::NAN)`, `-1/0` to
/// `Ok(f64::NEG_INFINITY)` — IEEE-754 semantics, not a refusal. Without the
/// `is_finite()` check below, this function would hand a caller a value it
/// could format and show as `"1/0 = inf"`, which is exactly the "error
/// item" acceptance criterion 4 forbids, just spelled differently.
pub(crate) fn evaluate(expr: &str) -> Option<f64> {
    let mut ns = EmptyNamespace;
    match fasteval::ez_eval(expr, &mut ns) {
        Ok(value) if value.is_finite() => Some(value),
        Ok(_) | Err(_) => None,
    }
}

/// Decimal places [`format_result`] keeps before trimming trailing zeros —
/// generous enough that `1/3` reads as `"0.3333333333"` rather than the
/// full `f64` precision (`0.3333333333333333`), which would print
/// sixteen-plus digits of noise for the common case (`0.1 + 0.2` is
/// `0.30000000000000004` in `f64` — see the test table).
const FIXED_DECIMALS: usize = 10;

/// At or above this magnitude, [`format_result`] switches to exponential
/// notation rather than printing a wall of digits: `"1e20"` reads as a
/// calculator answer, `"100000000000000000000"` reads as a typo.
const EXPONENTIAL_ABOVE: f64 = 1e15;

/// Below this magnitude (and not exactly zero), [`format_result`] also
/// switches to exponential notation. Chosen with a full order of magnitude
/// of margin over the point where fixed formatting at [`FIXED_DECIMALS`]
/// places rounds a genuinely nonzero value down to a string of all zeros —
/// measured directly: `format!("{:.10}", 4.9e-11)` is `"0.0000000000"`,
/// `format!("{:.10}", 5e-11)` is `"0.0000000001"`. `1e-9` sits well clear of
/// that rounding boundary rather than exactly on it.
const EXPONENTIAL_BELOW: f64 = 1e-9;

/// Formats an already-finite `value` for display and for
/// [`hop_protocol::CopyText`] alike.
///
/// `2.0 + 2.0` must read as `"4"`, never `"4.0"` — the rule: an exact zero
/// (which also catches negative zero, since `-0.0 == 0.0`) prints as
/// `"0"`; a magnitude inside `[EXPONENTIAL_BELOW, EXPONENTIAL_ABOVE)`
/// prints fixed at [`FIXED_DECIMALS`] places with trailing zeros — and a
/// trailing decimal point, once they're gone — trimmed; anything outside
/// that range prints in Rust's own `{:e}` form, which is already minimal
/// and needs no further trimming.
///
/// Every case in this doc comment, and more, is pinned by
/// `format_tests::format_result_matches_the_verified_table` below —
/// verified against a real `rustc` run before being written into this
/// plan, not estimated.
pub(crate) fn format_result(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    let magnitude = value.abs();
    if !(EXPONENTIAL_BELOW..EXPONENTIAL_ABOVE).contains(&magnitude) {
        return format!("{value:e}");
    }
    trim_trailing_zeros(&format!("{value:.FIXED_DECIMALS$}"))
}

/// Strips trailing `0`s after a decimal point, then the point itself if
/// nothing is left after it. A no-op on a string with no `.` at all — never
/// produced by `format!("{value:.FIXED_DECIMALS$}")` since
/// `FIXED_DECIMALS > 0`, but the guard costs nothing and keeps this
/// function correct for any caller, not just its one today.
fn trim_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

use hop_core::provider::CALCULATOR_PROVIDER_ID;
use hop_protocol::{Action, ActionId, ActionKind, Item, ItemId, Kind, limits::MAX_TITLE};

/// Builds the single item a routed term produces, or `None` if [`evaluate`]
/// could not turn it into a finite result — the one branch point between
/// "show a calculator item" and "show nothing," shared by
/// `CalculatorProvider::query` (Task 4) with no other logic layered on top
/// of it.
///
/// # The item id encodes the expression, not the result
///
/// The id is `calc:<term>` — the *expression* the user typed, not the
/// number it evaluates to. Two facts, both checked before writing this
/// function, argue for this over the alternative (encoding the formatted
/// result instead):
///
/// - [`hop_core::learning::Learning::record_launch`] and
///   [`hop_core::learning::Learning::boost_for`] both key **only** on the
///   bare item id string — `crates/hop-core/src/learning.rs`'s `record`
///   and `boost_for` never look past `item_id.as_str()`. If the id encoded
///   the result, `2+2` and `1+3` — two different expressions that happen
///   to land on the same number — would share one learning key, and
///   launching one would boost the other. Encoding the expression keeps
///   every distinct query its own row.
/// - This is already the shape the rest of the tree assumes:
///   `crates/hop-core/src/pipeline.rs` and `crates/hop-core/src/rank.rs`
///   both build `Kind::Calculator` test fixtures as `"calc:2+2"`,
///   `"calc:terminal"` and similar (`pipeline.rs:1215`, `:1890`, `:2285`;
///   `rank.rs:785`, among others) — this function matches a scheme the
///   tree already leans on, rather than inventing a third one.
///
/// # `ItemId::new` cannot fail here, and is still checked rather than
/// unwrapped
///
/// `term` reaches this function, in production, only after routing, and
/// [`hop_protocol::limits::MAX_QUERY_TEXT`] bounds a query's raw text at
/// 1 024 bytes — the largest `term` this function is ever handed by
/// `CalculatorProvider`. `"calc:"` adds 5 more, for 1 029 at the absolute
/// worst case, comfortably under [`hop_protocol::limits::MAX_ITEM_ID`]'s
/// 4 096. So the `.ok()?` below can be proven never to fire for any term
/// that arrived through the router — the guard exists because the type
/// allows the possibility, not because it is expected to, matching the
/// same reasoning `crates/hopd/src/apps.rs`'s `build_entry` gives for its
/// own `ItemId::new(...).ok()?`. This module's own tests call `build_item`
/// directly with hand-written terms that do not go through that bound at
/// all, so the guard is real for them.
///
/// # The title is truncated; the id is not
///
/// `title` is `"<term> = <result>"`, which can exceed
/// [`hop_protocol::limits::MAX_TITLE`] (1 024 bytes) even though `term`
/// alone cannot exceed 1 024: a term at or near that query bound, plus
/// `" = "` and the formatted result, clears it. Unlike `ItemId::new`
/// above, this is **not** a guard against something that cannot happen — a
/// long, syntactically valid, evaluable expression really can reach this
/// function, and without truncation
/// [`hop_core::pipeline::CheckedItems::check`] would reject the whole item
/// outright as a field-too-long rejection, silently dropping a
/// correctly-computed answer. Truncating the title at a char boundary
/// (never the id, which the bound above shows has room to spare) keeps
/// the item alive instead — pinned by
/// `item_tests::an_overlong_title_is_truncated_rather_than_dropping_the_item`.
// No consumer outside `#[cfg(test)]` until Task 4 wires this into
// `CalculatorProvider::query` — same shape as `evaluate`'s and
// `format_result`'s own dead-code exemption while they waited for this
// function to call them. `cfg_attr(not(test), ...)` rather than a bare
// `#[expect]`: this module's own tests already call `build_item` directly,
// so under `--cfg test` it is not dead at all, and an unconditional
// `#[expect]` would itself go unfulfilled on `cargo test`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no consumer until Task 4 (issue #58) wires this into CalculatorProvider::query"
    )
)]
pub(crate) fn build_item(term: &str) -> Option<Item> {
    let value = evaluate(term)?;
    let result = format_result(value);
    let id = ItemId::new(format!("calc:{term}")).ok()?;
    let title = truncate_to_byte_boundary(&format!("{term} = {result}"), MAX_TITLE);

    Some(Item {
        id,
        kind: Kind::Calculator,
        title,
        subtitle: None,
        icon: None,
        actions: vec![Action {
            id: ActionId::new("copy").expect("within bounds by construction"),
            kind: ActionKind::Copy,
            label: "Copy".to_string(),
        }],
        default_action: ActionId::new("copy").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: CALCULATOR_PROVIDER_ID.to_string(),
    })
}

/// Truncates `s` to at most `max` bytes, never splitting a multi-byte
/// character. Ported verbatim from `crates/hopd/src/apps.rs`'s function of
/// the same name — not shared via a common module; see this plan's Design
/// decision 7 for why `hopd/src/` stays flat rather than growing a shared
/// module for one ten-line function.
fn truncate_to_byte_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod item_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn build_item_sets_the_calc_prefixed_id_and_the_expr_equals_result_title() {
        let item = build_item("2+2").expect("2+2 evaluates");
        assert_eq!(item.id.as_str(), "calc:2+2");
        assert_eq!(item.title, "2+2 = 4");
        assert_eq!(item.kind, Kind::Calculator);
        assert_eq!(item.provider, CALCULATOR_PROVIDER_ID);
    }

    #[test]
    fn build_item_carries_exactly_one_copy_action_agreeing_with_default_action() {
        let item = build_item("2+2").unwrap();
        assert_eq!(item.actions.len(), 1);
        assert_eq!(item.actions[0].kind, ActionKind::Copy);
        assert_eq!(item.actions[0].id, item.default_action);
    }

    #[test]
    fn build_item_leaves_the_items_own_copy_text_field_unset() {
        // Deliberate — see this plan's Scope section on why the Copy
        // action + execute() round trip is the sole mechanism for
        // acceptance criterion 3, not a second, redundant path.
        let item = build_item("2+2").unwrap();
        assert_eq!(item.copy_text, None);
    }

    #[test]
    fn build_item_returns_none_for_input_evaluate_refuses() {
        assert!(build_item("hello").is_none());
        assert!(build_item("1/0").is_none());
        assert!(build_item("").is_none());
    }

    #[test]
    fn an_overlong_title_is_truncated_rather_than_dropping_the_item() {
        // "1" then "+1" repeated 510 times is a valid, evaluable expression
        // 1021 bytes long — short of MAX_QUERY_TEXT (a real router-derived
        // term could be this long), but long enough that
        // "<term> = <result>" (1021 + " = 511" = 1027 bytes) clears
        // MAX_TITLE (1024).
        let term = format!("1{}", "+1".repeat(510));
        assert_eq!(term.len(), 1021);
        let item = build_item(&term).expect("a long chain of additions still evaluates");
        assert!(item.title.len() <= MAX_TITLE);
        assert!(std::str::from_utf8(item.title.as_bytes()).is_ok());
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn format_result_matches_the_verified_table() {
        let cases: &[(f64, &str)] = &[
            (4.0, "4"),
            (-4.0, "-4"),
            (0.0, "0"),
            (-0.0, "0"),
            (1.0 / 3.0, "0.3333333333"),
            (0.1 + 0.2, "0.3"),
            (10.0 / 4.0, "2.5"),
            (2f64.sqrt(), "1.4142135624"),
            (1e14, "100000000000000"),
            (1e15, "1e15"),
            (1e20, "1e20"),
            (1e-9, "0.000000001"),
            (9e-10, "9e-10"),
            (1e-15, "1e-15"),
        ];
        for (value, expected) in cases {
            assert_eq!(
                &format_result(*value),
                expected,
                "format_result({value}) should be {expected}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Arithmetic the issue names by hand: unary minus and percent. ---

    #[test]
    fn a_simple_sum_evaluates() {
        assert_eq!(evaluate("2+2"), Some(4.0));
    }

    #[test]
    fn unary_minus_is_handled() {
        assert_eq!(evaluate("-4+10"), Some(6.0));
        assert_eq!(evaluate("-5"), Some(-5.0));
    }

    #[test]
    fn percent_is_modulo_not_percent_of() {
        // Verified against fasteval's own precedence table and empirically,
        // against the vendored 0.2.4 source: `%` is strict binary modulo,
        // matching Rust's own operator. See this plan's Design decision 5
        // — the old extension this issue's brief gestures at is not in
        // this repo to compare against, so this reading is fasteval's,
        // stated and tested rather than assumed.
        assert_eq!(evaluate("10%3"), Some(1.0));
        assert_eq!(evaluate("7%3"), Some(1.0));
    }

    #[test]
    fn a_bare_trailing_percent_is_not_an_expression() {
        // The percent-of reading a user might expect from "50%" does not
        // exist in fasteval's grammar: `%` is infix-only, so a value with
        // nothing on its right is a parse error, not 0.5. Pinned here so a
        // future change that adds percent-of support changes this test
        // rather than silently reversing it.
        assert_eq!(evaluate("50%"), None);
    }

    #[test]
    fn exponents_and_parentheses_are_handled() {
        assert_eq!(evaluate("2^10"), Some(1024.0));
        assert_eq!(evaluate("(1+2)*3"), Some(9.0));
    }

    #[test]
    fn surrounding_and_internal_whitespace_is_tolerated() {
        assert_eq!(evaluate("  2 + 2  "), Some(4.0));
    }

    // --- Criterion 4: not an expression, or not a usable answer. ---

    #[test]
    fn division_by_zero_yields_no_result_rather_than_infinity() {
        assert_eq!(evaluate("1/0"), None);
        assert_eq!(evaluate("-1/0"), None);
    }

    #[test]
    fn zero_over_zero_yields_no_result_rather_than_nan() {
        assert_eq!(evaluate("0/0"), None);
    }

    #[test]
    fn an_identifier_is_not_an_expression() {
        assert_eq!(evaluate("hello"), None);
    }

    #[test]
    fn trailing_garbage_after_a_valid_prefix_is_not_an_expression() {
        assert_eq!(evaluate("2+2x"), None);
    }

    #[test]
    fn an_unterminated_expression_is_not_an_expression() {
        assert_eq!(evaluate("2+"), None);
        assert_eq!(evaluate("(1+"), None);
    }

    #[test]
    fn empty_and_whitespace_only_input_is_not_an_expression() {
        assert_eq!(evaluate(""), None);
        assert_eq!(evaluate("   "), None);
    }
}
