# Replace-frame assembly (Issue #103) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Call `hop_core::pipeline::Pipeline::assemble` on the daemon's query
path, so provider items reach the client ranked, boosted, filtered and capped
instead of arriving unranked in provider-completion order — using the
**replace-frame** shape the maintainer decided on in issue #103's body.

**Architecture:** Each time a provider answers, the daemon re-runs `assemble`
over every checked item received so far for that query and sends a `results`
frame carrying the **complete current list**, which the client swaps in whole.
No provider is ever waited on. Three seams move to make that possible:
`hop-core`'s provider host streams `CheckedItems` rather than bare `Vec<Item>`
(the manifest-check guarantee has to survive the channel, and only a
`CheckedItems` can be handed to `assemble`); `hopd`'s `HostSource` gains an
accumulator task that owns the query's `Pipeline` and does the re-assembly; and
`hopd`'s connection driver replaces its retained set per frame instead of
extending it.

**Tech Stack:** Rust 2024, no new dependencies. `tokio::sync::Mutex` (the
`sync` feature `hopd` already enables) guards the shared `Pipeline`.

## Global Constraints

- **No new third-party dependencies.** Nothing in this slice needs one, so
  `deny.toml` and both `Cargo.toml` files are untouched.
- **Gate commands, all four required:** `cargo test --workspace` (501 tests
  green at the branch point) · `cargo fmt --all --check` · `cargo clippy
  --workspace --all-targets -- -D warnings` · `cargo deny check`.
- **No `.unwrap()` in production code** (`clippy::unwrap_used` + `-D
  warnings`). Test files and test modules open with
  `#![allow(clippy::unwrap_used)]`.
- **The latency contract (spec §3):** keystroke → ranked results < 10 ms; no
  disk reads, subprocess spawns or HTTP on the query path. `assemble` is pure
  and this slice adds no I/O to it.
- **`assemble` may only ever be reached through `CheckedItems`.** No task adds
  a constructor, a `From`, or any other route that builds a `CheckedItems`
  without `CheckedItems::check` having run. That type's docs explain at length
  why a caller-supplied manifest is a hole; the additions this plan makes
  (`Clone`, `items`, `absorb`, `truncate_items`) all take an already-checked
  value as their input and none of them can mint one.
- **Every task leaves the workspace green.** The four gate commands pass at
  every task boundary, not only at the end.
- **No AI attribution** in commits or the PR.

## Scope: what this slice is and is not

**In scope**, the seven acceptance criteria on issue #103:

1. The daemon calls `Pipeline::assemble` on the query path.
2. Each provider's arrival re-assembles over every item received so far for
   that query, and the result is sent as a frame the client replaces its list
   with.
3. No provider is waited on before the first frame is sent.
4. `max_results` and the pin budget are applied to the whole assembled set, not
   per provider.
5. Alias boosts, learning boosts, the exclusive-mode filter and inferred-mode
   promotion all take effect through the daemon, pinned by a test that drives
   two providers over a real socket.
6. `connection.rs`'s delivered/retained accounting is correct under
   replacement: `MAX_ITEMS_PER_QUERY` is not charged twice for a re-sent item,
   and #59's `Execute` resolves against the replaced set, not the union of
   every frame sent.
7. The frontend contract for replace-vs-append is documented in
   `hop-protocol`.

**Not in scope, deliberately:**

- **Bounding assembly's input by item count or field length.** That is issue
  #30, folded into #61's latency-gate slice as an acceptance criterion. This
  slice adds a daemon-side accumulation cap (`MAX_ITEMS_PER_QUERY`, Task 2)
  because replacement moves where per-query growth happens; that is a memory
  bound on the daemon's own buffer, not the input cap #30 describes, and it
  does not make `assemble` cheap for a hostile provider.
- **Persisted learning.** `Pipeline::default()` builds an in-memory
  `Learning`, so nothing this slice records survives a restart. Loading it from
  a real state directory is issue #60. Task 2 adds `HostSource::with_pipeline`
  precisely so #60 can swap a loaded `Pipeline` in without touching this
  wiring.
- **Recording launches into learning.** Nothing calls
  `Learning::record_launch` yet, because nothing dispatches an action yet
  (issue #59). Learning boosts are therefore reachable in this slice only by
  seeding a `Pipeline` — which is exactly how Task 6's test reaches them.
- **Making `max_results` configurable.** Task 2 introduces it as a constant
  with a documented value; issue #60's config load is where it becomes a
  setting.
- **Logging `Assembly::rejections`.** The provider host already logs the
  manifest-check half through its log seam before the items travel; the
  pin-budget half stays unlogged, as it is today. Task 2 documents that the
  daemon discards `Assembly::rejections` and why.

## Design decisions (read before any task)

**1. Assembly lives in `hopd`'s `HostSource`, not in `hop-core`'s
`ProviderHost`.** The host is deliberately "not a scheduler in the ranking
sense" (`CONTEXT.md`, **Provider host**): it decides *whether* a provider runs
and *for how long*, never in what order items appear. Giving it a `Pipeline`
would merge those two jobs and would also put the `Learning` store — state that
issue #60 will load from the daemon's state directory — inside a `hop-core`
type that has no business owning it. The composition "schedule → check →
accumulate → assemble" is exactly what `hopd`'s **result source** seam exists
for, so it goes there.

**2. The channel from the host carries `CheckedItems`, not `Vec<Item>`.**
`Pipeline::assemble` accepts nothing but a `CheckedItems`, and `CheckedItems`
can only be built by `CheckedItems::check` from a `ProviderOutput`, which in
turn can only be built from the dispatched `Provider` object itself. Only the
host holds those objects. So either the guarantee travels over the channel or
it is destroyed at the channel and cannot be rebuilt downstream — a
`Vec<Item>` accumulator in `hopd` could never call `assemble` at all. Widening
the channel's item type is what keeps the type-level guarantee intact end to
end, and it is why no task adds an escape hatch that fabricates a
`CheckedItems` from loose items.

The host keeps logging the manifest-check rejections through its log seam
before it sends, unchanged. They also travel inside the `CheckedItems` and
come back out in `Assembly::rejections`, where the daemon ignores them — the
double-carry is the price of not punching a hole in the type, and it is
documented at the discard site rather than left to be rediscovered.

**3. One `results` frame carries one complete list; a replacement is never
split across frames.** This is what makes replace semantics decidable by a
client: with a split, a client could not tell "the rest of the current list"
from "a new list replacing it", and would need a framing marker the wire does
not have. Two things enforce it. `MAX_RESULTS` (Task 2), the `max_results` the
daemon passes to `assemble`, is a compile-time-checked `<=
MAX_ITEMS_PER_RESULTS_FRAME`, so an honest assembled list always fits one
frame. And the connection driver (Task 3) truncates any list longer than
`MAX_ITEMS_PER_RESULTS_FRAME` and ends the exchange, rather than chunking —
because a source is untrusted (`ResultSource`'s own obligations section) and
chunking is the one thing replacement forbids.

**4. `MAX_ITEMS_PER_QUERY` moves from what the connection *delivers* to what
the source *accumulates*.** Under append semantics the two were the same
number: every item delivered was an item the daemon was holding for the first
time. Under replacement the same item is re-sent on every arrival, so counting
sends would charge one item many times and end a healthy query early — the
failure acceptance criterion 6 names. What actually grows per query now is the
accumulator, so that is where the cap applies: at most `MAX_ITEMS_PER_QUERY`
checked items are accumulated, the batch that crosses the line is truncated,
and the query ends — **truncate-and-terminate**, unchanged in meaning, moved
to where the growth is. The retained set the connection keeps is the last
assembled list, which `MAX_RESULTS` already bounds far below the old cap, so
the threat model's bounded-retained-state assumption (#25) holds more tightly
than before, not less.

**5. `API_VERSION` stays at `1`.** The change is to what a `results` frame
*means*, not to any frame's shape, and a semantics change under an unchanged
version number is normally exactly what a handshake exists to catch. It is
sound here for one reason that will not be true later: nothing has shipped.
The repo carries no git tags and no releases, `API_VERSION` has never left this
tree, and both peers that speak it — `hopd` and `hop-cli` — are built from this
same workspace and move together in this same commit. Bumping would gate this
tree against a client that does not exist. Task 4 records this in
`hop-protocol` so the next semantics change, which will be against a released
version, does not read this as precedent.

**6. Every provider arrival sends a frame, including one that changes
nothing.** A provider that answers with zero items still triggers a re-assembly
whose output equals the previous frame's, and that frame is sent anyway. The
alternative — comparing the new list against the last one and suppressing an
identical frame — buys one saved frame per silent provider at the cost of a
whole-list equality check on the keystroke path and a second rule a client
author has to know about. With a handful of registered providers the traffic
saved is negligible. Send unconditionally; document it.

**7. One `Pipeline` per daemon, shared behind a `tokio::sync::Mutex`.**
`Learning` is global state — what the user launched, across every connection —
so a `Pipeline` per connection would fragment it, and a `Pipeline` per query
would discard it. `assemble` takes `&mut self`, so the shared value needs a
lock; `tokio::sync::Mutex` rather than `std::sync::Mutex` because there is no
poisoning to handle without an `.unwrap()` the lint bans, and the guard is
never held across an `await` (`assemble` is synchronous). Contention is one
lock per provider arrival per in-flight query, against work measured in
microseconds for honest input.

**8. Wiring ranking makes non-matching items disappear, and that breaks two
existing tests on purpose.** `Ranker::rank` drops an item whose haystack does
not fuzzy-match the term at all. `crates/hopd/tests/socket.rs` and
`crates/hop-cli/tests/e2e.rs` each query a deliberately unmatchable canary
string (`hop-e2e-canary-9f3a1c`, `hop-cli-e2e-canary-5c2f91`) chosen so no
*installed application* could match it, and then assert the skeleton provider's
`Hello from hopd` item comes back. Once assembly is wired, the canary does not
match the skeleton item either, and both tests correctly get an empty list.
Task 2 re-points them at a term that matches the skeleton item's haystack
(`Hello from hopd` + `M2.2 walking skeleton`) while remaining implausible as an
installed application's name — see Task 2 for the exact term and the reasoning
about machine-dependence, which is the failure mode a whole-branch review
already caught once in this file.

## File Structure

**Created:**
- `crates/hopd/tests/assembly.rs` — the two-provider-over-a-real-socket
  integration test (acceptance criterion 5), plus the replacement, cap and
  no-gate assertions.

**Modified:**
- `crates/hop-core/src/pipeline.rs` — `CheckedItems` gains `Clone`, `items`,
  `absorb`, `truncate_items`.
- `crates/hop-core/src/host.rs` — `spawn_query`/`run_one` send `CheckedItems`;
  the module docs' "Ranking … is not enforced here" section is rewritten to say
  where it now happens.
- `crates/hopd/src/source.rs` — the accumulator/assembly task, `MAX_RESULTS`,
  `HostSource::with_pipeline`, the `ResultSource` contract's replacement rule.
- `crates/hopd/src/connection.rs` — replacement retained set, one frame per
  assembly, `MAX_ITEMS_PER_RESULTS_FRAME` as the defensive bound.
- `crates/hopd/src/lib.rs` — the module doc sentence naming #103 as an open gap.
- `crates/hopd/tests/lifecycle.rs` — test sources and assertions under
  replacement.
- `crates/hopd/tests/socket.rs`, `crates/hop-cli/tests/e2e.rs` — the canary
  queries (Design decision 8).
- `crates/hop-protocol/src/wire.rs` — `DaemonMsg::Results`' replace contract.
- `crates/hop-protocol/src/limits.rs` — `MAX_ITEMS_PER_QUERY` and
  `MAX_ITEMS_PER_RESULTS_FRAME` under replacement.
- `crates/hop-cli/src/lib.rs` — the reference client replaces rather than
  appends.
- `CONTEXT.md` — the glossary terms replacement changes.

---

### Task 1: `CheckedItems` travels the host's channel

**Files:** `crates/hop-core/src/pipeline.rs`, `crates/hop-core/src/host.rs`,
`crates/hopd/src/source.rs`

**Why:** `Pipeline::assemble` accepts only a `CheckedItems`, and nothing
downstream of the host can build one. Widening the channel is what lets a later
task assemble at all (Design decision 2). This task changes no behavior: what
reaches a client is byte-for-byte what it was before.

**Steps:**

- [ ] `CheckedItems` (and whatever it holds — `Rejection`, `FailedCheck`)
      derives `Clone`. `Item` is already `Clone`.
- [ ] Add, with doc comments that say why each is safe against the type's own
      "only `check` may mint one" rule:
      - `pub fn items(&self) -> &[Item]` — read-only view, for the accumulator's
        cap arithmetic.
      - `pub fn absorb(&mut self, other: CheckedItems)` — appends `other`'s
        items and rejections, in order. Both sides were checked, so the result
        is checked; this is the merge an accumulator needs and the only way to
        build the whole-query value `assemble` takes.
      - `pub fn truncate_items(&mut self, max: usize)` — keeps at most `max`
        items, leaving rejections alone. Dropping checked items cannot un-check
        anything.
- [ ] `ProviderHost::spawn_query` and `ProviderHost::run_one` take
      `mpsc::Sender<CheckedItems>` and send the `CheckedItems` value `run_one`
      already builds, instead of extracting its items. The rejection logging
      that happens before the send is unchanged, and its doc comment gains a
      sentence noting that the rejections also travel onward and that the
      receiver is expected to ignore them.
- [ ] `hopd`'s `HostSource::start` keeps returning
      `mpsc::Receiver<Vec<Item>>`: it now creates two channels of capacity 1 and
      spawns a forwarding task between them that turns each `CheckedItems` into
      its items (`items().to_vec()`) and sends that on. This task is the seam
      Task 2 replaces with real assembly; introducing it here, with behavior
      unchanged, is what lets the cancellation chain be verified on its own.
      The task returns — dropping the host's receiver, which cancels every
      running provider — as soon as a send downstream fails, and closes its
      outgoing channel when the host's closes.
- [ ] Update the host's own tests: `drain` and every `spawn_query` caller in
      `crates/hop-core/src/host.rs`'s test module now receive `CheckedItems`.

**Tests (in `crates/hop-core/src/host.rs`'s and `crates/hopd/src/source.rs`'s
test modules):**

- [ ] `absorb_concatenates_items_and_rejections_in_order` — two checked values
      merge with both lists preserved in order.
- [ ] `truncate_items_keeps_the_first_n_and_leaves_rejections_alone`.
- [ ] The existing host tests keep passing against the new channel type,
      including the two that assert cancellation via a dropped receiver.
- [ ] `dropping_the_forwarded_receiver_cancels_the_query` in `source.rs` — a
      `HostSource` query whose receiver is dropped stops the providers behind
      it, proving the added hop did not break the seam's cancellation contract.
      Verify the red first: a forwarding task that ignores a failed send and
      keeps looping must fail this test.

---

### Task 2: The accumulator — `assemble` on every arrival

**Files:** `crates/hopd/src/source.rs`, `crates/hopd/tests/socket.rs`,
`crates/hop-cli/tests/e2e.rs`, `crates/hopd/src/lib.rs`

**Why:** acceptance criteria 1, 2, 3 and 4. This is the slice's centre.

**Steps:**

- [ ] Add `pub const MAX_RESULTS: usize = 50;` to `crates/hopd/src/source.rs`
      with a doc comment: it is the `max_results` the daemon passes to
      `assemble`; a launcher renders tens of rows, not thousands; issue #60's
      config load is where it becomes a setting rather than a constant. Follow
      it with `const _: () = assert!(MAX_RESULTS <= MAX_ITEMS_PER_RESULTS_FRAME);`
      and a comment naming Design decision 3 — one assembled list must fit one
      frame, and this is what makes that true by construction rather than by
      habit.
- [ ] `HostSource` gains a `pipeline: Arc<tokio::sync::Mutex<Pipeline>>` field.
      `HostSource::new(host)` keeps its signature and builds
      `Pipeline::default()`. Add `pub fn with_pipeline(host: Arc<ProviderHost>,
      pipeline: Arc<Mutex<Pipeline>>) -> Self`, documented as the seam issue
      #60 loads a persisted `Learning` through and the one Task 6's test seeds
      aliases and learning through.
- [ ] Replace Task 1's forwarding task body with the accumulator. Per query it
      owns: the raw query text, a `CheckedItems` accumulator (start it from
      `CheckedItems::check(Vec::new())` — no new constructor), and a clone of
      the pipeline handle. On each arrival:
      1. Compute `room = MAX_ITEMS_PER_QUERY - accumulated.items().len()`. If
         the incoming value has at least `room` items, `truncate_items(room)`
         it and mark the query capped — the same "filling the room exactly is
         still a cap" rule `take_within_cap` documents today, for the same
         reason: a full accumulator has nothing to give a later batch.
      2. `absorb` it.
      3. Lock the pipeline, call `assemble(raw_query, accumulated.clone(),
         MAX_RESULTS)`, release the lock. Discard `Assembly::rejections` with a
         comment saying the host already logged the manifest-check half and the
         pin-budget half stays unlogged, as it is today (Scope).
      4. Send `Assembly::items` downstream. A failed send returns from the task,
         which drops the host's receiver and cancels the providers.
      5. If the query was capped, return — dropping the host's receiver and
         closing the outgoing channel, which is what makes the connection send
         its terminal frame.
- [ ] Document on the `ResultSource` trait that **each `Vec<Item>` a source
      sends is the complete current result list for that query, not an
      increment** — the seam's half of the replace contract — and that the
      per-item field-bound obligation in the existing docs is unchanged. Note
      that the accumulator's `clone()` per arrival is proportional to what
      providers have sent, and point at #30/#61 as the slice that bounds that
      input.
- [ ] `crates/hopd/src/lib.rs`: the module docs list result assembly as a gap
      naming issue #103. Rewrite that sentence to describe what the daemon now
      does.
- [ ] Re-point the two canary tests (Design decision 8). Use the query text
      `walking skeleton` in both `crates/hopd/tests/socket.rs` and
      `crates/hop-cli/tests/e2e.rs`, replacing the canary strings. It matches
      the skeleton item's haystack on both atoms, and an installed application
      whose title and subtitle contain both "walking" and "skeleton" is
      implausible — but the assertion must not assume it: assert that the
      returned list **contains** an item titled `Hello from hopd`, and keep the
      existing comment about the one root `spawn_daemon` cannot isolate,
      updated to say what the term is now chosen for. Do not assert an exact
      list length in these two tests; that is the machine-dependence this file
      has already been bitten by once.

**Tests (in `crates/hopd/src/source.rs`'s test module unless stated):**

- [ ] `each_arrival_re_assembles_over_every_item_received_so_far` — two
      providers with different delays; the first frame holds the fast
      provider's matching items, the second holds **both** providers' items,
      ranked together. This is criterion 2's unit-level pin.
- [ ] `the_first_frame_is_sent_without_waiting_for_the_slow_provider` —
      criterion 3. The slow provider's budget is long enough that a gate would
      be visible; assert the first frame arrives while it is still running.
- [ ] `max_results_is_applied_to_the_whole_assembled_set_not_per_provider` —
      two providers each returning fewer than `MAX_RESULTS` items but more than
      that in total; the frame holds exactly `MAX_RESULTS`. Criterion 4.
      Verify the red by hand: an implementation that assembles per batch and
      concatenates passes a naive count assertion, so assert on the *identity*
      of what survived (the highest-scoring items across both providers), not
      only on the length.
- [ ] `the_accumulator_caps_at_max_items_per_query_and_ends_the_query` — a
      provider returning more than `MAX_ITEMS_PER_QUERY` items truncates and
      the channel closes. Criterion 6's daemon-side half.
- [ ] `a_provider_answering_with_no_items_still_sends_a_frame` — Design
      decision 6.
- [ ] The two re-pointed canary tests pass.

---

### Task 3: The connection replaces its retained set

**Files:** `crates/hopd/src/connection.rs`, `crates/hopd/tests/lifecycle.rs`

**Why:** acceptance criterion 6. The driver still charges every item of every
frame against `MAX_ITEMS_PER_QUERY` and still chunks a long batch across
frames — both wrong under replacement.

**Steps:**

- [ ] `Exchange::delivered`'s doc comment is rewritten: it is the **last
      assembled list**, replaced whole on each arrival, and it is what #59's
      `Execute` resolves against. The reasoning it carries today — that a
      delivered item must stay resolvable after the exchange ends — survives
      unchanged for the *last* list; what changes is that an item the daemon
      has since replaced away is no longer resolvable, which is what criterion
      6 asks for.
- [ ] `forward_batch` becomes: take the incoming list; if it is longer than
      `MAX_ITEMS_PER_RESULTS_FRAME`, truncate it to that and mark the exchange
      capped (Design decision 3 — a replacement may not be split, so the only
      honest answers are truncate or refuse, and truncate-and-terminate is the
      shape this daemon already uses); replace `delivered` with it; send
      exactly one `Results { partial: true }` frame; if capped, send
      `QueryDone` and drop the source. The chunking loop and
      `take_within_cap` go away, along with their unit tests.
- [ ] The module docs and `Exchange`'s docs stop describing accumulation and
      describe replacement, naming `MAX_ITEMS_PER_RESULTS_FRAME` as the bound
      that applies here and pointing at `source.rs` for where
      `MAX_ITEMS_PER_QUERY` now applies.
- [ ] `crates/hopd/tests/lifecycle.rs`: its `ResultSource` implementations send
      complete lists now, not increments. Every test asserting the old
      accumulate-and-chunk behavior is rewritten against replacement — in
      particular the per-query-cap test and any chunking test. A test that
      merely asserts "two batches produce two frames" still holds and should
      keep its assertions.

**Tests (in `crates/hopd/src/connection.rs`'s test module and
`crates/hopd/tests/lifecycle.rs`):**

- [ ] `a_re_sent_item_is_not_charged_twice` — a source that sends the same
      50-item list 200 times produces 200 frames and no `QueryDone` from a cap.
      Under the old accounting this crosses `MAX_ITEMS_PER_QUERY` on frame 100
      and terminates, so the test is a genuine red against it. Criterion 6.
- [ ] `the_retained_set_is_the_last_list_not_the_union` — after two frames the
      retained set equals the second list exactly, including that an item only
      the first list held is gone.
- [ ] `a_list_over_the_frame_bound_is_truncated_and_terminates` — no chunking;
      one frame of `MAX_ITEMS_PER_RESULTS_FRAME` items followed by `QueryDone`.
- [ ] Cancellation, supersession and `QueryDone` behavior in
      `crates/hopd/tests/lifecycle.rs` are unchanged.

---

### Task 4: The replace contract, written down in `hop-protocol`

**Files:** `crates/hop-protocol/src/wire.rs`,
`crates/hop-protocol/src/limits.rs`

**Why:** acceptance criterion 7. `hop-protocol` is the contract both peers
read; the rule a frontend has to follow belongs there and nowhere else.

**Steps:**

- [ ] `DaemonMsg::Results`' doc comment states the replace rule for a client
      author: every `results` frame for a `query_id` carries the **complete
      current result list** for that query, and a client **replaces** whatever
      it is holding for that id rather than appending. A daemon never splits
      one list across frames, so a frame is never "the rest of" the previous
      one. Keep the existing `partial` contract intact — `partial` stays
      advisory and `QueryDone` stays the terminal signal — and say explicitly
      that `partial: true` now means "a later frame may replace this list",
      not "more items follow".
- [ ] Say why several frames still arrive per query: one per provider arrival,
      each a re-ranked list over everything received so far, which is what lets
      a fast provider's results render while a slow one is still running.
- [ ] Record Design decision 5 where a reader will find it — `API_VERSION` in
      `crates/hop-protocol/src/lib.rs` is the natural home: the meaning of
      `results` changed under version `1` because nothing had shipped it, and
      that reasoning expires the moment a release exists.
- [ ] `MAX_ITEMS_PER_RESULTS_FRAME`'s docs: under replacement this is the bound
      on one complete list, so it is now also the effective bound on what a
      client holds for one query — say so, and keep the "ceiling a hostile
      daemon cannot exceed, not what an honest one sends" framing.
- [ ] `MAX_ITEMS_PER_QUERY`'s docs: the paragraph describing a client
      accumulating across frames is no longer true and must be rewritten, not
      softened. Under replacement the daemon applies this to what it
      *accumulates from providers* for one query, which is where per-query
      growth now lives; a client's own guard is `MAX_ITEMS_PER_RESULTS_FRAME`,
      enforced at the parse. Name `hopd`'s source module as the enforcement
      site, the way this constant already names its enforcement sites.

**Tests:**

- [ ] No behavior changes here, so no new test. `cargo test --workspace` still
      passes, including the crate's doc tests.

---

### Task 5: The reference client replaces

**Files:** `crates/hop-cli/src/lib.rs`

**Why:** `hop-cli` is the worked example a frontend author copies. Under
replacement, appending would print every item once per provider arrival.

**Steps:**

- [ ] `try_run_query`'s results arm assigns the frame's items over the held
      list instead of extending it, with a comment pointing at
      `DaemonMsg::Results`' contract rather than restating it.
- [ ] Remove `QueryError::OverCap` and the accumulation check that raised it.
      Under replacement there is no accumulated total to guard: one frame is
      one complete list, and `MAX_ITEMS_PER_RESULTS_FRAME` is enforced at the
      parse by `de_results_items` before this code sees the frame. Removing a
      guard that can no longer fire is the honest change; leaving dead code
      that looks like a bound is not. Say that in the comment where the check
      used to be.
- [ ] The `hop_protocol::limits::MAX_ITEMS_PER_QUERY` import goes with it.

**Tests:**

- [ ] `crates/hop-cli/tests/e2e.rs` passes with the Task 2 change already in
      place.
- [ ] If any unit test in `crates/hop-cli/src/lib.rs` covers `OverCap`, it is
      deleted rather than weakened — the behavior is gone, not relaxed.

---

### Task 6: Two providers over a real socket

**Files:** `crates/hopd/tests/assembly.rs` (new)

**Why:** acceptance criterion 5, which names the four pipeline behaviors that
must be observable *through the daemon*, and the test shape that proves it.

**Notes for the implementer:** `crates/hopd/tests/common/mod.rs` already
provides `start_daemon<S: ResultSource>(source)`, `send`, `recv`, `hello`,
`ScriptedProvider` and `scripted_item`. Build a `ProviderHost`, register two
`ScriptedProvider`s with different delays and different declared kinds, wrap it
with `HostSource::with_pipeline` carrying a seeded `Pipeline`, and hand that to
`start_daemon`. Read `common/mod.rs` before writing anything — match its
existing helpers rather than adding parallel ones.

**Tests:**

- [ ] `two_providers_items_arrive_ranked_together_not_in_completion_order` —
      the fast provider answers with a weak match, the slow one with a strong
      match; the second frame puts the strong match first. Provider-completion
      order would put it last, so the assertion is a real red against the
      pre-slice behavior.
- [ ] `an_alias_boost_takes_effect_through_the_daemon` — seed `Aliases` on the
      pipeline so a term the alias boosts outranks an item that would otherwise
      win, and assert the order over the socket.
- [ ] `a_learning_boost_takes_effect_through_the_daemon` — seed `Learning` via
      `record_launch` for one item id, assert it outranks its equal-scoring
      sibling.
- [ ] `an_exclusive_route_filters_to_that_modes_kinds` — an explicit prefix
      whose mode one provider's kind serves and the other's does not; only the
      matching kind survives.
- [ ] `an_inferred_route_promotes_without_removing` — an inferred-mode term;
      that mode's kind leads and the other provider's items are still present
      behind it. Assert both halves: promotion that also drops the rest would
      pass a test that only checks the first item.
- [ ] `the_first_frame_arrives_before_the_slow_provider_finishes` — criterion
      3 over a real socket, complementing Task 2's unit-level version.

Every assertion above must be verified as a genuine red: state in the report,
per test, what implementation bug it fails against. A test that passes against
provider-completion order is not testing assembly.

---

### Task 7: The glossary catches up

**Files:** `CONTEXT.md`, `crates/hop-core/src/host.rs`

**Why:** three glossary entries describe behavior this slice changes, and one
module's docs describe assembly as something that does not happen.

**Steps:**

- [ ] **Retained set** — it is the last assembled list, replaced per frame, not
      the union of what an exchange delivered. Keep the rule it exists for
      (what `execute` resolves against) and say plainly what changed: an item
      replaced away is no longer resolvable, which is the decision issue #103
      recorded.
- [ ] **Truncate-and-terminate** — still the daemon's answer at a cap, but the
      cap it answers is now the accumulator's `MAX_ITEMS_PER_QUERY` in the
      result source, plus the connection's `MAX_ITEMS_PER_RESULTS_FRAME` on one
      list. The truncation-versus-refusal distinction the entry draws is
      unchanged.
- [ ] Add one new term for the wire's unit under replacement — a **replacement
      frame**: one `results` frame carrying a query's complete current list,
      which the client swaps in whole. Cross-reference **Stale-frame drop** and
      **Terminal frame**, both unchanged.
- [ ] **Result assembly** and **Provider host** — both say the daemon does not
      call `Pipeline::assemble` and name issue #103 as the gap. Rewrite them:
      the host still does not call it, and the **result source** now does, on
      every provider arrival. The sentence about the pin-budget half of
      `Assembly::rejections` going unlogged stays true and should say where the
      rejections now travel to be discarded.
- [ ] `crates/hop-core/src/host.rs`'s "What is not enforced here, and where it
      goes instead" section currently explains that wiring assembly needs a
      protocol answer and names #103. Replace it with the answer that was
      chosen and where assembly lives now.

**Tests:**

- [ ] Docs only; `cargo test --workspace` must still pass (doc tests included).

---

## Acceptance criteria coverage (from issue #103)

| Criterion | Where |
| --- | --- |
| 1. The daemon calls `Pipeline::assemble` on the query path | Task 2 |
| 2. Each arrival re-assembles over everything so far, sent as a replacing frame | Task 2 (`each_arrival_re_assembles_over_every_item_received_so_far`), Task 3 |
| 3. No provider is waited on before the first frame | Task 2 and Task 6 (`the_first_frame_arrives_before_the_slow_provider_finishes`) |
| 4. `max_results` and the pin budget apply to the whole set | Task 2 (`max_results_is_applied_to_the_whole_assembled_set_not_per_provider`) |
| 5. Alias, learning, exclusive filter, inferred promotion — two providers, real socket | Task 6 |
| 6. Delivered/retained accounting under replacement | Task 3 (`a_re_sent_item_is_not_charged_twice`, `the_retained_set_is_the_last_list_not_the_union`), Task 2's accumulator cap |
| 7. Replace-vs-append documented in `hop-protocol` | Task 4 |
