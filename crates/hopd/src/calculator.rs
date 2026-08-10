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

use std::sync::Arc;
use std::time::Duration;

use fasteval::EmptyNamespace;
use hop_core::provider::{
    CALCULATOR_PROVIDER_ID, Provider, ProviderError, ProviderManifest, QueryCtx,
};
use hop_core::router::{Mode, RoutedQuery};
use hop_protocol::{
    Action, ActionId, ActionKind, CopyText, ExecOutcome, Item, ItemId, Kind, limits::MAX_TITLE,
};

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
///   [`hop_core::learning::Learning::boost_for`] key on the provider and the
///   item id together (issue #72), never on anything past
///   `item_id.as_str()` for the id half. Every calculator item shares the
///   same provider, so the id string is still what has to distinguish one
///   expression's learning from another's within it: if the id encoded the
///   result instead, `2+2` and `1+3` — two different expressions that
///   happen to land on the same number — would share one learning key, and
///   launching one would boost the other. Encoding the expression keeps
///   every distinct query its own row.
/// - This is already the shape the rest of the tree assumes:
///   `crates/hop-core/src/pipeline.rs` and `crates/hop-core/src/rank.rs`
///   both build `Kind::Calculator` test fixtures as `"calc:2+2"`,
///   `"calc:terminal"` and similar — each file's `tests` module has an
///   `item(...)` fixture helper, and every `Kind::Calculator` call built
///   through it uses a `calc:`-prefixed id — this function matches a
///   scheme the tree already leans on, rather than inventing a third one.
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

/// The calculator provider: turns a routed term into zero or one
/// [`Item`] via [`build_item`], and dispatches its one action by
/// re-deriving the same result from the item id via [`copy_text_for`].
/// Holds no state — there is nothing to hold, since evaluation needs
/// nothing but the term itself (this module's own docs; this plan's
/// Design decision 6).
pub struct CalculatorProvider;

impl Provider for CalculatorProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            // Must be the shared constant, never a hand-written literal —
            // see this plan's Scope section and the issue's own first
            // comment. `hop_core::provider::CALCULATOR_PROVIDER_ID`'s own
            // docs spell out why a drift here would matter.
            id: CALCULATOR_PROVIDER_ID,
            kinds: vec![Kind::Calculator],
            // Mode::Calculator alone, deliberately not Mode::All — see this
            // plan's Design decision 1. `hop_core::router::route` already
            // sends both the exclusive `=2+2` and the inferred bare `2+2`
            // through `Mode::Calculator`, so `should_query`'s literal
            // containment check already reaches this provider on both
            // routes with no help from `ProviderHost::selected`'s
            // augmentation branch. Adding `Mode::All` would instead ask
            // this provider to attempt an evaluation on every non-math
            // keystroke of every query, for an outcome that is always
            // `None`. `crates/hop-core/src/host.rs`'s `tests` module has
            // `hop-core`'s own worked example of exactly this shape:
            // `an_inferred_route_selects_both_the_mode_all_provider_and_the_provider_declaring_that_mode`.
            modes: vec![Mode::Calculator],
            // 1, not 0: an empty term never evaluates (see `evaluate`'s
            // own tests), so this skips the guaranteed-failing case — the
            // bare `=` route — at the pre-filter, before a task is even
            // spawned.
            min_term_len: 1,
            budget: Duration::from_millis(5),
        }
    }

    async fn query(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(build_item(q.term.as_str()).into_iter().collect())
    }

    /// Ignores `_action_id` — safe today, for a reason worth writing down
    /// rather than leaving implicit.
    ///
    /// # Why ignoring the argument is sound
    ///
    /// `crates/hopd/src/connection.rs`'s dispatch loop refuses an
    /// `action_id` that is not on the *retained* item's own `actions`
    /// list — a check of the shape `if !item.actions.iter().any(|a| a.id
    /// == action_id)` — and answers the client with `ErrorCode::
    /// UnknownAction` before this `execute` is ever reached. [`build_item`]
    /// attaches exactly one action (`"copy"`, [`ActionKind::Copy`]) to
    /// every calculator item it builds, so any `action_id` that survives
    /// that upstream check can only be the id of that one action — there
    /// is no second action for this call to have been asked to
    /// distinguish itself from.
    ///
    /// If [`build_item`] ever grows a second action, this stops being true
    /// and becomes a real bug: this `execute` would need to start
    /// switching on `action_id`, fixed in the very same change that adds
    /// the second action, not after.
    async fn execute(
        self: Arc<Self>,
        item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        copy_text_for(&item_id)
            .map(ExecOutcome::CopyText)
            .ok_or_else(|| {
                ProviderError::Failed(format!(
                    "{} is not a live calculator result",
                    item_id.as_str()
                ))
            })
    }
}

/// Re-derives the copyable result for `item_id`, the way
/// `CalculatorProvider::execute` answers its one action: strips the
/// `calc:` prefix [`build_item`] gave the id, re-evaluates what is left
/// exactly as `query()` did, and formats it the same way. `None` covers
/// both "this id was never one of ours" (no `calc:` prefix) and — in
/// principle, since the same string that built the id always re-evaluates
/// the same way — the unreachable case where it no longer does; either way
/// `execute` reports it as an ordinary provider failure, never a panic.
fn copy_text_for(item_id: &ItemId) -> Option<CopyText> {
    let term = item_id.as_str().strip_prefix("calc:")?;
    let value = evaluate(term)?;
    CopyText::new(format_result(value)).ok()
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
mod provider_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hop_core::host::{NoopLog, ProviderHost};
    use hop_core::pipeline::{CheckedItems, ProviderOutput};
    use hop_core::provider::{CancellationFlag, should_query};
    use hop_core::router::route;

    fn ctx() -> QueryCtx {
        QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: std::time::Instant::now() + Duration::from_secs(1),
        }
    }

    // --- Manifest shape (Design decision 1). ---

    #[test]
    fn the_manifest_uses_the_shared_calculator_provider_id_constant() {
        assert_eq!(CalculatorProvider.manifest().id, CALCULATOR_PROVIDER_ID);
    }

    #[test]
    fn the_manifest_declares_only_mode_calculator_and_kind_calculator() {
        let manifest = CalculatorProvider.manifest();
        assert_eq!(manifest.modes, vec![Mode::Calculator]);
        assert_eq!(manifest.kinds, vec![Kind::Calculator]);
    }

    #[test]
    fn should_query_reaches_this_manifest_on_both_the_explicit_and_inferred_math_routes() {
        let manifest = CalculatorProvider.manifest();
        assert!(
            should_query(&manifest, &route("=2+2")),
            "explicit `=` route"
        );
        assert!(
            should_query(&manifest, &route("2+2")),
            "inferred bare-math route"
        );
        assert!(
            !should_query(&manifest, &route("firefox")),
            "an ordinary query must not reach this provider"
        );
    }

    #[test]
    fn min_term_len_skips_the_empty_term_but_not_a_single_digit() {
        let manifest = CalculatorProvider.manifest();
        assert!(
            !should_query(&manifest, &route("=")),
            "an empty term after the `=` sigil is skipped at the pre-filter"
        );
        assert!(
            should_query(&manifest, &route("5")),
            "a single digit is a term of length 1"
        );
    }

    #[tokio::test]
    async fn registered_with_a_real_host_the_provider_is_selected_for_a_math_looking_query() {
        let mut host = ProviderHost::with_log(Arc::new(NoopLog));
        host.register(CalculatorProvider).unwrap();
        let manifest = &host.manifests()[0];
        assert!(should_query(manifest, &route("2+2")));
    }

    // --- The provider's own output survives its own manifest checks. ---

    #[tokio::test]
    async fn the_providers_own_output_passes_its_own_manifest_checks() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider
            .clone()
            .query(Arc::new(route("2+2")), ctx())
            .await
            .unwrap();
        assert_eq!(items.len(), 1, "the fixture must actually produce an item");

        let checked = CheckedItems::check(vec![ProviderOutput::from_provider(&*provider, items)]);
        assert_eq!(
            checked.rejections(),
            &[],
            "the calculator provider's own honest output must survive its own manifest"
        );
        assert_eq!(checked.items().len(), 1);
    }

    // --- query(): the pure eval-and-format path, driven through Provider. ---

    #[tokio::test]
    async fn query_returns_the_calculator_item_for_a_math_looking_term() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider.query(Arc::new(route("2+2")), ctx()).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "2+2 = 4");
    }

    #[tokio::test]
    async fn query_returns_no_items_rather_than_an_error_for_input_that_is_not_an_expression() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider
            .query(Arc::new(route("=hello")), ctx())
            .await
            .unwrap();
        assert_eq!(
            items,
            vec![],
            "criterion 4: no items, and Ok — never an Err"
        );
    }

    #[tokio::test]
    async fn query_returns_no_items_for_a_non_finite_result() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider
            .query(Arc::new(route("=1/0")), ctx())
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    // --- execute(): re-derives the same result the item's title showed. ---

    #[tokio::test]
    async fn execute_copies_the_same_result_the_item_was_built_with() {
        let provider = Arc::new(CalculatorProvider);
        let item = build_item("10/4").unwrap();
        let outcome = provider
            .execute(item.id.clone(), item.default_action.clone())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ExecOutcome::CopyText(CopyText::new("2.5").unwrap())
        );
    }

    #[tokio::test]
    async fn execute_fails_rather_than_panicking_on_an_id_with_no_calc_prefix() {
        let provider = Arc::new(CalculatorProvider);
        let result = provider
            .execute(
                ItemId::new("app:firefox").unwrap(),
                ActionId::new("copy").unwrap(),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Failed(_))));
    }

    #[tokio::test]
    async fn execute_fails_rather_than_panicking_on_a_calc_prefixed_id_that_does_not_evaluate() {
        let provider = Arc::new(CalculatorProvider);
        let result = provider
            .execute(
                ItemId::new("calc:not an expression").unwrap(),
                ActionId::new("copy").unwrap(),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Failed(_))));
    }

    // --- Criterion 6 (this plan's "no I/O" claim): a structural witness. ---

    #[test]
    fn the_module_source_touches_no_disk_process_or_network() {
        // The mechanical half of this plan's Design decision 6 — grepping
        // this file's own source back, the way
        // `crates/hop-protocol/src/item.rs`'s
        // `every_test_this_file_names_in_its_docs_exists` does, and the way
        // the workspace's root `Cargo.toml` names `grep -rn unsafe_code
        // crates/` as the spot-check for its own `unsafe_code = "deny"`
        // claim. Not a substitute for the design argument (there is no
        // index here to build, unlike `apps.rs`'s `AppIndex`) — a second,
        // mechanical witness that would fail loudly if a future edit
        // reached for `std::fs`, spawned a process, or opened a socket on
        // this path.
        //
        // Scoped to non-comment source *before* the first `#[cfg(test)]`
        // marker, not the raw whole file: a naive `include_str!` scan of
        // the entire file is self-defeating twice over here. First, this
        // very needle list would contain the literal string `"std::fs"` as
        // one of its own array elements, so a scan that reached this test
        // module's own code would always find it, regardless of what the
        // production code does. Second, this file's own module doc
        // comment (top of file) names `std::fs` and `std::process` in
        // backticks as the very things it promises *not* to use — prose
        // that is honest and worth keeping, but that also defeats a raw
        // substring search if comment lines are left in. Dropping the
        // `#[cfg(test)]`-and-after tail handles the first; dropping
        // comment lines (`//`, `///`, `//!`) from what remains handles the
        // second. Design decision 6's own claim is about the module's
        // *production code*, not its prose, so this scoping matches the
        // claim actually being checked rather than accidentally widening
        // it into a check on documentation text.
        //
        // This split-on-first-marker approach has one failure mode of its
        // own, and it is silent rather than loud: if a future
        // `#[cfg(test)]`-gated helper is ever placed *before* this file's
        // real test modules, `.split("#[cfg(test)]").next()` truncates at
        // that earlier marker, and everything below it — including all of
        // this module's actual production code — drops out of `source`
        // unseen. The needle loop below would then find none of its
        // needles not because the module is clean, but because it was
        // never looked at, and the test would keep passing while checking
        // almost nothing. The two assertions immediately below guard
        // against exactly that: they require the scanned region to still
        // contain named production landmarks, so a truncation that loses
        // the real code turns this test red instead of green.
        let full_source = include_str!("calculator.rs");
        let production = full_source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(full_source);
        let source: String = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            source.contains("pub struct CalculatorProvider"),
            "the scanned region lost CalculatorProvider's declaration — the \
             #[cfg(test)] split likely truncated before reaching production \
             code, which would make the needle checks below pass vacuously"
        );
        assert!(
            source.contains("pub(crate) fn evaluate"),
            "the scanned region lost evaluate's signature — same \
             truncation risk as above"
        );
        for needle in [
            "std::fs",
            "std::process",
            "std::net",
            "TcpStream",
            "UdpSocket",
        ] {
            assert!(
                !source.contains(needle),
                "calculator.rs must not reference {needle}"
            );
        }
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
