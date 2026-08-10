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
//!
//! Exclusivity is a statement about which results are *shown*, never about
//! what was typed. Whatever named the mode — a prefix, a sigil, or the
//! trailing ` weather` — matches before any inference predicate runs and
//! returns on the spot, so an exclusive route hands its provider a term
//! nothing in this module has shape-checked: `$١٠٠ usd to eur` reaches
//! [`Mode::Currency`] with a numeric portion that is not an `f64`, and
//! `zurich weather` reaches [`Mode::Weather`] on a marker that trails the
//! term it forwards. See [`RoutedQuery`], under "An exclusive mode filters
//! results; it never checks the term's shape", for what that leaves the
//! provider owing and why checking the sigil path was rejected.

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
///
/// # A mode is not a shape check either
///
/// A separate claim from the one above, and both hold at once: a mode says
/// nothing about how the term must be escaped, *and* nothing about whether it
/// parses. [`Mode::Currency`] says the query asked for a currency conversion,
/// not that its term carries a number, and [`Mode::Calculator`] does not
/// promise an expression anything can evaluate.
///
/// Three variants are reachable both ways, and the value does not say which
/// way it was: [`Mode::Currency`], [`Mode::Calculator`] and
/// [`Mode::Timezone`] each have at least one route that checks the term —
/// against a pattern, or against the alias set — and at least one that
/// matches a marker and stops. `route("100 usd to eur")` matched digits an
/// `f64` accepts; `route("$١٠٠ usd to eur")` matched the `$` and never read
/// what followed. Both are [`Mode::Currency`], the same value with nothing on
/// it recording which route produced it, so the checked route's guarantee
/// cannot be recovered from the mode.
///
/// The remaining eight carry less still, not more. [`Mode::Windows`],
/// [`Mode::Apps`], [`Mode::Files`], [`Mode::Emoji`], [`Mode::Weather`] and
/// [`Mode::Actions`] are reachable only by an explicit route, which inspects
/// the prefix, sigil or trailing phrase that named the mode and nothing else;
/// [`Mode::All`] is what [`route`] falls back to once every predicate has
/// declined; and [`Mode::WebSearch`] is not produced at all, as above. So no
/// variant of this enum is evidence about the term — on three the evidence
/// exists on one route and is not carried, on the other eight there was never
/// any to carry. Which routes do carry a shape guarantee, and why the sigil
/// path deliberately carries none, is on [`RoutedQuery`] under "An exclusive
/// mode filters results; it never checks the term's shape".
/// # Defined in `hop-protocol`, documented here
///
/// The type itself moved to [`hop_protocol::mode`] when
/// [`DaemonMsg::QueryRouted`](hop_protocol::DaemonMsg::QueryRouted) began
/// carrying it: a mode now crosses a process boundary, and every type that
/// does is that crate's business — the same treatment
/// [`Kind`](hop_protocol::Kind) already gets. Everything above stays here
/// because it is about what [`route`] does and does not establish, which is a
/// property of this module rather than of the wire. `hop-protocol`'s copy
/// documents the two warnings a client needs when it reads the value out of a
/// frame with none of this context.
pub use hop_protocol::Mode;

/// Query text carried by a [`RoutedQuery`]: both [`RoutedQuery::term`] and
/// [`RoutedQuery::raw`] hold this type rather than a plain `String`, so a
/// value built from either field prints the same redacted marker wherever it
/// is formatted — inside the frame, or destructured out of it. Mirrors
/// `hop-protocol`'s [`hop_protocol::redaction::QueryText`], which is the
/// pattern this type extends one crate downstream (issue #83).
///
/// # Why this is not `QueryText`
///
/// Reusing `QueryText` here was considered and rejected.
/// [`Pipeline::assemble`] builds the `RoutedQuery` it hands to
/// [`crate::rank::Ranker::rank`] by substituting `alias_effect.effective_term`
/// into `term` — an alias **rewrite target**: arbitrary text out of a user's
/// config file, never bound to the wire. It can legitimately exceed
/// [`hop_protocol::limits::MAX_QUERY_TEXT`], the exact bound `QueryText::new`
/// enforces, so reusing `QueryText` for `term` would mean either refusing a
/// long alias rewrite at query time — a new failure mode on the one path
/// that is unbounded by design — or adding an unchecked constructor to
/// `QueryText`, which would discard the guarantee that type exists to carry.
/// The two values have genuinely different invariants: `QueryText` asserts a
/// bound because every byte of it crossed the wire and was checked against
/// one; `RoutedText` asserts none, on purpose, because at least one of its
/// producers never had a bound to check against. [`RoutedText::new`] is
/// therefore infallible — there is no refusal to report, ever. Do not add
/// one to make this type look more like `QueryText`; that is precisely the
/// difference between them.
///
/// [`Pipeline::assemble`]: crate::pipeline::Pipeline::assemble
///
/// # What `Debug` prints
///
/// `RoutedText(<redacted, N bytes>)`, where `N` is [`RoutedText::len`] — the
/// length of the text in bytes. The text itself never appears, `{:#?}`
/// prints the same one-line marker as `{:?}` (this `Debug` does not vary on
/// the alternate flag), and this holds for an empty value too: an empty
/// `RoutedText` still prints the marker, reporting `0 bytes`, rather than
/// looking like a value that was never redacted at all. See
/// `hop_protocol::redaction::QueryText`'s own docs for the full worked
/// reasoning; this type follows it exactly rather than re-deriving it.
///
/// # What reporting the length costs
///
/// Same trade `QueryText` makes, and for the same reason: reporting the byte
/// length rather than bucketing it is a disclosure, and it is worth pricing
/// rather than filing under "something about the value". A launcher sends a
/// `query` frame — and, downstream of it, a routed query — per keystroke, so
/// a typed secret produces a run of lengths climbing toward N one character
/// at a time, and a pasted one produces a single frame at N outright; a log
/// of redacted values tells those two apart, and in the paste case records
/// the pasted value's exact length on one line. That narrows the search
/// space for a credential, and it is not nothing. It is accepted here for
/// the same three reasons `QueryText` accepts it — the exact count is what a
/// bound refusal already reports elsewhere in this codebase, bucketing does
/// not recover the paste-versus-typing distinction (that shape is in the
/// *number* of redacted values, not in their lengths), and what this type
/// exists to close is the text, not its size — see
/// `hop_protocol::redaction::QueryText`'s "What reporting the length costs"
/// for the argument in full; nothing about it changes by moving one crate
/// downstream.
///
/// # No `Display`
///
/// Deliberately absent, exactly as `QueryText` omits it: a `Display` writing
/// the text would put it back within reach of `{}`, reached for without a
/// thought about `Debug` at all, and a `Display` writing the redacted form
/// instead would hand `{}` — what code reaches for to show a value to a
/// user — a marker instead of text, a different problem wearing the first
/// one's shape. The text is reached by name, through [`RoutedText::as_str`]
/// or [`RoutedText::into_string`], a visible act at the call site rather
/// than a formatting default. Pinned by the test
/// `tests::routed_text_does_not_implement_display`, which asserts in a
/// `const` block, so adding the impl fails the crate's test build rather
/// than silently reopening the path.
#[derive(Clone, PartialEq, Eq)]
pub struct RoutedText(String);

impl RoutedText {
    /// Builds routed text. Infallible, unlike `QueryText::new` — see this
    /// type's "Why this is not `QueryText`" for why no bound is enforced
    /// here.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The text as a string slice. This is the disclosing accessor: what it
    /// returns is a plain `&str` whose own `Debug` and `Display` print the
    /// characters, so formatting the result puts them wherever that
    /// formatting goes.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the text, yielding the string inside. Discloses as
    /// [`RoutedText::as_str`] does.
    pub fn into_string(self) -> String {
        self.0
    }

    /// The length of the text in bytes, which is what `Debug` reports.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for RoutedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RoutedText(<redacted, {} bytes>)", self.0.len())
    }
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
/// building `format!("https://api/...?q={}", q.term.as_str())` hands the
/// author of the query a free extra URL parameter. **Exclusive** is the user
/// having named the mode explicitly, not a finding that the text is fit for
/// whatever answers it.
///
/// Escaping the value for a sink is the provider's job, because only the
/// provider knows which sink it has; [`crate::provider::Provider::query`]
/// carries that contract and the reasoning for it. This type documents the
/// hazard and enforces nothing about it — both fields' text, once reached
/// through [`RoutedText::as_str`], will interpolate into anything, silently.
/// `RoutedText` redacts what formatting a field prints; it validates nothing
/// about what the text itself contains, so it closes none of this section's
/// hazard.
///
/// # An exclusive mode filters results; it never checks the term's shape
///
/// This is a different claim from the section above, and running the two
/// together is the mistake worth naming: that one is about **escaping** —
/// text that is hostile when interpolated into a sink — and this one is about
/// **shape**, whether the term parses at all. Neither implies the other.
/// `100 usd to eur` parses cleanly and still needs escaping before it reaches
/// a URL; `١٠٠ usd to eur` holds no path, shell or URL metacharacter at all
/// and still parses as nothing. Nothing in this section makes a term safe to
/// interpolate.
///
/// [`route`] tries every explicit marker first — the prefixes, the sigils and
/// the trailing ` weather` — and returns on the first match, so no inference
/// predicate ever sees a term that arrived through one. It makes no difference
/// which end of the query the marker sat at: `zurich weather` is routed by its
/// suffix and forwards the `zurich` that preceded it, as unread as the `١٠٠`
/// that follows a `$`. `exclusive` therefore means exactly that the user named
/// the mode and results are filtered to its kinds — no more:
///
/// ```text
/// "$١٠٠ usd to eur"  ->  Currency    term = "١٠٠ usd to eur"   exclusive
/// "=٢+٢"             ->  Calculator  term = "٢+٢"              exclusive
/// ```
///
/// Both are correct, and both are what the user asked for: typing the sigil
/// declares the mode whatever follows it. Neither term's numeric portion
/// parses as an `f64`. The same digits *without* a sigil reach [`Mode::All`]
/// instead, because `looks_like_currency` and `looks_like_math` both count
/// only ASCII digits — so the parseable-numeric-portion guarantee belongs to
/// the inferred currency route, never to [`Mode::Currency`] as such. Reading
/// the mode as the guarantee is how `q.term.as_str().parse::<f64>().unwrap()`
/// gets written, and that panic is two keystrokes away from any keyboard
/// that types `٢`.
///
/// Nor does inference imply a *usable* term. Each of the three inference
/// predicates checks something, but no two of them check the same kind of
/// thing. `looks_like_currency` checks the most, and the regex is the whole
/// of it: `[0-9]+(\.[0-9]+)?`, a three-letter code, `to`, a second code — so
/// the numeric portion is what `str::parse::<f64>` accepts. `looks_like_math`
/// checks an alphabet rather than an expression — it demands at least one
/// ASCII digit and rejects every character outside `0-9`, `+ - * / ( ) . %`
/// and whitespace, asking nothing about balanced parens or evaluable
/// structure. So `route("2+2x")` falls through to [`Mode::All`] on the `x`,
/// while `route("2+")` and `route("(1+")` are inferred [`Mode::Calculator`]
/// carrying terms no evaluator will accept: a digit is present, nothing
/// outside the class is, and that is the whole of the check. `infer_timezone`
/// constrains the term to the alias set on its two alias branches — the bare
/// token and the ` time` suffix — and forwards whatever was typed on the three
/// phrase-prefix ones. And the [`Mode::All`] fallback is `exclusive: false`
/// having been deduced from nothing at all, every predicate above it having
/// declined. Three predicates, three different guarantees, and not one of them
/// enough for a provider to skip parsing the term itself.
///
/// So the obligation sits with the provider, which
/// [`crate::provider::Provider::query`] records at the seam where it lands: a
/// provider parses a routed term defensively or not at all, and treats a
/// failed parse as an ordinary "no items" answer rather than an impossible
/// state. That is not a new obligation. `100 xyz to abc` satisfies the
/// currency shape check and still names no real currency pair, so even an
/// inferred route never promised the term was *semantically* usable — shape
/// was always the smaller half of what a provider has to establish.
///
/// The alternative — shape-check the sigil path, and fall through to
/// [`Mode::All`] on a malformed term — was considered and rejected (issue
/// #67). [`route`] runs on every keystroke while the currency check only
/// matches a *complete* conversion, so a checked sigil would leave `$`, `$1`,
/// `$100` and `$100 usd` all falling back to general results and snap into
/// [`Mode::Currency`] only on the final character. Avoiding that flicker would
/// mean checking the sigil path more weakly than the inferred one, at which
/// point [`Mode::Currency`] means two different things and the change has lost
/// the single-meaning advantage that motivated it.
///
/// # `Debug`-formatting this type does not print what the user typed
///
/// `RoutedQuery` derives `Debug`, and that is safe because `term` and `raw`
/// are [`RoutedText`], not plain `String`s: `format!("{q:?}")` — or a
/// `tracing` call capturing `?q` — prints each as `RoutedText(<redacted, N
/// bytes>)` rather than the characters. `hop-protocol`'s `QueryText` redacts
/// the same text one crate upstream, for `ClientMsg::Query.text`; that
/// redaction used to stop at [`route`], which takes a `&str` and used to
/// hand the text straight to a `String` field. Issue #83 closed the gap by
/// giving `RoutedText` the same shape `QueryText` has rather than by
/// widening `route`'s signature — see [`RoutedText`]'s own docs for why it
/// is not `QueryText` itself. Formatting a `RoutedQuery` is safe as a
/// result, but reaching into either field with [`RoutedText::as_str`] or
/// [`RoutedText::into_string`] and formatting *that* is exactly as unsafe as
/// it always was: the type only redacts while it is still the type.
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
    /// query string. [`RoutedText::as_str`] is how a caller that has decided
    /// it is safe to do so reaches the characters.
    pub term: RoutedText,
    /// `true` only when the user named the mode outright — an explicit
    /// prefix, a sigil, or a trailing phrase. An exclusive route should
    /// replace the general search; a non-exclusive (inferred) route should
    /// augment it.
    ///
    /// It records how the mode was chosen and which results are shown, and
    /// nothing about `term`: whichever marker named the mode decided the
    /// route on its own, and no predicate ever ran on the text it left behind
    /// — which sits before the marker on the ` weather` suffix and after it
    /// on every other explicit route. `false` is not the converse and does
    /// not mean checked: the [`Mode::All`] fallback carries it having been
    /// deduced from nothing, and `infer_timezone`'s phrase-prefix branches
    /// carry it having checked only the phrase. See the type's docs, "An
    /// exclusive mode filters results; it never checks the term's shape",
    /// before reading `mode` as a promise about what `term` holds.
    pub exclusive: bool,
    /// The untouched original input, exactly as passed to [`route`] — and
    /// untrusted exactly as `term` is, having had not even the trim applied:
    /// see the type's docs. Also [`RoutedText`], for the same reason `term`
    /// is: `raw` holds whatever the user typed in full, with nothing
    /// stripped, so it discloses under `Debug` exactly as much as `term`
    /// does and needs the same redaction.
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
    pub raw: RoutedText,
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
/// provider a term this predicate had just called a well-formed conversion,
/// with a numeric portion that was not an `f64` — a match here is the whole of
/// what makes that portion parseable, so accepting a digit `parse` rejects
/// broke the one guarantee inferring the mode carries. `[0-9]` is also what
/// [`looks_like_math`] means by a digit, so [`looks_like_currency`] and
/// [`looks_like_math`] now agree on what a number is.
///
/// That guarantee is this predicate's, not [`Mode::Currency`]'s. A `$` sigil
/// reaches the same mode without this pattern ever running, and carries no
/// such promise; [`RoutedQuery`]'s "An exclusive mode filters results; it
/// never checks the term's shape" is where that split is argued.
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
        term: RoutedText::new(term.trim()),
        exclusive: true,
        raw: RoutedText::new(raw),
    }
}

fn inferred(mode: Mode, term: &str, raw: &str) -> RoutedQuery {
    RoutedQuery {
        mode,
        term: RoutedText::new(term.trim()),
        exclusive: false,
        raw: RoutedText::new(raw),
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
        assert_eq!(r.term.as_str(), "fire");
        assert_eq!(r.raw.as_str(), "  w fire");
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
    fn explicit_sigils_forward_an_unparseable_term_unchanged() {
        // Pins issue #67's decision — option 1, providers validate their own
        // term — against a later change that quietly implements option 2. An
        // exclusive route is a *filtering* contract: the sigil matched before
        // any inference predicate ran, so nothing shape-checked what follows
        // it, and both terms below reach a mode whose provider cannot parse
        // them. That is the designed behavior, not a defect.
        //
        // Option 2 (shape-check the sigil path, fall through to `Mode::All`
        // when the term is malformed) was rejected because `route` runs on
        // every keystroke while the currency predicate only matches a
        // *complete* conversion: `$`, `$1`, `$100` and `$100 usd` would each
        // drop the user back to general results, snapping into currency mode
        // only on the final character. A weaker check on the sigil path would
        // avoid the flicker only by making `Mode::Currency` mean two different
        // things, which is the one thing option 2 was for. `=` is pinned here
        // beside `$` because the answer is one rule for every sigil: exclusive
        // has to mean the same thing whichever prefix set it, and `$` is the
        // case that forced the rule.
        //
        // The inferred half of the pair is pinned separately: the same digits
        // without a sigil reach `Mode::All`, by
        // `arabic_indic_digits_do_not_route_to_currency` in the ASCII-only
        // section below.
        let currency = route("$١٠٠ usd to eur");
        assert_eq!(
            (currency.mode, currency.term.as_str(), currency.exclusive),
            (Mode::Currency, "١٠٠ usd to eur", true)
        );

        let calculator = route("=٢+٢");
        assert_eq!(
            (
                calculator.mode,
                calculator.term.as_str(),
                calculator.exclusive
            ),
            (Mode::Calculator, "٢+٢", true)
        );

        // Asserting the routes alone would restate the routing table. What
        // this test is for is that both terms fail the parse a provider
        // reading `Mode::Currency` or `Mode::Calculator` as a shape guarantee
        // would reach for — the panic that guarantee would have licensed.
        for term in [currency.term.as_str(), calculator.term.as_str()] {
            let numeric: String = term
                .chars()
                .take_while(|c| !c.is_ascii_alphabetic() && !c.is_whitespace())
                .collect();
            assert!(
                numeric.parse::<f64>().is_err(),
                "{term:?} reached its mode through a sigil, so its numeric portion \
                 {numeric:?} must not be assumed parseable — if it now parses, the \
                 sigil path has started shape-checking and this contract changed"
            );
        }
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
                .as_str()
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

    // --- Issue #83: `RoutedText` redacts `term` and `raw` under `Debug`.

    /// A value distinctive enough that finding it in formatted output is
    /// finding this value and not a coincidence.
    const TYPED: &str = "correct horse battery staple";

    #[test]
    fn routed_text_debug_reports_a_marker_and_a_byte_count_instead_of_the_text() {
        let text = RoutedText::new(TYPED);
        assert_eq!(
            format!("{text:?}"),
            format!("RoutedText(<redacted, {} bytes>)", TYPED.len())
        );
    }

    #[test]
    fn routed_text_accessors_round_trip_the_text_unchanged() {
        let text = RoutedText::new(TYPED);
        assert_eq!(text.as_str(), TYPED);
        assert_eq!(text.clone().into_string(), TYPED);
        assert_eq!(text.len(), TYPED.len());
        assert!(!text.is_empty());
        assert!(RoutedText::new("").is_empty());
    }

    #[test]
    fn routed_text_accepts_a_term_longer_than_max_query_text() {
        // The alias-rewrite case (`Pipeline::assemble`'s `effective_term`,
        // built from `alias_effect.effective_term`) is arbitrary text out of
        // a user's config file, not bound to the wire — `QueryText::new`
        // would refuse this, and `RoutedText::new` must not.
        let long = "a".repeat(hop_protocol::limits::MAX_QUERY_TEXT + 1);
        let text = RoutedText::new(&long);
        assert_eq!(text.as_str(), long);
    }

    #[test]
    fn routing_a_distinctive_query_does_not_reveal_it_in_debug() {
        // The issue's own acceptance criterion, run against the whole
        // `RoutedQuery` — not just `term`. `raw` carries the same typed text
        // (route() strips nothing from it), so this only holds if `raw` is
        // redacted too.
        let routed = route(TYPED);
        let debug = format!("{routed:?}");
        assert!(!debug.contains(TYPED), "got: {debug}");
    }

    /// Answers "does `T` implement [`fmt::Display`]?" as a value, the same
    /// probe `hop_protocol::redaction`'s `QueryText` tests use: an inherent
    /// associated constant and a blanket trait one on the same name, so the
    /// inherent one wins where it exists.
    struct DisplayProbe<T>(std::marker::PhantomData<T>);

    trait MaybeDisplay {
        const IMPLEMENTS_DISPLAY: bool = false;
    }

    impl<T> MaybeDisplay for DisplayProbe<T> {}

    impl<T: std::fmt::Display> DisplayProbe<T> {
        const IMPLEMENTS_DISPLAY: bool = true;
    }

    #[test]
    fn routed_text_does_not_implement_display() {
        // Both are const blocks, so this fails at compile time rather than
        // at run time: adding the impl stops the crate's tests building.
        //
        // `String` is the control: it does implement `Display`, so a probe
        // that answered `false` for everything would fail here rather than
        // let the assertion below pass for the wrong reason.
        const {
            assert!(
                DisplayProbe::<String>::IMPLEMENTS_DISPLAY,
                "the probe reports no Display for a type that has one"
            );
        }
        const {
            assert!(
                !DisplayProbe::<RoutedText>::IMPLEMENTS_DISPLAY,
                "RoutedText must not implement Display; see its docs for why"
            );
        }
    }
}
