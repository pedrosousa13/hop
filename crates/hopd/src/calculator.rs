//! The calculator provider: evaluates arithmetic expressions with
//! `fasteval` and offers the formatted result as a single, copyable item.
//!
//! Every function in this module is pure — no `std::fs`, no `std::process`,
//! no network client anywhere in this file (acceptance criterion 4 on
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
// No consumer outside `#[cfg(test)]` until Task 3 wires this into
// `build_item` — matching `crates/hopd/src/apps.rs`'s own precedent for the
// identical shape of problem (`parse_desktop_entry` et al., landed with no
// caller ahead of the directory scan that used them). `cfg_attr(not(test),
// ...)` rather than a bare `#[expect]`: this module's own tests already call
// `evaluate` directly, so under `--cfg test` it is not dead at all, and an
// unconditional `#[expect]` would itself go unfulfilled on `cargo test`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no consumer until Task 3 (issue #58) wires this into build_item"
    )
)]
pub(crate) fn evaluate(expr: &str) -> Option<f64> {
    let mut ns = EmptyNamespace;
    match fasteval::ez_eval(expr, &mut ns) {
        Ok(value) if value.is_finite() => Some(value),
        Ok(_) | Err(_) => None,
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
