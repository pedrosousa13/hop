# Calculator Provider (Issue #58) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the calculator provider — the second and last of M2's two providers, and the one that proves a non-indexed, purely computational source works through the same seam a disk-backed one (`apps`) does, per issue #58's seven acceptance criteria.

**Architecture:** A new module, `crates/hopd/src/calculator.rs`, holds three pure functions — `evaluate` (a term to a finite `f64`, or `None`), `format_result` (an `f64` to a display string with a stated, tested rounding/notation rule), and `build_item` (the two combined into a `hop_protocol::Item`) — and a stateless `Provider` implementation, `CalculatorProvider`, whose `query` calls `build_item` and whose `execute` re-derives the same result from the item's id rather than caching anything. `hopd::server::build_host` registers it alongside `SkeletonProvider` and `AppsProvider`. Evaluation is done by `fasteval` (MIT), the expression engine the v1 design spec names for this slice.

**Tech Stack:** Rust 2024, the `fasteval` crate (v0.2, `default-features = false`) for expression evaluation — new to this workspace's dependency graph, but no new `deny.toml` entry (MIT is already allow-listed).

## Global Constraints

- **One new third-party dependency, deliberate and already-licensed.** `fasteval` 0.2.4 is MIT — verified against its own vendored `Cargo.toml` (`license = "MIT"`) — and MIT is already on `deny.toml`'s allow list (`deny.toml:138`), so **this plan makes no `deny.toml` edit**. `default-features = false` drops the crate's one feature, `alpha-keywords` (word operators `and`/`or`), which is also fasteval's *default* feature and which this provider has no use for — the same shape as issue #57's `inotify` dependency disabling its own default `stream` feature (`docs/superpowers/plans/2026-08-04-issue-57-apps-provider.md`, Task 6 Step 1). `fasteval` itself declares zero dependencies of its own (`[dependencies]` is empty in its vendored `Cargo.toml`), so adding it changes nothing else in `Cargo.lock`.
- **Gate commands, all four required:** `cargo test --workspace` (577 tests today, all green — verified by running the suite before writing this plan) · `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny check`.
- **No `.unwrap()` in production code** (`clippy::unwrap_used` + `-D warnings`). Test modules open with `#![allow(clippy::unwrap_used)]`.
- **No new `unsafe`.** `fasteval`'s public API this plan uses — `fasteval::ez_eval` and `fasteval::EmptyNamespace` — is entirely safe Rust. This workspace's `unsafe_code = "deny"` lint (root `Cargo.toml`) needs no exception from this slice.
- **The latency contract (spec §3):** keystroke → results < 10 ms; no disk reads, subprocess spawns or HTTP inside `Provider::query`. This provider has no I/O to forbid in the first place — see Design decision 6 — but `Provider::execute` is held to the same "no I/O" fact here too, since re-evaluating an expression is exactly as cheap the second time.
- **No AI attribution** in commits or the PR.
- **Two dependencies this plan leans on that did not exist when the sibling apps-provider plan was written**, verified by reading the current tree rather than assumed from that earlier plan:
  - **Issue #103** (wiring `Pipeline::assemble` into the daemon) has landed. `crates/hopd/src/lib.rs`'s own module doc says so directly: "Result *assembly* is no longer one of the gaps... every provider arrival re-runs `hop-core`'s `pipeline` over everything received so far... and replaces the client's list with the ranked, boosted, capped result." `crates/hopd/src/source.rs`'s `HostSource::start` is where this happens. Nothing in this plan needs to wire assembly — it already runs over whatever this provider returns.
  - **Issue #59** (`hop exec`) has landed. `crates/hopd/src/connection.rs`'s `Execute` arm (`connection.rs:316-408`) resolves an `Execute` frame against the connection's retained item set and dispatches through `ResultSource::execute` → `ProviderHost::execute` → `Provider::execute`, all the way to a `DaemonMsg::Executed` reply. Unlike `docs/superpowers/plans/2026-08-04-issue-57-apps-provider.md`, which explicitly deferred testing `execute` over the socket ("Not in scope... Dispatching `execute` through the daemon") because that machinery did not exist yet, **this plan's Task 6 drives `execute` over the real socket as a first-class part of proving acceptance criterion 7.**

## Scope: what this slice is and is not

**In scope**, the seven acceptance criteria on issue #58:

1. A simple arithmetic query returns its result as an item — Tasks 1, 3, 4, 6.
2. Unary minus and percent are handled — Task 1 (`evaluate`'s own tests), Task 6 (`unary_minus_and_percent_are_handled_over_the_socket`).
3. The default action on a calculator item copies the result — Task 3 (the item's one `Copy` action), Task 4 (`execute`), Task 6.
4. Input that is not an expression yields no calculator items rather than an error item — Task 1 (every failure mode folded into `None`), Task 4 (`query` returns `Ok(vec![])`, never an `Err`), Task 6.
5. Calculator results augment rather than replace other providers' results, per the router's existing semantics — Design decision 1, Task 4, Task 6 (`a_math_looking_query_augments_rather_than_replaces_other_providers_results`).
6. The provider performs no disk, subprocess or network work — Design decision 6, Task 4 (a structural test).
7. An integration test drives it through the daemon over a real socket — Task 6, and (new, since #59 landed) over `execute` too.

**Not in scope, deliberately:**

- **Augmentation/promotion machinery.** `hop_core::router::looks_like_math` (`router.rs:497-510`) already routes a bare `2+2` to `Mode::Calculator` with `exclusive: false`, and an explicit `=2+2` to the same mode with `exclusive: true`. `ProviderHost::selected` (`host.rs:551-567`) already includes every `Mode::All` provider whenever the route is not exclusive — see its own `an_inferred_route_selects_both_the_mode_all_provider_and_the_provider_declaring_that_mode` test (`host.rs:1692-1711`), which already builds a `"calculator"`-id, `Mode::Calculator`-only fixture provider as its worked example. `Pipeline::assemble`'s `promote_kinds` (`pipeline.rs:826-831`) already pins an inferred mode's items on top without hiding the rest, pinned by `inferred_utility_pins_on_top_without_hiding_others` (`pipeline.rs:1207-1228`), whose own test data is a `Kind::Calculator` item titled `"2+2 = 4"` with id `"calc:2+2"` — i.e. this exact provider's expected shape, already assumed elsewhere in the tree. **This plan adds no augmentation code of its own.** Task 6 still adds one integration-level test proving it end to end for this specific provider, because "the router already does it" is a claim about `hop-core`, not evidence this provider's own manifest is shaped to receive the benefit — see Design decision 1.
- **Router changes.** `looks_like_math`, the `=` sigil, and `Mode::Calculator`'s routing rules are M1 work, already implemented and tested (`router.rs:640-945`). This issue only adds the provider that consumes the existing route.
- **Percent-of semantics.** The issue's brief says "percent" without defining it, and the old extension it gestures at is not in this repository to compare against. `fasteval` defines `%` as strict binary modulo — verified against its own source and empirically (Design decision 5) — and that is what this plan implements. Inventing a percent-of reading (`50%` → `0.5`) is explicitly declined; see Design decision 5 and "What I could not verify" below.
- **`Item.copy_text`.** `Item` carries an optional `copy_text` field distinct from the `Copy` action's `ExecOutcome::CopyText` — `CONTEXT.md:413-417` names the distinction ("an item's `copy_text` is not [a command-shaped outcome] either — it reaches the same clipboard, but by way of an item rather than an outcome"). Acceptance criterion 3 asks for the *action* to copy the result, which the `Copy` action + `execute()` round trip satisfies on its own; this plan leaves `Item.copy_text: None`, matching `crates/hopd/src/apps.rs`'s own precedent (every item it builds also leaves `copy_text: None`) rather than opening a second, unrequested path to the same string that would need to be kept in sync forever.
- **A `providers/` subdirectory.** `docs/superpowers/plans/2026-08-04-issue-57-apps-provider.md`'s Design decision 1 left this question open for "whichever issue adds the second [provider]" — that is this issue. Design decision 7, below, answers it: still no.
- **A `CONTEXT.md` glossary addition.** `Mode::Calculator` and `Kind::Calculator` are already documented (`CONTEXT.md:18-19` for `Kind`, `:56-58` for `Mode`); nothing this provider introduces is new vocabulary the way "Desktop entry" and "App id" were for the apps provider. See Self-review notes.
- **A provider config gate.** `crates/hopd/src/config.rs` has one knob, `max_results`; registration in `build_host` is unconditional, matching the apps provider's own precedent.
- **fasteval's SI-suffix literals and built-in functions** (`1.23K` = 1230, `abs(x)`, `e()`, `pi()`, ...), reachable only through the exclusive `=` route since `looks_like_math`'s shape check excludes letters from the inferred route. Not restricted or specially handled by this plan — see "What I could not verify."

## Design decisions (read before any task)

**1. The manifest declares `modes: vec![Mode::Calculator]` alone — deliberately not `Mode::All`.** `ProviderHost::selected` (`host.rs:551-567`) selects a registration when *either* `should_query` matches by literal mode containment, *or* the route is non-exclusive and the registration declares `Mode::All` (the "augmentation" branch). `hop_core::router::route` (`router.rs:395-449`) sends **both** the explicit `=2+2` (`exclusive: true`) and the inferred bare `2+2` (`exclusive: false`) to `Mode::Calculator` — the same mode value either way. So a manifest declaring `modes: [Mode::Calculator]` already reaches both routes through `should_query`'s literal check alone; it needs no help from the augmentation branch to run for either shape of math query. `host.rs:1692-1711`'s `an_inferred_route_selects_both_the_mode_all_provider_and_the_provider_declaring_that_mode` is the load-bearing precedent here — it is `hop-core`'s own worked example of exactly this shape (`ScriptedProvider::new("calculator", vec![Kind::Calculator], vec![])` with `manifest.modes = vec![Mode::Calculator]`), asserting both it *and* a separate `Mode::All` provider are selected together for `route("2+2")`.

   Adding `Mode::All` on top would cost something real, not merely be redundant: it would ask this provider to attempt an evaluation on **every non-math keystroke of every query** in the launcher — the exact cost this plan's own task list warns against — for an outcome that is always `None` (`evaluate` refuses anything that doesn't parse). `Mode::Calculator` alone is what the router's own routing already promises: this provider is asked to run *only* when the router itself has decided a query looks like math, either by shape (`looks_like_math`) or by the user's own `=` sigil.

   `min_term_len: 1`, not `0`: an empty term never evaluates (`evaluate("")` and `evaluate("   ")` both refuse — see Task 1's tests), so `min_term_len: 0` would let the bare `=` route (`RoutedQuery { mode: Calculator, term: "", exclusive: true }`) reach `query()` only to fail deterministically. `min_term_len: 1` skips that task spawn and evaluation attempt at the pre-filter instead, at zero cost to any real query (`should_query`'s length check is `term.chars().count() < min_term_len`, so a genuine one-character term like `"5"` still passes).

   `budget: Duration::from_millis(5)` — matching `AppsProvider`'s own manifest (`apps.rs:1545`), comfortably above what parsing and evaluating a string under `MAX_QUERY_TEXT` (1 024 bytes) needs, and well under `ProviderHost`'s `MAX_PROVIDER_BUDGET` ceiling (50 ms).

**2. The item id is `calc:<term>` — the expression, not the result — and `execute()` re-evaluates from it rather than reading a cached value.** Two facts, checked before choosing, both argue the same way:

   - **The learning store keys on the bare item id alone.** `Learning::record_launch` and `Learning::boost_for` (`learning.rs:1193-1195`, `:1302-1306`) both resolve to `self.record(query, item_id.as_str())` / `... (item_id.as_str())` — no other dimension is read, for either the in-memory `selections` map or the persisted `global_frequency` map. If the id instead encoded the *result* (e.g. `calc:4`), two different expressions that happen to land on the same number — `2+2` and `1+3`, or `10/4` and `2.5`— would share one learning key, and launching one would silently boost the other's frecency. Encoding the expression keeps every distinct query its own learning row, which is the whole point of frecency.
   - **This is already the shape the rest of the tree assumes.** `crates/hop-core/src/pipeline.rs` and `crates/hop-core/src/rank.rs` both build `Kind::Calculator` test fixtures as `"calc:2+2"`, `"calc:terminal"`, `"calc:1"`, `"calc:evil"` and similar (`pipeline.rs:1215`, `:1452`, `:1454`, `:1890`, `:2275`, `:2285`, `:2289`; `rank.rs:785`) — every one of them the *expression*, not a computed value, with a title of the form `"<expr> = <result>"` (`pipeline.rs:1215`: `item(Kind::Calculator, "calc:2+2", "2+2 = 4")`). This plan matches a scheme the tree already leans on rather than inventing a third one.

   Since evaluation is pure and — per `fasteval`'s own documented `TooLong`/`TooDeep`/`SlabOverflow` safety limits (`error.rs`) — bounded in cost regardless of input, `execute()` re-deriving the result from the id costs about what building it in `query()` cost the first time. There is nothing to cache correctly *because* there is nothing that can go stale: the same term always evaluates to the same value. `CalculatorProvider` therefore holds no state at all — unlike `AppsProvider`, which owns an `Arc<AppIndex>`.

   **The `MAX_ITEM_ID` bound.** `MAX_QUERY_TEXT` bounds a query's raw text at 1 024 bytes (`limits.rs:96`), which is the largest `term` this provider is ever handed in production (it is the routed term, itself derived from that bound). `"calc:"` adds 5 more bytes, for 1 029 at the absolute worst case — comfortably under `MAX_ITEM_ID`'s 4 096 (`limits.rs:104`). So `ItemId::new(format!("calc:{term}"))` can be proven never to fail for anything the router hands this provider. It is still coded as `.ok()?` rather than `.expect(...)` — the guard exists because the type allows the possibility, not because it is expected to fire, matching `crates/hopd/src/apps.rs`'s own `build_entry` (`apps.rs:222-227`) for the identical reasoning — and `build_item` is also exercised directly by this module's own unit tests with hand-written terms that do not go through the router's bound at all.

**3. Result formatting: a stated, table-tested rule, verified against a real `rustc` run rather than estimated.** `fasteval` evaluates to `f64`, and `2.0 + 2.0` must read as `"4"`, never `"4.0"`. The rule:

   - An exact `0.0` (which also catches `-0.0`, since `-0.0 == 0.0` under IEEE-754) prints as `"0"` — never the surprising `"-0"`.
   - A magnitude in `[1e-9, 1e15)` prints fixed at 10 decimal places (`format!("{value:.10}")`), then has trailing `0`s trimmed, then a bare trailing `.` trimmed if nothing is left after it.
   - Everything else (magnitude `>= 1e15`, or nonzero and `< 1e-9`) prints in Rust's own `{:e}` form, which is already minimal (`format!("{:e}", 1e20)` is `"1e20"`, not a zero-padded mantissa) and needs no further trimming.

   `1e-9` (not, say, `1e-10`) is the small-magnitude cutoff with deliberate margin: at 10 fixed decimal places, a magnitude below `5e-11` rounds to the literal string `"0.0000000000"` — verified by direct measurement (`format!("{:.10}", 4.9e-11)` is `"0.0000000000"`; `format!("{:.10}", 5e-11)` is `"0.0000000001"`). `1e-9` sits a full order of magnitude above that rounding boundary rather than exactly on it.

   The table below was produced by actually compiling and running the rule (`rustc --edition 2021`, not hand-computed):

   | Value | Formats as | What it demonstrates |
   | --- | --- | --- |
   | `2.0 + 2.0` | `"4"` | no trailing `.0` |
   | `1.0 / 3.0` | `"0.3333333333"` | the precision cap (10 threes, not sixteen) |
   | `0.1 + 0.2` | `"0.3"` | float noise (`0.30000000000000004`) cleaned by the round + trim |
   | `-4.0` | `"-4"` | sign preserved, no trailing zero |
   | `0.0` / `-0.0` | `"0"` | both zeros, one string |
   | `10.0 / 4.0` | `"2.5"` | a genuine fractional trailing digit survives |
   | `2f64.sqrt()` | `"1.4142135624"` | rounds at the 10th place (`...23730951` → `...24`) |
   | `1e14` | `"100000000000000"` | just under the exponent threshold, fixed form |
   | `1e15` | `"1e15"` | exponent threshold, inclusive |
   | `1e20` | `"1e20"` | large magnitude |
   | `1e-9` | `"0.000000001"` | small-magnitude threshold, inclusive (fixed) |
   | `9e-10` | `"9e-10"` | just under the small threshold, exponential |
   | `1e-15` | `"1e-15"` | far below, exponential |

**4. Non-finite and non-parseable input both yield `None` from one function — never a `ProviderError`, never an error item.** Verified directly against the vendored `fasteval` 0.2.4 source with a throwaway probe crate (built against the cached registry source, not assumed from docs): `fasteval::ez_eval` does **not** error on division by zero — `"1/0"` evaluates to `Ok(f64::INFINITY)`, `"0/0"` to `Ok(f64::NAN)`, `"-1/0"` to `Ok(f64::NEG_INFINITY)`. Division is plain IEEE-754 `f64` division inside `fasteval`'s evaluator, with no special-casing. So a naive `Result::ok()` on `ez_eval`'s output would hand `build_item` a value it could format and show as an item titled `"1/0 = inf"` — exactly the "error item" acceptance criterion 4 forbids, just spelled with a valid-looking title rather than an `ErrorCode`. `evaluate` (Task 1) therefore checks `value.is_finite()` on every `Ok` and folds the non-finite case into `None`, identically to how it folds a genuine parse failure (`"hello"` → `Err(Undefined("hello"))`, `"2+2x"` → `Err(UnparsedTokensRemaining("x"))`, `""` → `Err(EofWhileParsing("value"))`) into the same `None`. `CalculatorProvider::query` never distinguishes the two: `build_item(&q.term).into_iter().collect()` is `Ok(vec![])` either way, never an `Err(ProviderError::Failed(_))`.

**5. `%` is modulo — read from fasteval's own source, not invented, and flagged as the one reversible judgment call in this plan.** The issue's brief says "Unary minus and percent are handled, matching the old extension's behavior," and that extension is not in this repository to check. The v1 design spec names `fasteval` as the engine for this slice (§5), and `fasteval`'s own module docs state its operator precedence table plainly: `^` (exponentiation) `>` `%` (modulo) `>` `/` `>` `*` `>` `-` `>` `+` — `%` is strict binary modulo, the same operator Rust's own `%` is. Verified empirically against the vendored source too: `"10%3"` → `Ok(1.0)`, matching Rust's `10 % 3`. There is no percent-of arm in `fasteval`'s grammar at all — `"50%"` alone (no right-hand operand) is a **parse error** (`EofWhileParsing("value")`), not `0.5`. A user who types `50%` expecting "half of a hundred" gets **no calculator item whatsoever**, the same outcome as typing any other malformed expression. This plan implements modulo — the only reading `fasteval` supports — and documents the choice in three places, as instructed: this module's own doc comment, this section, and a line for the PR body. It does not invent percent-of semantics. This is the one judgment call in this plan a future decision could reasonably reverse, and it is flagged again under "What I could not verify."

**6. No I/O — true by construction, and pinned by a structural test rather than asserted alone.** Unlike `AppsProvider`, this provider has no index to build, no filesystem to watch, and nothing to persist: `evaluate`, `format_result`, `build_item`, `copy_text_for`, `CalculatorProvider::query` and `CalculatorProvider::execute` each take and return only `&str`/`String`/`f64`/`Item`/`ItemId`/`ExecOutcome` — nothing capable of naming a path, spawning a process, or opening a socket. That is inspectable directly (no `std::fs`, `std::process`, or networking symbol appears anywhere in the module's dependency graph). Task 4 also pins this the way `crates/hop-protocol/src/item.rs`'s `every_test_this_file_names_in_its_docs_exists` reads its own source back, and the way root `Cargo.toml`'s `unsafe_code` comment names `grep -rn unsafe_code crates/` as its own spot-check: a test that reads `include_str!("calculator.rs")` and asserts it never contains `std::fs`, `std::process`, `std::net`, `TcpStream` or `UdpSocket`. What this catches that the design argument alone would not: a future edit that "helpfully" adds a lookup table loaded from disk, or shells out to `bc` for arbitrary-precision arithmetic, fails this test immediately rather than only being caught by someone re-reading the module's dependency graph by hand.

**7. The code lives in `crates/hopd/src/calculator.rs`, a flat module — the `providers/` subdirectory question, reopened and declined again.** `docs/superpowers/plans/2026-08-04-issue-57-apps-provider.md`'s own Design decision 1 left this open on purpose: "if issue #58 (calculator) or a later provider make a shared parent module worth having, that is a decision for whichever issue adds the second one, not this one." This is that issue, and the answer is still no. `crates/hopd/src/` today holds `apps.rs`, `calculator.rs` (this plan), `config.rs`, `connection.rs`, `runtime_dir.rs`, `server.rs`, `source.rs`, `state_dir.rs` — eight flat files, still well within what a flat directory serves comfortably. The two functions the two providers could plausibly share (`truncate_to_byte_boundary`) are ten lines each; this plan duplicates the calculator's own copy rather than introducing a shared module for one ten-line function, the same cost/benefit apps.rs's plan weighed for a whole crate boundary and declined for less reason. Renaming `apps.rs`'s existing, tested, integration-tested path into a new `providers/` directory as a side effect of *this* issue would also be unrelated churn to a file this issue does not otherwise touch — a decision for whichever change actually needs the structure, still not this one.

## File Structure

**Created:**
- `crates/hopd/src/calculator.rs` — expression evaluation, result formatting, item construction, and `CalculatorProvider` itself.
- `crates/hopd/tests/calculator.rs` — the integration test driving `CalculatorProvider` through the daemon over a real socket, including `execute` (acceptance criterion 7).

**Modified:**
- `crates/hop-core/src/provider.rs` — add `CALCULATOR_PROVIDER_ID`, alongside `APPS_PROVIDER_ID`.
- `crates/hopd/src/lib.rs` — declare `pub mod calculator;`; retire the module-doc sentence that names #58's calculator as the remaining gap.
- `crates/hopd/src/server.rs` — `build_host` registers `CalculatorProvider`; its existing `build_host_tests` module is extended.
- `Cargo.toml` (workspace root) — add `fasteval = { version = "0.2", default-features = false }` to `[workspace.dependencies]`.
- `crates/hopd/Cargo.toml` — add `fasteval.workspace = true` to `[dependencies]`.

**Not modified, deliberately:** `deny.toml` (MIT already allow-listed) and `CONTEXT.md` (no new vocabulary) — see Global Constraints and Scope above.

---

### Task 1: Pure expression evaluation

**Files:**
- Create: `crates/hopd/src/calculator.rs`
- Modify: `Cargo.toml` (workspace root), `crates/hopd/Cargo.toml`, `crates/hopd/src/lib.rs` (add `pub mod calculator;` only — leave the module-doc fix to Task 5)

**Interfaces:**
- Produces, for Task 2 and Task 3:
  ```rust
  pub(crate) fn evaluate(expr: &str) -> Option<f64>;
  ```

This task's function is pure: no `hop-protocol` types, no `hop-core` types, nothing but `fasteval` and `&str`/`f64`.

- [ ] **Step 1: Add the dependency to the workspace**

In the workspace root `Cargo.toml`'s `[workspace.dependencies]`, add (after `regex = "1"`, before the `toml` comment block):

```toml
# The v1 design spec names fasteval for the calculator provider (§5's
# providers table, M2.6 / issue #58). MIT — already covered by deny.toml's
# existing MIT allow-list entry, so this needs no new license line.
# `default-features = false` drops the crate's one feature, `alpha-keywords`
# (word operators `and`/`or`), which is also fasteval's *default* feature and
# which this provider has no use for.
fasteval = { version = "0.2", default-features = false }
```

- [ ] **Step 2: Add the dependency to `hopd` and confirm the gate**

In `crates/hopd/Cargo.toml`'s `[dependencies]`, add (after `inotify.workspace = true`):

```toml
fasteval.workspace = true
```

Run `cargo build -p hopd` once to confirm `Cargo.lock` picks up `fasteval` alone (it has zero dependencies of its own — confirmed against its vendored `Cargo.toml`, so nothing else should change). Then run `cargo deny check` and confirm it is green with no edit to `deny.toml` — this is the check that proves MIT already covers it.

- [ ] **Step 3: Write the failing tests**

Create `crates/hopd/src/calculator.rs`:

```rust
//! The calculator provider: evaluates arithmetic expressions with
//! `fasteval` and offers the formatted result as a single, copyable item.
//!
//! Every function in this module is pure — no `std::fs`, no `std::process`,
//! no network client anywhere in this file (acceptance criterion 4 on
//! issue #58; pinned structurally once the whole module exists, by
//! `provider_tests::the_module_source_touches_no_disk_process_or_network`
//! in Task 4). There is no index to build, no state to watch, and no cache
//! between a `query()` and the `execute()` that follows it — the expression
//! is re-evaluated from the item id, because re-evaluating is as cheap as
//! evaluating was the first time. See this crate's implementation plan
//! (`docs/superpowers/plans/2026-08-09-issue-58-calculator-provider.md`)
//! for the full reasoning behind the item-id scheme, the formatting rule,
//! and the one deliberate reading of `%` as modulo rather than percent-of —
//! `fasteval`'s own grammar has no percent-of operator, and this module
//! does not add one.

use fasteval::EmptyNamespace;

/// Evaluates `expr` as an arithmetic expression, folding every failure
/// mode into `None`: a parse error (an unbalanced paren, a bare `%` with no
/// right-hand operand, an unknown identifier), *and* a successful
/// evaluation whose result is not finite. Acceptance criterion 4 draws no
/// distinction between "this is not an expression" and "this expression's
/// answer is not a number worth copying," so this function draws none
/// either — every caller downstream treats `None` as the single "no
/// calculator item" outcome.
///
/// # `fasteval` does not error on `1/0` or `0/0`
///
/// Verified directly against the vendored `fasteval` 0.2.4 source: division
/// is plain `f64` division with no special-casing, so `1/0` evaluates to
/// `Ok(f64::INFINITY)`, `0/0` to `Ok(f64::NAN)`, `-1/0` to
/// `Ok(f64::NEG_INFINITY)` — IEEE-754 semantics, not a refusal. Without the
/// `is_finite()` check below, this function would hand a caller a value it
/// could format and show as `"1/0 = inf"`, which is exactly the "error
/// item" acceptance criterion 4 forbids, just spelled differently.
pub(crate) fn evaluate(expr: &str) -> Option<f64> {
    let mut ns = EmptyNamespace;
    match fasteval::ez_eval(expr, &mut ns) {
        Ok(value) if value.is_finite() => Some(value),
        Ok(_) | Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Arithmetic the issue names by hand: unary minus and percent. ---

    #[test]
    fn a_simple_sum_evaluates() {
        assert_eq!(evaluate("2+2"), Some(4.0));
    }

    #[test]
    fn unary_minus_is_handled() {
        assert_eq!(evaluate("-4+10"), Some(6.0));
        assert_eq!(evaluate("-5"), Some(-5.0));
    }

    #[test]
    fn percent_is_modulo_not_percent_of() {
        // Verified against fasteval's own precedence table and empirically,
        // against the vendored 0.2.4 source: `%` is strict binary modulo,
        // matching Rust's own operator. See this plan's Design decision 5
        // — the old extension this issue's brief gestures at is not in
        // this repo to compare against, so this reading is fasteval's,
        // stated and tested rather than assumed.
        assert_eq!(evaluate("10%3"), Some(1.0));
        assert_eq!(evaluate("7%3"), Some(1.0));
    }

    #[test]
    fn a_bare_trailing_percent_is_not_an_expression() {
        // The percent-of reading a user might expect from "50%" does not
        // exist in fasteval's grammar: `%` is infix-only, so a value with
        // nothing on its right is a parse error, not 0.5. Pinned here so a
        // future change that adds percent-of support changes this test
        // rather than silently reversing it.
        assert_eq!(evaluate("50%"), None);
    }

    #[test]
    fn exponents_and_parentheses_are_handled() {
        assert_eq!(evaluate("2^10"), Some(1024.0));
        assert_eq!(evaluate("(1+2)*3"), Some(9.0));
    }

    #[test]
    fn surrounding_and_internal_whitespace_is_tolerated() {
        assert_eq!(evaluate("  2 + 2  "), Some(4.0));
    }

    // --- Criterion 4: not an expression, or not a usable answer. ---

    #[test]
    fn division_by_zero_yields_no_result_rather_than_infinity() {
        assert_eq!(evaluate("1/0"), None);
        assert_eq!(evaluate("-1/0"), None);
    }

    #[test]
    fn zero_over_zero_yields_no_result_rather_than_nan() {
        assert_eq!(evaluate("0/0"), None);
    }

    #[test]
    fn an_identifier_is_not_an_expression() {
        assert_eq!(evaluate("hello"), None);
    }

    #[test]
    fn trailing_garbage_after_a_valid_prefix_is_not_an_expression() {
        assert_eq!(evaluate("2+2x"), None);
    }

    #[test]
    fn an_unterminated_expression_is_not_an_expression() {
        assert_eq!(evaluate("2+"), None);
        assert_eq!(evaluate("(1+"), None);
    }

    #[test]
    fn empty_and_whitespace_only_input_is_not_an_expression() {
        assert_eq!(evaluate(""), None);
        assert_eq!(evaluate("   "), None);
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test -p hopd calculator::`
Expected: FAIL to compile — the `calculator` module is not declared in `lib.rs` yet.

- [ ] **Step 5: Add the module declaration**

In `crates/hopd/src/lib.rs`, add `pub mod calculator;` to the existing module list, alphabetically (`apps`, `calculator`, `config`, `connection`, `runtime_dir`, `server`, `source`, `state_dir`). Do not touch the module-doc paragraph naming issue #58 yet; Task 5 does that.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p hopd calculator::`
Expected: PASS, every test above.

- [ ] **Step 7: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: all four green.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/hopd/Cargo.toml crates/hopd/src/calculator.rs crates/hopd/src/lib.rs Cargo.lock
git commit -m "hopd: pure expression evaluation for the calculator provider"
```

---

### Task 2: Result formatting

**Files:**
- Modify: `crates/hopd/src/calculator.rs`

**Interfaces:**
- Produces, for Task 3:
  ```rust
  pub(crate) fn format_result(value: f64) -> String;
  ```

Pure: takes and returns nothing but numbers and strings. No new dependency.

- [ ] **Step 1: Write the failing tests**

Append to `crates/hopd/src/calculator.rs` (below `evaluate`, above its `#[cfg(test)] mod tests`):

```rust
/// Decimal places [`format_result`] keeps before trimming trailing zeros —
/// generous enough that `1/3` reads as `"0.3333333333"` rather than the
/// full `f64` precision (`0.3333333333333333`), which would print
/// sixteen-plus digits of noise for the common case (`0.1 + 0.2` is
/// `0.30000000000000004` in `f64` — see the test table).
const FIXED_DECIMALS: usize = 10;

/// At or above this magnitude, [`format_result`] switches to exponential
/// notation rather than printing a wall of digits: `"1e20"` reads as a
/// calculator answer, `"100000000000000000000"` reads as a typo.
const EXPONENTIAL_ABOVE: f64 = 1e15;

/// Below this magnitude (and not exactly zero), [`format_result`] also
/// switches to exponential notation. Chosen with a full order of magnitude
/// of margin over the point where fixed formatting at [`FIXED_DECIMALS`]
/// places rounds a genuinely nonzero value down to a string of all zeros —
/// measured directly: `format!("{:.10}", 4.9e-11)` is `"0.0000000000"`,
/// `format!("{:.10}", 5e-11)` is `"0.0000000001"`. `1e-9` sits well clear of
/// that rounding boundary rather than exactly on it.
const EXPONENTIAL_BELOW: f64 = 1e-9;

/// Formats an already-finite `value` for display and for
/// [`hop_protocol::CopyText`] alike.
///
/// `2.0 + 2.0` must read as `"4"`, never `"4.0"` — the rule: an exact zero
/// (which also catches negative zero, since `-0.0 == 0.0`) prints as
/// `"0"`; a magnitude inside `[EXPONENTIAL_BELOW, EXPONENTIAL_ABOVE)`
/// prints fixed at [`FIXED_DECIMALS`] places with trailing zeros — and a
/// trailing decimal point, once they're gone — trimmed; anything outside
/// that range prints in Rust's own `{:e}` form, which is already minimal
/// and needs no further trimming.
///
/// Every case in this doc comment, and more, is pinned by
/// `format_tests::format_result_matches_the_verified_table` below —
/// verified against a real `rustc` run before being written into this
/// plan, not estimated.
pub(crate) fn format_result(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    let magnitude = value.abs();
    if !(EXPONENTIAL_BELOW..EXPONENTIAL_ABOVE).contains(&magnitude) {
        return format!("{value:e}");
    }
    trim_trailing_zeros(&format!("{value:.FIXED_DECIMALS$}"))
}

/// Strips trailing `0`s after a decimal point, then the point itself if
/// nothing is left after it. A no-op on a string with no `.` at all — never
/// produced by `format!("{value:.FIXED_DECIMALS$}")` since
/// `FIXED_DECIMALS > 0`, but the guard costs nothing and keeps this
/// function correct for any caller, not just its one today.
fn trim_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn format_result_matches_the_verified_table() {
        let cases: &[(f64, &str)] = &[
            (4.0, "4"),
            (-4.0, "-4"),
            (0.0, "0"),
            (-0.0, "0"),
            (1.0 / 3.0, "0.3333333333"),
            (0.1 + 0.2, "0.3"),
            (10.0 / 4.0, "2.5"),
            (2f64.sqrt(), "1.4142135624"),
            (1e14, "100000000000000"),
            (1e15, "1e15"),
            (1e20, "1e20"),
            (1e-9, "0.000000001"),
            (9e-10, "9e-10"),
            (1e-15, "1e-15"),
        ];
        for (value, expected) in cases {
            assert_eq!(
                &format_result(*value),
                expected,
                "format_result({value}) should be {expected}"
            );
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd calculator::`
Expected: FAIL to compile — `format_result`, `FIXED_DECIMALS`, `EXPONENTIAL_ABOVE`, `EXPONENTIAL_BELOW` are undefined.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p hopd calculator::`
Expected: PASS, every test in both `tests` and `format_tests`.

- [ ] **Step 4: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/src/calculator.rs
git commit -m "hopd: format calculator results for display"
```

---

### Task 3: Item construction

**Files:**
- Modify: `crates/hop-core/src/provider.rs` (add `CALCULATOR_PROVIDER_ID`)
- Modify: `crates/hopd/src/calculator.rs`

**Interfaces:**
- Consumes: Task 1's `evaluate`; Task 2's `format_result`; `hop_core::provider::CALCULATOR_PROVIDER_ID` (added in this task).
- Produces, for Task 4:
  ```rust
  pub(crate) fn build_item(term: &str) -> Option<hop_protocol::Item>;
  ```

- [ ] **Step 1: Add the shared id constant to `hop-core`**

In `crates/hop-core/src/provider.rs`, immediately after `APPS_PROVIDER_ID`'s definition (`provider.rs:46`), add:

```rust
/// The [`ProviderManifest::id`] the calculator provider answers to (M2.6,
/// issue #58).
///
/// Unlike [`APPS_PROVIDER_ID`], nothing in `hop-core` names this constant as
/// an alias-boost target today — there is no `AliasTarget` variant for it,
/// and none is being added by this issue. What this constant still buys is
/// the provenance half of [`crate::pipeline::CheckedItems::check`]:
/// `CalculatorProvider::manifest`'s `id` and every item's own `provider`
/// field must be the *same* string for that item to survive the check, and
/// a hand-written literal typed twice — once in the manifest, once
/// wherever an [`Item`](hop_protocol::Item) is built — is a literal that
/// can drift silently. One constant used in both places cannot.
pub const CALCULATOR_PROVIDER_ID: &str = "calculator";
```

- [ ] **Step 2: Write the failing tests**

Append to `crates/hopd/src/calculator.rs` (below `format_result` and its trim helper, above `#[cfg(test)] mod format_tests`):

```rust
use hop_core::provider::CALCULATOR_PROVIDER_ID;
use hop_protocol::{Action, ActionId, ActionKind, Item, ItemId, Kind, limits::MAX_TITLE};

/// Builds the single item a routed term produces, or `None` if [`evaluate`]
/// could not turn it into a finite result — the one branch point between
/// "show a calculator item" and "show nothing," shared by
/// `CalculatorProvider::query` (Task 4) with no other logic layered on top
/// of it.
///
/// # The item id encodes the expression, not the result
///
/// The id is `calc:<term>` — the *expression* the user typed, not the
/// number it evaluates to. Two facts, both checked before writing this
/// function, argue for this over the alternative (encoding the formatted
/// result instead):
///
/// - [`hop_core::learning::Learning::record_launch`] and
///   [`hop_core::learning::Learning::boost_for`] both key **only** on the
///   bare item id string — `crates/hop-core/src/learning.rs`'s `record`
///   and `boost_for` never look past `item_id.as_str()`. If the id encoded
///   the result, `2+2` and `1+3` — two different expressions that happen
///   to land on the same number — would share one learning key, and
///   launching one would boost the other. Encoding the expression keeps
///   every distinct query its own row.
/// - This is already the shape the rest of the tree assumes:
///   `crates/hop-core/src/pipeline.rs` and `crates/hop-core/src/rank.rs`
///   both build `Kind::Calculator` test fixtures as `"calc:2+2"`,
///   `"calc:terminal"` and similar (`pipeline.rs:1215`, `:1890`, `:2285`;
///   `rank.rs:785`, among others) — this function matches a scheme the
///   tree already leans on, rather than inventing a third one.
///
/// # `ItemId::new` cannot fail here, and is still checked rather than
/// unwrapped
///
/// `term` reaches this function, in production, only after routing, and
/// [`hop_protocol::limits::MAX_QUERY_TEXT`] bounds a query's raw text at
/// 1 024 bytes — the largest `term` this function is ever handed by
/// `CalculatorProvider`. `"calc:"` adds 5 more, for 1 029 at the absolute
/// worst case, comfortably under [`hop_protocol::limits::MAX_ITEM_ID`]'s
/// 4 096. So the `.ok()?` below can be proven never to fire for any term
/// that arrived through the router — the guard exists because the type
/// allows the possibility, not because it is expected to, matching the
/// same reasoning `crates/hopd/src/apps.rs`'s `build_entry` gives for its
/// own `ItemId::new(...).ok()?`. This module's own tests call `build_item`
/// directly with hand-written terms that do not go through that bound at
/// all, so the guard is real for them.
///
/// # The title is truncated; the id is not
///
/// `title` is `"<term> = <result>"`, which can exceed
/// [`hop_protocol::limits::MAX_TITLE`] (1 024 bytes) even though `term`
/// alone cannot exceed 1 024: a term at or near that query bound, plus
/// `" = "` and the formatted result, clears it. Unlike `ItemId::new`
/// above, this is **not** a guard against something that cannot happen — a
/// long, syntactically valid, evaluable expression really can reach this
/// function, and without truncation
/// [`hop_core::pipeline::CheckedItems::check`] would reject the whole item
/// outright as a field-too-long rejection, silently dropping a
/// correctly-computed answer. Truncating the title at a char boundary
/// (never the id, which the bound above shows has room to spare) keeps
/// the item alive instead — pinned by
/// `item_tests::an_overlong_title_is_truncated_rather_than_dropping_the_item`.
pub(crate) fn build_item(term: &str) -> Option<Item> {
    let value = evaluate(term)?;
    let result = format_result(value);
    let id = ItemId::new(format!("calc:{term}")).ok()?;
    let title = truncate_to_byte_boundary(&format!("{term} = {result}"), MAX_TITLE);

    Some(Item {
        id,
        kind: Kind::Calculator,
        title,
        subtitle: None,
        icon: None,
        actions: vec![Action {
            id: ActionId::new("copy").expect("within bounds by construction"),
            kind: ActionKind::Copy,
            label: "Copy".to_string(),
        }],
        default_action: ActionId::new("copy").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: CALCULATOR_PROVIDER_ID.to_string(),
    })
}

/// Truncates `s` to at most `max` bytes, never splitting a multi-byte
/// character. Ported verbatim from `crates/hopd/src/apps.rs`'s function of
/// the same name — not shared via a common module; see this plan's Design
/// decision 7 for why `hopd/src/` stays flat rather than growing a shared
/// module for one ten-line function.
fn truncate_to_byte_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod item_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn build_item_sets_the_calc_prefixed_id_and_the_expr_equals_result_title() {
        let item = build_item("2+2").expect("2+2 evaluates");
        assert_eq!(item.id.as_str(), "calc:2+2");
        assert_eq!(item.title, "2+2 = 4");
        assert_eq!(item.kind, Kind::Calculator);
        assert_eq!(item.provider, CALCULATOR_PROVIDER_ID);
    }

    #[test]
    fn build_item_carries_exactly_one_copy_action_agreeing_with_default_action() {
        let item = build_item("2+2").unwrap();
        assert_eq!(item.actions.len(), 1);
        assert_eq!(item.actions[0].kind, ActionKind::Copy);
        assert_eq!(item.actions[0].id, item.default_action);
    }

    #[test]
    fn build_item_leaves_the_items_own_copy_text_field_unset() {
        // Deliberate — see this plan's Scope section on why the Copy
        // action + execute() round trip is the sole mechanism for
        // acceptance criterion 3, not a second, redundant path.
        let item = build_item("2+2").unwrap();
        assert_eq!(item.copy_text, None);
    }

    #[test]
    fn build_item_returns_none_for_input_evaluate_refuses() {
        assert!(build_item("hello").is_none());
        assert!(build_item("1/0").is_none());
        assert!(build_item("").is_none());
    }

    #[test]
    fn an_overlong_title_is_truncated_rather_than_dropping_the_item() {
        // "1" then "+1" repeated 510 times is a valid, evaluable expression
        // 1021 bytes long — short of MAX_QUERY_TEXT (a real router-derived
        // term could be this long), but long enough that
        // "<term> = <result>" (1021 + " = 511" = 1027 bytes) clears
        // MAX_TITLE (1024).
        let term = format!("1{}", "+1".repeat(510));
        assert_eq!(term.len(), 1021);
        let item = build_item(&term).expect("a long chain of additions still evaluates");
        assert!(item.title.len() <= MAX_TITLE);
        assert!(std::str::from_utf8(item.title.as_bytes()).is_ok());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p hopd calculator::`
Expected: FAIL to compile — `CALCULATOR_PROVIDER_ID` undefined until Step 1 lands, then `build_item` undefined until the function above is added.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p hopd calculator::`
Expected: PASS, every test in `item_tests`, plus everything from Tasks 1 and 2 still green.

- [ ] **Step 5: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/hop-core/src/provider.rs crates/hopd/src/calculator.rs
git commit -m "hopd: build calculator items from an evaluated expression"
```

---

### Task 4: `CalculatorProvider` — the `Provider` implementation

**Files:**
- Modify: `crates/hopd/src/calculator.rs`

**Interfaces:**
- Consumes: Task 1's `evaluate`; Task 2's `format_result`; Task 3's `build_item`.
- Produces, for Task 5 and Task 6:
  ```rust
  pub struct CalculatorProvider;
  impl hop_core::provider::Provider for CalculatorProvider { ... }
  ```

- [ ] **Step 1: Write the failing tests**

Append to `crates/hopd/src/calculator.rs` (below `item_tests`, above `#[cfg(test)] mod format_tests` — or after it; placement among the three test modules does not matter, only that this new code sits above all three `#[cfg(test)]` blocks):

```rust
use std::sync::Arc;
use std::time::Duration;

use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery};
use hop_protocol::{CopyText, ExecOutcome};

/// The calculator provider: turns a routed term into zero or one
/// [`Item`] via [`build_item`], and dispatches its one action by
/// re-deriving the same result from the item id via [`copy_text_for`].
/// Holds no state — there is nothing to hold, since evaluation needs
/// nothing but the term itself (this module's own docs; this plan's
/// Design decision 6).
pub struct CalculatorProvider;

impl Provider for CalculatorProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: CALCULATOR_PROVIDER_ID,
            kinds: vec![Kind::Calculator],
            // Mode::Calculator alone, deliberately not Mode::All — see this
            // plan's Design decision 1. `hop_core::router::route` already
            // sends both the exclusive `=2+2` and the inferred bare `2+2`
            // through `Mode::Calculator`, so `should_query`'s literal
            // containment check already reaches this provider on both
            // routes with no help from `ProviderHost::selected`'s
            // augmentation branch. Adding `Mode::All` would instead ask
            // this provider to attempt an evaluation on every non-math
            // keystroke of every query, for an outcome that is always
            // `None`. `crates/hop-core/src/host.rs:1692-1711`'s
            // `an_inferred_route_selects_both_the_mode_all_provider_and_the_provider_declaring_that_mode`
            // is `hop-core`'s own worked example of exactly this shape.
            modes: vec![Mode::Calculator],
            // 1, not 0: an empty term never evaluates (see `evaluate`'s
            // own tests), so this skips the guaranteed-failing case — the
            // bare `=` route — at the pre-filter, before a task is even
            // spawned.
            min_term_len: 1,
            budget: Duration::from_millis(5),
        }
    }

    async fn query(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(build_item(&q.term).into_iter().collect())
    }

    async fn execute(
        self: Arc<Self>,
        item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        copy_text_for(&item_id)
            .map(ExecOutcome::CopyText)
            .ok_or_else(|| {
                ProviderError::Failed(format!(
                    "{} is not a live calculator result",
                    item_id.as_str()
                ))
            })
    }
}

/// Re-derives the copyable result for `item_id`, the way
/// `CalculatorProvider::execute` answers its one action: strips the
/// `calc:` prefix [`build_item`] gave the id, re-evaluates what is left
/// exactly as `query()` did, and formats it the same way. `None` covers
/// both "this id was never one of ours" (no `calc:` prefix) and — in
/// principle, since the same string that built the id always re-evaluates
/// the same way — the unreachable case where it no longer does; either way
/// `execute` reports it as an ordinary provider failure, never a panic.
fn copy_text_for(item_id: &ItemId) -> Option<CopyText> {
    let term = item_id.as_str().strip_prefix("calc:")?;
    let value = evaluate(term)?;
    CopyText::new(format_result(value)).ok()
}

#[cfg(test)]
mod provider_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hop_core::host::{NoopLog, ProviderHost};
    use hop_core::pipeline::{CheckedItems, ProviderOutput};
    use hop_core::provider::{CancellationFlag, should_query};
    use hop_core::router::route;

    fn ctx() -> QueryCtx {
        QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: std::time::Instant::now() + Duration::from_secs(1),
        }
    }

    // --- Manifest shape (Design decision 1). ---

    #[test]
    fn the_manifest_uses_the_shared_calculator_provider_id_constant() {
        assert_eq!(CalculatorProvider.manifest().id, CALCULATOR_PROVIDER_ID);
    }

    #[test]
    fn the_manifest_declares_only_mode_calculator_and_kind_calculator() {
        let manifest = CalculatorProvider.manifest();
        assert_eq!(manifest.modes, vec![Mode::Calculator]);
        assert_eq!(manifest.kinds, vec![Kind::Calculator]);
    }

    #[test]
    fn should_query_reaches_this_manifest_on_both_the_explicit_and_inferred_math_routes() {
        let manifest = CalculatorProvider.manifest();
        assert!(should_query(&manifest, &route("=2+2")), "explicit `=` route");
        assert!(should_query(&manifest, &route("2+2")), "inferred bare-math route");
        assert!(
            !should_query(&manifest, &route("firefox")),
            "an ordinary query must not reach this provider"
        );
    }

    #[test]
    fn min_term_len_skips_the_empty_term_but_not_a_single_digit() {
        let manifest = CalculatorProvider.manifest();
        assert!(
            !should_query(&manifest, &route("=")),
            "an empty term after the `=` sigil is skipped at the pre-filter"
        );
        assert!(
            should_query(&manifest, &route("5")),
            "a single digit is a term of length 1"
        );
    }

    #[tokio::test]
    async fn registered_with_a_real_host_the_provider_is_selected_for_a_math_looking_query() {
        let mut host = ProviderHost::with_log(Arc::new(NoopLog));
        host.register(CalculatorProvider).unwrap();
        let manifest = &host.manifests()[0];
        assert!(should_query(manifest, &route("2+2")));
    }

    // --- The provider's own output survives its own manifest checks. ---

    #[tokio::test]
    async fn the_providers_own_output_passes_its_own_manifest_checks() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider
            .clone()
            .query(Arc::new(route("2+2")), ctx())
            .await
            .unwrap();
        assert_eq!(items.len(), 1, "the fixture must actually produce an item");

        let checked = CheckedItems::check(vec![ProviderOutput::from_provider(&*provider, items)]);
        assert_eq!(
            checked.rejections(),
            &[],
            "the calculator provider's own honest output must survive its own manifest"
        );
        assert_eq!(checked.items().len(), 1);
    }

    // --- query(): the pure eval-and-format path, driven through Provider. ---

    #[tokio::test]
    async fn query_returns_the_calculator_item_for_a_math_looking_term() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider.query(Arc::new(route("2+2")), ctx()).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "2+2 = 4");
    }

    #[tokio::test]
    async fn query_returns_no_items_rather_than_an_error_for_input_that_is_not_an_expression() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider
            .query(Arc::new(route("=hello")), ctx())
            .await
            .unwrap();
        assert_eq!(items, vec![], "criterion 4: no items, and Ok — never an Err");
    }

    #[tokio::test]
    async fn query_returns_no_items_for_a_non_finite_result() {
        let provider = Arc::new(CalculatorProvider);
        let items = provider.query(Arc::new(route("=1/0")), ctx()).await.unwrap();
        assert!(items.is_empty());
    }

    // --- execute(): re-derives the same result the item's title showed. ---

    #[tokio::test]
    async fn execute_copies_the_same_result_the_item_was_built_with() {
        let provider = Arc::new(CalculatorProvider);
        let item = build_item("10/4").unwrap();
        let outcome = provider
            .execute(item.id.clone(), item.default_action.clone())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ExecOutcome::CopyText(CopyText::new("2.5").unwrap())
        );
    }

    #[tokio::test]
    async fn execute_fails_rather_than_panicking_on_an_id_with_no_calc_prefix() {
        let provider = Arc::new(CalculatorProvider);
        let result = provider
            .execute(
                ItemId::new("app:firefox").unwrap(),
                ActionId::new("copy").unwrap(),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Failed(_))));
    }

    #[tokio::test]
    async fn execute_fails_rather_than_panicking_on_a_calc_prefixed_id_that_does_not_evaluate() {
        let provider = Arc::new(CalculatorProvider);
        let result = provider
            .execute(
                ItemId::new("calc:not an expression").unwrap(),
                ActionId::new("copy").unwrap(),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Failed(_))));
    }

    // --- Criterion 4 (this plan's "no I/O" claim): a structural witness. ---

    #[test]
    fn the_module_source_touches_no_disk_process_or_network() {
        // The mechanical half of this plan's Design decision 6 — grepping
        // this file's own source back, the way
        // `crates/hop-protocol/src/item.rs`'s
        // `every_test_this_file_names_in_its_docs_exists` does, and the way
        // the workspace's root `Cargo.toml` names `grep -rn unsafe_code
        // crates/` as the spot-check for its own `unsafe_code = "deny"`
        // claim. Not a substitute for the design argument (there is no
        // index here to build, unlike `apps.rs`'s `AppIndex`) — a second,
        // mechanical witness that would fail loudly if a future edit
        // reached for `std::fs`, spawned a process, or opened a socket on
        // this path.
        let source = include_str!("calculator.rs");
        for needle in ["std::fs", "std::process", "std::net", "TcpStream", "UdpSocket"] {
            assert!(!source.contains(needle), "calculator.rs must not reference {needle}");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd calculator::`
Expected: FAIL to compile — `CalculatorProvider` is undefined.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p hopd calculator::`
Expected: PASS, every test in `provider_tests`.

- [ ] **Step 4: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: all four green.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/src/calculator.rs
git commit -m "hopd: CalculatorProvider — the Provider implementation"
```

---

### Task 5: Wire `CalculatorProvider` into `build_host`

**Files:**
- Modify: `crates/hopd/src/server.rs`
- Modify: `crates/hopd/src/lib.rs`

**Interfaces:** none new — this task registers what Tasks 1–4 produced.

- [ ] **Step 1: Write the failing test**

In `crates/hopd/src/server.rs`, the existing `build_host_tests` module (`server.rs:169-185`) already checks that the skeleton and apps providers are both registered. Replace its one test with a version that also checks the calculator provider — a rename-and-extend, not a second test alongside the old one:

```rust
#[cfg(test)]
mod build_host_tests {
    use super::*;

    #[test]
    fn build_host_registers_the_skeleton_apps_and_calculator_providers() {
        // Not a behavior test of any one provider (each has its own suite
        // already) — this pins that `build_host` actually calls every
        // wiring function this crate has, so a future edit that adds a
        // provider but forgets to register it fails here rather than
        // silently shipping a daemon with a gap.
        let host = build_host();
        let ids: Vec<_> = host.manifests().iter().map(|m| m.id).collect();
        assert!(ids.contains(&"skeleton"));
        assert!(ids.contains(&hop_core::provider::APPS_PROVIDER_ID));
        assert!(ids.contains(&hop_core::provider::CALCULATOR_PROVIDER_ID));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hopd build_host_registers_the_skeleton_apps_and_calculator_providers`
Expected: FAIL — the calculator provider is not registered yet.

- [ ] **Step 3: Register the provider**

In `crates/hopd/src/server.rs`, modify `build_host`:

```rust
pub(crate) fn build_host() -> ProviderHost {
    let mut host = ProviderHost::with_log(Arc::new(StderrLog));
    if let Err(err) = host.register(SkeletonProvider) {
        eprintln!("hopd: could not register the skeleton provider: {err}");
    }
    if let Err(err) = host.register(crate::apps::build_apps_provider()) {
        eprintln!("hopd: could not register the apps provider: {err}");
    }
    if let Err(err) = host.register(crate::calculator::CalculatorProvider) {
        eprintln!("hopd: could not register the calculator provider: {err}");
    }
    host
}
```

- [ ] **Step 4: Retire the stale module doc**

In `crates/hopd/src/lib.rs`'s module doc, replace the sentence naming #58's calculator as the remaining gap:

```rust
//! What it is not yet: a daemon with every provider — the query router and the
//! provider host are wired ([`source`]), and the walking skeleton's,
//! [`apps`]'s and [`calculator`]'s providers are all registered now
//! ([`apps`] as of issue #57, [`calculator`] as of this issue, #58) — or
//! anything with a lifecycle beyond "runs until killed". Result *assembly* is
//! no longer one of the gaps: every provider arrival re-runs `hop-core`'s
//! [`pipeline`](hop_core::pipeline) over everything received so far for that
//! query and replaces the client's list with the ranked, boosted, capped
//! result (issue #103; see [`source`] for the accumulator that does it).
//! Each remaining gap is named where it applies, in [`runtime_dir`],
//! [`server`] and [`source`].
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p hopd build_host_registers_the_skeleton_apps_and_calculator_providers`
Expected: PASS.

- [ ] **Step 6: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: all four green.

- [ ] **Step 7: Commit**

```bash
git add crates/hopd/src/server.rs crates/hopd/src/lib.rs
git commit -m "hopd: register the calculator provider in build_host"
```

---

### Task 6: Integration tests over a real socket — query and execute

**Files:**
- Create: `crates/hopd/tests/calculator.rs`

**Interfaces:**
- Consumes: `crates/hopd/tests/common/mod.rs`'s `hello`, `recv`, `send`, `start_daemon`; `hop_core::host::{NoopLog, ProviderHost}`; `hop_core::provider::CALCULATOR_PROVIDER_ID`; `hopd::calculator::CalculatorProvider` (already `pub`, from Task 4); `hopd::source::{HostSource, SkeletonProvider}`.

This is the acceptance-criterion-7 test: "an integration test drives it through the daemon over a real socket." It follows `crates/hopd/tests/apps.rs`'s established shape — plain `#[test]` functions over a blocking `std::os::unix::net::UnixStream`, an in-process daemon from `start_daemon`, no second harness invented — and, since issue #59 has landed (see Global Constraints), also drives `Execute` over the same socket, which `apps.rs`'s own integration suite could not do when it was written.

- [ ] **Step 1: Write the tests**

Create `crates/hopd/tests/calculator.rs`:

```rust
//! The calculator provider through the daemon, over a real socket:
//! acceptance criterion 7 on issue #58. `calculator.rs`'s own unit tests
//! cover evaluation, formatting and the `Provider` impl directly; this file
//! covers what a client receives over the wire — including `execute`,
//! which issue #59 wires all the way from `ClientMsg::Execute` to
//! `Provider::execute`, and which this provider is the first in the tree to
//! answer with `ExecOutcome::CopyText` rather than `ExecOutcome::Done`.
//!
//! Plain `#[test]` functions over a blocking `std::os::unix::net::UnixStream`
//! client, matching `apps.rs`'s, `host.rs`'s and `lifecycle.rs`'s shape — no
//! second harness invented here.

#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use common::{hello, recv, send, start_daemon};
use hop_core::host::{NoopLog, ProviderHost};
use hop_protocol::{ClientMsg, CopyText, DaemonMsg, ExecOutcome, Item, QueryText};
use hopd::calculator::CalculatorProvider;
use hopd::source::{HostSource, SkeletonProvider};

fn calculator_daemon() -> common::TestDaemon {
    let mut host = ProviderHost::with_log(Arc::new(NoopLog));
    host.register(CalculatorProvider).unwrap();
    start_daemon(HostSource::new(Arc::new(host)))
}

fn connect(daemon: &common::TestDaemon) -> UnixStream {
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    hello(&mut stream);
    stream
}

/// Drives one query to completion, returning the *last* `Results` frame's
/// items — never accumulated across frames. Each `Results` frame is a
/// **full replacement** of the current list, per issue #103's contract
/// (`crates/hopd/src/source.rs`'s own module docs): concatenating batches
/// with `.extend(...)`, the way `tests/apps.rs`'s single-provider suite
/// gets away with (there, only ever one frame is ever sent), would
/// double-count an item across two frames the moment more than one
/// provider is registered, as the augmentation test below does.
fn run_query(stream: &mut UnixStream, id: u64, text: &str) -> Vec<Item> {
    send(
        stream,
        &ClientMsg::Query {
            id,
            text: QueryText::new(text).unwrap(),
        },
    );
    let mut items = Vec::new();
    loop {
        match recv(stream) {
            DaemonMsg::Results {
                query_id,
                items: batch,
                ..
            } if query_id == id => items = batch,
            DaemonMsg::QueryDone { query_id } if query_id == id => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    items
}

#[test]
fn a_query_over_the_socket_returns_the_calculator_result() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    let items = run_query(&mut stream, 1, "2+2");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "2+2 = 4");
    assert_eq!(items[0].provider, hop_core::provider::CALCULATOR_PROVIDER_ID);
}

#[test]
fn unary_minus_and_percent_are_handled_over_the_socket() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    let minus = run_query(&mut stream, 1, "-5+2");
    assert_eq!(minus.len(), 1);
    assert_eq!(minus[0].title, "-5+2 = -3");

    let percent = run_query(&mut stream, 2, "10%3");
    assert_eq!(percent.len(), 1);
    assert_eq!(percent[0].title, "10%3 = 1");
}

#[test]
fn executing_the_default_action_copies_the_result() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    let items = run_query(&mut stream, 1, "10/4");
    assert_eq!(items.len(), 1);
    let item = &items[0];

    send(
        &mut stream,
        &ClientMsg::Execute {
            query_id: 1,
            item_id: item.id.clone(),
            action_id: item.default_action.clone(),
        },
    );

    assert_eq!(
        recv(&mut stream),
        DaemonMsg::Executed {
            query_id: 1,
            outcome: ExecOutcome::CopyText(CopyText::new("2.5").unwrap()),
        }
    );
}

#[test]
fn input_that_is_not_an_expression_yields_a_clean_query_done_with_no_items() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("just some ordinary text").unwrap(),
        },
    );

    // No Results frame at all: the manifest's Mode::Calculator-only
    // declaration (Design decision 1) means this provider is never even
    // selected for a non-math query, so QueryDone is the very first frame.
    assert_eq!(recv(&mut stream), DaemonMsg::QueryDone { query_id: 1 });
}

#[test]
fn a_math_looking_query_augments_rather_than_replaces_other_providers_results() {
    let mut host = ProviderHost::with_log(Arc::new(NoopLog));
    host.register(SkeletonProvider).unwrap();
    host.register(CalculatorProvider).unwrap();
    let daemon = start_daemon(HostSource::new(Arc::new(host)));
    let mut stream = connect(&daemon);

    let items = run_query(&mut stream, 1, "2+2");

    assert!(
        items
            .iter()
            .any(|i| i.provider == hop_core::provider::CALCULATOR_PROVIDER_ID),
        "the calculator's own item must be present"
    );
    assert!(
        items.iter().any(|i| i.title == "Hello from hopd"),
        "the skeleton's item must still be present — augment, not hijack"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test -p hopd --test calculator`
Expected: FAIL first if any name is misspelled or a symbol is not yet `pub` (`CalculatorProvider` was declared `pub` in Task 4, so this should compile cleanly the first time); PASS once corrected.

- [ ] **Step 3: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: all four green — this is the landing gate for the whole issue.

- [ ] **Step 4: Commit**

```bash
git add crates/hopd/tests/calculator.rs
git commit -m "hopd: integration tests driving the calculator provider through the daemon over a real socket"
```

---

## Acceptance criteria coverage (from issue #58)

| Criterion | Where |
| --- | --- |
| A simple arithmetic query returns its result as an item | Task 1 (`evaluate`'s tests); Task 3 (`build_item` tests); Task 4 (`query_returns_the_calculator_item_for_a_math_looking_term`); Task 6 (`a_query_over_the_socket_returns_the_calculator_result`) |
| Unary minus and percent are handled | Task 1 (`unary_minus_is_handled`, `percent_is_modulo_not_percent_of`); Task 6 (`unary_minus_and_percent_are_handled_over_the_socket`) |
| The default action on a calculator item copies the result | Task 3 (`build_item_carries_exactly_one_copy_action_agreeing_with_default_action`); Task 4 (`execute_copies_the_same_result_the_item_was_built_with`); Task 6 (`executing_the_default_action_copies_the_result`) |
| Input that is not an expression yields no calculator items rather than an error item | Task 1 (every `None`-producing test); Task 4 (`query_returns_no_items_rather_than_an_error_for_input_that_is_not_an_expression`, `query_returns_no_items_for_a_non_finite_result`); Task 6 (`input_that_is_not_an_expression_yields_a_clean_query_done_with_no_items`) |
| Calculator results augment rather than replace other providers' results, per the router's existing semantics | Design decision 1 (citing `host.rs:1692-1711`); Task 4 (`should_query_reaches_this_manifest_on_both_the_explicit_and_inferred_math_routes`); Task 6 (`a_math_looking_query_augments_rather_than_replaces_other_providers_results`) |
| The provider performs no disk, subprocess or network work | Design decision 6; Task 4 (`the_module_source_touches_no_disk_process_or_network`) |
| An integration test drives it through the daemon over a real socket | Task 6 (all five tests, including `execute`) |

## Self-review notes

- **Spec coverage.** §5's providers table is the justification for `fasteval`; §3's latency contract is trivially met (Design decision 6) rather than actively defended the way `AppsProvider`'s index-vs-disk argument was, since this provider has nothing to keep off the query path in the first place.
- **Deliberate omissions**, each argued in Scope: `Item.copy_text` left unset, no `providers/` subdirectory (Design decision 7, explicitly re-answering the question `docs/superpowers/plans/2026-08-04-issue-57-apps-provider.md`'s Design decision 1 left open), no `deny.toml` edit, no `CONTEXT.md` edit.
- **The one new dependency in this plan:** `fasteval` 0.2 (MIT, already allow-listed — no `deny.toml` change), argued in Global Constraints and Task 1.
- **Verified against the actual files, not assumed from the issue text:** `Provider`'s exact signature and `ProviderManifest`'s fields, read from `crates/hop-core/src/provider.rs`; `ProviderHost::selected`'s augmentation rule and its own `"calculator"`-id, `Mode::Calculator`-only test fixture, read from the current `crates/hop-core/src/host.rs` (`host.rs:1692-1711`); `route`'s exact behavior for `=2+2` versus bare `2+2`, read from `crates/hop-core/src/router.rs`; `Pipeline::assemble`'s `promote_kinds` and its own `Kind::Calculator`/`"calc:2+2"`/`"2+2 = 4"` test fixture, read from `crates/hop-core/src/pipeline.rs` (`pipeline.rs:1207-1228`); `Learning::record_launch`/`boost_for`'s exact keying, read from `crates/hop-core/src/learning.rs`, before choosing the item-id scheme rather than after; `CheckedItems::check`'s exact per-item checks (kind, then provenance, then field-length), read from `crates/hop-core/src/pipeline.rs`; that issues #103 and #59 have both landed since the sibling apps-provider plan was written, read from `crates/hopd/src/lib.rs`'s current module doc and `crates/hopd/src/connection.rs`'s `Execute` arm directly, not assumed; `ItemId`/`ActionId`/`CopyText`'s exact constructors and bounds, read from `crates/hop-protocol/src/item.rs` and `content.rs`; `MAX_ITEM_ID` (4 096), `MAX_QUERY_TEXT` (1 024), `MAX_TITLE` (1 024) read from `crates/hop-protocol/src/limits.rs`; `crates/hopd/tests/apps.rs`'s and `tests/common/mod.rs`'s actual, currently-committed shape (not the shape described in the older apps-provider plan document, which has since drifted from what landed) — read in full so Task 6 does not invent a second harness or copy a stale pattern; `fasteval` 0.2.4's actual published source (`Cargo.toml`, `lib.rs`'s operator-precedence table, `ez.rs`'s `ez_eval` signature, `error.rs`'s `Error` enum, `evalns.rs`'s `EmptyNamespace`) fetched from the local registry cache and read directly; every claim about what `fasteval::ez_eval` returns for `1/0`, `0/0`, `50%`, `hello`, `2+2x` and the rest of Task 1's test table was run against a throwaway probe crate built against that same vendored source, not guessed at; every entry in Design decision 3's formatting table was produced by an actual `rustc --edition 2021` run of the exact `format_result` logic this plan specifies, not hand-computed; the current test count (577, across all crates) obtained by running `cargo test --workspace` before writing this plan.

## What I could not verify or fully resolve, for the maintainer's attention

- **`%` is modulo, read from `fasteval`'s own grammar rather than the (unavailable) old extension.** This is the one judgment call in this plan a future decision could reasonably reverse — see Design decision 5. A concrete, user-visible consequence worth knowing: a query of exactly `50%` (no right-hand operand) is a `fasteval` parse error, so it produces **no calculator item at all**, not `0.5`. If product feedback wants percent-of semantics, that is a new operator this provider would have to hand-implement (fasteval's grammar has no arm for it), not a flag to flip.
- **`FIXED_DECIMALS` (10), `EXPONENTIAL_ABOVE` (`1e15`) and `EXPONENTIAL_BELOW` (`1e-9`) are chosen, not derived.** No acceptance criterion specifies exact thresholds; these are round numbers with measured margin (Design decision 3), not values tuned against real user complaints, because none exist yet for a provider that has not shipped.
- **`fasteval`'s SI-suffix literals (`1.23K` = 1230, `1.23m` = 0.00123, ...) and built-in functions (`abs`, `sin`, `e()`, `pi()`, ...) are reachable through the exclusive `=` route** (`looks_like_math`'s shape check excludes letters from the inferred route, but the `=` sigil bypasses that check entirely). `=5M` silently evaluates to `5000000` through fasteval's own suffix grammar. This plan neither restricts nor documents that grammar beyond noting it here — it is a property of the engine the design spec named, not a decision this issue makes.
- **Whether `Item.copy_text` should also be populated** (redundant with what `execute()`'s `ExecOutcome::CopyText` already returns, but zero-round-trip for a client that reads it directly) was decided against for scope discipline, matching `apps.rs`'s own precedent of leaving it unset. Worth reconsidering if a client author asks for instant-copy without waiting on an `Execute` round trip.
- **`min_term_len: 1` and a `Mode::Calculator`-only manifest is a pattern new to *production* code in this plan** — `host.rs`'s own precedent for the shape (`host.rs:1692-1711`) is test-only. Worth a second look if a later provider needs a different balance between `should_query`'s literal match and `ProviderHost::selected`'s augmentation branch.
