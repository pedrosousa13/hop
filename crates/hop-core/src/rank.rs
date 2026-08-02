//! Typo-tolerant fuzzy ranking on top of `nucleo-matcher`.
//!
//! Replaces the previous GNOME extension's substring-only matcher, which
//! the audit called a fatal flaw: drop or transpose one character and the
//! result vanished entirely. `nucleo-matcher` matches subsequences (needle
//! characters must appear, in order, in the haystack, but gaps between them
//! are fine), so a dropped character no longer loses the match. It is
//! *not* an edit-distance matcher, though: it recovers dropped characters
//! reliably, but not substituted or transposed ones — see the
//! `// DIVERGENCE:` notes on
//! [`tests::one_character_substitution_typo_is_not_recovered`] and
//! [`tests::adjacent_transposition_typo_is_not_recovered`] for the cases
//! that distinction actually costs us. Word-boundary (acronym-style)
//! matching for short queries is a related gap — see the `// DIVERGENCE:`
//! note on
//! [`tests::word_boundary_does_not_yet_beat_scattered_for_short_acronym_queries`].
//!
//! ## The term is matched literally, not as a query DSL
//!
//! [`Ranker::rank`] builds its pattern with `Pattern::new(...,
//! AtomKind::Fuzzy)`, **not** `Pattern::parse`. The two differ in one
//! respect that matters a great deal here: `parse` reads `$`, `!`, `'` and
//! `^` at word boundaries as a query language — negation, substring, prefix,
//! postfix, exact — while `new` gives those four characters no special
//! meaning at all. Both split the term on unescaped whitespace into one atom
//! per word, so a multi-word query like `firefox workspace` still matches
//! word by word (see
//! [`tests::a_multi_word_term_still_matches_word_by_word`]); only the sigils
//! change.
//!
//! Parsing the term was wrong in both directions. A term of `^`, `'`, `!` or
//! `$` alone parsed to an atom with an empty needle, which `parse` discards,
//! leaving a pattern with no atoms — and a pattern with no atoms matches
//! *every* candidate, so a single stray character returned the entire result
//! set. (Why an empty atom list matches everything, and why this module now
//! guards against it directly, is set out in full on `Matching::for_term`.)
//! And `!firefox` did something worse than nothing: it inverted the query,
//! returning every item that does not contain "firefox".
//!
//! Nothing in this launcher's surface ever offered that DSL to users. It was
//! inherited implicitly from the library, along with an escaping obligation
//! nobody was discharging: not the router, which strips prefixes and hands
//! the rest through untouched, and least of all
//! [`crate::pipeline::Pipeline::assemble`], which substitutes an alias's
//! rewrite target into the term — text the user never typed and cannot
//! proofread. **That obligation is now gone rather than reassigned:** every
//! caller passes its term verbatim and gets literal matching; nobody has to
//! escape anything on the way in.
//!
//! If a query syntax is ever wanted, it should be an explicit, documented
//! decision at one named seam — a routed prefix, say, or a config flag that
//! selects `Pattern::parse` — so that opting in is visible at the place it
//! happens. What it must not be again is a default every caller inherits
//! silently, which is what made an alias config able to invert matching from
//! a file the user last edited months ago.
//!
//! One residual quirk, kept rather than papered over: `Pattern::new` still
//! honors `\` as nucleo's whitespace escape, so a term is not *perfectly*
//! literal. `\ ` (backslash-space) matches a literal space and joins the two
//! words into one atom instead of splitting them; and in a term containing
//! any non-ASCII character, nucleo 0.3.1 duplicates a backslash while
//! applying that escape, so `\é` looks for `\\é` and fails to match a
//! haystack that really does contain `\é`. Backslashes in search terms are
//! vanishingly rare next to `!` and `^`, and removing the last of this would
//! mean giving up whitespace-splitting (a single whole-term `Atom`, which
//! takes `escape_whitespace: false`) — a worse trade, and a behavior change
//! well beyond fixing the sigils. Noted here so it is a known cost rather
//! than a surprise.
//!
//! ## Score normalization
//!
//! `nucleo_matcher::pattern::Pattern::score` returns a raw `u32` on a scale
//! that has nothing to do with the old JS scorer's numbers, and isn't
//! comparable across needle lengths (a longer needle racks up more
//! `SCORE_MATCH`-per-character credit just by having more characters).
//! [`Ranker::rank`] normalizes it to **average score per matched
//! character** (`raw / needle.chars().count()`), which:
//!
//! - Removes the needle-length bias, so a 3-character and an 8-character
//!   query land on the same rough scale.
//! - Happens to land in roughly the same 0–30 range as the default weight
//!   table (see [`Weights::default`]) for realistic queries — which is
//!   exactly what the brief asks for: weight gaps of 6–30 can break ties
//!   between *comparably good* matches (see
//!   `tests::source_weight_breaks_ties`), while a boost of ~180 (used by
//!   aliases/learning in later slices) is roughly 6x the realistic
//!   per-character ceiling and so always outranks even a very strong fuzzy
//!   match (see `tests::boost_overrides_fuzzy_order`).
//!
//! `Ranker::new` builds `Matcher` on `Config::DEFAULT` rather than
//! `Config::DEFAULT.match_paths()`: items aren't filesystem paths (the
//! haystack is `title subtitle` free text), so the path-oriented delimiter
//! and boundary tuning `match_paths` applies would be the wrong bonuses to
//! reach for here.

use std::collections::{HashMap, HashSet};

use hop_protocol::{Item, ItemId, Kind};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::router::RoutedQuery;

/// Per-kind score weights, and the fuzzy-score floor a match must clear.
pub struct Weights {
    pub per_kind: HashMap<Kind, f32>,
    /// The minimum *fuzzy* score (post-normalization) a non-empty-term
    /// match must reach to survive. Never applied to empty-term queries —
    /// see [`Ranker::rank`].
    pub min_score: f32,
}

impl Default for Weights {
    /// Mirrors the previous extension's tuning (`lib/fuzzy.js`'s
    /// `sourceWeight` table): windows outrank everything, actions and web
    /// search sit just under them (the extension had a single `action`
    /// kind that its web-search provider produced; this vocabulary splits
    /// it in two, and neither should outrank windows), apps outrank files,
    /// and the "smart provider" kinds (emoji, calculator, currency,
    /// timezone, weather) trail behind.
    fn default() -> Self {
        let per_kind = HashMap::from([
            (Kind::Window, 30.0),
            (Kind::Action, 25.0),
            (Kind::WebSearch, 25.0),
            (Kind::App, 20.0),
            (Kind::File, 12.0),
            (Kind::Emoji, 8.0),
            (Kind::Calculator, 6.0),
            (Kind::Currency, 6.0),
            (Kind::Timezone, 6.0),
            (Kind::Weather, 6.0),
        ]);
        Weights {
            per_kind,
            min_score: 0.0,
        }
    }
}

/// Learned/aliased per-item score bumps. Empty by default. Two dimensions,
/// summed at lookup time in `Ranker::rank_matching`:
///
/// - `by_provider_item`: an **alias** boost, keyed by `(provider, ItemId)`
///   and applied only to the item whose own [`Item::provider`] equals that
///   key's provider. A bare `ItemId` key cannot express this: two items can
///   legitimately share an id while coming from two different, individually
///   honest providers — an id-namespace collision, not the impersonation
///   [`crate::pipeline::CheckedItems::check`] catches — and only one of them
///   is who the alias actually means. See
///   `tests::provider_scoped_boost_only_applies_to_the_matching_producer`.
///
///   **This is a documented boundary, not an enforced one, and it holds only
///   for one calling path.** `Item::provider` is self-asserted; matching
///   against it here is safe *only* when every item reaching this lookup
///   already passed [`crate::pipeline::CheckedItems::check`], which verifies
///   `item.provider` against the actual producer's own manifest before the
///   item can become part of a [`crate::pipeline::CheckedItems`]. That is
///   true for items reaching this struct through
///   [`crate::pipeline::Pipeline::assemble`] — the *only* path this
///   guarantee covers — and true for nothing else. [`Ranker::rank`] itself is
///   `pub`, takes a bare `Vec<Item>` with no such check, and
///   [`crate::pipeline::Pipeline::ranker`] is a public field: nothing stops
///   a caller from building a `Boosts` and calling
///   `pipeline.ranker.rank(raw_items, …, &boosts)` directly on items no
///   manifest ever vouched for, at which point `item.provider` is exactly as
///   trustworthy as it was before issue #31 — i.e. not at all — and boost
///   theft is back in full. If a future caller adds a second ranking path
///   (a preview pane, a re-rank, a benchmark harness) that skips
///   `CheckedItems`, it inherits that hole; this comment is the only thing
///   telling it so.
/// - `by_item_id`: a **learning** boost, applied to any item bearing this id
///   regardless of which provider produced it — the sum of
///   `Learning::frequency_boost` (backed by the persisted `global_frequency`
///   map) and `Learning::query_boost` (backed by the in-memory, per-query
///   `selections` map), both keyed on the bare id string.
///   DECISION: kept unscoped, deliberately. The maintainer's issue #31 scope
///   decision put the persisted learning store's id namespace out of scope
///   for this change — adding a provider dimension to `global_frequency` is
///   a persisted-format migration on the same load path issues #37/#38
///   already target, not an in-memory rekey; `selections` is deferred
///   alongside it rather than resolved on its own. Filed as issue #72; see
///   the comment at the call site in `Pipeline::assemble` where this field
///   is populated.
///
/// One further boundary neither dimension closes: `CheckedItems::check`
/// never requires that two answering providers declare *distinct*
/// `manifest.id`s. `by_provider_item`'s guarantee is really "the item came
/// from whichever provider declared this id", not "the item came from *the*
/// provider everyone means by that id" — see [`crate::provider::APPS_PROVIDER_ID`]'s
/// doc comment for what that costs if a provider registry ever allows two
/// providers to share an id.
#[derive(Default)]
pub struct Boosts {
    pub by_provider_item: HashMap<(String, ItemId), f32>,
    pub by_item_id: HashMap<ItemId, f32>,
}

/// An [`Item`] together with the final score it was ranked with.
pub struct Ranked {
    pub item: Item,
    pub score: f32,
}

/// Ranks items against a routed query. Owns the `nucleo_matcher::Matcher`,
/// whose scratch memory is expensive to allocate (~135KB) — build one
/// `Ranker` per long-lived caller and reuse it across queries, never per
/// keystroke.
pub struct Ranker {
    matcher: Matcher,
}

impl Default for Ranker {
    fn default() -> Self {
        Self::new()
    }
}

impl Ranker {
    /// Builds the underlying matcher once. See the module docs for why
    /// `Config::DEFAULT` (not `.match_paths()`) is the right starting
    /// point for this haystack shape.
    pub fn new() -> Self {
        Ranker {
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Ranks `items` against `query`, best-first. No disk reads, subprocess
    /// spawns, or network calls — this runs on every keystroke.
    ///
    /// - An empty `query.term` returns every item, scored by kind weight
    ///   and boost only — no fuzzy component, and `weights.min_score` is
    ///   **not** applied (mirrors the previous extension's `hasQuery`
    ///   guard).
    /// - A non-empty term fuzzy-matches each item's haystack (`title`, a
    ///   space, then `subtitle` if present, trimmed). An item that doesn't
    ///   match at all, or whose fuzzy component alone falls below
    ///   `weights.min_score`, is dropped — the threshold applies to the
    ///   fuzzy score, not the final total.
    /// - **The term is matched literally.** It is split on unescaped
    ///   whitespace into one atom per word, and that is the only
    ///   interpretation applied: `$`, `!`, `'` and `^` are ordinary
    ///   characters that must appear in the haystack like any others, not
    ///   nucleo's negation/substring/prefix/postfix syntax. Callers pass the
    ///   term verbatim and owe no escaping — including
    ///   [`crate::pipeline::Pipeline::assemble`], which passes an alias's
    ///   rewrite target through here as the effective term. See the module
    ///   docs for why opting into the DSL would have to be an explicit
    ///   decision at a seam, and for the one residual `\` quirk.
    /// - The "doesn't match at all is dropped" rule above holds for *every*
    ///   non-empty term, including one that yields no matchable atoms. It is
    ///   never weakened into "scores zero and survives `min_score`" — see
    ///   [`Matching::for_term`] for why that distinction needs enforcing
    ///   rather than coming for free.
    /// - Surviving items score `fuzzy + kind_weight + boost`, sorted
    ///   descending by score; ties break by kind weight descending, then
    ///   by title ascending.
    /// - `append_to_end` items are excluded entirely — a later slice pins
    ///   them after the ranked block instead of ranking them here.
    /// - Results are deduped after sorting, keeping the first (best-
    ///   scoring) occurrence: apps key on title alone; every other kind
    ///   keys on kind, id and title together. This split is deliberate and
    ///   security-relevant, not an oversight — see the doc comment on
    ///   `dedupe` for why apps drop id from the key, what that costs, and
    ///   what `CheckedItems::check` does and does not contain about it.
    /// - No result cap — callers that need one (the pipeline, in a later
    ///   slice) apply it themselves.
    pub fn rank(
        &mut self,
        items: Vec<Item>,
        query: &RoutedQuery,
        weights: &Weights,
        boosts: &Boosts,
    ) -> Vec<Ranked> {
        let matching = Matching::for_term(query.term.trim());
        self.rank_matching(&matching, items, weights, boosts)
    }

    /// The body of [`rank`](Ranker::rank), taking the already-classified
    /// [`Matching`] rather than a query. Split out so
    /// [`Matching::Nothing`] — which no term input currently produces, see
    /// [`Matching::for_term`] — is still reachable from a test.
    fn rank_matching(
        &mut self,
        matching: &Matching,
        items: Vec<Item>,
        weights: &Weights,
        boosts: &Boosts,
    ) -> Vec<Ranked> {
        let mut buf = Vec::new();
        let mut ranked: Vec<Ranked> = items
            .into_iter()
            .filter(|item| !item.append_to_end)
            .filter_map(|item| {
                let fuzzy = match matching {
                    // The zero-atom guard, and the whole reason this arm is
                    // written out rather than folded into `Everything` — see
                    // `Matching::for_term` for what it stops.
                    Matching::Nothing => return None,
                    Matching::Everything => 0.0,
                    Matching::Fuzzy {
                        pattern,
                        term_chars,
                    } => {
                        let haystack = haystack_of(&item);
                        let raw =
                            pattern.score(Utf32Str::new(&haystack, &mut buf), &mut self.matcher)?;
                        let normalized = raw as f32 / *term_chars as f32;
                        if normalized < weights.min_score {
                            return None;
                        }
                        normalized
                    }
                };

                let weight = kind_weight(weights, &item.kind);
                // `by_provider_item` is empty on virtually every keystroke —
                // `Aliases::apply` only ever populates it when the routed
                // term matches an alias key exactly. `HashMap::get` already
                // short-circuits on an empty map before hashing, but
                // building the lookup key clones two `String`s
                // (`item.provider` and the `ItemId`'s inner `String`), and
                // that allocation happens unconditionally as soon as the key
                // expression is evaluated — before `get` ever runs. Guarding
                // on `is_empty()` skips constructing the key at all on the
                // overwhelmingly common empty-map path, which is the only
                // per-item allocation this loop would otherwise do for an
                // empty query term (the `Matching::Everything` arm above
                // does no `haystack_of` allocation).
                let provider_boost = if boosts.by_provider_item.is_empty() {
                    0.0
                } else {
                    boosts
                        .by_provider_item
                        .get(&(item.provider.clone(), item.id.clone()))
                        .copied()
                        .unwrap_or(0.0)
                };
                let learning_boost = boosts.by_item_id.get(&item.id).copied().unwrap_or(0.0);
                let boost = provider_boost + learning_boost;
                Some(Ranked {
                    score: fuzzy + weight + boost,
                    item,
                })
            })
            .collect();

        ranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| {
                    kind_weight(weights, &b.item.kind)
                        .total_cmp(&kind_weight(weights, &a.item.kind))
                })
                .then_with(|| a.item.title.cmp(&b.item.title))
        });

        dedupe(ranked)
    }
}

/// What a query term means for matching, decided once per [`Ranker::rank`]
/// call rather than re-derived per candidate.
///
/// This is an enum rather than an `Option<Pattern>` because there are three
/// cases, not two, and the third one is easy to lose: a term can be empty
/// (match everything), can carry atoms to match against, or can be non-empty
/// yet carry *no* atoms — which must match nothing. Collapsing the first and
/// third into a single "no pattern" case is precisely the bug set out on
/// [`Matching::for_term`].
enum Matching {
    /// The term is empty. Every item passes with no fuzzy component, and
    /// `min_score` is deliberately not applied — see [`Ranker::rank`].
    Everything,
    /// A non-empty term with at least one atom. `term_chars` is the term's
    /// character count, which the raw nucleo score is divided by; see the
    /// module docs on score normalization.
    Fuzzy { pattern: Pattern, term_chars: usize },
    /// A non-empty term that yielded no atoms at all. Matches nothing.
    Nothing,
}

impl Matching {
    /// Classifies a **trimmed** term. This is where the zero-atom rationale
    /// the rest of the module points at lives, in full.
    ///
    /// A term whose pattern carries no atoms must be [`Matching::Nothing`],
    /// never a zero-scoring match. `Pattern::score` short-circuits to
    /// `Some(0)` when `atoms` is empty, and a normalized `0.0` clears the
    /// default `min_score` of `0.0` (`0.0 < 0.0` is false), so without that
    /// branch a non-empty term the user actually typed would return the
    /// entire candidate set — the failure mode that made a bare `^` match
    /// everything before this module stopped parsing its term as a DSL.
    ///
    /// No term currently reaches it. `Pattern::new` splits on unescaped
    /// whitespace and keeps every piece whose needle is non-empty; a trimmed
    /// non-empty term always has at least one non-empty piece, and
    /// `Atom::new`'s only rewrite (`\ ` becomes a space) cannot empty one.
    /// Brute-forcing every combination of up to four spaces, tabs,
    /// backslashes, newlines and assorted zero-width and combining
    /// characters produced no zero-atom term. That is a property of
    /// nucleo-matcher 0.3.1's internals, not a promise in its API, so the
    /// branch stays: this module's documented contract ("an item that
    /// doesn't match at all is dropped") should hold because this module
    /// enforces it, not because a dependency's atom construction happens to
    /// be shaped conveniently. Because no term reaches it, the guard is
    /// pinned instead through [`Ranker::rank_matching`], by
    /// [`tests::matching_nothing_drops_every_item`].
    fn for_term(term: &str) -> Matching {
        if term.is_empty() {
            return Matching::Everything;
        }
        let pattern = Pattern::new(
            term,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        if pattern.atoms.is_empty() {
            Matching::Nothing
        } else {
            Matching::Fuzzy {
                pattern,
                term_chars: term.chars().count(),
            }
        }
    }
}

fn kind_weight(weights: &Weights, kind: &Kind) -> f32 {
    weights.per_kind.get(kind).copied().unwrap_or(0.0)
}

/// `title`, then a space, then `subtitle` if present, trimmed — matches
/// the previous extension's `primaryText + ' ' + secondaryText`.
fn haystack_of(item: &Item) -> String {
    match &item.subtitle {
        Some(subtitle) => format!("{} {subtitle}", item.title).trim().to_string(),
        None => item.title.trim().to_string(),
    }
}

/// Dedupes an already-sorted `Vec<Ranked>`, keeping the first (best-
/// scoring) occurrence of each key. Apps key on title alone; every other
/// kind keys on kind, id and title together (the issue's wording, not the
/// JS's — the JS also folded `secondaryText` into the key, this doesn't).
///
/// DECISION: apps drop id from the key and everyone else keeps it, because a
/// title collision means opposite things in the two cases. Two `Window`s (or
/// `File`s, `Action`s, ...) that happen to share a title are almost always
/// genuinely different things a user might want to pick between — two
/// windows both called "Terminal" in two workspaces are two windows, not one
/// duplicated — so their distinct ids keep both in the result (see
/// `tests::non_app_kinds_with_same_title_and_different_ids_both_survive_dedupe`).
/// An app, by contrast, is identified for the user by its title, not by
/// whichever id its provider's index happened to assign it; nothing stops
/// two different ids from naming the same real application as far as the
/// user can see, and title is what the user identifies an app by (see
/// `tests::duplicate_apps_deduped_by_title`, which merges two `App` items
/// with different ids and an identical title on exactly that basis).
/// Dropping id from the app key is what makes that merge happen.
///
/// The cost: dedupe runs on an already best-first-sorted list and keeps only
/// the first match per key, so *any* two `App` items that share a title
/// collapse into whichever one sorted first — not just the honest
/// duplicates above. A higher-scoring item, for any reason, evicts a
/// lower-scoring, genuinely different `App` outright: not ranked below it,
/// not flagged, simply absent from the result with nothing left to show it
/// was ever there. This is issue #31's "eviction" abuse: a forged item
/// claiming `kind: App` and the real Firefox's title, boosted past the
/// genuine item's score by stolen boosts, used to delete the genuine
/// Firefox from the list this way.
///
/// [`crate::pipeline::CheckedItems::check`] narrows who can cause that; it
/// does not close it. An item reaching this function via
/// [`crate::pipeline::Pipeline::assemble`] is now guaranteed to be a
/// genuinely-declared `App` — its kind checked against its own producer's
/// manifest `kinds`, and its `provider` string checked against that
/// manifest's `id` — before it can evict anything (see
/// `tests::a_rejected_item_cannot_evict_a_genuine_item_through_dedupe` in
/// `pipeline.rs`). What that guarantee honestly does *not* cover: two
/// genuinely-declared `App` items from the same honest provider that happen
/// to share a title still collapse to one — that is this rule working as
/// intended, not a residual hole — and two genuinely-declared `App` items
/// sharing a title from two *different* honest providers collapse the same
/// way, with no requirement that the survivor be the genuine one: equal
/// scores tie-break to input order. That residual is not intended behaviour;
/// closing it means changing the dedupe rule, which #31 puts out of scope.
/// The guarantee also only holds on the `Pipeline::assemble` path.
/// `Ranker::rank` is `pub`, takes a bare `Vec<Item>`, and nothing stops a
/// caller from invoking it directly on items no manifest ever checked; on
/// that path this function dedupes exactly as trustingly as it did before
/// issue #31.
///
/// DECISION: this rule — apps by title alone, everything else by kind, id
/// and title — is deliberate and pinned by
/// `tests::duplicate_apps_deduped_by_title` and
/// `tests::non_app_kinds_with_same_title_and_different_ids_both_survive_dedupe`.
/// Changing it (folding id into the app key, say, to close the eviction gap
/// outright) is a separate decision with its own cost — it would stop
/// merging the honest duplicate-id case above — and is out of issue #31's
/// scope. Do not "fix" it here in passing.
fn dedupe(ranked: Vec<Ranked>) -> Vec<Ranked> {
    let mut seen = HashSet::new();
    ranked
        .into_iter()
        .filter(|r| seen.insert(dedupe_key(&r.item)))
        .collect()
}

fn dedupe_key(item: &Item) -> (Option<Kind>, Option<ItemId>, String) {
    if item.kind == Kind::App {
        (None, None, item.title.clone())
    } else {
        (
            Some(item.kind.clone()),
            Some(item.id.clone()),
            item.title.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::router::route;
    use hop_protocol::{Action, ActionId, ActionKind};

    /// Builds an `Item` with one default `Open` action, so test bodies
    /// stay readable.
    fn item(kind: Kind, id: &str, title: &str, subtitle: Option<&str>) -> Item {
        Item {
            id: ItemId(id.to_string()),
            kind,
            title: title.to_string(),
            subtitle: subtitle.map(str::to_string),
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

    // --- Ported from the previous extension's tests/fuzzy.test.mjs (16
    // cases). Most port directly; the ones that don't are marked
    // `// DIVERGENCE:` per the brief, with the reason inline.

    /// Ports "fuzzy scoring tolerates crome typo for Google Chrome" and
    /// doubles as the brief's `typo_tolerant_match` acceptance case: a
    /// dropped character still finds its target, and an unrelated
    /// candidate that doesn't contain the needle at all is dropped
    /// outright (not merely ranked lower).
    #[test]
    fn typo_tolerant_match() {
        let query = route("crome");
        let items = vec![
            item(Kind::App, "app:chrome", "Chrome", None),
            item(Kind::App, "app:files", "Files", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(
            ranked.len(),
            1,
            "\"crome\" has no valid subsequence alignment in \"Files\" at all"
        );
        assert_eq!(ranked[0].item.title, "Chrome");
    }

    // DIVERGENCE: nucleo is a strict left-to-right subsequence matcher —
    // every needle character must align, in order, to some haystack
    // character. A true adjacent-character transposition changes that
    // required order (needle position i now wants what haystack position
    // i+1 held, and vice versa), and unless both swapped letters happen to
    // reappear elsewhere in the right order, no alignment exists at all.
    // "termianl" (swapping "na" for "an" in "terminal") is exactly that:
    // it does not match "Terminal" even weakly — `None`, not a low score.
    //
    // Acceptance criterion #2 ("a query with a transposed or missing
    // character still finds its target") is only half true: the
    // missing-character half holds (`typo_tolerant_match`, above); this
    // documents that the transposed-character half does not, so nothing
    // here would silently regress unnoticed if that ever mattered later.
    // Recovering it would need the same edit-distance fallback the old JS
    // scorer had (see `one_character_substitution_typo_is_not_recovered`,
    // below), which nucleo's subsequence algorithm has no equivalent of.
    #[test]
    fn adjacent_transposition_typo_is_not_recovered() {
        let query = route("termianl");
        let items = vec![item(Kind::App, "app:terminal", "Terminal", None)];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert!(
            ranked.is_empty(),
            "nucleo cannot align a transposed adjacent pair as a subsequence"
        );
    }

    // DIVERGENCE: the JS scorer accepts this via its own Levenshtein-
    // within-1 fallback (`rankResults('budjet', ..., {minFuzzyScore: 30})`
    // keeps "Budget 2026"). nucleo's fuzzy matching is subsequence-based,
    // not edit-distance based: every needle character must appear, in
    // order, in the haystack. "budjet"'s 'j' does not appear anywhere in
    // "Google Docs - Budget 2026 - Brave", so there is no valid alignment
    // — the match is `None` regardless of `min_score`. nucleo tolerates
    // dropped/reordered characters (see `typo_tolerant_match`); it does
    // not tolerate a character substituted for one absent from the
    // target. This documents that actual (narrower) behavior instead of
    // silently dropping the ported case.
    #[test]
    fn one_character_substitution_typo_is_not_recovered() {
        let query = route("budjet");
        let items = vec![item(
            Kind::Window,
            "window:1",
            "Google Docs - Budget 2026 - Brave",
            Some("Brave - Workspace 1"),
        )];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert!(
            ranked.is_empty(),
            "nucleo cannot align a substituted character that's absent from the haystack"
        );
    }

    // DIVERGENCE: absolute-score assertion (`computeFuzzyScore(...) >
    // 20`) on the JS scorer's own scale, which this ranker doesn't share
    // (see the module doc comment on normalization). Asserting the
    // ordering property the absolute check was actually protecting:
    // "chr" favors "Google Chrome" over an unrelated candidate.
    #[test]
    fn short_query_chr_favors_google_chrome_over_unrelated_candidate() {
        let query = route("chr");
        let items = vec![
            item(Kind::App, "app:chrome", "Google Chrome", None),
            item(Kind::App, "app:archiver", "Archiver", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked[0].item.title, "Google Chrome");
    }

    // DIVERGENCE: the JS suite's `recent` kind has no equivalent in this
    // protocol's `Kind` enum. Ported using `File` in its place — `File`
    // (12) sits below `App` (20) just as `recent` (10) did in the
    // original, preserving the tie-break intent (windows > apps > the
    // lowest-weighted kind on offer).
    #[test]
    fn ranking_prefers_windows_over_apps_over_files_on_tie() {
        let query = route("chrome");
        let items = vec![
            item(Kind::File, "file:notes", "Chrome Notes", None),
            item(Kind::App, "app:chrome", "Chrome", None),
            item(Kind::Window, "window:chrome", "Chrome", Some("Workspace 1")),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked[0].item.kind, Kind::Window);
    }

    /// Ports "window title substring can win" directly: an unrelated app
    /// ("Files") doesn't contain "ranking" as a subsequence at all, so
    /// the matching window survives alone.
    #[test]
    fn window_title_substring_can_win() {
        let query = route("ranking");
        let items = vec![
            item(
                Kind::Window,
                "window:1",
                "Fix launcher ranking bug",
                Some("Code - Workspace 2"),
            ),
            item(Kind::App, "app:files", "Files", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked[0].item.kind, Kind::Window);
    }

    // DIVERGENCE: same `recent` substitution as
    // `ranking_prefers_windows_over_apps_over_files_on_tie`.
    #[test]
    fn empty_query_falls_back_to_source_weighting_order() {
        let query = route("");
        let items = vec![
            item(Kind::App, "app:calc", "Calculator", None),
            item(Kind::Window, "window:term", "Terminal", None),
            item(Kind::File, "file:notes", "notes.txt", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked[0].item.kind, Kind::Window);
    }

    /// Ports "new smart-provider kinds stay below windows and apps with
    /// default weights" directly. All five haystacks contain "terminal"
    /// as an equally clean match, so this exercises pure weight ordering:
    /// window > app > (file/emoji/calculator, which don't need
    /// distinguishing here).
    #[test]
    fn smart_provider_kinds_stay_below_windows_and_apps() {
        let query = route("terminal");
        let items = vec![
            item(Kind::File, "file:terminal", "terminal.md", None),
            item(Kind::Emoji, "emoji:terminal", "Terminal Face", None),
            item(Kind::Calculator, "calc:terminal", "terminal = 1", None),
            item(Kind::App, "app:terminal", "Terminal", None),
            item(Kind::Window, "window:terminal", "Terminal", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked[0].item.kind, Kind::Window);
        assert_eq!(ranked[1].item.kind, Kind::App);
    }

    // DIVERGENCE: the JS `rankResults` clamps `maxResults` to at least
    // one and truncates to it. `Ranker::rank` has no result cap at all —
    // that's the pipeline's job in a later slice (M1.7). Asserting the
    // property that actually applies here: every surviving item comes
    // back, uncapped.
    #[test]
    fn rank_returns_every_surviving_item_with_no_cap() {
        let query = route("");
        let items = vec![
            item(Kind::App, "app:calc", "Calculator", None),
            item(Kind::Window, "window:term", "Terminal", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked.len(), 2);
    }

    // DIVERGENCE: the JS test asserts an implementation detail — that
    // `rankResults` mutates each item object with a cached
    // `_searchHaystack`/`_searchHaystackLower`. The Rust `Ranker` caches
    // nothing on `Item`; it owns a reusable `Matcher` instead (built once
    // in `Ranker::new`, never per query). The property that actually
    // matters: reusing one `Ranker` across repeated calls with the same
    // input is deterministic and doesn't corrupt its internal scratch
    // state.
    #[test]
    fn reusing_one_ranker_across_calls_is_deterministic() {
        let query = route("br");
        let build_items = || {
            vec![item(
                Kind::App,
                "app:brave",
                "Brave Browser",
                Some("Web browser"),
            )]
        };
        let mut ranker = Ranker::new();
        let first = ranker.rank(
            build_items(),
            &query,
            &Weights::default(),
            &Boosts::default(),
        );
        let second = ranker.rank(
            build_items(),
            &query,
            &Weights::default(),
            &Boosts::default(),
        );
        assert_eq!(first.len(), second.len());
        assert_eq!(first[0].item.title, second[0].item.title);
        assert_eq!(first[0].score, second[0].score);
    }

    /// Ports both "ranking deduplicates duplicate result identities" and
    /// "ranking deduplicates app rows with different ids but same visible
    /// text", and doubles as the brief's `duplicate_apps_deduped_by_title`
    /// acceptance case: apps dedupe on title alone, regardless of
    /// whether the ids match or differ.
    #[test]
    fn duplicate_apps_deduped_by_title() {
        let query = route("firefox");
        let items = vec![
            item(Kind::App, "brave-browser.desktop", "Firefox", None),
            item(Kind::App, "brave-browser-alt.desktop", "Firefox", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].item.title, "Firefox");
    }

    /// Ports "ranking applies external item score boosts" directly, and
    /// doubles as the brief's `boost_overrides_fuzzy_order` /
    /// "boost applies to the right item" coverage: same title, boost one
    /// id, and only that item's rank changes.
    #[test]
    fn boost_applies_to_the_right_item() {
        let query = route("firefox");
        let items = vec![
            item(Kind::Window, "window:1", "Firefox", None),
            item(Kind::App, "app:firefox", "Firefox", None),
        ];
        // Without the boost, Window (weight 30) outranks App (weight 20)
        // on this tie. +50 flips it.
        let mut boosts = Boosts::default();
        boosts.by_item_id.insert(ItemId("app:firefox".into()), 50.0);
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &boosts);
        assert_eq!(ranked[0].item.id, ItemId("app:firefox".into()));
    }

    /// Ports "ranking filters non-empty query matches under
    /// minFuzzyScore" directly: a real match is still dropped once
    /// `min_score` is set high enough.
    #[test]
    fn min_score_filters_out_real_matches_when_set_high_enough() {
        let query = route("ter");
        let items = vec![
            item(Kind::App, "app:terminal", "Terminal", None),
            item(Kind::App, "app:firefox", "Mozilla Firefox", None),
        ];
        let weights = Weights {
            min_score: 999.0,
            ..Weights::default()
        };
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &weights, &Boosts::default());
        assert!(ranked.is_empty());
    }

    /// Ports "ranking keeps empty-query ordering even with
    /// minFuzzyScore" directly: contract point 1 — `min_score` is not
    /// applied when the term is empty, no matter how high it's set.
    #[test]
    fn empty_term_ignores_min_score() {
        let query = route("");
        let items = vec![
            item(Kind::Window, "window:term", "Terminal", None),
            item(Kind::App, "app:calc", "Calculator", None),
        ];
        let weights = Weights {
            min_score: 40.0,
            ..Weights::default()
        };
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &weights, &Boosts::default());
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].item.kind, Kind::Window);
    }

    /// Ports the combined intent of "ranking rejects dispersed
    /// long-query letter matches" (`minFuzzyScore: 30`) and "fuzzy
    /// scoring makes dispersed brave matches non-positive": a query whose
    /// letters are scattered thinly across unrelated candidates should
    /// not surface those candidates above (or alongside) the real match.
    ///
    /// `min_score: 20.0` here is not a magic number reverse-engineered to
    /// force this one case: probing nucleo's actual per-character
    /// normalized scores for these exact strings shows a real gap (~28.0
    /// for the true match, topping out at ~15.6 for the best-scoring
    /// distractor — "Oracle VirtualBox... single host computer"), so 20.0
    /// sits cleanly in the gap. This mirrors the JS test's own choice of a
    /// custom, non-default `minFuzzyScore` for the same purpose.
    #[test]
    fn dispersed_matches_are_rejected_below_the_real_match() {
        let query = route("brave");
        let items = vec![
            item(
                Kind::App,
                "app:brave",
                "Brave Web Browser",
                Some("Access the Internet"),
            ),
            item(
                Kind::App,
                "app:bluetooth",
                "Bluetooth Transfer",
                Some("Send files via Bluetooth"),
            ),
            item(
                Kind::App,
                "app:report",
                "Report a problem...",
                Some("Report a malfunction to developers"),
            ),
            item(
                Kind::App,
                "app:webstorm",
                "WebStorm",
                Some("The smartest JavaScript IDE"),
            ),
        ];
        let weights = Weights {
            min_score: 20.0,
            ..Weights::default()
        };
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &weights, &Boosts::default());
        let titles: Vec<_> = ranked.iter().map(|r| r.item.title.as_str()).collect();
        assert_eq!(titles, vec!["Brave Web Browser"]);
    }

    // --- From the plan (not in the JS suite).

    // DIVERGENCE: the plan specifies this exact case — `route("vsc")` over
    // "Visual Studio Code" versus "vscodium helper thing" — expecting the
    // word-boundary (acronym-style) match to win. It doesn't: nucleo scores
    // a literal contiguous prefix match ("vscodium...", raw 88) higher than
    // a 3-letter acronym spread across three word boundaries ("Visual
    // Studio Code", raw 72).
    //
    // This isn't a `Config` knob we failed to reach for. nucleo-matcher
    // 0.3.1's boundary-bonus tuning (`bonus_boundary_white`,
    // `bonus_boundary_delimiter`, `delimiter_chars`) is `pub(crate)` —
    // unreachable from outside the crate — and `Pattern::score` overwrites
    // `Config::ignore_case`/`normalize` from the atom on every call, so the
    // only externally-tunable knob left is `prefer_prefix`. Probed: setting
    // it doesn't flip the order either (96 vs 80 — same direction, same
    // gap). Nucleo's own module docs describe it as fundamentally a
    // substring-matching tool, not an acronym matcher, which is exactly
    // this behavior.
    //
    // Fixing this for real would mean not relying on `Pattern::score`
    // alone: compute match indices via `Pattern::indices`, detect which
    // matched characters land on word-boundary positions ourselves, and
    // add an explicit boundary-density bonus in this module's own scoring
    // layer — at the cost of the slower `indices` API on every candidate
    // instead of `score`, on every keystroke. That's a real design change
    // and a real cost, out of scope for this slice. Recording the gap here
    // rather than quietly rewriting the query until it passes.
    #[test]
    fn word_boundary_does_not_yet_beat_scattered_for_short_acronym_queries() {
        let query = route("vsc");
        let items = vec![
            item(Kind::App, "app:vscode", "Visual Studio Code", None),
            item(Kind::App, "app:vscodium", "vscodium helper thing", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(
            ranked[0].item.title, "vscodium helper thing",
            "documents the actual (undesired) behavior: nucleo currently \
             prefers the contiguous prefix match over the word-boundary one"
        );
    }

    #[test]
    fn source_weight_breaks_ties() {
        let query = route("terminal");
        let items = vec![
            item(Kind::File, "file:terminal", "Terminal", None),
            item(Kind::Window, "window:terminal", "Terminal", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked[0].item.kind, Kind::Window);
    }

    #[test]
    fn boost_overrides_fuzzy_order() {
        let query = route("chrome");
        // "Google Chrome" is a clean contiguous match (raw 166, ~27.7 per
        // character). The second candidate is deliberately constructed so
        // each needle character lands mid-word, far apart, and off any
        // word boundary — a genuinely "barely-there" scattered subsequence
        // match (raw 31, ~5.2 per character), not merely a somewhat weaker
        // one. The two are both `Kind::App`, so weight doesn't separate
        // them either. That ~22.5-point-per-character gap means a boost of
        // ~2 would do nothing; only a boost that actually closes a gap
        // this size proves anything about 180 (this ranker's boost scale)
        // doing real load-bearing work.
        let build_items = || {
            vec![
                item(Kind::App, "app:chrome", "Google Chrome", None),
                item(
                    Kind::App,
                    "app:scattered",
                    "czzzzzzzzzzzzzzzhzzzzzzzzzzzzzzzrzzzzzzzzzzzzzzzozzzzzzzzzzzzzzzmzzzzzzzzzzzzzzze",
                    None,
                ),
            ]
        };

        let mut ranker = Ranker::new();
        let unboosted = ranker.rank(
            build_items(),
            &query,
            &Weights::default(),
            &Boosts::default(),
        );
        assert_eq!(
            unboosted[0].item.title, "Google Chrome",
            "sanity check: without a boost, the genuinely better match wins"
        );

        let mut boosts = Boosts::default();
        boosts
            .by_item_id
            .insert(ItemId("app:scattered".into()), 40.0);
        let boosted = ranker.rank(build_items(), &query, &Weights::default(), &boosts);
        assert_eq!(boosted[0].item.id, ItemId("app:scattered".into()));
    }

    /// Issue #31's boost-theft gap, closed for aliases: two items can
    /// legitimately share an [`ItemId`] while being produced by two
    /// different, individually honest providers — an id-namespace
    /// collision, not the impersonation `CheckedItems::check` already
    /// catches (each item's `provider` field agrees with its own producer).
    /// A boost meant for one provider's item must not land on the other's
    /// just because the id string matches.
    #[test]
    fn provider_scoped_boost_only_applies_to_the_matching_producer() {
        let query = route("firefox");
        let items = vec![
            Item {
                provider: "apps".into(),
                ..item(Kind::App, "app:firefox", "Firefox", None)
            },
            Item {
                provider: "evil".into(),
                ..item(Kind::Window, "app:firefox", "Firefox", None)
            },
        ];
        // Weight alone puts Window (30) ahead of App (20) on this tie; the
        // boost is keyed to "apps" specifically and must flip that only for
        // the item "apps" actually produced.
        let mut boosts = Boosts::default();
        boosts
            .by_provider_item
            .insert(("apps".to_string(), ItemId("app:firefox".into())), 180.0);
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &boosts);
        assert_eq!(
            ranked[0].item.provider, "apps",
            "the boost keyed to \"apps\" must not lift the identically-id'd \
             item the \"evil\" provider produced"
        );
    }

    #[test]
    fn empty_term_returns_all_sorted_by_weight_and_boost() {
        let query = route("w "); // Windows-exclusive prefix, empty term.
        let items = vec![
            item(Kind::File, "file:a", "Alpha", None),
            item(Kind::App, "app:b", "Bravo", None),
            item(Kind::Window, "window:c", "Charlie", None),
        ];
        // 12 (File) + 25 = 37, enough to outrank Window's 30.
        let mut boosts = Boosts::default();
        boosts.by_item_id.insert(ItemId("file:a".into()), 25.0);
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &boosts);
        let titles: Vec<_> = ranked.iter().map(|r| r.item.title.as_str()).collect();
        assert_eq!(titles, vec!["Alpha", "Charlie", "Bravo"]);
    }

    #[test]
    fn below_min_score_dropped() {
        let query = route("zzzqxk");
        let items = vec![
            item(Kind::App, "app:chrome", "Google Chrome", None),
            item(Kind::App, "app:firefox", "Firefox", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert!(ranked.is_empty());
    }

    // --- The term is matched literally, not as nucleo's query DSL.

    /// Builds a `RoutedQuery` carrying `term` verbatim, bypassing [`route`].
    ///
    /// Every other test in this file routes its input, which is the honest
    /// thing when the point is what a user typed. This helper is the honest
    /// model of the *other* sink: `Pipeline::assemble` hands the ranker
    /// `alias_effect.effective_term` — arbitrary text from an alias's rewrite
    /// target, which never passes through `route` at all. Use it only for a
    /// term `route` cannot deliver, and say why at the call site.
    fn term_query(term: &str) -> RoutedQuery {
        RoutedQuery {
            mode: crate::router::Mode::All,
            term: term.to_string(),
            exclusive: false,
            raw: term.to_string(),
        }
    }

    /// Each of `^`, `'` and `!` is a leading sigil in `Pattern::parse`'s DSL,
    /// and a term consisting of one alone parsed to an atom with an empty
    /// needle, which `parse` then discarded — leaving a pattern with no
    /// atoms, which `Pattern::score` scores `Some(0)` for every candidate.
    /// Matched literally, each is instead an ordinary one-character needle
    /// that neither haystack contains.
    ///
    /// Routed, because a user really can type these three and have them reach
    /// the ranker intact: none is a routing prefix, so `route` classifies
    /// each as [`Mode::All`](crate::router::Mode::All) with the term
    /// untouched. The DSL's fourth sigil, `$`, is the one that cannot be
    /// routed — hence the separate test that follows.
    #[test]
    fn dsl_sigils_alone_match_nothing_rather_than_everything() {
        for term in ["^", "'", "!"] {
            let query = route(term);
            let items = vec![
                item(Kind::App, "app:firefox", "Firefox", None),
                item(Kind::App, "app:files", "Files", None),
            ];
            let mut ranker = Ranker::new();
            let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
            assert!(
                ranked.is_empty(),
                "{term:?} matches neither candidate literally, so the \
                 documented contract (a non-matching item is dropped) must \
                 drop both"
            );
        }
    }

    /// The fourth sigil, kept separate from its three siblings above because
    /// it is the one [`route`] cannot deliver: `$` is the *currency* prefix,
    /// so `route("$")` strips it and hands the ranker an empty term — the
    /// match-everything path, not the one under test. A bare `$` reaches the
    /// ranker only through the alias sink, as the effective term of a rewrite
    /// whose target is `$`, which is what [`term_query`] stands in for here.
    /// The conclusion is the sigils' conclusion: matched literally, `$` is an
    /// ordinary one-character needle that neither haystack contains.
    #[test]
    fn a_bare_dollar_term_matches_nothing_rather_than_everything() {
        let query = term_query("$");
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox", None),
            item(Kind::App, "app:files", "Files", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert!(
            ranked.is_empty(),
            "\"$\" matches neither candidate literally, so the documented \
             contract (a non-matching item is dropped) must drop both"
        );
    }

    /// The headline case from the issue. Under `Pattern::parse`, a leading
    /// `!` makes the atom a *negated substring*: `!firefox` returned every
    /// candidate that does **not** contain "firefox" — the exact inverse of
    /// what the user asked for, and a silent one. Matched literally, the
    /// needle is the eight characters `!firefox`, which only the item
    /// literally containing them can satisfy.
    ///
    /// Both halves matter, so both are asserted: the literal item is found,
    /// and the two items that negation would have inverted the treatment of
    /// (`Firefox`, excluded before; `Files`, returned before) are neither
    /// excluded-as-a-special-case nor swept in.
    #[test]
    fn leading_bang_is_a_literal_character_not_an_exclusion() {
        let query = route("!firefox");
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox", None),
            item(Kind::App, "app:files", "Files", None),
            item(Kind::Action, "action:bug", "!firefox crash note", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        let titles: Vec<_> = ranked.iter().map(|r| r.item.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["!firefox crash note"],
            "the only item whose haystack literally contains \"!firefox\"; \
             negation would instead have returned \"Files\" alone"
        );
    }

    /// The other half of the negation criterion, kept separate because it is
    /// a different behavior: an ordinary query must still find `Firefox`.
    /// Without this, `leading_bang_is_a_literal_character_not_an_exclusion`
    /// above would still pass if the ranker had simply started dropping
    /// everything.
    #[test]
    fn an_ordinary_term_still_finds_firefox() {
        let query = route("firefox");
        let items = vec![
            item(Kind::App, "app:firefox", "Firefox", None),
            item(Kind::App, "app:files", "Files", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        let titles: Vec<_> = ranked.iter().map(|r| r.item.title.as_str()).collect();
        assert_eq!(titles, vec!["Firefox"]);
    }

    /// Whitespace-splitting into one atom per word is `Pattern::new`'s
    /// behavior just as much as `Pattern::parse`'s, and it is the desirable
    /// half of the old behavior: a multi-word query must keep matching an
    /// item whose words are split across title and subtitle, in either
    /// order. Pinned here so a future move to a single whole-term `Atom`
    /// (the other way to get literal matching) can't quietly break it.
    #[test]
    fn a_multi_word_term_still_matches_word_by_word() {
        let query = route("firefox workspace");
        let items = vec![
            // Deliberately reversed: the haystack is "Workspace 2 Mozilla
            // Firefox", so "workspace" precedes "firefox" in it. Matched as
            // one contiguous needle ("firefox workspace") there is no valid
            // subsequence — the needle wants "firefox" first. Matched as two
            // independent atoms, both hit.
            item(
                Kind::Window,
                "window:1",
                "Workspace 2",
                Some("Mozilla Firefox"),
            ),
            item(Kind::App, "app:files", "Files", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        let titles: Vec<_> = ranked.iter().map(|r| r.item.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Workspace 2"],
            "the two words must be matched as separate atoms, not as one \
             contiguous needle"
        );
    }

    /// Part two of the fix, and the reason [`Matching`] exists as a named
    /// type rather than an `Option<Pattern>`: [`Matching::Nothing`] drops
    /// every candidate, where a zero-atom pattern reaching the scorer would
    /// have kept the lot. See [`Matching::for_term`] for why, and for why no
    /// term input currently produces this arm — which is what makes an
    /// unreachable guard worth a test at all, and why this one goes through
    /// the ranking path: the hole it closes was a *scoring* hole, items
    /// surviving with score `0.0`.
    #[test]
    fn matching_nothing_drops_every_item() {
        let items = vec![
            item(Kind::Window, "window:1", "Terminal", None),
            item(Kind::App, "app:firefox", "Firefox", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank_matching(
            &Matching::Nothing,
            items,
            &Weights::default(),
            &Boosts::default(),
        );
        assert!(
            ranked.is_empty(),
            "a term that produced no atoms must drop every item, not score \
             them all zero and keep them"
        );
    }

    // --- Coverage neither source reaches.

    #[test]
    fn append_to_end_items_are_excluded_with_empty_term() {
        let query = route("");
        let mut pinned = item(Kind::WebSearch, "web:search", "Search the web", None);
        pinned.append_to_end = true;
        let items = vec![item(Kind::App, "app:a", "Alpha", None), pinned];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].item.title, "Alpha");
    }

    #[test]
    fn append_to_end_items_are_excluded_with_matching_term() {
        let query = route("search");
        let mut pinned = item(Kind::WebSearch, "web:search", "Search the web", None);
        pinned.append_to_end = true;
        let items = vec![pinned];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert!(ranked.is_empty());
    }

    #[test]
    fn non_app_kinds_with_same_title_and_different_ids_both_survive_dedupe() {
        let query = route("terminal");
        let items = vec![
            item(Kind::Window, "window:1", "Terminal", None),
            item(Kind::Window, "window:2", "Terminal", None),
        ];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn subtitle_participates_in_matching() {
        let query = route("workspace");
        let items = vec![item(
            Kind::Window,
            "window:1",
            "Terminal",
            Some("Workspace 2"),
        )];
        let mut ranker = Ranker::new();
        let ranked = ranker.rank(items, &query, &Weights::default(), &Boosts::default());
        assert_eq!(
            ranked.len(),
            1,
            "\"workspace\" only appears in the subtitle, not the title"
        );
    }
}
