//! Query routing: decides which search mode(s) a raw query string should
//! feed, without doing any of the searching itself.
//!
//! `route` is a pure function — no disk reads, no subprocess spawns, no
//! network calls — because it runs on every keystroke (design spec §3).
//! Anything expensive to build (regexes, the timezone-alias set) is
//! precompiled exactly once via [`std::sync::LazyLock`].
//!
//! Explicit prefixes (`w `, `f `, `$`, ...) are *exclusive*: the user asked
//! for one specific mode, so nothing else should be shown. Inferred modes
//! (a bare city name, a bare arithmetic expression) are *not* exclusive —
//! they augment the general search results instead of hijacking the
//! launcher into a single mode. That distinction is the fix for an audited
//! defect in the previous GNOME extension, where a bare city name or sum
//! would hide the apps and files the user was actually reaching for.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

/// Which search mode a routed query should be interpreted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    All,
    Windows,
    Apps,
    Files,
    Emoji,
    Timezone,
    Currency,
    Calculator,
    Weather,
    Actions,
    /// Part of the routing vocabulary, but `route()` never returns it yet —
    /// no explicit prefix or inference rule targets it in this milestone.
    WebSearch,
}

/// The result of routing a raw query string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedQuery {
    pub mode: Mode,
    /// The query with any recognized prefix/suffix stripped, and trimmed.
    pub term: String,
    /// `true` only when the user typed an explicit prefix or sigil. An
    /// exclusive route should replace the general search; a non-exclusive
    /// (inferred) route should augment it.
    pub exclusive: bool,
    /// The untouched original input, exactly as passed to `route`.
    pub raw: String,
}

/// The timezone aliases known to the router. Ported from the previous
/// extension's `lib/data/timezone-aliases.js`; only the keys are kept here
/// — routing needs membership only, and the IANA zone names each key maps
/// to belong to the timezone provider that lands in M4.
static TIMEZONE_ALIASES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "utc",
        "gmt",
        "pst",
        "pdt",
        "mst",
        "mdt",
        "cst",
        "cdt",
        "est",
        "edt",
        "tokyo",
        "berlin",
        "london",
        "paris",
        "lisbon",
        "sydney",
        "seoul",
        "singapore",
        "mumbai",
        "sao_paulo",
        "nyc",
        "la",
    ]
    .into_iter()
    .collect()
});

/// Matches `100 usd to eur`, `100usd to eur`, `100.50 usd to eur`, etc.
/// against the ASCII-lowercased, trimmed query.
///
/// The digits are `[0-9]` rather than `\d` because the regex crate's `\d` is
/// Unicode-aware: it also accepts Arabic-Indic, Devanagari and fullwidth
/// digits, which `str::parse::<f64>` then rejects. That handed the currency
/// provider a term whose numeric portion routing had implied was already
/// shape-checked. `[0-9]` is also what [`looks_like_math`] means by a digit,
/// so the two inference predicates now agree on what a number is.
///
/// `\s` is deliberately left Unicode-aware. Whitespace never lands inside the
/// numeric portion, so it carries none of that hazard, and [`looks_like_math`]
/// already accepts whitespace Unicode-wide via `char::is_whitespace` — whose
/// set `\s` matches exactly. Narrowing it here would trade one disagreement
/// between the predicates for another, and would reject amounts pasted from
/// documents that separate them with a non-breaking space.
static CURRENCY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9]+(\.[0-9]+)?\s*[a-z]{3}\s+to\s+[a-z]{3}$")
        .expect("CURRENCY_RE pattern is a fixed literal and must compile")
});

/// Routes a raw query string to a search mode.
///
/// Match order (first match wins): explicit prefixes/sigils, then inferred
/// modes, then a fallback to [`Mode::All`]. See the module docs for why
/// explicit routes are exclusive and inferred routes are not.
pub fn route(raw: &str) -> RoutedQuery {
    let q = raw.trim_start();

    if let Some(rest) = strip_prefix_ci(q, "w ") {
        return exclusive(Mode::Windows, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, "a ") {
        return exclusive(Mode::Apps, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, "f ") {
        return exclusive(Mode::Files, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, ":emoji ") {
        return exclusive(Mode::Emoji, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, "emoji ") {
        return exclusive(Mode::Emoji, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, "tz ") {
        return exclusive(Mode::Timezone, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, "timezone ") {
        return exclusive(Mode::Timezone, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, "weather ") {
        return exclusive(Mode::Weather, rest, raw);
    }
    if let Some(rest) = strip_prefix_ci(q, "wx ") {
        return exclusive(Mode::Weather, rest, raw);
    }
    if let Some(rest) = strip_suffix_ci(q, " weather") {
        return exclusive(Mode::Weather, rest, raw);
    }
    if let Some(rest) = q.strip_prefix('$') {
        return exclusive(Mode::Currency, rest, raw);
    }
    if let Some(rest) = q.strip_prefix('=') {
        return exclusive(Mode::Calculator, rest, raw);
    }
    if let Some(rest) = q.strip_prefix('>') {
        return exclusive(Mode::Actions, rest, raw);
    }

    if looks_like_math(q) {
        return inferred(Mode::Calculator, q, raw);
    }
    if looks_like_currency(q) {
        return inferred(Mode::Currency, q, raw);
    }
    if let Some(term) = infer_timezone(q) {
        return inferred(Mode::Timezone, &term, raw);
    }

    inferred(Mode::All, q, raw)
}

fn exclusive(mode: Mode, term: &str, raw: &str) -> RoutedQuery {
    RoutedQuery {
        mode,
        term: term.trim().to_string(),
        exclusive: true,
        raw: raw.to_string(),
    }
}

fn inferred(mode: Mode, term: &str, raw: &str) -> RoutedQuery {
    RoutedQuery {
        mode,
        term: term.trim().to_string(),
        exclusive: false,
        raw: raw.to_string(),
    }
}

/// Case-insensitive prefix strip. Safe against multi-byte input: `get`
/// returns `None` both when `q` is too short and when the byte offset
/// would land mid-character, so this never panics on a slice boundary.
/// Correct as a case-insensitive match because every `prefix` we call this
/// with is a pure-ASCII literal.
fn strip_prefix_ci<'a>(q: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = q.get(0..prefix.len())?;
    candidate
        .eq_ignore_ascii_case(prefix)
        .then(|| &q[prefix.len()..])
}

/// Case-insensitive suffix strip. Same char-boundary safety as
/// [`strip_prefix_ci`].
fn strip_suffix_ci<'a>(q: &'a str, suffix: &str) -> Option<&'a str> {
    let split_at = q.len().checked_sub(suffix.len())?;
    if !q.is_char_boundary(split_at) {
        return None;
    }
    let candidate = &q[split_at..];
    candidate
        .eq_ignore_ascii_case(suffix)
        .then(|| &q[..split_at])
}

/// The trimmed query is non-empty, contains at least one digit, and
/// consists only of digits, basic arithmetic operators/punctuation, and
/// whitespace.
fn looks_like_math(q: &str) -> bool {
    let q = q.trim();
    if q.is_empty() {
        return false;
    }
    if !q.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    q.chars().all(|c| {
        c.is_ascii_digit()
            || matches!(c, '+' | '-' | '*' | '/' | '(' | ')' | '.' | '%')
            || c.is_whitespace()
    })
}

/// `^[0-9]+(\.[0-9]+)?\s*[a-z]{3}\s+to\s+[a-z]{3}$` against the
/// ASCII-lowercased, trimmed query — e.g. `100 usd to eur` or `100usd to eur`.
///
/// The fold is `to_ascii_lowercase`, not `to_lowercase`, so that it shares the
/// alphabet of the `[a-z]` classes it feeds. Full Unicode folding maps U+212A
/// KELVIN SIGN to an ASCII `k` — the only char outside ASCII it does that for
/// — which let a code the pattern spells in ASCII match while the term
/// forwarded to the currency provider still held the non-ASCII char. (Full
/// folding can also change a string's length: U+0130 lowercases to two chars.
/// That one is harmless here, because the second is a combining mark that
/// `[a-z]` rejects either way.)
fn looks_like_currency(q: &str) -> bool {
    CURRENCY_RE.is_match(&q.trim().to_ascii_lowercase())
}

/// Returns the timezone term (prefix/suffix phrasing stripped) if `q` looks
/// like a timezone query, or `None` otherwise. See the module-level routing
/// table doc for the three conditions this checks, in order.
fn infer_timezone(q: &str) -> Option<String> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        return None;
    }

    // `time in ` must be checked before `time ` — the latter is a prefix of
    // the former, so checking it first would wrongly leave "in " glued to
    // the term.
    if let Some(rest) = strip_prefix_ci(trimmed, "time in ") {
        return Some(rest.to_string());
    }
    if let Some(rest) = strip_prefix_ci(trimmed, "time ") {
        return Some(rest.to_string());
    }
    if let Some(rest) = strip_prefix_ci(trimmed, "now in ") {
        return Some(rest.to_string());
    }

    if let Some(prefix_part) = strip_suffix_ci(trimmed, " time") {
        let token = collapse_whitespace(&prefix_part.trim().to_lowercase());
        if token.chars().count() >= 2 && TIMEZONE_ALIASES.contains(token.as_str()) {
            return Some(prefix_part.to_string());
        }
    }

    let whole = collapse_whitespace(&trimmed.to_lowercase());
    if whole.chars().count() >= 2 && TIMEZONE_ALIASES.contains(whole.as_str()) {
        return Some(trimmed.to_string());
    }

    None
}

/// Collapses runs of whitespace into a single `_`, mirroring the previous
/// extension's `query.replace(/\s+/g, '_')` used to turn "sao paulo" into
/// the alias-set key `sao_paulo`.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_whitespace = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_whitespace {
                out.push('_');
            }
            in_whitespace = true;
        } else {
            out.push(c);
            in_whitespace = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // --- Ported from the previous extension's tests/query-router.test.mjs.
    // Each case now also asserts `exclusive`, which the JS suite had no
    // concept of — that flag is the entire point of this port, so a test
    // that only checked mode/term would miss the defect being fixed.

    #[test]
    fn prefix_f_routes_to_files() {
        let r = route("f report");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Files, "report", true)
        );
    }

    #[test]
    fn colon_emoji_prefix_routes_to_emoji() {
        let r = route(":emoji smile");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Emoji, "smile", true)
        );
    }

    #[test]
    fn emoji_keyword_routes_to_emoji() {
        let r = route("emoji smile");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Emoji, "smile", true)
        );
    }

    #[test]
    fn prefix_w_routes_to_windows() {
        let r = route("w terminal");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Windows, "terminal", true)
        );
    }

    #[test]
    fn math_like_query_routes_to_calculator_and_augments() {
        let r = route("2+2");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Calculator, "2+2", false)
        );
    }

    #[test]
    fn timezone_keyword_routes_to_timezone() {
        let r = route("time tokyo");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "tokyo", false)
        );
    }

    #[test]
    fn time_in_phrase_routes_to_timezone() {
        let r = route("time in zurich");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "zurich", false)
        );
    }

    #[test]
    fn timezone_alias_routes_to_timezone() {
        let r = route("pst");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "pst", false)
        );
    }

    // DIVERGENCE: the original JS case was `zurich`, which is not one of
    // the 22 keys in `TIMEZONE_ALIASES` above. The old extension resolved
    // `zurich` through its full city dataset, not a small embedded alias
    // set; that full dataset arrives with the timezone provider in M4, out
    // of scope here. `berlin` is substituted because it *is* an
    // alias-set key, preserving the case's intent (a bare city token
    // infers timezone mode) within this milestone's scope. Do not "fix"
    // this by adding zurich to the alias set.
    #[test]
    fn bare_alias_city_routes_to_timezone_and_augments() {
        let r = route("berlin");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "berlin", false)
        );
    }

    // DIVERGENCE: the original JS case was `zurich time`. Substituted with
    // `berlin time` for the same reason as above — `zurich` is not one of
    // the 22 keys in `TIMEZONE_ALIASES`, the old extension only resolved it
    // through its full city dataset, and that dataset arrives with the
    // timezone provider in M4, out of scope here. `berlin` is in the alias
    // set, so it preserves the original case's intent within this
    // milestone's scope.
    #[test]
    fn city_time_suffix_routes_to_timezone_and_augments() {
        let r = route("berlin time");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "berlin", false)
        );
    }

    #[test]
    fn weather_keyword_routes_to_weather() {
        let r = route("weather berlin");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Weather, "berlin", true)
        );
    }

    #[test]
    fn wx_shorthand_routes_to_weather() {
        let r = route("wx 94103");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Weather, "94103", true)
        );
    }

    #[test]
    fn city_weather_suffix_routes_to_weather() {
        let r = route("zurich weather");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Weather, "zurich", true)
        );
    }

    #[test]
    fn currency_conversion_text_routes_to_currency() {
        let r = route("100 usd to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Currency, "100 usd to eur", false)
        );
    }

    #[test]
    fn compact_currency_conversion_routes_to_currency() {
        let r = route("100usd to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Currency, "100usd to eur", false)
        );
    }

    #[test]
    fn default_route_keeps_all_mode() {
        let r = route("firefox");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "firefox", false)
        );
    }

    // --- Regression tests named directly in the brief.

    #[test]
    fn explicit_prefix_with_empty_term_lists_all_of_kind() {
        let r = route("w ");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Windows, "", true)
        );
    }

    #[test]
    fn calculator_prefix_and_inferred() {
        assert_eq!(route("=2+2").mode, Mode::Calculator);
        assert!(route("=2+2").exclusive);
        let inferred = route("2+2");
        assert_eq!(inferred.mode, Mode::Calculator);
        assert!(
            !inferred.exclusive,
            "inferred math must augment, not hijack"
        );
    }

    #[test]
    fn bare_city_token_is_not_exclusive() {
        let r = route("paris");
        assert!(
            !r.exclusive,
            "typing a city name must still show apps/files (fix for extension bug B4)"
        );
    }

    // --- Coverage the JS suite didn't reach.

    #[test]
    fn prefix_a_routes_to_apps() {
        let r = route("a firefox");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Apps, "firefox", true)
        );
    }

    #[test]
    fn prefix_tz_routes_to_timezone() {
        let r = route("tz tokyo");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "tokyo", true)
        );
    }

    #[test]
    fn prefix_timezone_routes_to_timezone() {
        let r = route("timezone tokyo");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "tokyo", true)
        );
    }

    #[test]
    fn sigil_dollar_routes_to_currency() {
        // A sigil is the user naming the mode outright, so nothing shape-checks
        // what follows it — the guarantee pinned by
        // `inferred_currency_terms_carry_a_parseable_numeric_portion` covers
        // inferred routes only.
        let r = route("$100 usd to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Currency, "100 usd to eur", true)
        );
    }

    #[test]
    fn sigil_gt_routes_to_actions() {
        let r = route(">empty trash");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Actions, "empty trash", true)
        );
    }

    #[test]
    fn raw_is_untouched_original_with_leading_whitespace() {
        let r = route("  w fire");
        assert_eq!(r.term, "fire");
        assert_eq!(r.raw, "  w fire");
    }

    #[test]
    fn prefix_matching_is_case_insensitive() {
        let r = route("W fire");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Windows, "fire", true)
        );

        let r = route("WX berlin");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Weather, "berlin", true)
        );
    }

    #[test]
    fn empty_query_routes_to_all_with_empty_term() {
        let r = route("");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "", false)
        );
    }

    #[test]
    fn whitespace_only_query_routes_to_all_with_empty_term() {
        let r = route("   ");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "", false)
        );
    }

    #[test]
    fn short_alias_token_at_minimum_length_routes_to_timezone() {
        let r = route("la");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "la", false)
        );
    }

    #[test]
    fn single_char_token_below_alias_minimum_routes_to_all() {
        let r = route("x");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "x", false)
        );
    }

    #[test]
    fn compact_currency_reaches_currency_not_calculator() {
        // The math check rejects "100usd to eur" outright because it
        // contains letters outside the arithmetic character class, so
        // ordering never lets it shadow the currency check.
        let r = route("100usd to eur");
        assert_eq!(r.mode, Mode::Currency);
    }

    // --- The currency shape check is ASCII-only. See CURRENCY_RE and
    // `looks_like_currency` for why each half is.

    #[test]
    fn arabic_indic_digits_do_not_route_to_currency() {
        let r = route("١٠٠ usd to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "١٠٠ usd to eur", false)
        );
    }

    #[test]
    fn devanagari_digits_do_not_route_to_currency() {
        let r = route("१०० usd to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "१०० usd to eur", false)
        );
    }

    #[test]
    fn fullwidth_digits_do_not_route_to_currency() {
        let r = route("１００ usd to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "１００ usd to eur", false)
        );
    }

    #[test]
    fn kelvin_sign_does_not_fold_into_a_currency_code() {
        // U+212A is the only char outside ASCII whose Unicode lowercase is
        // an ASCII letter, so it alone could smuggle a non-ASCII code past
        // an ASCII-only character class.
        let r = route("100 us\u{212a} to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::All, "100 us\u{212a} to eur", false)
        );
    }

    #[test]
    fn uppercase_ascii_currency_codes_still_route_to_currency() {
        // Guards the folding in `looks_like_currency` against being dropped
        // rather than narrowed to ASCII.
        let r = route("100 USD to EUR");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Currency, "100 USD to EUR", false)
        );
    }

    #[test]
    fn non_breaking_space_still_routes_to_currency() {
        let r = route("100\u{a0}usd to eur");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Currency, "100\u{a0}usd to eur", false)
        );
    }

    #[test]
    fn inferred_currency_terms_carry_a_parseable_numeric_portion() {
        // The guarantee the currency provider is being built to lean on: it
        // may read the leading numeric portion of the term straight into an
        // f64. Asserting the mode alone would not pin that.
        //
        // Only inferred routes are checked. The `$` sigil is the user naming
        // the mode outright, so it carries no shape guarantee at all — see
        // the note on `sigil_dollar_routes_to_currency`.
        let candidates = [
            "100 usd to eur",
            "100usd to eur",
            "100.50 usd to eur",
            "100\u{a0}usd to eur",
            "١٠٠ usd to eur",
            "१०० usd to eur",
            "１００ usd to eur",
            "100 us\u{212a} to eur",
        ];

        let mut checked = 0;
        for q in candidates {
            let r = route(q);
            if r.mode != Mode::Currency {
                continue;
            }
            let numeric: String = r
                .term
                .chars()
                .take_while(|c| !c.is_ascii_alphabetic() && !c.is_whitespace())
                .collect();
            assert!(
                numeric.parse::<f64>().is_ok(),
                "routed {q:?} to currency, but its numeric portion {numeric:?} is not an f64"
            );
            checked += 1;
        }
        assert_eq!(
            checked, 4,
            "the four ASCII-digit candidates must still reach currency mode; \
             a vacuous pass above would prove nothing"
        );
    }
}
