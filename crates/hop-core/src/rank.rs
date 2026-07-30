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
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
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

/// Learned/aliased per-item score bumps, keyed by [`ItemId`]. Empty by
/// default — where boosts come from (aliases in M1.6, learning in M1.5) is
/// out of scope for this slice.
#[derive(Default)]
pub struct Boosts {
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
    /// - Surviving items score `fuzzy + kind_weight + boost`, sorted
    ///   descending by score; ties break by kind weight descending, then
    ///   by title ascending.
    /// - `append_to_end` items are excluded entirely — a later slice pins
    ///   them after the ranked block instead of ranking them here.
    /// - Results are deduped after sorting, keeping the first (best-
    ///   scoring) occurrence: apps key on title alone; every other kind
    ///   keys on kind, id and title together.
    /// - No result cap — callers that need one (the pipeline, in a later
    ///   slice) apply it themselves.
    pub fn rank(
        &mut self,
        items: Vec<Item>,
        query: &RoutedQuery,
        weights: &Weights,
        boosts: &Boosts,
    ) -> Vec<Ranked> {
        let term = query.term.trim();
        let pattern = (!term.is_empty())
            .then(|| Pattern::parse(term, CaseMatching::Ignore, Normalization::Smart));

        let mut buf = Vec::new();
        let mut ranked: Vec<Ranked> = items
            .into_iter()
            .filter(|item| !item.append_to_end)
            .filter_map(|item| {
                let fuzzy = match &pattern {
                    None => 0.0,
                    Some(pattern) => {
                        let haystack = haystack_of(&item);
                        let raw =
                            pattern.score(Utf32Str::new(&haystack, &mut buf), &mut self.matcher)?;
                        let normalized = raw as f32 / term.chars().count() as f32;
                        if normalized < weights.min_score {
                            return None;
                        }
                        normalized
                    }
                };

                let weight = kind_weight(weights, &item.kind);
                let boost = boosts.by_item_id.get(&item.id).copied().unwrap_or(0.0);
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
