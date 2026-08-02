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
use crate::provider::{Provider, ProviderManifest};
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
/// ## Why the manifest cannot be supplied by the caller
///
/// Both fields are private and [`ProviderOutput::from_provider`] is the only
/// constructor, because a manifest a caller can name is a manifest a forged
/// item can select. The failure that shape invites is not hypothetical: a
/// scheduler holding a flat `Vec<Item>` would naturally group it for checking
/// by reading each item's own `provider` string and looking the matching
/// manifest up by that id — at which point both checks are tautologies. The
/// provenance check would compare a claimed id against a manifest chosen *by*
/// that claimed id, and the kind check would run against the impersonated
/// provider's declared kinds. Every abuse in issue #31 would be back, with
/// the checks still nominally in place.
///
/// Taking the dispatched [`Provider`] itself removes the string from the
/// path: the manifest comes from [`Provider::manifest`] on the object that
/// was asked, so nothing an item says about itself can influence which
/// manifest it is checked against. The one freedom left to a caller is which
/// provider object it hands over alongside which items, and that is a pairing
/// made where the provider is in hand — not something derivable from item
/// data, and not something `dyn Provider` can launder either, since
/// [`Provider`]'s RPITIT methods make it dyn-incompatible by construction.
///
/// The manifest is owned rather than borrowed because [`Provider::manifest`]
/// returns a fresh value. That is two small allocations per provider per
/// query — `ProviderManifest`'s clone copies both its `kinds` and `modes`
/// `Vec`s — on a path that then fuzzy-matches every item that provider
/// returned.
#[derive(Debug)]
pub struct ProviderOutput {
    manifest: ProviderManifest,
    items: Vec<Item>,
}

impl ProviderOutput {
    /// Pairs `items` with the manifest of the provider that produced them,
    /// asking `provider` for that manifest directly. See the type's docs for
    /// why this is the only way to build one.
    ///
    /// `items` is what this provider's own [`Provider::query`] returned;
    /// dispatching providers, honouring their budgets and collecting their
    /// answers is M2 daemon work that happens upstream of this crate.
    ///
    /// ## When the manifest is read, and what that costs
    ///
    /// Now — *after* `query` has already returned. This call is the only
    /// [`Provider::manifest`] call anywhere on this crate's path, so the
    /// manifest an item is checked against is whatever the provider chooses
    /// to answer with at check time, and nothing here can tell that apart
    /// from the manifest the same provider gave at registration. That the two
    /// agree is [`Provider::manifest`]'s documented stability requirement —
    /// a contract this constructor rests on and does not enforce. Read that
    /// method's docs for the abuse a provider that ignores it recovers.
    ///
    /// It cannot be enforced from here: `hop-core` has no registry and no
    /// scheduler, so there is no earlier, trusted manifest in this crate to
    /// compare against. A host that keeps one is in a strictly stronger
    /// position — a manifest captured once at registration cannot be
    /// re-minted in response to what a provider decided to return — and such
    /// a host should compare its captured manifest against
    /// [`Provider::manifest`] and refuse the provider on any mismatch. What
    /// it must not do is hand the captured manifest to this crate to be
    /// checked against: a constructor taking a caller-supplied manifest is
    /// the hole the section above exists to keep closed, and it does not stop
    /// being that hole because this particular caller would have passed a
    /// trustworthy value.
    pub fn from_provider<P: Provider>(provider: &P, items: Vec<Item>) -> Self {
        ProviderOutput {
            manifest: provider.manifest(),
            items,
        }
    }
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
/// That shape is the enforcement: unchecked items cannot travel the assembly
/// path, because there is no way to build the value `assemble` demands except
/// by running the checks. A free function that returns `Vec<Item>` — or
/// public fields here — would leave the checks advisory, and a caller could
/// skip them by simply not calling, which is exactly the failure mode this
/// seam exists to remove. The compiler enforces it instead of a reviewer
/// noticing.
///
/// The guarantee is scoped to that seam, and deliberately not claimed for
/// scoring in general: [`Ranker::rank`] is public, takes a bare `Vec<Item>`,
/// and [`Pipeline::ranker`] is a public field, so `pipeline.ranker.rank(…)`
/// still reaches the fuzzy matcher and the title-dedupe with items no
/// manifest vouched for. What this type guarantees is that *assembly* — the
/// nine-step contract the daemon calls per query, where boosts, the exclusive
/// filter and the pinned tail all live — has no unchecked entrance.
///
/// The rejections ride along inside the value, and come back out in
/// [`Assembly`], rather than being handed back from `check` separately: what
/// assembly refused belongs to the query it refused them for, so one call
/// yields one outcome. It is worth being precise about what that does *not*
/// buy, since it would be easy to read as more: nothing obliges a caller to
/// look at them. [`Assembly`]'s fields are public and `.items` discards the
/// rejections in one character, which is exactly what the tests below do.
/// Until there is a logging seam (issue #34) that makes ignoring them a real
/// mistake, this shape keeps rejections available and attached to their
/// query — it does not make them unignorable.
#[derive(Debug)]
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
    /// provider's namespace. *Alias* boosts got that provider dimension in
    /// this branch (`Boosts::by_provider_item`, tagged via
    /// `AliasEffect::boosts`); learning boosts deliberately did not — see the
    /// DECISION at the learning-boost call site in `Pipeline::assemble`, and
    /// issue #72.
    pub fn check(outputs: Vec<ProviderOutput>) -> Self {
        let mut items = Vec::new();
        let mut rejections = Vec::new();

        for output in outputs {
            // Each item is checked against `output.manifest` and nothing
            // else. Hoisting the declared kinds or the ids out of this loop —
            // into one set spanning every provider that answered — would look
            // like a harmless optimisation and would silently restore both
            // abuses: any answering provider's kind would vouch for any item,
            // and any answering provider's id would satisfy provenance. See
            // `tests::an_item_is_checked_against_its_own_producer_not_the_union_of_every_manifest`.
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
#[derive(Debug)]
pub struct Assembly {
    /// The final result list: the ranked body followed by the pinned tail,
    /// truncated to the `max_results` the call asked for.
    pub items: Vec<Item>,
    /// Every item the manifest checks refused for this query, in the order
    /// [`CheckedItems::check`] rejected them. Empty when every provider was
    /// honest about its own output. Nothing obliges a caller to read this —
    /// see [`CheckedItems`] on what that does and does not buy.
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
        for ((provider, id), boost) in &alias_effect.boosts {
            *boosts
                .by_provider_item
                .entry((provider.clone(), id.clone()))
                .or_insert(0.0) += *boost;
        }
        // DECISION: the learning boost stays keyed on the bare item id, with
        // no provider dimension, unlike the alias boost above. Issue #31's
        // boost-theft criterion is only *partially* met here on purpose —
        // `Learning::boost_for` sums `frequency_boost` (from the persisted
        // `global_frequency` map) and `query_boost` (from the per-query
        // `selections` map, kept in memory only, never written to disk), and
        // both are keyed on the bare id string. Giving `global_frequency` a
        // provider dimension is a persisted-format migration (version bump,
        // load-path migration) on the same load path issues #37/#38 already
        // target, not an in-memory rekey like `Boosts::by_provider_item`
        // above; `selections` is deferred alongside it rather than resolved
        // on its own. Filed as issue #72.
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
    use crate::provider::{APPS_PROVIDER_ID, ProviderError, QueryCtx};
    use hop_protocol::{Action, ActionId, ActionKind, ExecOutcome, ItemId};
    use std::time::Duration;

    /// Every [`Kind`] there is. The `test` provider below declares all of
    /// them, so the ordering, filtering, promotion and truncation tests can
    /// keep using items of whatever kind the behaviour under test needs
    /// without each one having to stand up a provider of its own.
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

    /// A provider that exists only to be a provider: [`ProviderOutput`] can
    /// be built no other way, so a test that wants to pair items with a
    /// manifest has to have something implementing [`Provider`] to ask. Its
    /// `query` is never called — assembly's input is items a provider has
    /// *already* returned, and these tests hand-write those items so they can
    /// forge the claims the checks are about.
    struct FakeProvider {
        manifest: ProviderManifest,
    }

    impl Provider for FakeProvider {
        fn manifest(&self) -> ProviderManifest {
            self.manifest.clone()
        }

        async fn query(
            &self,
            _q: &RoutedQuery,
            _ctx: &QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Ok(Vec::new())
        }

        async fn execute(
            &self,
            _item_id: &ItemId,
            _action_id: &ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    fn provider(id: &'static str, kinds: Vec<Kind>) -> FakeProvider {
        FakeProvider {
            manifest: ProviderManifest {
                id,
                kinds,
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(50),
            },
        }
    }

    /// One provider's answer: the items `id` claims to have produced, paired
    /// with `id`'s own manifest the only way [`ProviderOutput`] allows.
    fn output(id: &'static str, kinds: Vec<Kind>, items: Vec<Item>) -> ProviderOutput {
        ProviderOutput::from_provider(&provider(id, kinds), items)
    }

    /// Checks well-behaved output from the single fake provider most tests
    /// here share, and asserts nothing was rejected — so a test written about
    /// ordering can never quietly turn into a test about rejection.
    fn checked(items: Vec<Item>) -> CheckedItems {
        let checked = CheckedItems::check(vec![output("test", ALL_KINDS.to_vec(), items)]);
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

        // The competing `ALIAS_BOOST` beats the learning boost (capped at
        // `LEARNING_BOOST_CAP`) on the other item. The alias targets
        // `app:winner`, which the `"fire" -> {"appId":"winner"}` alias means
        // as the apps provider's item — so, unlike the sanity check above,
        // this item must actually come from that provider for the boost to
        // land.
        let assembly = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    "test",
                    ALL_KINDS.to_vec(),
                    vec![item(Kind::App, "app:learned", "Fireplace")],
                ),
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:winner", "Fire Alarm")
                    }],
                ),
            ]),
            10,
        );
        // Restores the guard `checked()` gives every other test in this
        // file for free: without it, this is an ordering test that could
        // quietly become a rejection test instead (e.g. if a future change
        // to the manifest/provider wiring above started rejecting
        // "app:winner", the assertion below on a *shorter* `out` could still
        // find `out[0]` equal to itself trivially wrong in a way this guard
        // catches immediately).
        assert!(
            assembly.rejections.is_empty(),
            "both providers here are self-consistent; neither should be rejected"
        );
        assert_eq!(
            assembly.items[0].id,
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
            CheckedItems::check(vec![output("calc", vec![Kind::Calculator], items)]),
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
        let items = vec![
            Item {
                provider: APPS_PROVIDER_ID.into(),
                ..item(Kind::App, "app:files", "Files")
            },
            Item {
                provider: "not-the-apps-provider".into(),
                ..item(Kind::App, "app:firefox", "Firefox")
            },
        ];
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![output(APPS_PROVIDER_ID, vec![Kind::App], items)]),
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
        let out = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:fireplace", "Fireplace")
                    }],
                ),
                // Produced by `evil`, but claiming to be the apps provider's
                // work — the forged item from the issue.
                output(
                    "evil",
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox Impostor")
                    }],
                ),
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

    // --- Task 2: alias boosts scoped to their target provider. ---
    //
    // A gap the two manifest checks above cannot close on their own: a
    // provider that declares itself *honestly* — `id: "evil"`, its own
    // `kinds` — can still emit an item whose id collides with the apps
    // provider's namespace an `AppBoost` alias targets. That item passes
    // both manifest checks cleanly (its `provider` field agrees with its
    // own producer), so it survives into `CheckedItems::items()` right
    // alongside the genuine apps-provider item sharing its id. Only
    // `Boosts::by_provider_item`'s provider dimension (via
    // `AliasEffect::boosts` tagging every `AppBoost` with
    // [`APPS_PROVIDER_ID`]) tells the two apart at scoring time.

    /// The acceptance case this scoping exists for: an alias boost
    /// configured for the apps provider must not land on an identically-id'd
    /// item a different, honestly self-declared provider produced.
    #[test]
    fn alias_boost_does_not_land_on_an_identically_id_item_from_a_different_provider() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        let out = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    }],
                ),
                // Honestly declares itself as a Window provider — no
                // impersonation, so this item passes both manifest checks —
                // but happens to reuse the id "app:firefox" the alias above
                // targets.
                output(
                    "windows",
                    vec![Kind::Window],
                    vec![Item {
                        provider: "windows".into(),
                        ..item(Kind::Window, "app:firefox", "Firefox")
                    }],
                ),
            ]),
            10,
        );
        assert!(
            out.rejections.is_empty(),
            "both providers are honest about their own output; neither should be rejected"
        );
        assert_eq!(
            out.items[0].kind,
            Kind::App,
            "without the fix, the boost keyed only to the id would also lift \
             the Window item — weight 30 to App's 20 — and it would stay on \
             top despite not being who the alias actually targets"
        );
    }

    /// The other half: the fix must not stop the boost from landing on the
    /// item it is actually for. Same shape as
    /// `rank::tests::boost_applies_to_the_right_item`, run through the full
    /// pipeline with the apps provider now spelled out explicitly, so the
    /// resulting order is provably unchanged from before this change scoped
    /// the boost to a provider.
    #[test]
    fn alias_boost_still_lands_on_the_genuine_apps_item_same_order_as_before() {
        let mut pipeline = Pipeline {
            aliases: Aliases::from_json(
                r#"[{"alias":"fire","type":"app","target":{"appId":"firefox"}}]"#,
            )
            .unwrap(),
            ..Default::default()
        };
        // Without the boost, Window (weight 30) would outrank App (weight
        // 20) on this tie — `ALIAS_BOOST` must still flip it.
        let assembly = pipeline.assemble(
            "fire",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    }],
                ),
                output(
                    "windows",
                    vec![Kind::Window],
                    vec![Item {
                        provider: "windows".into(),
                        ..item(Kind::Window, "window:1", "Firefox")
                    }],
                ),
            ]),
            10,
        );
        assert!(
            assembly.rejections.is_empty(),
            "both providers are self-consistent; neither should be rejected"
        );
        // The full order, not just who's first: an assertion on `out[0]`
        // alone would still pass if the Window item vanished entirely
        // (dropped by a future exclusive-filter change, a CheckedItems
        // regression, ...) without the alias boost ever applying — proving
        // nothing about the boost. Asserting both positions is what actually
        // pins "the same resulting order as before this change".
        assert_eq!(
            ids(&assembly.items),
            vec!["app:firefox", "window:1"],
            "the genuine apps-provider item still receives its alias boost \
             and outranks the higher-weighted Window item, exactly as it did \
             before the boost was scoped to a provider"
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
        let out = pipeline.assemble(
            "firefox",
            CheckedItems::check(vec![
                output(
                    APPS_PROVIDER_ID,
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    }],
                ),
                output(
                    "evil",
                    vec![Kind::App],
                    vec![Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:evil", "Firefox")
                    }],
                ),
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
        let out = pipeline.assemble(
            "w fire",
            CheckedItems::check(vec![
                output(
                    "windows",
                    vec![Kind::Window],
                    vec![Item {
                        provider: "windows".into(),
                        ..item(Kind::Window, "window:1", "Firefox")
                    }],
                ),
                output(
                    "calc",
                    vec![Kind::Calculator],
                    vec![Item {
                        provider: "calc".into(),
                        ..item(Kind::Window, "window:evil", "Firefox Impostor")
                    }],
                ),
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
        let out = pipeline.assemble(
            "w fire",
            CheckedItems::check(vec![
                output(
                    "web",
                    vec![Kind::WebSearch],
                    vec![Item {
                        provider: "web".into(),
                        ..pinned(Kind::WebSearch, "web:search", "Search the web for firefox")
                    }],
                ),
                output(
                    "evil",
                    vec![Kind::WebSearch],
                    vec![Item {
                        provider: "web".into(),
                        ..pinned(Kind::WebSearch, "web:evil", "Search the web, evilly")
                    }],
                ),
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
        let out = pipeline.assemble(
            "",
            CheckedItems::check(vec![output(
                "evil",
                vec![Kind::Calculator],
                vec![Item {
                    provider: APPS_PROVIDER_ID.into(),
                    ..item(Kind::Window, "app:firefox", "Firefox")
                }],
            )]),
            10,
        );
        assert!(out.items.is_empty());
        assert_eq!(
            out.rejections,
            vec![Rejection {
                item_id: ItemId("app:firefox".into()),
                claimed_kind: Kind::Window,
                claimed_provider: APPS_PROVIDER_ID.into(),
                producer_id: "evil".into(),
                check: FailedCheck::Kind,
            }]
        );
    }

    /// Each item is checked against *its own* producer's manifest, never
    /// against the union of every manifest that answered. Both impostors here
    /// are well-behaved by the union's standards and rejected by their own
    /// producer's: `apps` emits a Calculator item, a kind `calc` (also
    /// answering) declares; `calc` emits an item claiming provider `apps`, an
    /// id `apps` (also answering) really has. An implementation that hoisted
    /// the declared kinds or the ids into one set spanning `outputs` — an
    /// easy thing to reach for with many providers — would let both through
    /// while keeping every other test in this module green.
    #[test]
    fn an_item_is_checked_against_its_own_producer_not_the_union_of_every_manifest() {
        let checked = CheckedItems::check(vec![
            output(
                APPS_PROVIDER_ID,
                vec![Kind::App],
                vec![
                    Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::App, "app:firefox", "Firefox")
                    },
                    Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::Calculator, "calc:evil", "2+2 = 5")
                    },
                ],
            ),
            output(
                "calc",
                vec![Kind::Calculator],
                vec![
                    Item {
                        provider: "calc".into(),
                        ..item(Kind::Calculator, "calc:2+2", "2+2 = 4")
                    },
                    Item {
                        provider: APPS_PROVIDER_ID.into(),
                        ..item(Kind::Calculator, "calc:impostor", "2+2 = 6")
                    },
                ],
            ),
        ]);

        assert_eq!(
            ids(checked.items()),
            vec!["app:firefox", "calc:2+2"],
            "only each provider's own honest items survive, in the order the \
             providers returned them"
        );
        assert_eq!(
            checked.rejections(),
            vec![
                Rejection {
                    item_id: ItemId("calc:evil".into()),
                    claimed_kind: Kind::Calculator,
                    claimed_provider: APPS_PROVIDER_ID.into(),
                    producer_id: APPS_PROVIDER_ID.into(),
                    check: FailedCheck::Kind,
                },
                Rejection {
                    item_id: ItemId("calc:impostor".into()),
                    claimed_kind: Kind::Calculator,
                    claimed_provider: APPS_PROVIDER_ID.into(),
                    producer_id: "calc".into(),
                    check: FailedCheck::Provenance,
                },
            ],
            "a kind another answering provider declares does not vouch for \
             this one's item, and neither does another answering provider's id"
        );
    }

    /// The association is only worth anything if it is the *right* manifest:
    /// [`ProviderOutput::from_provider`] must take it from the provider it is
    /// handed, not from anywhere the caller could substitute. Pairing the
    /// same items with a different provider rejects every one of them, which
    /// is what makes the pairing load-bearing rather than decorative.
    #[test]
    fn from_provider_takes_the_manifest_from_the_provider_it_is_given() {
        let items = vec![Item {
            provider: APPS_PROVIDER_ID.into(),
            ..item(Kind::App, "app:firefox", "Firefox")
        }];

        let own = CheckedItems::check(vec![ProviderOutput::from_provider(
            &provider(APPS_PROVIDER_ID, vec![Kind::App]),
            items.clone(),
        )]);
        assert!(own.rejections().is_empty());
        assert_eq!(ids(own.items()), vec!["app:firefox"]);

        let someone_elses = CheckedItems::check(vec![ProviderOutput::from_provider(
            &provider("windows", vec![Kind::App]),
            items,
        )]);
        assert!(someone_elses.items().is_empty());
        assert_eq!(
            someone_elses.rejections()[0].producer_id,
            "windows",
            "the manifest checked against is the one the given provider \
             describes itself with"
        );
        assert_eq!(someone_elses.rejections()[0].check, FailedCheck::Provenance);
    }
}
