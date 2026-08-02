//! The assembly function: the pure step that turns provider output into the
//! final, ordered, capped result list. This is where routing, aliases,
//! learning and ranking — each built in an earlier M1 slice — meet for the
//! first time.
//!
//! No disk reads, subprocess spawns, or network calls happen anywhere in
//! this module: [`Pipeline::assemble`] runs on every keystroke.
//!
//! It is also where an item's self-asserted `kind` and `provider` stop being
//! taken on trust. Items reach assembly as [`CheckedItems`] — built only by
//! [`CheckedItems::check`], from each producing provider's
//! [`ProviderOutput`] — so every item ranked here was vouched for by the
//! manifest of the provider that actually produced it, and the ones that
//! weren't come back as [`Rejection`]s.

use hop_protocol::{Item, ItemId, Kind};

use crate::aliases::Aliases;
use crate::learning::Learning;
use crate::provider::ProviderManifest;
use crate::rank::{Boosts, Ranker, Weights};
use crate::router::{Mode, RoutedQuery, route};

/// One provider's answer to one query, still attached to the manifest of the
/// provider that produced it.
///
/// An [`Item`] describes its own `kind` and its own `provider`, and nothing
/// downstream can tell a truthful self-description from a forged one on the
/// item alone. The association between a producer and what it produced is
/// known only at the moment a provider returns — a scheduler that flattens
/// every provider's items into one `Vec<Item>` destroys it, and no amount of
/// care further down can reconstruct it. So the association travels: this
/// type is what a scheduler hands to [`CheckedItems::check`], one value per
/// provider that answered.
///
/// The manifest is borrowed rather than owned because a manifest is a
/// provider's static self-description, held by the caller for the life of the
/// query; the checks only read `id` and `kinds` from it.
pub struct ProviderOutput<'a> {
    pub manifest: &'a ProviderManifest,
    pub items: Vec<Item>,
}

/// Which of the two manifest checks an item failed. See [`Rejection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedCheck {
    /// The item's `kind` is not among the producing provider's declared
    /// [`ProviderManifest::kinds`]. A provider declaring `kinds:
    /// [Calculator]` returning a `Kind::Window` item is the motivating abuse:
    /// the forged kind would have survived a `w `-exclusive filter and
    /// inherited Window's ranking weight.
    Kind,
    /// The item's `provider` string is not equal to the producing provider's
    /// [`ProviderManifest::id`]. The item claims to have come from somewhere
    /// it did not.
    Provenance,
}

/// One item assembly refused, and why.
///
/// Rejections are *returned as data* rather than logged, because this
/// codebase has no logging seam yet and [`Pipeline::assemble`] is pure — it
/// runs on every keystroke and may not perform side effects. Everything here
/// is owned, so a rejection outlives both the item it describes and the
/// borrow of the manifest that refused it: a future logging seam can move a
/// `Vec<Rejection>` off the query path and format it whenever it likes,
/// without this type having to change shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The rejected item's id.
    pub item_id: ItemId,
    /// The kind the rejected item claimed for itself.
    pub claimed_kind: Kind,
    /// The provider the rejected item claimed to come from — the forged
    /// value under [`FailedCheck::Provenance`].
    pub claimed_provider: String,
    /// The [`ProviderManifest::id`] of the provider that actually produced
    /// the item, which is what the claims above were checked against.
    pub producer_id: String,
    /// Which check failed. An item that fails both is reported once, against
    /// the kind check — see [`CheckedItems::check`].
    pub check: FailedCheck,
}

/// Items that have been checked against the manifest of the provider that
/// produced them, and the [`Rejection`]s from doing so.
///
/// ## Why this type exists at all
///
/// This is the only item collection [`Pipeline::assemble`] accepts, its
/// fields are private, and [`CheckedItems::check`] is its only constructor.
/// That shape is the enforcement: a caller cannot route unchecked items into
/// ranking, because there is no way to build the value that ranking's entry
/// point demands except by running the checks. A free function that returns
/// `Vec<Item>` — or public fields here — would leave the checks advisory, and
/// a caller could skip them by simply not calling, which is exactly the
/// failure mode this seam exists to remove. The compiler enforces it instead
/// of a reviewer noticing.
///
/// The rejections ride along inside the value rather than being handed back
/// separately, so that they arrive at assembly and leave in its return value:
/// what assembly refused is part of assembly's outcome, and splitting it off
/// would let a caller drop it on the floor without noticing.
pub struct CheckedItems {
    items: Vec<Item>,
    rejections: Vec<Rejection>,
}

impl CheckedItems {
    /// Runs both manifest checks over every provider's output, in the order
    /// the outputs were given, keeping each provider's items in the order
    /// that provider returned them.
    ///
    /// An item is kept only if its `kind` is one its producer declared, and
    /// its `provider` string equals its producer's manifest `id`. Anything
    /// else becomes a [`Rejection`] and never reaches boosts, dedupe,
    /// filtering or ranking.
    ///
    /// DECISION: an item that fails both checks is reported once, against
    /// [`FailedCheck::Kind`]. A rejection identifies an item that is already
    /// gone; enumerating every way in which it lied would make the rejection
    /// list a variable-length report of a single event, for no gain to the
    /// only consumer it has (a future logging seam that wants to say what was
    /// dropped and why).
    ///
    /// Note what this does *not* check: that the producing manifest itself is
    /// truthful. A provider that honestly declares `id: "evil"` and `kinds:
    /// [App]` can still return an item whose id collides with another
    /// provider's namespace. Boosts keyed on the bare id string are what make
    /// that collision worth something, and giving them a provider dimension
    /// is separate work.
    pub fn check(outputs: Vec<ProviderOutput<'_>>) -> Self {
        let mut items = Vec::new();
        let mut rejections = Vec::new();

        for output in outputs {
            for item in output.items {
                let failed = if !output.manifest.kinds.contains(&item.kind) {
                    Some(FailedCheck::Kind)
                } else if item.provider != output.manifest.id {
                    Some(FailedCheck::Provenance)
                } else {
                    None
                };

                match failed {
                    Some(check) => rejections.push(Rejection {
                        item_id: item.id,
                        claimed_kind: item.kind,
                        claimed_provider: item.provider,
                        producer_id: output.manifest.id.to_string(),
                        check,
                    }),
                    None => items.push(item),
                }
            }
        }

        CheckedItems { items, rejections }
    }

    /// The items that passed both checks, in the order [`CheckedItems::check`]
    /// received them.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The items that failed a check, in the order they were rejected.
    pub fn rejections(&self) -> &[Rejection] {
        &self.rejections
    }
}

/// What [`Pipeline::assemble`] returns: the ordered, capped item list, and
/// every [`Rejection`] the manifest checks produced for the same query.
pub struct Assembly {
    pub items: Vec<Item>,
    pub rejections: Vec<Rejection>,
}

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
    /// The items arrive as [`CheckedItems`], not as a `Vec<Item>`, and that
    /// is deliberate: an item's `kind` and `provider` are self-asserted, so
    /// every one of them has been checked against the manifest of the
    /// provider that actually produced it before this function can be called
    /// at all. See [`CheckedItems`] for why the constraint lives in the type
    /// rather than in a helper a caller could forget. The [`Rejection`]s that
    /// check produced come back out in [`Assembly::rejections`], including
    /// for `append_to_end` items — the pinned tail bypasses the exclusive
    /// filter (step 5), so an unchecked pinned item would be a hole straight
    /// through this.
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
        checked: CheckedItems,
        max_results: usize,
    ) -> Assembly {
        let CheckedItems {
            items: provider_items,
            rejections,
        } = checked;

        // Step 1: route.
        let routed = route(raw_query);

        // Step 2: apply aliases to the routed term.
        let alias_effect = self.aliases.apply(&routed.term);

        // Step 3: collect boosts — alias boosts plus a learning boost per
        // candidate item. Where both apply to the same item, they add.
        //
        // DECISION: the learning boost is keyed on `routed.term` — the
        // query after any prefix was stripped, but *before* the alias
        // rewrite above. An alias rewrite is a ranking substitution the user
        // never typed, so crediting it to learning would be recording a fact
        // that didn't happen. That distinction is the point here — not that
        // the term is the typed spelling, which it is not in every case:
        // routing canonicalizes an alias-matched timezone query, so
        // `sao paulo` and `SAO PAULO` share one learning key. See CONTEXT.md
        // on **Term**. This is a judgement call M2 may revisit once the daemon
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
        // deliberately does not port that behavior — the issue specifies
        // "concatenate, then truncate" — so a cap that the ranked body fills
        // squeezes the tail out here, where the JS would have squeezed the
        // ranked body instead.
        ranked_items.truncate(max_results);
        Assembly {
            items: ranked_items,
            rejections,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hop_protocol::{Action, ActionId, ActionKind, ItemId};
    use std::time::Duration;

    /// Every [`Kind`] there is. `test_manifest` vouches for all of them, so
    /// the ordering, filtering, promotion and truncation tests below can keep
    /// using items of whatever kind the behaviour under test needs without
    /// each one having to spell out a manifest.
    const ALL_KINDS: [Kind; 10] = [
        Kind::App,
        Kind::Window,
        Kind::File,
        Kind::Calculator,
        Kind::Currency,
        Kind::Timezone,
        Kind::Weather,
        Kind::Emoji,
        Kind::WebSearch,
        Kind::Action,
    ];

    fn manifest(id: &'static str, kinds: Vec<Kind>) -> ProviderManifest {
        ProviderManifest {
            id,
            kinds,
            modes: vec![Mode::All],
            min_term_len: 0,
            budget: Duration::from_millis(50),
        }
    }

    /// The manifest of the one provider the `item` helper's output claims to
    /// come from: id `test`, and every kind.
    fn test_manifest() -> ProviderManifest {
        manifest("test", ALL_KINDS.to_vec())
    }

    /// Checks well-behaved output from the single fake provider most tests
    /// here share, and asserts nothing was rejected — so a test written about
    /// ordering can never quietly turn into a test about rejection.
    fn checked(items: Vec<Item>) -> CheckedItems {
        let manifest = test_manifest();
        let checked = CheckedItems::check(vec![ProviderOutput {
            manifest: &manifest,
            items,
        }]);
        assert!(
            checked.rejections().is_empty(),
            "this helper is for well-behaved provider output only"
        );
        checked
    }

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
        let out = pipeline.assemble("w fire", checked(items), 10).items;
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
    // against `nucleo_matcher` directly: `Pattern::new("2+2", ...,
    // AtomKind::Fuzzy).score(Utf32Str::new("2048", ...), ...)` returns
    // `None`. (This comment said `Pattern::parse` until the ranker stopped
    // parsing its term as a query DSL — see the "matched literally" section
    // of `rank.rs`'s module docs. The conclusion is unchanged either way:
    // `+` is not one of the four sigils the two constructors disagree about,
    // so "2048" fails to match for the same reason under both.)
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
        let out = pipeline.assemble("2+2", checked(items), 10).items;
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
        let out = pipeline.assemble("firefox", checked(items), 10).items;
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
        let out = pipeline.assemble("ff", checked(items), 10).items;
        assert_eq!(
            out.len(),
            1,
            "ranking must behave as if \"firefox\" had been typed"
        );
        assert_eq!(out[0].title, "Firefox");
    }

    /// The second, non-interactive sink for the same bug the ranker fixes:
    /// step 6 substitutes `alias_effect.effective_term` into the query the
    /// ranker sees, so a rewrite target reaches the ranker as text the *user
    /// never typed* and cannot proofread. While the ranker parsed its term
    /// as a query DSL, an alias whose target began with `!` silently
    /// inverted matching — `nf` here would have returned every item except
    /// the ones matching "firefox", which is both wrong and impossible to
    /// diagnose from the alias config alone. The effective term is matched
    /// literally now, so the target means the eight characters it spells.
    #[test]
    fn an_alias_rewriting_to_a_leading_bang_does_not_invert_matching() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"nf","type":"rewrite","target":{"query":"!firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox"),
            item(Kind::App, "app:files", "Files"),
            item(Kind::Action, "action:bug", "!firefox crash note"),
        ];
        let out = pipeline.assemble("nf", checked(items), 10).items;
        let titles: Vec<_> = out.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["!firefox crash note"],
            "the rewrite target must match literally; inverted matching would \
             have returned \"Files\" — everything *but* the firefox items"
        );
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
        let sanity = unaliased_pipeline
            .assemble("fire", checked(items.clone()), 10)
            .items;
        assert_eq!(
            sanity[0].id,
            ItemId("app:learned".into()),
            "learning boost alone should move its item to the front"
        );

        // The competing alias boost (180) beats the learning boost (capped
        // at 85) on the other item.
        let out = pipeline.assemble("fire", checked(items), 10).items;
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
        let out = pipeline.assemble("", checked(items), 3).items;
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
        let out = pipeline.assemble("w terminal", checked(items), 10).items;
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
        let out = pipeline.assemble("firefox", checked(items), 10).items;
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
        let out = pipeline.assemble("", checked(items), 10).items;
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
        let out = pipeline.assemble("w fire", checked(items), 10).items;
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
        let out = pipeline.assemble("", checked(items), 2).items;
        assert_eq!(out.len(), 2);
        assert!(
            out.iter().all(|i| i.kind != Kind::WebSearch),
            "the pinned tail is squeezed out entirely once the ranked body alone fills the cap"
        );
    }

    // --- The two manifest checks, and the three abuses they close. ---

    /// Convenience for the tests below: the ids of the assembled items, which
    /// is what "never appears in the assembled output" is asserted against.
    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.id.0.as_str()).collect()
    }

    #[test]
    fn item_whose_kind_is_outside_its_producers_declared_kinds_is_rejected() {
        let mut pipeline = Pipeline::default();
        let calculator = manifest("calc", vec![Kind::Calculator]);
        let items = vec![
            Item {
                provider: "calc".into(),
                ..item(Kind::Calculator, "calc:2+2", "2+2 = 4")
            },
            Item {
                provider: "calc".into(),
                ..item(Kind::Window, "window:1", "Firefox")
            },
        ];
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![ProviderOutput {
                manifest: &calculator,
                items,
            }]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["calc:2+2"],
            "a provider declaring kinds: [Calculator] cannot also emit a Window item"
        );
        assert_eq!(out.rejections.len(), 1);
        assert_eq!(out.rejections[0].check, FailedCheck::Kind);
    }

    #[test]
    fn item_whose_provider_string_does_not_match_its_producer_is_rejected() {
        let mut pipeline = Pipeline::default();
        let apps = manifest("apps", vec![Kind::App]);
        let items = vec![
            Item {
                provider: "apps".into(),
                ..item(Kind::App, "app:files", "Files")
            },
            Item {
                provider: "not-the-apps-provider".into(),
                ..item(Kind::App, "app:firefox", "Firefox")
            },
        ];
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![ProviderOutput {
                manifest: &apps,
                items,
            }]),
            10,
        );
        assert_eq!(ids(&out.items), vec!["app:files"]);
        assert_eq!(out.rejections.len(), 1);
        assert_eq!(out.rejections[0].check, FailedCheck::Provenance);
    }

    /// Abuse 1 — boost theft. The alias `fire` boosts item id `app:firefox`
    /// by [`crate::aliases::ALIAS_BOOST`], far more than any fuzzy score
    /// separates these two titles by, so an impostor carrying that id leads
    /// the list if it survives at all.
    #[test]
    fn a_rejected_item_collects_no_boost() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let apps = manifest("apps", vec![Kind::App]);
        let evil = manifest("evil", vec![Kind::App]);
        let out = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                ProviderOutput {
                    manifest: &apps,
                    items: vec![Item {
                        provider: "apps".into(),
                        ..item(Kind::App, "app:fireplace", "Fireplace")
                    }],
                },
                ProviderOutput {
                    manifest: &evil,
                    // Produced by `evil`, but claiming to be the apps
                    // provider's work — the forged item from the issue.
                    items: vec![Item {
                        provider: "apps".into(),
                        ..item(Kind::App, "app:firefox", "Firefox Impostor")
                    }],
                },
            ]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["app:fireplace"],
            "the impostor must not appear at all, let alone lead on an alias \
             boost keyed to the id it forged"
        );
    }

    /// Abuse 2 — eviction. `Ranker::rank` dedupes apps on **title alone**
    /// (see `rank::tests::duplicate_apps_deduped_by_title`), keeping the
    /// best-scoring occurrence, so an impostor sharing the genuine Firefox's
    /// title and outscoring it on a learning boost silently deletes the
    /// genuine item from the list.
    #[test]
    fn a_rejected_item_cannot_evict_a_genuine_item_through_dedupe() {
        let mut pipeline = Pipeline::default();
        for _ in 0..10 {
            pipeline
                .learning
                .record_launch("firefox", &ItemId("app:evil".into()));
        }
        let apps = manifest("apps", vec![Kind::App]);
        let evil = manifest("evil", vec![Kind::App]);
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                ProviderOutput {
                    manifest: &apps,
                    items: vec![Item {
                        provider: "apps".into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    }],
                },
                ProviderOutput {
                    manifest: &evil,
                    items: vec![Item {
                        provider: "apps".into(),
                        ..item(Kind::App, "app:evil", "Firefox")
                    }],
                },
            ]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["app:firefox"],
            "the genuine item must survive: the impostor it shares a title \
             with was rejected before dedupe could prefer the impostor"
        );
    }

    /// Abuse 3 — exclusive-mode bypass. A provider declaring `kinds:
    /// [Calculator]` returns a `Kind::Window` item, which without the kind
    /// check passes step 5's `w `-exclusive filter and inherits Window's
    /// ranking weight.
    #[test]
    fn a_rejected_item_cannot_survive_an_exclusive_mode_filter() {
        let mut pipeline = Pipeline::default();
        let windows = manifest("windows", vec![Kind::Window]);
        let calculator = manifest("calc", vec![Kind::Calculator]);
        let out = pipeline.assemble(
            "w fire",
            CheckedItems::check(vec![
                ProviderOutput {
                    manifest: &windows,
                    items: vec![Item {
                        provider: "windows".into(),
                        ..item(Kind::Window, "window:1", "Firefox")
                    }],
                },
                ProviderOutput {
                    manifest: &calculator,
                    items: vec![Item {
                        provider: "calc".into(),
                        ..item(Kind::Window, "window:evil", "Firefox Impostor")
                    }],
                },
            ]),
            10,
        );
        assert_eq!(
            ids(&out.items),
            vec!["window:1"],
            "only the provider that declared Kind::Window may answer a \
             Windows-exclusive query"
        );
        assert_eq!(out.rejections[0].check, FailedCheck::Kind);
    }

    /// The pinned tail is split off before step 5 and never ranked, so it is
    /// the one path into the output that no later step filters — an unchecked
    /// pinned item would be a hole straight through this work. The query here
    /// is `w `-exclusive precisely because that is the filter a pinned item
    /// legitimately bypasses.
    #[test]
    fn a_rejected_append_to_end_item_is_rejected_too() {
        let mut pipeline = Pipeline::default();
        let web = manifest("web", vec![Kind::WebSearch]);
        let evil = manifest("evil", vec![Kind::WebSearch]);
        let out = pipeline.assemble(
            "w fire",
            CheckedItems::check(vec![
                ProviderOutput {
                    manifest: &web,
                    items: vec![Item {
                        provider: "web".into(),
                        ..pinned(Kind::WebSearch, "web:search", "Search the web for firefox")
                    }],
                },
                ProviderOutput {
                    manifest: &evil,
                    items: vec![Item {
                        provider: "web".into(),
                        ..pinned(Kind::WebSearch, "web:evil", "Search the web, evilly")
                    }],
                },
            ]),
            10,
        );
        assert_eq!(ids(&out.items), vec!["web:search"]);
        assert_eq!(out.rejections.len(), 1);
        assert_eq!(out.rejections[0].item_id, ItemId("web:evil".into()));
        assert_eq!(out.rejections[0].check, FailedCheck::Provenance);
    }

    /// The whole rejection record, field by field — this is what a future
    /// logging seam gets to work with. The item here fails *both* checks
    /// (wrong kind for its producer, and a forged provider string), which
    /// pins the DECISION on [`CheckedItems::check`]: one rejection per
    /// rejected item, reported against the kind check.
    #[test]
    fn a_rejection_names_the_item_the_claim_the_producer_and_the_failed_check() {
        let mut pipeline = Pipeline::default();
        let evil = manifest("evil", vec![Kind::Calculator]);
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![ProviderOutput {
                manifest: &evil,
                items: vec![Item {
                    provider: "apps".into(),
                    ..item(Kind::Window, "app:firefox", "Firefox")
                }],
            }]),
            10,
        );
        assert!(out.items.is_empty());
        assert_eq!(
            out.rejections,
            vec![Rejection {
                item_id: ItemId("app:firefox".into()),
                claimed_kind: Kind::Window,
                claimed_provider: "apps".into(),
                producer_id: "evil".into(),
                check: FailedCheck::Kind,
            }]
        );
    }

    /// Each item is checked against *its own* producer's manifest, not
    /// against the union of every manifest that answered: both items below
    /// are well-behaved, and each would be rejected by the other's producer.
    #[test]
    fn each_providers_items_are_checked_against_that_providers_own_manifest() {
        let apps = manifest("apps", vec![Kind::App]);
        let calculator = manifest("calc", vec![Kind::Calculator]);
        let checked = CheckedItems::check(vec![
            ProviderOutput {
                manifest: &apps,
                items: vec![Item {
                    provider: "apps".into(),
                    ..item(Kind::App, "app:firefox", "Firefox")
                }],
            },
            ProviderOutput {
                manifest: &calculator,
                items: vec![Item {
                    provider: "calc".into(),
                    ..item(Kind::Calculator, "calc:2+2", "2+2 = 4")
                }],
            },
        ]);
        assert!(checked.rejections().is_empty());
        assert_eq!(
            ids(checked.items()),
            vec!["app:firefox", "calc:2+2"],
            "checking must not reorder what the providers returned"
        );
    }
}
