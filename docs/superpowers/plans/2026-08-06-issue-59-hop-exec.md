# hop exec — action dispatch bound to the live result set (Issue #59) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development (recommended) or superpowers:executing-plans, plus superpowers:test-driven-development. Work task-by-task, red-green-refactor, and keep the workspace green at every task boundary.

**Goal:** Implement action execution end to end so issue #59's acceptance
criteria pass: an `execute` frame resolves against the items the daemon
actually delivered for that query id (never a stale query, never an id it
never emitted), and the CLI gains an `exec` subcommand that drives every
execution path headlessly with a meaningful exit code.

**Architecture today (what #59 adds the missing edges to):**
- Wire: `ClientMsg::Execute { query_id, item_id, action_id }` →
  `DaemonMsg::Executed { query_id, outcome: ExecOutcome }`, and
  `DaemonMsg::Error { query_id: Option<u64>, error: ProtoError }` with
  `ErrorCode::{UnknownItem, UnknownAction, ProviderFailed, Internal, ...}`.
  `ExecOutcome = { Done, CopyText(CopyText), OpenUrl(OpenUrl) }`.
  UnknownItem / UnknownAction are query-scoped by construction
  (`Some(query_id)`), and query-scoped errors are non-terminal to the
  connection (the client's `hop-cli` already drops errors naming an id that
  is not its own current query).
- Daemon (`hopd`): `connection.rs::handle_message` current catch-all
  `(Ready, _other)` arm refuses any `Execute` with `Internal` +
  "not implemented yet". The retained set is `Exchange::delivered: Vec<Item>`
  — the **last** assembled list, bounded by `MAX_ITEMS_PER_RESULTS_FRAME`
  (1 000), replaced whole per frame (issue #103 replace-frame). Its doc says
  explicitly it is the state issue #59's execute resolves against, and that an
  item the daemon has since replaced away is no longer resolvable.
  `Item` carries `pub provider: String` and `actions: Vec<Action>`.
  `connection::drive/handle_message` are generic over `S: ResultSource`
  (`crates/hopd/src/source.rs`); `ResultSource` today has a single method,
  `start(text)`. The production source is `HostSource`, which wraps
  `hop-core::host::ProviderHost`.
- Core (`hop-core`): `Provider` trait has `query` and
  `execute(item_id, action_id) -> Result<ExecOutcome, ProviderError>`
  (provider.rs ~299-303). `AppsProvider::execute` is a real implementation
  (apps.rs ~1557-1572, dispatch via `focus_or_launch`); `SkeletonProvider`
  returns `Err(ProviderError::Failed(...))`. `ProviderHost` registers
  providers (`register_arc`) but has **no public execute dispatch** — #59
  must add one (look up a provider by id, call its `execute`).
- CLI (`hop-cli`): hand-rolled `Command` enum (`Version | Query(String) |
  Usage`), no clap, no tokio. `run_query` connects, handshakes, sends one
  `Query` frame (fixed `QUERY_ID = 1`), reads until `query_done`. Module doc
  explicitly anticipates `exec` landing. `hop-cli/src/main.rs` dispatches on
  `Command` and maps `Usage` → exit 2, everything else → 0/1 via `ExitCode`.

## Global Constraints

- **No new third-party dependencies.** Nothing here needs one; `Cargo.toml`s
  and `deny.toml` stay untouched. The module doc note that exec "tips the
  parser toward clap" is a future trade — nothing in this slice should adopt
  a dependency to satisfy it.
- **Gate commands, all four required at every task boundary:**
  `cargo test --workspace` · `cargo fmt --all --check` · `cargo clippy
  --workspace --all-targets -- -D warnings` · `cargo deny check`.
- **No `.unwrap()` in production code** (`clippy::unwrap_used` + `-D
  warnings`). Test files / test modules open with
  `#![allow(clippy::unwrap_used)]`.
- **No AI attribution** in commits or the PR.
- **The `(Ready, _other)` catch-all must not silently become a wider net.**
  After adding the `Execute` arm, re-examine the remaining `_other` arm (a
  second `Hello`) so its error stays accurate.

## In scope — the issue's acceptance criteria

1. The `exec` subcommand launches an application through the apps provider.
2. An item id the daemon never delivered under that query id is refused with
   the **unknown-item** error, not acted on.
3. An execute frame naming a **stale query id** is refused.
4. The binding matches the shape recorded in the threat model, and the code
   points at that decision (**#25** — the live-result-set rule, already
   encoded in `Exchange::delivered`'s docs).
5. Resolution runs against the retained set bounded by #55's documented
   per-query cap, and an item **lost to that cap is distinguishable from one
   the daemon never emitted** (**#53**).
6. Exit codes distinguish success, unknown item, unknown action, and provider
   failure.
7. An integration test covers one successful execution and each refusal path.

## Design decisions (read before any task)

**1. The execute seam is on `ResultSource`, not a second connection generic
parameter.** `handle_connection`/`drive`/`handle_message` are already generic
over `S: ResultSource`, and the connection's `Exchange::delivered` gives it
everything it needs to resolve item → provider. Adding
`async fn execute(&self, provider: &str, item_id: ItemId, action_id: ActionId)
-> Result<ExecOutcome, ProviderError>` to `ResultSource`, with `HostSource`
implementing it by dispatching through its `ProviderHost`, keeps the daemon
from reaching into `hop-core` internals and keeps the fake/test sources
(`ScriptedSource`, any test `ResultSource`) able to answer `execute` too.

**2. `ProviderHost` gains a public execute dispatch keyed by provider id.**
The connection resolves the `Item` (so it knows `item.provider` and
`item.actions`), then calls `source.execute(provider, item_id, action_id)`.
`ProviderHost` looks up the registration whose id equals `provider` and calls
its `Provider::execute`, returning a `ProviderFailed`-shaped error when no
provider by that id is registered (a provider named on an item but gone —
treat as provider failure, not unknown item; the item was delivered, the
executor is missing). The implementing method must not mint an `Item` or
bypass the registry.

**3. Validation lives in the connection, before any dispatch.**
`handle_message`'s new `(Ready, ClientMsg::Execute { query_id, item_id,
action_id })` arm:
- No `exchange` with `exchange.id == query_id` → **stale query id**; send a
  query-scoped `UnknownItem` error (`Some(query_id)`) and keep the connection
  open.
- `exchange.delivered` holds no item with `item_id` → **unknown item**;
  query-scoped `UnknownItem`.
- The item is present but `action_id` is not among `item.actions` → **unknown
  action**; query-scoped `UnknownAction`.
- Otherwise: `source.execute(item.provider.clone(), item_id, action_id)`.
  `Ok(outcome)` → send `DaemonMsg::Executed { query_id, outcome }`.
  `Err(ProviderError)` → query-scoped `ProviderFailed`.
  All four are query-scoped `Some(query_id)` errors, non-terminal to the
  connection (`DaemonMsg::Error`'s contract). Executing does **not** end the
  exchange and does not touch `delivered`.

**4. The cap-vs-never-emitted distinction (criterion 5) must be decided
against the replace-frame shape, not the pre-#103 accumulated set.** Under
#103, `Exchange::delivered` is the **last** assembled list (≤
`MAX_ITEMS_PER_RESULTS_FRAME` = 1 000), replaced whole per frame; per-query
*accumulation* is bounded at `MAX_ITEMS_PER_QUERY` (5 000) inside
`source.rs`, upstream of the connection. So execute resolution reads only the
current delivered list. Read `forward_batch` and the `limits.rs` docs before
deciding what "lost to the cap" means now. The honest outcome may be that
under replacement an item not in `delivered` is simply unresolvable
(`UnknownItem`) — the client was never showing it — and the distinction
criterion 5 asks for is satisfied by *documenting* that an item is either in
`delivered` (it was shown) or it is not (never shown / already replaced), so
a single `UnknownItem` is not a silent conflation. Whatever you decide, it
must be (a) grounded in `forward_batch` + `limits.rs`, (b) documented in the
code at the resolution site, and (c) pinned by a test that a hypothetical
"lost to the cap" id and a "never emitted" id are both/indistinguishably
refused **and that the reason is not a silent fall-through** (the error text
or the refusal must reflect the decision). Do not re-litigate #25: the
live-result-set binding shape is already chosen.

**5. Declined/other decisions recorded in #59's comments stay as decided.**
`ErrorCode::Internal` is the wrong code for any of these refusals; use
`UnknownItem` / `UnknownAction` / `ProviderFailed` only. Query-id reuse on
`ClientMsg::Query` is not enforced in this slice (the issue's own comments
defer it as a decision for the implementer; the obvious safe posture is to
leave it unenforced but documented, matching `ClientMsg::Query`'s doc).

**6. The CLI `exec` invocation shape is the implementer's call, but it must
be headless and drivable with no UI.** A sane default that satisfies every
criterion: `hop exec <query> <item-id> <action-id>` performs the query
(`QUERY_ID = 1`), reads frames until `query_done`, resolves `<item-id>` /
`<action-id>` against the **last** `results` frame's items (the live result
set), sends `ClientMsg::Execute`, and maps the reply to an exit code. Consult
the v1 design spec §3 / §13 (referenced by the issue) for any authoritative
shape before choosing. The CLI must treat a query-scoped `Error` naming its
own query id as terminal for its single exchange (it already does for
`query` — extend, don't regress).

## File structure

**Created:**
- `crates/hop-cli/tests/exec_e2e.rs` (or extend `tests/e2e.rs`) — exec
  integration test through the fake_daemon harness (see Task 3; the
  fake_daemon must be extended to answer `Execute`).
- `crates/hopd/tests/exec.rs` — real-socket integration tests for one
  successful execution and each refusal path (criterion 7), using the
  `common` harness (drive `Query` then `Execute` through a real daemon /
  `ScriptedProvider` or `AppsProvider`).

**Modified:**
- `crates/hopd/src/source.rs` — `ResultSource` gains `execute`;
  `HostSource::execute` implemented via host dispatch. Keep the trait's
  obligations docs accurate.
- `crates/hop-core/src/host.rs` — `ProviderHost` gains a public execute
  dispatch (find registration by provider id → `Provider::execute`). No
  public `Item` fabrication.
- `crates/hopd/src/connection.rs` — the `(Ready, Execute)` arm; the retained
  `_other` arm's message re-audited.
- `crates/hop-cli/src/lib.rs` — `Command` grows `Exec(...)`; parser, the exec
  flow, query-scoped error handling, and the exit-code mapping (criterion 6).
- `crates/hop-cli/src/main.rs` — dispatch the new `Command::Exec` arm.
- `crates/hop-cli/tests/e2e.rs` — `fake_daemon` learns an `Execute` path
  (extend the harness; it currently only answers `Query`).
- `CONTEXT.md` — update the glossary if exec / execute-resolution adds terms
  (per `/domain-modeling` rules).

## Tasks (work in order; every boundary green)

### Task 1 — `ResultSource` + `ProviderHost` execute seam (daemon plumbing)
- [ ] Add `async fn execute(&self, provider: &str, item_id: ItemId,
      action_id: ActionId) -> Result<ExecOutcome, ProviderError>;` to
      `ResultSource`. Update its obligations doc.
- [ ] Add `ProviderHost::execute` (or `exec_*`) public dispatch keyed by
      provider id; return a provider-failed error when unregistered.
- [ ] Implement `HostSource::execute` calling through to the host.
- [ ] Tests: host dispatch resolves the right registered provider and
      forwards outcome; unregistered id → provider-failed. Fake/source
      implementations (`ScriptedSource`, any test `ResultSource`) implement
      the new method.

### Task 2 — connection resolve-and-dispatch (criterion 2, 3, 4, 5)
- [ ] New `(Ready, ClientMsg::Execute ..)` arm in `handle_message`: stale
      query id → `UnknownItem`; missing item → `UnknownItem`; unknown action
      → `UnknownAction`; else dispatch and send `Executed` / `ProviderFailed`,
      all query-scoped.
- [ ] Honor Design decision 4: decide and document the cap-vs-never-emitted
      semantics at the resolution site; do not collapse silently.
- [ ] Re-audit the `(Ready, _other)` arm after removing `Execute` from its
      reach.
- [ ] Tests (unit + real-socket): each refusal path and one success. Wire an
      `Executed` round-trip through `crates/hopd/tests/common`-style harness.

### Task 3 — CLI `exec` subcommand (criterion 1, 6)
- [ ] `Command::Exec(...)` + `parse`; `main` dispatch.
- [ ] The exec flow: handshake → query → read to `query_done` → resolve
      item/action against the last `results` frame → `ClientMsg::Execute` →
      handle `Executed` and query-scoped errors.
- [ ] Exit-code mapping distinguishes success / unknown item / unknown action
      / provider failure (criterion 6). Document the numeric mapping.
- [ ] Extend `fake_daemon` to answer `Execute`; add `exec_e2e` (or extend
      `e2e.rs`) so criterion 1 is exercised headlessly.

### Task 4 — integration coverage + docs (criterion 7)
- [ ] A real-socket test drives a successful execution and **each** refusal
      path (criterion 7), through `AppsProvider` or a scripted source.
- [ ] `CONTEXT.md` glossary updated per `/domain-modeling` if needed.
- [ ] `hop-cli` crate-level doc: update the "two subcommands exist today"
      module doc to three.

## Acceptance mapping

| Criterion | Where |
| --- | --- |
| 1 exec launches via apps provider | Task 3 (e2e), Task 4 |
| 2 unknown item refused, not acted on | Task 2, Task 4 |
| 3 stale query id refused | Task 2, Task 4 |
| 4 binding matches threat model / points at #25 | Design decision 3/4, code citation |
| 5 cap-lost vs never-emitted distinguishable | Design decision 4, Task 2 + test |
| 6 exit codes distinguish 4 outcomes | Task 3 |
| 7 integration test success + refusals | Task 4 |

## Verification

At the end, all four gates green:
`cargo test --workspace` · `cargo fmt --all --check` · `cargo clippy
--workspace --all-targets -- -D warnings` · `cargo deny check`.
Then issue #59's brief is the review Spec for `/review`.
