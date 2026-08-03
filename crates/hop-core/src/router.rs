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
///
/// # A mode is not a sink, and no mode is the safe one
///
/// A mode is an interpretation of a query, and it says nothing about how the
/// term routed under it must be escaped. The sink is a property of the
/// *provider*: whichever provider answers is what decides whether the term
/// becomes a path, an argv element or a URL, so a mode says only which
/// providers were asked. [`crate::provider::Provider::query`] carries the
/// escaping contract, and [`RoutedQuery`] documents why its fields need one.
///
/// [`Mode::All`] is where this matters most, and it is the opposite of a safe
/// default. A provider that wants to answer ordinary, unprefixed search
/// **must** list `All` among its modes or it is never asked at all — see
/// [`ProviderManifest::modes`](crate::provider::ProviderManifest::modes) — so
/// `All` is the mode under which the most sinks are reachable at once, a
/// files provider's path sink and an actions provider's command sink
/// included, and it is the mode most providers will actually serve.
///
/// Four variants do let a sink be named without knowing which provider
/// answers: [`Mode::Files`] implies a path sink, [`Mode::Actions`] a command
/// sink, and [`Mode::Weather`] and [`Mode::WebSearch`] HTTP/URL sinks —
/// `hop-protocol`'s `ExecOutcome::OpenUrl` is already the outcome that opens
/// one, and [`Mode::WebSearch`] is not yet produced by [`route`] (see the
/// variant). That makes the sink easier to *guess* on those routes. It does
/// not make it absent on the others.
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
///
/// # Both string fields are unvalidated, untrusted input
///
/// [`route`] chooses a mode, and removes the text that named it where any
/// did. It applies no
/// **content rule**, no escaping and no **refusal**. Exactly one gate has run
/// upstream of this struct, and only for text that arrived off the wire:
/// `hop-protocol`'s `MAX_QUERY_TEXT` **bound**, which `QueryText` applies at
/// the deserialization boundary to refuse an over-long
/// `ClientMsg::Query.text`. A bound restricts how long a value may be and
/// says nothing whatever about what it may contain — and [`route`] takes a
/// `&str`, so even that much is a fact about one caller rather than a
/// guarantee this struct carries.
///
/// Treat `term` and `raw` alike as hostile text: the input box takes pastes,
/// so what lands in it was not necessarily composed by the person sitting in
/// front of it.
///
/// The two fields are untrusted in *different shapes*, and confusing them is
/// the mistake this warning exists to prevent:
///
/// - `term` has been trimmed. Beyond the trim it has had a prefix or suffix
///   removed where one named the mode, and on `infer_timezone`'s two alias
///   branches it has been replaced outright by the alias key it matched.
/// - `raw` has had none of that: not trimmed, not stripped, not
///   canonicalized. It is the whole input, including everything `term` had
///   removed, and it is no cleaner for being unmodified.
///
/// Stripping and **exclusive** are independent, which is the easy thing to
/// get backwards. An **exclusive** route always strips what named the mode.
/// An **inferred** route strips on `infer_timezone`'s phrase branches
/// (`time in `, `time `, `now in `, and the ` time` suffix — every one of
/// them `exclusive: false`) and strips nothing on the shape-inferred ones, a
/// bare sum or a bare currency amount, which reach `term` as typed but for
/// the trim. Either way none of it is sanitizing:
/// `f ../../../../etc/passwd` loses `f ` and keeps the traversal, so the
/// routed side is not a cleaned side, whatever the name suggests.
///
/// The one place a term is constrained to a *known set* is those two alias
/// branches, which forward a key from the fixed `TIMEZONE_ALIASES` set
/// instead of the spelling that was typed. Stripping narrows a term too, on
/// the routes that strip, but only by removing what named the mode — it
/// constrains nothing about the rest. And even the alias constraint is a
/// property of those two branches rather than of [`Mode::Timezone`] or of
/// this type: `tz `, `timezone `, `time in `, `time ` and `now in ` all reach
/// the same mode carrying whatever followed them, and nothing on a
/// `RoutedQuery` says which branch produced it. Do not read a timezone route
/// as a checked one.
///
/// Worked examples, all of them faithfully forwarded and all **exclusive**,
/// so results are filtered to that mode's kinds and nothing else shows:
///
/// ```text
/// "f ../../../../etc/passwd"  ->  Files    term = "../../../../etc/passwd"
/// "wx Berlin&key=leak"        ->  Weather  term = "Berlin&key=leak"
/// "> rm -rf /"                ->  Actions  term = "rm -rf /"
/// ```
///
/// The second is the concrete parameter-injection shape: a weather provider
/// building `format!("https://api/...?q={}", q.term)` hands the author of the
/// query a free extra URL parameter. **Exclusive** is the user having named
/// the mode explicitly, not a finding that the text is fit for whatever
/// answers it.
///
/// Escaping the value for a sink is the provider's job, because only the
/// provider knows which sink it has; [`crate::provider::Provider::query`]
/// carries that contract and the reasoning for it. This type documents the
/// hazard and enforces nothing about it — both fields are plain `String`s
/// that will interpolate into anything, silently.
///
/// # Debug-formatting this type prints what the user typed
///
/// `RoutedQuery` derives `Debug` and holds `term` and `raw` as plain
/// `String`s, so `format!("{q:?}")` — or a `tracing` call capturing `?q` —
/// discloses the query verbatim. `hop-protocol`'s `QueryText` redacts the
/// same text one crate upstream, but that redaction stops at [`route`], which
/// takes a `&str`. Issue #83 is open on the gap; this type does **not** close
/// it, so do not treat a `RoutedQuery` as safe to format into a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedQuery {
    pub mode: Mode,
    /// The query with any recognized prefix/suffix stripped, and trimmed —
    /// plus, where routing matched a known key rather than just a shape, the
    /// canonical form of that key: an alias-matched timezone route carries
    /// the alias key it matched (lowercased, whitespace runs collapsed to
    /// `_`) rather than the spelling that was typed. See `infer_timezone` in
    /// this module for why that route forwards the key and the phrase-prefix
    /// routes do not.
    ///
    /// Trimmed, sometimes stripped, never sanitized: see the type's docs
    /// before interpolating this into a path, an argv element, a URL or a
    /// query string.
    pub term: String,
    /// `true` only when the user typed an explicit prefix or sigil. An
    /// exclusive route should replace the general search; a non-exclusive
    /// (inferred) route should augment it.
    pub exclusive: bool,
    /// The untouched original input, exactly as passed to [`route`] — and
    /// untrusted exactly as `term` is, having had not even the trim applied:
    /// see the type's docs.
    ///
    /// **No code consults this field's value, and no prospective consumer is
    /// known.** Checked when this comment was written: the only read in the
    /// workspace is this module's own
    /// `raw_is_untouched_original_with_leading_whitespace` test —
    /// `Pipeline::assemble` carries the field through a struct-update
    /// expression without ever looking at it — and none of the M2 slices that
    /// build against this seam (#54 through #60) names a use for the raw
    /// query as against the term: M2's providers are apps (#57) and
    /// calculator (#58), and neither brief mentions the raw query at all. So
    /// this documents an *absence* rather than an intended consumer; do not
    /// read it as naming one.
    ///
    /// It is kept rather than deleted for two reasons. First, `CONTEXT.md`
    /// defines **raw query** as a domain term and names this field as its
    /// carrier, exactly as it names `term` for **term**; deleting the field
    /// would strand a defined concept, which is a vocabulary change belonging
    /// to the domain model. Second, [`crate::provider::Provider::query`]
    /// receives a `&RoutedQuery` and nothing else, so a provider that needs
    /// the text as typed — whatever prefix or sigil named the mode, where one
    /// did, or the whitespace, or the spelling an alias branch canonicalized
    /// away — has no other way to reach it once this field is gone.
    ///
    /// If M2 ends with this still unread, that is the point to settle the
    /// question rather than carry it further: either a provider names the
    /// field, or it and `CONTEXT.md`'s **raw query** entry are retired
    /// together, as one change.
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
/// so [`looks_like_currency`] and [`looks_like_math`] now agree on what a
/// number is.
///
/// Those are two of this file's three inference predicates; the agreement
/// stops there. [`infer_timezone`] still normalizes with full `to_lowercase`,
/// which folds U+212A KELVIN SIGN to an ASCII `k` — so a `tokyo` spelled with
/// one still matches the alias set. Do not read this file as uniformly
/// ASCII-folded. Why that fold is left as it is belongs with the calls that
/// make it, and is recorded on [`infer_timezone`].
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
///
/// An alias match returns the alias key it matched, so the term can differ
/// from what the user typed; the phrase-prefix forms return the term as
/// typed. The comment on the alias branches says why.
///
/// Both alias branches normalize with full `to_lowercase` rather than
/// `to_ascii_lowercase`, which folds U+212A KELVIN SIGN to an ASCII `k`: a
/// `tokyo` spelled with one matches the alias key and routes as a timezone.
/// That widened match is not known to be wanted, and nothing depends on it.
/// It is left alone because narrowing the fold would change *which queries
/// match at all*, where forwarding the matched key only changed which term a
/// match carries — a different and larger question than the one this
/// function was last edited to answer. [`CURRENCY_RE`]'s doc records this as
/// the one place the file is not ASCII-folded.
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

    // Both alias branches forward the normalized token, not the spelling it
    // was normalized from: the term must be the representation that
    // authorized the route. Deciding on one representation and forwarding
    // another is what let a `sao paulo` written with U+00A0 match the key
    // `sao_paulo` and still hand the provider that char, and `PST` match
    // `pst` and still hand it the uppercase.
    //
    // Normalizing is deliberately *not* extended to the phrase-prefix
    // branches above. Those are authorized by the prefix the user typed, and
    // their term is never checked against the alias set — or against anything
    // else — so they have no matched representation to forward. The visible
    // asymmetry (`sao paulo` comes back canonical, `time in São Paulo` comes
    // back as typed) is that difference showing through, not an oversight.
    if let Some(prefix_part) = strip_suffix_ci(trimmed, " time") {
        let token = collapse_whitespace(&prefix_part.trim().to_lowercase());
        if token.chars().count() >= 2 && TIMEZONE_ALIASES.contains(token.as_str()) {
            return Some(token);
        }
    }

    let whole = collapse_whitespace(&trimmed.to_lowercase());
    if whole.chars().count() >= 2 && TIMEZONE_ALIASES.contains(whole.as_str()) {
        return Some(whole);
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

    // --- The worked examples on `RoutedQuery`, pinned.

    #[test]
    fn sink_shaped_payloads_are_forwarded_verbatim_and_exclusively() {
        // Not a behavior change and not a defect report: routing is supposed
        // to forward these, and the provider is supposed to escape them for
        // its own sink. This exists because `RoutedQuery`'s docs quote these
        // three routes to a provider author as the reason to distrust the
        // term, so a later edit that changed any of them would leave that
        // warning describing a router the tree no longer has.
        let traversal = route("f ../../../../etc/passwd");
        assert_eq!(
            (traversal.mode, traversal.term.as_str(), traversal.exclusive),
            (Mode::Files, "../../../../etc/passwd", true)
        );

        let url_param = route("wx Berlin&key=leak");
        assert_eq!(
            (url_param.mode, url_param.term.as_str(), url_param.exclusive),
            (Mode::Weather, "Berlin&key=leak", true)
        );

        let command = route("> rm -rf /");
        assert_eq!(
            (command.mode, command.term.as_str(), command.exclusive),
            (Mode::Actions, "rm -rf /", true)
        );
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
        // Green before this commit as well as after: `\s` was left
        // Unicode-aware on purpose, so nothing about NBSP changed here. The
        // test pins that deliberate exception against a later sweep that
        // narrows the whole pattern to ASCII on the assumption it was missed.
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
        // Only inferred routes are checked. A `$` sigil is the user naming the
        // mode outright, so nothing shape-checks what follows it and it carries
        // no such guarantee.
        //
        // Every candidate here must both reach currency mode and parse. The
        // terms that must *not* reach it are pinned by name in the tests
        // above, so listing them here too would only weaken this one into
        // "whatever routed must parse".
        let candidates = [
            "100 usd to eur",
            "100usd to eur",
            "100.50 usd to eur",
            "100\u{a0}usd to eur",
        ];

        for q in candidates {
            let r = route(q);
            assert_eq!(r.mode, Mode::Currency, "{q:?} must reach currency mode");
            let numeric: String = r
                .term
                .chars()
                .take_while(|c| !c.is_ascii_alphabetic() && !c.is_whitespace())
                .collect();
            assert!(
                numeric.parse::<f64>().is_ok(),
                "routed {q:?} to currency, but its numeric portion {numeric:?} is not an f64"
            );
        }
    }

    // --- An alias-matched timezone route forwards the alias key it matched.
    // See `infer_timezone` for why the phrase-prefix branches do not.

    #[test]
    fn non_breaking_space_alias_forwards_the_matched_alias_key() {
        // The alias set holds no NBSP; `sao_paulo` is what authorized this
        // route, so `sao_paulo` is what the provider must be handed.
        let r = route("sao\u{a0}paulo");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "sao_paulo", false)
        );
    }

    #[test]
    fn uppercase_alias_forwards_the_matched_alias_key() {
        let r = route("PST");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "pst", false)
        );
    }

    #[test]
    fn city_time_suffix_alias_forwards_the_matched_alias_key() {
        // The ` time` suffix branch reaches the alias set by the same route
        // as the bare-token branch, so it owes the same term.
        let r = route("Sao\u{a0}Paulo Time");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "sao_paulo", false)
        );
    }

    #[test]
    fn kelvin_sign_alias_forwards_the_ascii_alias_key() {
        // U+212A KELVIN SIGN folds to an ASCII `k` under `to_lowercase`, so
        // this still reaches `tokyo` — see `infer_timezone`'s doc for why
        // that widened match stands. Forwarding the key is what keeps the
        // char out of the term, and this pins that half.
        let r = route("to\u{212a}yo");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "tokyo", false)
        );
    }

    #[test]
    fn phrase_prefix_forwards_the_term_as_typed() {
        // Green before the alias branches started forwarding their key as
        // well as after: this guards the asymmetry against a later sweep
        // reading it as an oversight.
        let r = route("time in São Paulo");
        assert_eq!(
            (r.mode, r.term.as_str(), r.exclusive),
            (Mode::Timezone, "São Paulo", false)
        );
    }
}
