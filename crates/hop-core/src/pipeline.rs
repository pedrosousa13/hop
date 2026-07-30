//! The assembly function: the pure step that turns provider output into the
//! final, ordered, capped result list. This is where routing, aliases,
//! learning and ranking — each built in an earlier M1 slice — meet for the
//! first time.
//!
//! No disk reads, subprocess spawns, or network calls happen anywhere in
//! this module: [`Pipeline::assemble`] runs on every keystroke.

use hop_protocol::{Item, Kind};

use crate::aliases::Aliases;
use crate::learning::Learning;
use crate::rank::{Boosts, Ranker, Weights};
use crate::router::{Mode, RoutedQuery, route};

/// Wires together a [`Ranker`], [`Aliases`] table and [`Learning`] store —
/// each an M1 slice in its own right — into the one pure step the daemon
/// (and every test here) calls per query: [`Pipeline::assemble`].
///
/// `Default` builds all four fields from their own defaults, so a `Pipeline`
/// can be constructed without touching the filesystem — useful for tests and
/// for a future daemon that loads a persisted `Learning` separately and
/// swaps it in.
#[derive(Default)]
pub struct Pipeline {
    pub ranker: Ranker,
    pub aliases: Aliases,
    pub learning: Learning,
    pub weights: Weights,
}

/// The [`Kind`]s a given [`Mode`] serves — used by both the exclusive-mode
/// filter (step 5) and the inferred-mode promotion (step 7), so it's written
/// once. `Mode::All` deliberately returns `None`: it neither filters nor
/// promotes anything.
fn kinds_for_mode(mode: Mode) -> Option<&'static [Kind]> {
    match mode {
        Mode::Windows => Some(&[Kind::Window]),
        Mode::Apps => Some(&[Kind::App]),
        Mode::Files => Some(&[Kind::File]),
        Mode::Emoji => Some(&[Kind::Emoji]),
        Mode::Timezone => Some(&[Kind::Timezone]),
        Mode::Currency => Some(&[Kind::Currency]),
        Mode::Calculator => Some(&[Kind::Calculator]),
        Mode::Weather => Some(&[Kind::Weather]),
        Mode::Actions => Some(&[Kind::Action]),
        Mode::WebSearch => Some(&[Kind::WebSearch]),
        Mode::All => None,
    }
}

/// Stably moves every item whose kind is in `kinds` to the front of `items`,
/// preserving the relative order within each of the two groups and dropping
/// nothing. This is the augment-not-hijack rule from step 7: an inferred
/// utility result (e.g. a calculator hit for `2+2`) leads, but the rest of
/// the ranked body — including an app literally named `2048` — stays.
fn promote_kinds(items: &mut Vec<Item>, kinds: &[Kind]) {
    let (promoted, rest): (Vec<Item>, Vec<Item>) =
        items.drain(..).partition(|item| kinds.contains(&item.kind));
    items.extend(promoted);
    items.extend(rest);
}

impl Pipeline {
    /// Runs the pipeline's nine-step contract over one query's raw text and
    /// the items providers already returned for it. Provider *scheduling*
    /// (parallel dispatch, budgets, partial-result streaming) happens
    /// upstream of this call and is out of scope here — `assemble` is pure:
    /// same inputs, same output, no I/O.
    ///
    /// 1. Route `raw_query`.
    /// 2. Apply aliases to the routed term, producing `effective_term`
    ///    (what ranking uses) and any alias boosts.
    /// 3. Collect boosts: the alias boosts plus a learning boost for every
    ///    candidate item, summed into one [`Boosts`] map.
    /// 4. Split off `append_to_end` items — the pinned tail, never ranked
    ///    (`Ranker::rank` drops them itself, so this split is what keeps
    ///    them alive at all).
    /// 5. If the route is exclusive, filter the remaining items to that
    ///    mode's kinds.
    /// 6. Rank what remains, using `effective_term`.
    /// 7. If the mode was *inferred* (`!exclusive && mode != Mode::All`),
    ///    stably promote that mode's kinds to the front without removing
    ///    anything else.
    /// 8. Concatenate the pinned tail after the ranked body.
    /// 9. Truncate to `max_results` — see the comment above the truncate
    ///    call for why the cap counts the pinned tail too.
    pub fn assemble(
        &mut self,
        raw_query: &str,
        provider_items: Vec<Item>,
        max_results: usize,
    ) -> Vec<Item> {
        // Step 1: route.
        let routed = route(raw_query);

        // Step 2: apply aliases to the routed term.
        let alias_effect = self.aliases.apply(&routed.term);

        // Step 3: collect boosts — alias boosts plus a learning boost per
        // candidate item. Where both apply to the same item, they add.
        //
        // DECISION: the learning boost is keyed on `routed.term` — the
        // query after any prefix was stripped, but *before* the alias
        // rewrite above. Learning records what the user actually typed; an
        // alias rewrite is a ranking substitution the user never typed, so
        // crediting it to learning would be recording a fact that didn't
        // happen. This is a judgement call M2 may revisit once the daemon
        // records real launches and can observe how users actually expect
        // aliased queries to be learned from.
        let mut boosts = Boosts::default();
        for (id, boost) in &alias_effect.boosts {
            *boosts.by_item_id.entry(id.clone()).or_insert(0.0) += *boost;
        }
        for item in &provider_items {
            let learned = self.learning.boost_for(&routed.term, &item.id);
            if learned != 0.0 {
                *boosts.by_item_id.entry(item.id.clone()).or_insert(0.0) += learned;
            }
        }

        // Step 4: split off the pinned tail before anything else touches
        // the list — both the exclusive-mode filter (step 5) and the
        // ranker itself must never see these items.
        let (tail, mut body): (Vec<Item>, Vec<Item>) = provider_items
            .into_iter()
            .partition(|item| item.append_to_end);

        // Step 5: an exclusive route filters the ranked body to its mode's
        // kinds. The pinned tail was already split off above, so a pinned
        // item survives an exclusive filter unconditionally, regardless of
        // its kind — see
        // `tests::pinned_item_survives_exclusive_filter_regardless_of_kind`.
        if routed.exclusive
            && let Some(kinds) = kinds_for_mode(routed.mode)
        {
            body.retain(|item| kinds.contains(&item.kind));
        }

        // Step 6: rank the (possibly filtered) body against the effective
        // term. The routed query is otherwise unchanged — only `term`
        // differs from `routed`.
        let effective_query = RoutedQuery {
            term: alias_effect.effective_term,
            ..routed.clone()
        };
        let ranked = self
            .ranker
            .rank(body, &effective_query, &self.weights, &boosts);
        let mut ranked_items: Vec<Item> = ranked.into_iter().map(|r| r.item).collect();

        // Step 7: promote an *inferred* mode's kinds to the front, without
        // removing anything else. An explicit (exclusive) route was already
        // filtered down to exactly this mode's kinds in step 5 — promoting
        // again there would be a no-op at best and a bug-hiding no-op at
        // worst, which is why this is conditioned on `!exclusive`.
        if !routed.exclusive
            && routed.mode != Mode::All
            && let Some(kinds) = kinds_for_mode(routed.mode)
        {
            promote_kinds(&mut ranked_items, kinds);
        }

        // Step 8: concatenate the pinned tail after the ranked body.
        ranked_items.extend(tail);

        // Step 9: truncate to max_results.
        //
        // DECISION: truncation is plain — "concatenate, then truncate" with
        // nothing smarter. If the ranked body alone already reaches or
        // exceeds `max_results`, the pinned tail is squeezed out entirely
        // rather than the cap making room for it. No acceptance criterion
        // asks for reserved tail space, and this keeps the rule simple and
        // predictable: the cap is a hard ceiling on the whole list, not a
        // negotiation between its two halves. See
        // `tests::max_results_cap_squeezes_out_the_pinned_tail_when_the_ranked_body_alone_fills_it`.
        //
        // DIVERGENCE: the old extension's `combineRankedWithTail`
        // (`lib/searchResultsLayout.js`) does the opposite — it *reserves*
        // room for the tail by truncating the ranked body first, then
        // appending the tail (see `tests/search-results-layout.test.mjs`'s
        // "reserves space for tail rows within max results": 3 ranked + 2
        // tail capped at 3 yields 1 ranked + both tail rows). This slice
        // deliberately does not port that behavior — see the brief's
        // decision above — so a cap that the ranked body alone fills
        // squeezes the tail out here, where the JS would have squeezed the
        // ranked body instead.
        ranked_items.truncate(max_results);
        ranked_items
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hop_protocol::{Action, ActionId, ActionKind, ItemId};

    fn item(kind: Kind, id: &str, title: &str) -> Item {
        Item {
            id: ItemId(id.to_string()),
            kind,
            title: title.to_string(),
            subtitle: None,
            icon: None,
            actions: vec![Action {
                id: ActionId("open".into()),
                kind: ActionKind::Open,
                label: "Open".into(),
            }],
            default_action: ActionId("open".into()),
            copy_text: None,
            append_to_end: false,
            provider: "test".into(),
        }
    }

    fn pinned(kind: Kind, id: &str, title: &str) -> Item {
        Item {
            append_to_end: true,
            ..item(kind, id, title)
        }
    }

    // --- Named directly in the brief. ---

    #[test]
    fn exclusive_mode_filters_to_kind() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::Window, "window:1", "Firefox"),
            item(Kind::App, "app:firefox", "Firefox"),
        ];
        let out = pipeline.assemble("w fire", items, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Kind::Window);
    }

    // NOTE ON TEST DATA: the brief's illustrative example — "2+2" over a
    // calculator item and an app titled "2048" — doesn't survive contact
    // with the actual ranker built in M1.4. Nucleo's fuzzy matcher requires
    // every needle character to appear, in order, in the haystack (the same
    // property `rank.rs::tests::one_character_substitution_typo_is_not_recovered`
    // documents); "2048" contains no `+` at all, so `Ranker::rank` would
    // drop it as a non-match *before* step 7 ever ran — no promotion logic
    // could resurrect an item the ranker already filtered out, and that
    // would be true no matter how step 7 is written. Confirmed empirically
    // against `nucleo_matcher` directly: `Pattern::parse("2+2", ...)
    // .score(Utf32Str::new("2048", ...), ...)` returns `None`.
    //
    // This test keeps the exact mechanism the acceptance criterion is
    // actually about — promotion reorders without removing an item that
    // legitimately ranks — using an app title that does fuzzy-match the
    // term, so the app survives step 6 on its own merits and this test can
    // isolate what step 7 does.
    #[test]
    fn inferred_utility_pins_on_top_without_hiding_others() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            // App (weight 20) would rank above Calculator (weight 6) on
            // fuzzy score alone: both titles match "2+2" as a clean prefix,
            // so weight is what decides the unpromoted order.
            item(Kind::App, "app:puzzle", "2+2 Puzzle"),
            item(Kind::Calculator, "calc:2+2", "2+2 = 4"),
        ];
        let out = pipeline.assemble("2+2", items, 10);
        assert_eq!(
            out[0].kind,
            Kind::Calculator,
            "the inferred utility result must lead, even though App outweighs Calculator"
        );
        assert!(
            out.iter().any(|i| i.kind == Kind::App),
            "the audited fix: promoting the calculator result must not hide the app"
        );
        assert_eq!(out.len(), 2, "nothing should be dropped by the promotion");
    }

    #[test]
    fn append_to_end_items_come_last_regardless_of_score() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            pinned(Kind::WebSearch, "web:search", "Search the web for firefox"),
            item(Kind::App, "app:firefox", "Firefox"),
        ];
        let out = pipeline.assemble("firefox", items, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].kind,
            Kind::App,
            "the ranked app must come first even though WebSearch (25) outweighs App (20)"
        );
        assert_eq!(out[1].id, ItemId("web:search".into()));
    }

    #[test]
    fn alias_rewrite_changes_ranking_term() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"ff","type":"rewrite","target":{"query":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox"),
            item(Kind::App, "app:files", "Files"),
        ];
        let out = pipeline.assemble("ff", items, 10);
        assert_eq!(
            out.len(),
            1,
            "ranking must behave as if \"firefox\" had been typed"
        );
        assert_eq!(out[0].title, "Firefox");
    }

    #[test]
    fn learning_boost_applied_and_beaten_by_alias() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"winner"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        for _ in 0..10 {
            pipeline
                .learning
                .record_launch("fire", &ItemId("app:learned".into()));
        }
        let items = vec![
            item(Kind::App, "app:learned", "Fireplace"),
            item(Kind::App, "app:winner", "Fire Alarm"),
        ];

        // Sanity check: learning alone moves "app:learned" ahead of an
        // otherwise-equal competitor.
        let mut unaliased_pipeline = Pipeline::default();
        for _ in 0..10 {
            unaliased_pipeline
                .learning
                .record_launch("fire", &ItemId("app:learned".into()));
        }
        let sanity = unaliased_pipeline.assemble("fire", items.clone(), 10);
        assert_eq!(
            sanity[0].id,
            ItemId("app:learned".into()),
            "learning boost alone should move its item to the front"
        );

        // The competing alias boost (180) beats the learning boost (capped
        // at 85) on the other item.
        let out = pipeline.assemble("fire", items, 10);
        assert_eq!(
            out[0].id,
            ItemId("app:winner".into()),
            "an alias boost on a competing item must still win over learning"
        );
    }

    #[test]
    fn max_results_cap_counts_pinned_tail() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:a", "Alpha"),
            item(Kind::App, "app:b", "Bravo"),
            pinned(Kind::WebSearch, "web:search", "Search the web"),
        ];
        let out = pipeline.assemble("", items, 3);
        assert_eq!(
            out.len(),
            3,
            "cap of 3 over 2 ranked + 1 pinned yields 3, not 4"
        );
        assert_eq!(
            out[2].id,
            ItemId("web:search".into()),
            "the pinned item stays last"
        );
    }

    // --- Not named in the brief, but required by it. ---

    // What this test does and does not establish: with today's
    // one-kind-per-mode table (see `kinds_for_mode`), step 5's exclusive
    // filter always leaves `body` homogeneous — every survivor is already
    // the one kind the mode serves — so re-running `promote_kinds` on it in
    // step 7 would be a structural no-op regardless of the `!exclusive`
    // guard. Deleting that guard entirely would not make this test fail.
    // What this test *does* pin is the observable behavior of an explicit
    // prefix: exactly the mode's kind comes back, nothing else. The guard
    // itself is still correct to keep, because the mapping is not
    // guaranteed to stay one-kind-per-mode — if a mode is ever widened to
    // serve several kinds, an exclusive query's `body` would become
    // heterogeneous after step 5, and running step 7's promotion again on
    // it would then be observable (and wrong, since step 5 already ordered
    // it exactly as the user asked). `tests::promote_kinds_is_a_stable_reorder`
    // below pins the promotion helper's own behavior directly, independent
    // of whether any caller's guard is present.
    #[test]
    fn explicit_prefix_does_not_trigger_step_seven_promotion() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:terminal", "Terminal"),
            item(Kind::Window, "window:terminal", "Terminal"),
        ];
        let out = pipeline.assemble("w terminal", items, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Kind::Window);
    }

    // Direct coverage of `promote_kinds` itself, independent of `assemble`
    // and its `!exclusive` guard (see the comment on
    // `explicit_prefix_does_not_trigger_step_seven_promotion` above for why
    // that guard's absence is not currently detectable through `assemble`).
    #[test]
    fn promote_kinds_is_a_no_op_on_a_homogeneous_list() {
        let mut items = vec![
            item(Kind::Window, "window:1", "Alpha"),
            item(Kind::Window, "window:2", "Bravo"),
        ];
        let before: Vec<_> = items.iter().map(|i| i.id.clone()).collect();
        promote_kinds(&mut items, &[Kind::Window]);
        let after: Vec<_> = items.iter().map(|i| i.id.clone()).collect();
        assert_eq!(before, after, "nothing to promote, nothing to reorder");
    }

    #[test]
    fn promote_kinds_stably_reorders_a_heterogeneous_list() {
        let mut items = vec![
            item(Kind::File, "file:1", "Alpha"),
            item(Kind::Calculator, "calc:1", "Bravo"),
            item(Kind::App, "app:1", "Charlie"),
            item(Kind::Calculator, "calc:2", "Delta"),
            item(Kind::File, "file:2", "Echo"),
        ];
        promote_kinds(&mut items, &[Kind::Calculator]);
        let ids: Vec<_> = items.iter().map(|i| i.id.0.as_str()).collect();
        assert_eq!(
            ids,
            vec!["calc:1", "calc:2", "file:1", "app:1", "file:2"],
            "promoted kind leads, relative order preserved within both \
             groups, nothing dropped"
        );
    }

    #[test]
    fn mode_all_neither_filters_nor_promotes() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox"),
            item(Kind::File, "file:firefox", "firefox.txt"),
        ];
        let out = pipeline.assemble("firefox", items, 10);
        assert_eq!(out.len(), 2, "Mode::All must not filter anything out");
    }

    #[test]
    fn empty_term_returns_everything_ordered_by_weight_and_boost_with_tail_last() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::File, "file:a", "Alpha"),
            item(Kind::App, "app:b", "Bravo"),
            item(Kind::Window, "window:c", "Charlie"),
            pinned(Kind::WebSearch, "web:search", "Search the web"),
        ];
        let out = pipeline.assemble("", items, 10);
        let titles: Vec<_> = out.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Charlie", "Bravo", "Alpha", "Search the web"],
            "window > app > file by weight, with the pinned tail still last"
        );
    }

    // Deliberate choice, pinned per the brief: step 4 splits the tail off
    // *before* step 5's exclusive-mode filter runs, so the filter never
    // sees pinned items at all. A pinned item therefore survives an
    // exclusive filter unconditionally, even when its own kind doesn't
    // match the mode the user asked for.
    #[test]
    fn pinned_item_survives_exclusive_filter_regardless_of_kind() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::Window, "window:1", "Firefox"),
            pinned(Kind::WebSearch, "web:search", "Search the web for firefox"),
        ];
        let out = pipeline.assemble("w fire", items, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.last().unwrap().id,
            ItemId("web:search".into()),
            "the pinned WebSearch item survives a Windows-exclusive filter \
             because step 4 already removed it from consideration by step 5"
        );
    }

    // Pins the truncation decision documented above the `truncate` call in
    // `assemble`: a ranked body that alone reaches `max_results` squeezes
    // the pinned tail out entirely, rather than the cap reserving room for
    // it.
    #[test]
    fn max_results_cap_squeezes_out_the_pinned_tail_when_the_ranked_body_alone_fills_it() {
        let mut pipeline = Pipeline::default();
        let items = vec![
            item(Kind::App, "app:a", "Alpha"),
            item(Kind::App, "app:b", "Bravo"),
            pinned(Kind::WebSearch, "web:search", "Search the web"),
        ];
        let out = pipeline.assemble("", items, 2);
        assert_eq!(out.len(), 2);
        assert!(
            out.iter().all(|i| i.kind != Kind::WebSearch),
            "the pinned tail is squeezed out entirely once the ranked body alone fills the cap"
        );
    }
}
