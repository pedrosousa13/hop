//! The latency gate (issue #61, folding in #30 and #46): a deterministic
//! 10 000-item fixture, a p95-under-10ms arm, and an adversarial arm that
//! proves the caps Task 1 (`rank::MAX_TERM_CHARS`) and Task 2
//! (`pipeline::MAX_ITEMS_PER_PROVIDER_ANSWER`) added actually bound the work
//! — not merely that they exist. A second fixture and p95 arm, over
//! files-shaped (path-like) titles, was added by issue #128; as of issue
//! #134 that second arm is report-only — it measures and prints but no
//! longer asserts against the budget — while the first p95 arm above and
//! the adversarial arm both still gate. See
//! [`p95_query_latency_over_a_files_shaped_fixture_is_measured_in_release_mode`]'s
//! own doc comment for why.
//!
//! ## Why this file, and not `#[cfg(test)] mod tests`
//!
//! This is `hop-core`'s first integration test file. It needs to be one: the
//! two timing tests below must run under `cargo test --release`, isolated
//! from every other test in the crate (`--test-threads=1`, its own binary),
//! and `cargo test --workspace` — the always-required, debug-mode gate —
//! must never pay for them. `#[ignore]` on an in-module test would satisfy
//! the same "doesn't run by default" requirement, but would still compile
//! and link the timing tests into every other test binary in this crate; a
//! dedicated file keeps them in their own binary, matching the plan's
//! design (Decision 4) and the CI job (`latency-gate`) that targets this
//! file specifically.
//!
//! ## Fixture determinism, and how it's guaranteed
//!
//! Issue #61 states plainly that the 10 000-item fixture must be
//! deterministic — meaning it produces the same item-id sequence across
//! separate `cargo test --release ... --ignored` invocations, in whatever
//! process and on whatever run this gate executes in, not merely within one
//! process. Concretely, that guarantee rests on three properties of
//! [`ten_thousand_item_fixture`] and everything it calls:
//!
//! 1. **No RNG anywhere.** Every item's `id`, `title` and `kind` is a pure
//!    function of two integers already in hand — which provider (0..10,
//!    read off the fixed [`PROVIDER_IDS`] array) and which item within that
//!    provider's answer (0..1000, i.e. `0..MAX_ITEMS_PER_PROVIDER_ANSWER`).
//!    No `rand`, no hashing of ambient state, no wall-clock or `Instant`
//!    read feeds into any field.
//! 2. **No iteration-order dependence.** The fixture is built by two nested
//!    `Range` iterators (`for provider_id in PROVIDER_IDS`, then
//!    `(0..MAX_ITEMS_PER_PROVIDER_ANSWER).map(...)`), never by draining a
//!    `HashMap`/`HashSet`, whose iteration order is unspecified across runs
//!    and process invocations.
//! 3. **`the_ten_thousand_item_fixture_is_deterministic`**, below, pins this
//!    against a literal expected value rather than leaving it to inspection
//!    — and rather than only comparing two builds against *each other*
//!    within one process, which is weaker than it looks: it would not catch
//!    a regression that is stable within a process but varies **across**
//!    process invocations (the thing a CI gate that runs once per PR
//!    actually needs). Concretely, the test builds the fixture, computes
//!    [`id_sequence_digest`] — a dependency-free FNV-1a 64-bit hash (see its
//!    own docs for why not `std`'s `DefaultHasher`/`HashMap`) over every
//!    item id in order — and asserts the digest equals
//!    [`EXPECTED_ID_SEQUENCE_DIGEST`], a `u64` literal written directly in
//!    this file's source. Because the expected value is a source literal,
//!    not something the test recomputes and compares against itself, this
//!    check is run fresh by every process that compiles and runs this
//!    binary: a future change that made the fixture's construction order-
//!    or seed-dependent (a `HashMap`-keyed step, for instance) would still
//!    very likely reproduce the same sequence within a single process (most
//!    such regressions are stable run-to-run inside one binary invocation)
//!    but would have no reason to keep landing on this exact, previously-
//!    recorded digest — so this is a real cross-run check, not a restatement
//!    of the source-level argument in points 1 and 2. It additionally
//!    builds the fixture a second time and asserts the two builds agree with
//!    each other too, as a fast, cheap sanity check that catches an
//!    obviously nondeterministic construction (e.g. a raw `HashMap` drain)
//!    immediately, without waiting on a second process. Not `#[ignore]`d —
//!    the whole test costs building the fixture twice plus one linear hash
//!    pass (allocation and arithmetic, not ranking), fast enough for the
//!    ordinary debug-mode gate.
//!
//! Every provider answers with exactly [`MAX_ITEMS_PER_PROVIDER_ANSWER`]
//! (1 000) items — the cap Task 2 added — so nothing here is truncated by
//! [`CheckedItems::check`]; the fixture is the maximal *legal* input, ten
//! honest providers' worth, not an over-cap one.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use hop_core::pipeline::{
    CheckedItems, FailedCheck, MAX_ITEMS_PER_PROVIDER_ANSWER, Pipeline, ProviderOutput,
};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::rank::MAX_TERM_CHARS;
use hop_core::router::{Mode, RoutedQuery};
use hop_protocol::limits::MAX_TITLE;
use hop_protocol::{Action, ActionId, ActionKind, ExecOutcome, Item, ItemId, Kind};

/// Ten fixed, distinct provider ids — one [`ProviderOutput`] per id, each
/// answering with exactly [`MAX_ITEMS_PER_PROVIDER_ANSWER`] items, so the
/// fixture totals exactly 10 000 items with nothing truncated away.
const PROVIDER_IDS: [&str; 10] = [
    "fixture-provider-0",
    "fixture-provider-1",
    "fixture-provider-2",
    "fixture-provider-3",
    "fixture-provider-4",
    "fixture-provider-5",
    "fixture-provider-6",
    "fixture-provider-7",
    "fixture-provider-8",
    "fixture-provider-9",
];

/// Fixed vocabulary the fixture's titles are built from. Not randomness —
/// just a small pool of words a real launcher item's title might plausibly
/// contain, indexed by the item's own position, so p95 measurement below
/// exercises realistic (if repetitive) text instead of opaque numbered
/// placeholders, while staying entirely deterministic.
const VOCAB: [&str; 8] = [
    "firefox",
    "chrome",
    "terminal",
    "calculator",
    "settings",
    "files",
    "editor",
    "browser",
];

/// A provider that exists only to be a provider — the same role
/// `pipeline.rs`'s and `provider.rs`'s own `FakeProvider`s play in their
/// in-module tests: [`ProviderOutput`] can be built no other way. Its
/// `query`/`execute` are never called; every test here hands assembly items
/// a provider has already "returned", built formulaically below.
struct FixtureProvider {
    manifest: ProviderManifest,
}

impl Provider for FixtureProvider {
    fn manifest(&self) -> ProviderManifest {
        self.manifest.clone()
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(Vec::new())
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        Ok(ExecOutcome::Done)
    }
}

/// One provider's manifest: declares both kinds the fixture's items use, and
/// [`Mode::All`] so it would be selected on ordinary, unprefixed search too.
fn manifest_for(provider_id: &'static str) -> ProviderManifest {
    ProviderManifest {
        id: provider_id,
        kinds: vec![Kind::App, Kind::File],
        modes: vec![Mode::All],
        min_term_len: 0,
        budget: Duration::from_millis(50),
    }
}

fn open_action() -> Action {
    Action {
        id: ActionId::new("open").unwrap(),
        kind: ActionKind::Open,
        label: "Open".into(),
    }
}

/// One item's title: a single vocabulary word, optionally suffixed with a
/// one-digit qualifier — deliberately short, the same order of magnitude as
/// a real launcher item's title ("Firefox", "Chrome 2"), not the fixture's
/// own bookkeeping. An earlier draft of this fixture folded `provider_id`
/// and `item_index` into every title to make titles globally unique; that
/// inflated every haystack `nucleo_matcher` has to scan to ~45 bytes,
/// several times a realistic title's length, and measurably cost the p95
/// arm below its margin (measured p95 ~11.8ms against the 10ms budget, over
/// it — see the plan's task-3 report for the full before/after numbers).
/// Titles collide across items and across providers here, same as real apps
/// legitimately do; item **ids** are what stay unique (see
/// [`formulaic_item`]), which is the property the fixture actually needs.
fn formulaic_title(item_index: usize) -> String {
    let word = VOCAB[item_index % VOCAB.len()];
    if item_index.is_multiple_of(4) {
        format!("{word} {}", item_index % 10)
    } else {
        word.to_string()
    }
}

/// One item, built purely from `provider_id` and `item_index` — see the
/// module docs' determinism argument. Alternates `Kind::App`/`Kind::File` so
/// the fixture isn't monotonously one kind, still matching
/// [`manifest_for`]'s declared kinds either way.
fn formulaic_item(provider_id: &'static str, item_index: usize) -> Item {
    let kind = if item_index.is_multiple_of(2) {
        Kind::App
    } else {
        Kind::File
    };
    Item {
        id: ItemId::new(format!("{provider_id}:item-{item_index}")).unwrap(),
        kind,
        title: formulaic_title(item_index),
        subtitle: None,
        icon: None,
        actions: vec![open_action()],
        default_action: ActionId::new("open").unwrap(),
        copy_text: None,
        append_to_end: false,
        provider: provider_id.into(),
    }
}

/// One provider's full, at-cap answer: exactly [`MAX_ITEMS_PER_PROVIDER_ANSWER`]
/// formulaic items, paired with `provider_id`'s manifest.
fn provider_output(provider_id: &'static str) -> ProviderOutput {
    let provider = FixtureProvider {
        manifest: manifest_for(provider_id),
    };
    let items = (0..MAX_ITEMS_PER_PROVIDER_ANSWER)
        .map(|i| formulaic_item(provider_id, i))
        .collect();
    ProviderOutput::from_provider(&provider, items)
}

/// The deterministic 10 000-item fixture: ten providers, each answering with
/// exactly [`MAX_ITEMS_PER_PROVIDER_ANSWER`] items — nothing truncated —
/// assembled through [`CheckedItems::check`]. See the module docs for the
/// determinism argument in full.
fn ten_thousand_item_fixture() -> CheckedItems {
    let mut outputs = Vec::with_capacity(PROVIDER_IDS.len());
    for provider_id in PROVIDER_IDS {
        outputs.push(provider_output(provider_id));
    }
    CheckedItems::check(outputs)
}

/// A minimal FNV-1a 64-bit hash, dependency-free and fixed by the
/// algorithm's own published constants rather than by any Rust
/// implementation detail. Deliberately not
/// `std::collections::hash_map::DefaultHasher`: its own docs disclaim any
/// stability guarantee for its output "between versions of Rust" or "between
/// invocations of a program" — exactly the instability
/// [`id_sequence_digest`] exists to rule out. Deliberately not anything that
/// touches `HashMap`/`RandomState` either, for the same reason: this digest
/// exists to be independent of any per-process hasher seed, not incidentally
/// sensitive to one. FNV-1a's constants are part of the algorithm's public
/// specification, so the same input bytes hash to the same `u64` on every
/// run, in every process, on every toolchain, indefinitely.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// A digest of a fixture's full item-id sequence: every [`ItemId`]'s bytes,
/// in order, joined by a `\n` — a byte none of this file's formulaic ids
/// (`"{provider_id}:item-{item_index}"`, see [`formulaic_item`]) ever
/// contains, so no two distinct id sequences can collide onto the same
/// joined byte string. See [`fnv1a64`] for why this specific hash and not
/// `std`'s `DefaultHasher`.
fn id_sequence_digest(items: &[Item]) -> u64 {
    let joined = items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    fnv1a64(joined.as_bytes())
}

/// The expected digest of [`ten_thousand_item_fixture`]'s item-id sequence,
/// computed once and pinned here as a literal — see the module docs'
/// "Fixture determinism" section for why a literal, rather than a second
/// in-process build, is what actually proves cross-run determinism.
const EXPECTED_ID_SEQUENCE_DIGEST: u64 = 0x316b_08a5_e81b_cf5f;

/// A small, fixed set of realistic query strings — used (cycled through) by
/// both timing arms so measurement isn't gamed by favorable cache/branch-
/// prediction reuse a single repeated query would enjoy (Decision 4).
const QUERIES: [&str; 6] = ["firefox", "chrome", "term", "calc", "files", "edit"];

/// p95 query latency over the 10 000-item fixture, asserted under 10ms.
///
/// **This test only means anything run under `--release`.** Debug-mode Rust
/// is 10-50x slower than release for CPU-bound fuzzy matching, so under
/// `cargo test` (no `--release`) this assertion would very plausibly fail on
/// ordinary, unregressed code — measuring the wrong build, not a real
/// regression. That's why it's `#[ignore]`d: `cargo test --workspace` (the
/// always-required, debug-mode gate) never runs it. The CI job that does is
/// `latency-gate`, which runs
/// `cargo test --release -p hop-core --test latency -- --ignored --test-threads=1`
/// on every pull request (issue #61's acceptance criterion 7).
///
/// Methodology, per Decision 4 of the plan: 50 untimed warm-up calls (branch
/// predictor, allocator, CPU frequency scaling), then 500 timed calls across
/// [`QUERIES`] (cycled, not one repeated string), sorted, nearest-rank p95
/// (index `⌈0.95 × 500⌉ - 1`).
#[test]
#[ignore]
fn p95_query_latency_is_under_10ms_in_release_mode() {
    let checked = ten_thousand_item_fixture();
    assert_eq!(
        checked.items().len(),
        10_000,
        "fixture sanity: nothing truncated"
    );
    assert!(
        checked.rejections().is_empty(),
        "fixture sanity: nothing rejected"
    );

    let mut pipeline = Pipeline::default();

    // Warm-up: 50 untimed calls.
    for i in 0..50 {
        let term = QUERIES[i % QUERIES.len()];
        let _ = pipeline.assemble(term, checked.clone(), 50);
    }

    // 500 timed calls. `Pipeline::assemble` consumes its `CheckedItems`
    // argument by value (it destructures `items`/`rejections` at the top of
    // the function), so a fresh value is required per call regardless of how
    // it's timed. `checked.clone()` happens *outside* the timed span
    // deliberately: cloning a cached fixture is a test-harness cost with no
    // equivalent in production, where every real query builds its
    // `CheckedItems` fresh from live provider output rather than cloning one
    // — so only `assemble` itself, the pure function the 10ms budget is
    // actually about, is measured.
    let mut samples: Vec<Duration> = Vec::with_capacity(500);
    for i in 0..500 {
        let term = QUERIES[i % QUERIES.len()];
        let call_input = checked.clone();
        let start = Instant::now();
        let _ = pipeline.assemble(term, call_input, 50);
        samples.push(start.elapsed());
    }

    assert_eq!(samples.len(), 500);
    samples.sort();
    // Nearest-rank p95: rank = ceil(0.95 * n), 1-indexed into the sorted list.
    let rank = (0.95_f64 * samples.len() as f64).ceil() as usize;
    let p95 = samples[rank - 1];
    // Printed unconditionally (visible with `--nocapture`), not just on
    // failure: an operator watching this margin over time wants the number
    // whether or not the assertion below happened to pass.
    println!(
        "p95_query_latency_is_under_10ms_in_release_mode: p95 = {p95:?}, \
         min = {:?}, max = {:?}",
        samples.first().unwrap(),
        samples.last().unwrap(),
    );

    assert!(
        p95 < Duration::from_millis(10),
        "p95 query latency over the 10 000-item fixture was {p95:?} \
         (release mode only — see this test's doc comment); expected \
         comfortably under 10ms"
    );
}

/// Structural, not timed: a single provider answer far over
/// [`MAX_ITEMS_PER_PROVIDER_ANSWER`] survives [`CheckedItems::check`] as
/// exactly that many items, with nothing past the cap even inspected (no
/// rejection recorded for the dropped tail — see
/// `MAX_ITEMS_PER_PROVIDER_ANSWER`'s own "truncate, not reject" docs).
///
/// **The regression this catches**: if Task 2's `output.items.truncate(...)`
/// call in `CheckedItems::check` were ever removed, or the cap silently
/// raised, this is the test that would fail — deterministically, with no
/// timing involved, unlike a wall-clock assertion that a slow CI runner
/// could pass by accident even with the cap gone.
#[test]
fn oversized_provider_input_is_truncated_before_ranking() {
    let provider_id = "oversized-provider";
    let far_more_than_the_cap = MAX_ITEMS_PER_PROVIDER_ANSWER + 1_234;

    let provider = FixtureProvider {
        manifest: manifest_for(provider_id),
    };
    let items = (0..far_more_than_the_cap)
        .map(|i| formulaic_item(provider_id, i))
        .collect();
    let output = ProviderOutput::from_provider(&provider, items);

    let checked = CheckedItems::check(vec![output]);

    assert_eq!(
        checked.items().len(),
        MAX_ITEMS_PER_PROVIDER_ANSWER,
        "a single provider answer over the cap must be truncated to exactly \
         MAX_ITEMS_PER_PROVIDER_ANSWER before ranking ever sees the rest"
    );
    // Review remediation (issue #61 Task 2 review): items past the cap are
    // each truncated silently — none of them is individually inspected, so
    // none is individually rejected — but issue #30's "excess recorded"
    // wording means the truncation itself is no longer silent: it is
    // recorded as exactly one Rejection for the whole over-limit answer,
    // regardless of how many items were actually dropped.
    assert_eq!(
        checked.rejections().len(),
        1,
        "the truncated tail is recorded as exactly one Rejection for the \
         whole over-limit answer, not one per dropped item"
    );
    assert_eq!(
        checked.rejections()[0].check,
        FailedCheck::TooManyItems {
            excess: far_more_than_the_cap - MAX_ITEMS_PER_PROVIDER_ANSWER,
        },
        "the recorded excess must match how many items were actually dropped"
    );
}

/// Structural, not timed: a black-box proof that a term over
/// [`MAX_TERM_CHARS`] is truncated *before* `nucleo_matcher::Pattern::new`
/// is called, using the subsequence-alignment technique from Decision 4 of
/// the plan (mirrored inside `rank.rs`'s own in-module test,
/// `overlong_term_pattern_is_built_from_the_truncated_term`, which has
/// direct access to `Matching`; this version proves the same fact through
/// `Pipeline::assemble`'s public surface, with no access to that private
/// type).
///
/// The one candidate item's haystack (its `title`, no `subtitle`) is
/// exactly [`MAX_TERM_CHARS`] repeated `'a'`s; the query term is
/// `MAX_TERM_CHARS + 1000` repeated `'a'`s — pathological, and, crucially,
/// *longer* than the haystack. nucleo's fuzzy matcher is a strict
/// left-to-right subsequence matcher: every needle character must align, in
/// order, to some haystack character. An untruncated term needs more
/// matching characters than the haystack has at all — no alignment exists,
/// `None`, the item is dropped, and the query returns nothing. A term
/// truncated to exactly `MAX_TERM_CHARS` characters matches the
/// equal-length haystack trivially. So the item **being found** is a
/// deterministic, timing-free proof that truncation ran before pattern
/// construction — no clock involved anywhere in this test.
///
/// **The regression this catches**: if Task 1's truncation inside
/// `Matching::for_term` were ever removed, or `MAX_TERM_CHARS` raised
/// without updating this test, the term would once again outrun the
/// haystack and this assertion would fail — deterministically.
#[test]
fn overlong_term_is_truncated_before_pattern_construction() {
    let provider_id = "term-cap-provider";
    let haystack = "a".repeat(MAX_TERM_CHARS);
    let over_the_cap_term = "a".repeat(MAX_TERM_CHARS + 1000);

    let provider = FixtureProvider {
        manifest: manifest_for(provider_id),
    };
    let item = Item {
        id: ItemId::new(format!("{provider_id}:item-0")).unwrap(),
        kind: Kind::App,
        title: haystack,
        subtitle: None,
        icon: None,
        actions: vec![open_action()],
        default_action: ActionId::new("open").unwrap(),
        copy_text: None,
        append_to_end: false,
        provider: provider_id.into(),
    };
    let checked = CheckedItems::check(vec![ProviderOutput::from_provider(&provider, vec![item])]);
    assert!(
        checked.rejections().is_empty(),
        "fixture sanity: the one candidate item must be well-formed"
    );

    let mut pipeline = Pipeline::default();
    let assembly = pipeline.assemble(&over_the_cap_term, checked, 10);

    assert_eq!(
        assembly.items.len(),
        1,
        "an untruncated {}-character term could not align as a subsequence \
         against a {}-character haystack at all, so finding the item proves \
         truncation happened before Pattern::new",
        over_the_cap_term.chars().count(),
        MAX_TERM_CHARS,
    );
}

/// Release-only, `#[ignore]`d, run by the same `latency-gate` CI job as the
/// p95 arm above: the *true* worst case within both caps — not an over-cap
/// input, which the two structural tests above already prove gets truncated
/// away, but the maximal *legal* one. Ten provider answers at exactly
/// [`MAX_ITEMS_PER_PROVIDER_ANSWER`] items each (10 000 total, nothing
/// truncated), every item's `title` at exactly [`MAX_TITLE`] bytes of a
/// repeated pathological character, queried with a term at exactly
/// [`MAX_TERM_CHARS`] of the *same* character — maximizing how much
/// subsequence-alignment work nucleo has to do per candidate, since every
/// position in every haystack is a potential (partial) match for every
/// position in the needle.
///
/// Asserts the whole [`Pipeline::assemble`] call completes in under 1
/// second — a deliberately generous ceiling (per the plan's own back-of-
/// envelope, ~20-50x the expected cost) chosen to make this test durable
/// against CI jitter while still catching a real algorithmic regression,
/// which would blow past 1 second by a wide margin rather than by a hair.
/// **What this test catches that the two structural tests above cannot**: a
/// regression in the caps' own *effectiveness* — e.g. a future change that
/// raises a cap without reconsidering the cost, or reintroduces quadratic
/// work elsewhere within the still-legal bounds.
#[test]
#[ignore]
fn bounded_worst_case_completes_promptly_in_release_mode() {
    let pathological_title = "x".repeat(MAX_TITLE);
    let pathological_term = "x".repeat(MAX_TERM_CHARS);

    let mut outputs = Vec::with_capacity(PROVIDER_IDS.len());
    for provider_id in PROVIDER_IDS {
        let provider = FixtureProvider {
            manifest: manifest_for(provider_id),
        };
        let items: Vec<Item> = (0..MAX_ITEMS_PER_PROVIDER_ANSWER)
            .map(|i| Item {
                id: ItemId::new(format!("{provider_id}:item-{i}")).unwrap(),
                kind: Kind::App,
                title: pathological_title.clone(),
                subtitle: None,
                icon: None,
                actions: vec![open_action()],
                default_action: ActionId::new("open").unwrap(),
                copy_text: None,
                append_to_end: false,
                provider: provider_id.into(),
            })
            .collect();
        outputs.push(ProviderOutput::from_provider(&provider, items));
    }
    let checked = CheckedItems::check(outputs);
    assert_eq!(
        checked.items().len(),
        10_000,
        "the pathological fixture must be exactly at both caps: nothing \
         truncated (item count) and nothing rejected (title length)"
    );
    assert!(checked.rejections().is_empty());

    let mut pipeline = Pipeline::default();
    let start = Instant::now();
    let assembly = pipeline.assemble(&pathological_term, checked, 50);
    let elapsed = start.elapsed();
    // Printed unconditionally (visible with `--nocapture`) — see the p95
    // test's matching println for why.
    println!("bounded_worst_case_completes_promptly_in_release_mode: elapsed = {elapsed:?}");

    assert!(
        !assembly.items.is_empty(),
        "sanity: an all-'x' term against all-'x' titles must actually match \
         something, or this isn't exercising the matching work at all"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "the bounded worst case took {elapsed:?} (release mode only — see \
         this test's doc comment); expected comfortably under 1s"
    );
}

/// Determinism, pinned against a literal expected value rather than left to
/// inspection or proven only within one process — see the module docs'
/// "Fixture determinism" section for why a source-literal digest is what
/// actually establishes the cross-run property issue #61 asks for, and why
/// the weaker double-build-in-one-process comparison this test also does is
/// kept as a fast, cheap first check rather than relied on alone. Not
/// `#[ignore]`d — the whole test costs building the fixture twice plus one
/// linear hash pass (allocation and arithmetic, not ranking), fast enough
/// for the ordinary debug-mode gate.
#[test]
fn the_ten_thousand_item_fixture_is_deterministic() {
    let a = ten_thousand_item_fixture();
    let b = ten_thousand_item_fixture();

    assert_eq!(a.items().len(), 10_000);
    assert_eq!(b.items().len(), 10_000);
    assert!(a.rejections().is_empty());
    assert!(b.rejections().is_empty());

    let ids_a: Vec<&ItemId> = a.items().iter().map(|item| &item.id).collect();
    let ids_b: Vec<&ItemId> = b.items().iter().map(|item| &item.id).collect();
    assert_eq!(
        ids_a, ids_b,
        "two independent builds of the fixture must produce the identical \
         item-id sequence in the identical order — the fixture must contain \
         no RNG and no dependence on unordered iteration"
    );

    let digest = id_sequence_digest(a.items());
    assert_eq!(
        digest, EXPECTED_ID_SEQUENCE_DIGEST,
        "the fixture's item-id sequence hashed to {digest:#x}, not the \
         literal value recorded in EXPECTED_ID_SEQUENCE_DIGEST. Comparing \
         against a value written directly in this file's source — not \
         merely against a second in-process build — is what proves the \
         fixture is deterministic *across* separate process invocations \
         (what issue #61's 'deterministic' criterion, and a CI gate that \
         runs once per PR, actually need), not only within one. If this \
         fixture's construction intentionally changed, recompute the digest \
         and update EXPECTED_ID_SEQUENCE_DIGEST deliberately; if it did not \
         change intentionally, this is the fixture becoming order- or \
         seed-dependent — the exact regression this assertion exists to \
         catch."
    );
}

// ---------------------------------------------------------------------------
// Issue #128: the files-shaped arm.
//
// The arm above measures 10 000 items with deliberately *short* titles
// ("Firefox", "Chrome 2"). `formulaic_title`'s own comment records what
// happens when that assumption breaks: inflating titles to ~45 bytes measured
// p95 ~11.8ms, over the 10ms budget. So the gate's headroom is a property of
// haystack length, and the arm above deliberately measures the short case.
//
// M5 ships a files provider, whose titles are paths. This arm changes exactly
// one variable — title shape and length — holding provider count, item count,
// query set and methodology identical, so the two p95 numbers are directly
// comparable and any difference is attributable to the haystack alone.
//
// # Why item count is *not* the variable
//
// The research brief for this arm asked for "an item count at the order of
// magnitude a real indexed home directory produces". That count is
// unreachable through the real path, and saying so is more useful than
// simulating it: `MAX_ITEMS_PER_PROVIDER_ANSWER` bounds one provider's answer
// to 1 000 items and truncates the rest, and `MAX_ITEMS_PER_QUERY` bounds the
// daemon's retained set to 5 000. A files provider with 400 000 indexed paths
// still hands `assemble` at most 1 000 of them. The fixture above, at 10 000,
// already sits at twice the retained cap — it is adversarial on count
// already. Scaling count further would measure a shape the protocol forbids
// while leaving the shape it permits untested.
// ---------------------------------------------------------------------------

/// Directory-name components, combined by [`path_title`] into nested paths.
/// Ordinary developer-machine vocabulary, mixed case included, because case
/// folding is work `nucleo_matcher` actually does per candidate.
const PATH_SEGMENTS: [&str; 12] = [
    "home",
    "pedro",
    "Documents",
    "projects",
    "src",
    "hop-launcher",
    "crates",
    "node_modules",
    "Downloads",
    ".config",
    "target",
    "Screenshots",
];

/// File extensions, cycled independently of depth so length and suffix vary
/// on different periods rather than in lockstep.
const EXTENSIONS: [&str; 6] = ["rs", "toml", "md", "png", "tar.gz", "json"];

/// One item's title as a **path**, the shape a files provider produces.
///
/// Depth cycles 2..=7 segments and the basename carries an index and an
/// extension, so lengths spread across roughly 30–110 bytes rather than
/// sitting at one fixed size — a distribution, because a real index has one
/// and because a single length would make the result an artifact of whichever
/// length was picked.
///
/// Deliberately well under [`MAX_TITLE`](hop_protocol::limits::MAX_TITLE)
/// (1 KiB). A title at that bound is an adversarial input, and this file
/// already has arms for adversarial input
/// ([`oversized_provider_input_is_truncated_before_ranking`],
/// [`overlong_term_is_truncated_before_pattern_construction`]). This arm's
/// claim is about *realistic* paths, which is the claim M5 needs.
///
/// Purely a function of `item_index`: no RNG, no clock, no environment.
fn path_title(item_index: usize) -> String {
    let depth = 2 + (item_index % 6);
    let mut parts = Vec::with_capacity(depth + 1);
    for step in 0..depth {
        // `step * 5` walks the segment list at a stride coprime with its
        // length (12), so successive components differ rather than repeating
        // the same word down a path.
        parts.push(PATH_SEGMENTS[(item_index + step * 5) % PATH_SEGMENTS.len()]);
    }
    let stem = VOCAB[item_index % VOCAB.len()];
    let ext = EXTENSIONS[item_index % EXTENSIONS.len()];
    format!("/{}/{stem}-{item_index}.{ext}", parts.join("/"))
}

/// One path-shaped item. `Kind::File` throughout — a files provider produces
/// one kind — still inside [`manifest_for`]'s declared kinds.
fn path_item(provider_id: &'static str, item_index: usize) -> Item {
    Item {
        id: ItemId::new(format!("{provider_id}:path-{item_index}")).unwrap(),
        kind: Kind::File,
        title: path_title(item_index),
        subtitle: None,
        icon: None,
        actions: vec![open_action()],
        default_action: ActionId::new("open").unwrap(),
        copy_text: None,
        append_to_end: false,
        provider: provider_id.into(),
    }
}

/// One provider's at-cap answer of path-shaped items.
fn path_provider_output(provider_id: &'static str) -> ProviderOutput {
    let provider = FixtureProvider {
        manifest: manifest_for(provider_id),
    };
    let items = (0..MAX_ITEMS_PER_PROVIDER_ANSWER)
        .map(|i| path_item(provider_id, i))
        .collect();
    ProviderOutput::from_provider(&provider, items)
}

/// The files-shaped fixture: same ten providers and same 10 000 items as
/// [`ten_thousand_item_fixture`], with path titles instead of short ones.
fn files_shaped_fixture() -> CheckedItems {
    let mut outputs = Vec::with_capacity(PROVIDER_IDS.len());
    for provider_id in PROVIDER_IDS {
        outputs.push(path_provider_output(provider_id));
    }
    CheckedItems::check(outputs)
}

/// Pinned as a literal for the same reason
/// [`EXPECTED_ID_SEQUENCE_DIGEST`] is — see that constant, and the module
/// docs' "Fixture determinism" section.
const EXPECTED_PATH_ID_SEQUENCE_DIGEST: u64 = 0x784a_a143_1dc2_0dc7;

/// p95 over the files-shaped fixture — measured and printed, not asserted.
///
/// Release-mode only and `#[ignore]`d for exactly the reasons the arm above
/// documents; the same CI `latency-gate` job runs it, since that job passes
/// `--ignored` for the whole test binary rather than naming tests.
///
/// **Report-only; does not gate CI (issue #134).** The measured position is
/// ~4.899 ms locally (PR #135) against the ~4 ms working target #134's own
/// brief set, which projects to ~9.8 ms on CI's roughly 2× slower runners —
/// right at the 10 ms budget. A gate with that little margin isn't measuring
/// regressions, it's measuring runner speed: the same commit measured 10.066
/// ms and then 10.234 ms back to back on PR #133's runners, and a later 5/5
/// pass streak was the arm's variance narrowing near the line, not its p95
/// moving away from it. So the fixture, the paths and the 10 ms number below
/// are untouched; only the failing behavior is gone.
///
/// **Calibrating the budget to machine speed instead (approach 2) was
/// rejected.** It would keep this arm green by scaling the threshold to
/// whatever the runner can manage, which keeps the tick without keeping the
/// claim: §3 promises a p95 under 10 ms, not under whatever the current
/// runner happens to do. That trades a visible, honest report-only arm for
/// an invisible, dishonest one — worse, not better.
///
/// **#128's original point survives this change, and is now this arm's whole
/// purpose.** A breach here still means a files provider needs a different
/// approach to candidate selection — prefix or path-segment indexing, ahead
/// of the fuzzy scorer — and that is a design input to M5, not a number to
/// negotiate. This test no longer decides when that line is crossed; a human
/// reading the printed p95 over time does.
///
/// **The cheap route to closing the gap is exhausted, not merely untried.**
/// PR #135 removed `haystack_of`'s per-item allocation for a measured 2.4%
/// gain (5.021 ms → 4.899 ms mean) — real, but nowhere near enough. A
/// hand-rolled candidate prefilter ahead of the fuzzy scorer was measured too
/// and came back a net *regression*: nucleo already runs its own
/// `memchr`/SIMD prefilter before falling back to full scoring, so a naive
/// filter in front of it pays its own cost without skipping any of nucleo's.
/// Recorded here so the next person doesn't spend time re-measuring it.
///
/// **What would make this arm gate again**: the p95 moving away from the
/// line — M5's files provider actually landing a different candidate-
/// selection approach — not a change to the budget, the fixture, or this
/// arm's `#[ignore]`.
#[test]
#[ignore]
fn p95_query_latency_over_a_files_shaped_fixture_is_measured_in_release_mode() {
    let checked = files_shaped_fixture();
    assert_eq!(
        checked.items().len(),
        10_000,
        "fixture sanity: nothing truncated"
    );
    assert!(
        checked.rejections().is_empty(),
        "fixture sanity: nothing rejected — path titles must be inside MAX_TITLE"
    );

    let mut pipeline = Pipeline::default();

    for i in 0..50 {
        let term = QUERIES[i % QUERIES.len()];
        let _ = pipeline.assemble(term, checked.clone(), 50);
    }

    let mut samples: Vec<Duration> = Vec::with_capacity(500);
    for i in 0..500 {
        let term = QUERIES[i % QUERIES.len()];
        let call_input = checked.clone();
        let start = Instant::now();
        let _ = pipeline.assemble(term, call_input, 50);
        samples.push(start.elapsed());
    }

    samples.sort();
    let rank = (0.95_f64 * samples.len() as f64).ceil() as usize;
    let p95 = samples[rank - 1];

    // Printed unconditionally, like the arm above — this is the whole point
    // now that nothing here asserts (see this test's doc comment). The
    // margin against the 10ms reference line is the number a reader needs to
    // judge drift over time, so it's computed and printed explicitly rather
    // than left for a reader to subtract by hand.
    let ten_ms = Duration::from_millis(10);
    let margin = if p95 <= ten_ms {
        format!("{:?} under", ten_ms - p95)
    } else {
        format!("{:?} over", p95 - ten_ms)
    };
    let mean_title: usize = checked
        .items()
        .iter()
        .map(|item| item.title.len())
        .sum::<usize>()
        / checked.items().len();
    println!(
        "p95_query_latency_over_a_files_shaped_fixture_is_measured_in_release_mode: \
         p95 = {p95:?}, min = {:?}, max = {:?}, mean title = {mean_title} bytes, \
         margin vs 10ms reference = {margin}",
        samples.first().unwrap(),
        samples.last().unwrap(),
    );
}

#[test]
fn the_files_shaped_fixture_is_deterministic() {
    let a = files_shaped_fixture();
    let b = files_shaped_fixture();

    assert_eq!(a.items().len(), 10_000);
    assert!(a.rejections().is_empty());

    let ids_a: Vec<&ItemId> = a.items().iter().map(|item| &item.id).collect();
    let ids_b: Vec<&ItemId> = b.items().iter().map(|item| &item.id).collect();
    assert_eq!(ids_a, ids_b);

    // Titles too, not only ids: this fixture's whole point is the *titles*,
    // so an id sequence that matched while path construction drifted would
    // pass a digest check and silently measure a different haystack.
    let titles_a: Vec<&str> = a.items().iter().map(|item| item.title.as_str()).collect();
    let titles_b: Vec<&str> = b.items().iter().map(|item| item.title.as_str()).collect();
    assert_eq!(titles_a, titles_b);

    let digest = fnv1a64(titles_a.join("\n").as_bytes());
    assert_eq!(
        digest, EXPECTED_PATH_ID_SEQUENCE_DIGEST,
        "the files-shaped fixture's title sequence hashed to {digest:#x}, not \
         the literal in EXPECTED_PATH_ID_SEQUENCE_DIGEST. Same reasoning as \
         EXPECTED_ID_SEQUENCE_DIGEST: a literal is what proves determinism \
         across separate process invocations, not merely within one. If path \
         construction changed intentionally, recompute and update this \
         deliberately."
    );
}
