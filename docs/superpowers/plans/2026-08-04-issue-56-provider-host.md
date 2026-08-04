# Provider Host (Issue #56) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the daemon's provider host — the thing that owns registered providers, captures each manifest once, decides which providers a keystroke reaches, runs their queries under a host-enforced budget, and contains their panics and their error text.

**Architecture:** `hop-core` gains the enforcement points (`sanitize`, `host`) and `Provider`'s signature becomes genuinely spawnable; `hopd` gains a `ResultSource` over a `ProviderHost` and re-expresses the walking skeleton's hardcoded item as a real registered provider, so the daemon's production query path runs through the host from this slice onward. One `tokio::spawn` per provider per query is what buys both panic containment (`JoinError::is_panic`) and a cut-off a hung provider cannot refuse (`timeout` + `abort`).

**Tech Stack:** Rust 2024, tokio (`rt-multi-thread`, already configured), `thiserror`, no new third-party dependencies.

## Global Constraints

- **No new third-party dependencies.** `cargo deny check` gates advisories, bans, licenses and sources; the license allow-list has **no Apache-2.0**, and a path dependency must carry `version` alongside `path`. Everything this slice needs is in `std`, `tokio` and `thiserror`.
- **Gate commands, all four required:** `cargo test --workspace` · `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny check`.
- **No `.unwrap()` in production code** (`clippy::unwrap_used` + `-D warnings`). Test files open with `#![allow(clippy::unwrap_used)]`.
- **No `unsafe`** (`unsafe_code = "deny"` workspace-wide).
- **The latency contract (spec §3):** keystroke → ranked results < 10 ms; no disk reads, subprocess spawns or HTTP on the query path.
- **Glossary discipline:** `docs/agents/domain.md` requires `CONTEXT.md`'s glossary to carry the vocabulary this slice resolves. Task 8 does that; it is not optional (a prior slice was pulled up on exactly this by `/review`'s Standards axis).
- **No AI attribution** in commits or the PR.

---

## Scope: what this slice is and is not

**In scope**, exactly the eight acceptance criteria on issue #56 — registration with manifests captured once, a manifest that changes its answers afterwards not changing scheduling, host clamps on budget and minimum term length, a non-cooperating provider cut off at its budget and reported as a timeout, a panicking provider yielding a panic-shaped error naming it while other providers' results still reach the client, provider error text truncated and stripped before it can leave the daemon, failures and budget misses emitting records through a logging seam, and a scripted fake-provider fixture the integration tests use.

**Not in scope, deliberately:** wiring `Pipeline::assemble` into the daemon. Nothing in #56's brief or its criteria mentions ranking, boosts, the pin budget, `max_results` or result ordering, and wiring assembly would first need a protocol answer this slice has no mandate to give — the wire streams *append-only* batches (`Results { partial: true }`, per #55), while `assemble` is a whole-list pure function, so "rank the streamed set" means either re-sending the whole list per batch or gating on the slowest provider, and §3 forbids the latter outright. The host therefore streams each provider's **manifest-checked** items as its own batch, in the order providers answer. Task 8 files a follow-up issue naming this gap so it is tracked rather than assumed.

## Design decisions (read before any task)

**1. The enforcement points live in `hop-core`; the `ResultSource` adapter lives in `hopd`.** #28 asks for an exported enforcement point "so a scheduler uses it rather than reimplementing it", #32 for a caller inside the crate, #34 for a logging seam in the crate. All three are `hop-core`'s. What is `hopd`'s is the adapter from `ProviderHost` to `source::ResultSource` and the choice of log backend.

**2. `Provider`'s methods take owned/`Arc` arguments so the returned future is `'static`.** This is the breaking change #29 exists to make, and spec §6's 2026-07-31 amendment sanctions it by name: the seam "stays open to change throughout v1 development", and #29 is one of the two gaps it says can only be closed by changing these types. `tokio::spawn` requires `'static`; borrowed `&self`/`&RoutedQuery`/`&QueryCtx` cannot give it. New shape:

```rust
fn query(self: Arc<Self>, q: Arc<RoutedQuery>, ctx: QueryCtx)
    -> impl Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static;
```

`Arc<RoutedQuery>` rather than a clone per provider because every selected provider reads the same routed query, and `QueryCtx` is owned because it is two cheap fields (an `Arc`-backed flag and an `Instant`).

**3. The registry stores `Arc<dyn ErasedProvider>`, and `ErasedProvider` is private to `hop-core`.** `Provider` is dyn-incompatible by construction (RPITIT), and `pipeline.rs` *relies* on that — "not something `dyn Provider` can launder either" is load-bearing for the `ProviderOutput::from_provider` argument. So the host erases through a crate-private trait with a blanket `impl<P: Provider>`, and the blanket impl is where `ProviderOutput::from_provider(self, items)` is called — with the concrete `P` in hand. Nothing an item claims about itself can pick the manifest it is checked against, exactly as before erasure. `ErasedProvider` must stay crate-private: a public one would be a second, dyn-compatible route to supplying a manifest.

**4. Attribution belongs on a host-produced type, never on the provider-constructed error.** `ProviderError` stays the provider's vocabulary and gains nothing. The host produces `ProviderFailure { provider, kind, message, elapsed }`, whose `provider` comes from the **captured** manifest. This is the same argument `ProviderOutput` makes: the untrusted party must not name itself. It is why this plan adds no `ProviderError::Panicked` variant — #29's criterion asks for "a panic-shaped error ... naming that provider", and `ProviderFailure { kind: FailureKind::Panicked, provider }` is that, produced by the only party that knows the id truthfully.

**5. The logging seam is a trait, not a `tracing` dependency.** #34's out-of-scope excludes "choosing the daemon's logging backend", and a trait is what makes the seam testable (a recording impl asserts records; a macro needs a subscriber and a new dev-dependency). It is also the only shape that satisfies the promise `pipeline.rs` already wrote down — rejections stay ignorable "until there is a logging seam (issue #34) that makes ignoring them a real mistake." A `tracing::warn!` at a call site makes nothing unignorable; a `&dyn ProviderLog` the host must be constructed with does. Spec §9's `tracing` remains the daemon's eventual backend, wired behind this seam by a later slice.

**6. A budget miss and a provider-volunteered `Timeout` are different events.** `FailureKind::Timeout` is a provider returning `Err(ProviderError::Timeout)` on its own. `ProviderEvent::BudgetMiss` is the host cutting a provider off. #34 names budget misses separately from failures, and the distinction is real: only the second one proves enforcement happened.

**7. Strip, then truncate.** Truncating first would let a stripped-away character consume budget. Truncation is at a `char` boundary, on bytes, because every bound in `hop_protocol::limits` counts bytes.

**8. The daemon's production path goes through the host in this slice.** `SkeletonSource` is retired and its hardcoded item becomes `SkeletonProvider`, a real registered provider. Without this, #32's "the enforcement predicate has a caller outside tests" would be satisfied only by tests, and `hop query` would regress to returning nothing until #57 lands. The item already declares `kind: Action` and `provider: "skeleton"`, so it passes its own manifest checks unchanged and `hop-cli`'s existing e2e assertions stay green.

## File Structure

**Created:**
- `crates/hop-core/src/sanitize.rs` — the one implementation of string sanitization spec §9 asks for: bounded, control-stripped, bidi-stripped text.
- `crates/hop-core/src/host.rs` — the host: policy and clamps, registration with capture-once, the reporting vocabulary, the log seam, and per-provider isolated execution.
- `crates/hopd/tests/host.rs` — integration tests over a real socket, driven by the scripted fixture.

**Modified:**
- `crates/hop-core/src/lib.rs` — declare `host`, `sanitize`.
- `crates/hop-core/src/provider.rs` — spawnable signature; `ProviderManifest: PartialEq, Eq`; docs.
- `crates/hop-core/src/pipeline.rs` — its two test providers adopt the new signature.
- `crates/hopd/Cargo.toml` — depend on `hop-core`.
- `crates/hopd/src/source.rs` — `SkeletonProvider` replaces `SkeletonSource`; `HostSource` implements `ResultSource`.
- `crates/hopd/src/server.rs` — build the host, register the skeleton provider, serve a `HostSource`.
- `crates/hopd/src/lib.rs` — module docs stop saying `hop-core` is unused.
- `crates/hopd/tests/common/mod.rs` — the scripted fake-provider fixture.
- `CONTEXT.md` — glossary.

---

### Task 1: Bounded, control-stripped, bidi-stripped provider text

**Files:**
- Create: `crates/hop-core/src/sanitize.rs`
- Modify: `crates/hop-core/src/lib.rs`

**Interfaces:**
- Produces: `pub const MAX_PROVIDER_MESSAGE: usize = 256;` and `pub fn sanitize_provider_message(raw: &str) -> String`. Task 3 calls it when building a `ProviderFailure`.

- [ ] **Step 1: Write the failing tests**

Create `crates/hop-core/src/sanitize.rs`:

```rust
//! The one implementation of string sanitization this workspace has, per spec
//! §9 ("string sanitization: one implementation in hop-core").
//!
//! What it sanitizes today is provider-supplied error text — the free-form
//! `String` in [`ProviderError::Failed`](crate::provider::ProviderError::Failed),
//! which is untrusted text a provider chooses and which is bound for a GTK
//! label by way of
//! [`ProtoError`](hop_protocol::ProtoError). Issue #34 is the finding: nothing
//! capped it, nothing escaped it, and a provider failing every query with a
//! 50 MB string prefixed by terminal escapes would have had all of it rendered.
//!
//! # Why this is not in `hop-protocol`'s content rules
//!
//! [`content`](hop_protocol::content) *refuses* a value that breaks a rule —
//! that is right for a value arriving off the wire, where a refusal names a
//! peer's mistake. Here the value is a diagnostic about a failure that already
//! happened, and refusing it would replace the reason a provider failed with
//! the reason its explanation was unacceptable. So this module rewrites rather
//! than refuses, and the rewrite is lossy on purpose.

/// The most bytes of provider-supplied text that may leave the daemon, after
/// stripping.
///
/// It has to fit *inside*
/// [`MAX_ERROR_MESSAGE`](hop_protocol::limits::MAX_ERROR_MESSAGE), the 1 024-byte
/// bound on the wire field this text ends up in, with room left for the host's
/// own attribution — which provider, and what kind of failure. 256 leaves 768
/// bytes for that framing, which is more than any of it needs and keeps the
/// arithmetic obvious rather than tight.
///
/// The unit is bytes, not characters, because every bound in
/// [`limits`](hop_protocol::limits) counts bytes and a second unit here would
/// make the two impossible to compare.
pub const MAX_PROVIDER_MESSAGE: usize = 256;

/// The bidirectional formatting characters this module removes.
///
/// These are Unicode's explicit bidi controls — the "Trojan Source" set. They
/// reorder how the characters around them *display* without changing the
/// characters themselves, so text carrying one can render as something other
/// than what it says: an error message that appears to name a different
/// provider, or to end before it does.
///
/// [`char::is_control`] does not reach them, and
/// [`CopyText`](hop_protocol::content::CopyText) says so in place — "nor the
/// bidirectional format characters such as U+202E, which can reorder how a
/// string renders ... this type does not address" — deferring the concern to
/// whoever needed it first. This is that place.
///
/// # Why this list rather than all of `Cf`
///
/// Unicode's format category also holds characters that carry meaning in
/// ordinary text — U+200D ZERO WIDTH JOINER is what holds a multi-codepoint
/// emoji together, and a provider whose failure message contains an emoji has
/// done nothing wrong. Stripping the whole category would mangle honest text to
/// reach a set that is enumerable and stable, so the set is enumerated.
pub const BIDI_CONTROLS: &[char] = &[
    '\u{061C}', // ARABIC LETTER MARK
    '\u{200E}', // LEFT-TO-RIGHT MARK
    '\u{200F}', // RIGHT-TO-LEFT MARK
    '\u{202A}', // LEFT-TO-RIGHT EMBEDDING
    '\u{202B}', // RIGHT-TO-LEFT EMBEDDING
    '\u{202C}', // POP DIRECTIONAL FORMATTING
    '\u{202D}', // LEFT-TO-RIGHT OVERRIDE
    '\u{202E}', // RIGHT-TO-LEFT OVERRIDE
    '\u{2066}', // LEFT-TO-RIGHT ISOLATE
    '\u{2067}', // RIGHT-TO-LEFT ISOLATE
    '\u{2068}', // FIRST STRONG ISOLATE
    '\u{2069}', // POP DIRECTIONAL ISOLATE
];

/// Rewrites provider-supplied text into something safe to render: every
/// [`char::is_control`] character and every [`BIDI_CONTROLS`] character
/// removed, then truncated to [`MAX_PROVIDER_MESSAGE`] bytes at a `char`
/// boundary.
///
/// # Strip before truncate
///
/// In that order, and it matters: truncating first would let characters that
/// are about to be removed spend the budget, so a message padded with 300
/// escape characters would arrive empty rather than arriving as its first 256
/// readable bytes.
///
/// Truncation stops at a `char` boundary, so the result is always valid UTF-8
/// and is never a partial code point — which is what would otherwise happen at
/// a byte cut through a multi-byte character.
pub fn sanitize_provider_message(raw: &str) -> String {
    let stripped: String = raw
        .chars()
        .filter(|c| !c.is_control() && !BIDI_CONTROLS.contains(c))
        .collect();

    if stripped.len() <= MAX_PROVIDER_MESSAGE {
        return stripped;
    }

    let mut end = MAX_PROVIDER_MESSAGE;
    while end > 0 && !stripped.is_char_boundary(end) {
        end -= 1;
    }
    stripped[..end].to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn ordinary_text_passes_through_unchanged() {
        assert_eq!(
            sanitize_provider_message("could not reach the index"),
            "could not reach the index"
        );
    }

    #[test]
    fn an_oversized_message_is_truncated_to_the_documented_maximum() {
        let raw = "a".repeat(MAX_PROVIDER_MESSAGE * 4);
        let out = sanitize_provider_message(&raw);
        assert_eq!(out.len(), MAX_PROVIDER_MESSAGE);
    }

    #[test]
    fn escape_sequences_and_newlines_are_removed() {
        let out = sanitize_provider_message("\u{1b}[31mred\u{1b}[0m\nand more\t here");
        assert!(
            !out.contains('\u{1b}'),
            "ESC opens a terminal control sequence and must not survive"
        );
        assert!(!out.contains('\n'));
        assert!(!out.contains('\t'));
        assert_eq!(out, "[31mred[0mand more here");
    }

    #[test]
    fn direction_override_characters_are_removed() {
        // A right-to-left override is what lets text render as something other
        // than what it says — the display-spoofing case `CopyText`'s docs
        // defer to this module.
        let out = sanitize_provider_message("apps\u{202e}failed\u{202c}");
        assert_eq!(out, "appsfailed");
        for c in BIDI_CONTROLS {
            assert!(
                !sanitize_provider_message(&format!("x{c}y")).contains(*c),
                "{c:?} must be stripped"
            );
        }
    }

    #[test]
    fn a_zero_width_joiner_survives_because_it_is_not_a_direction_control() {
        // The reason `BIDI_CONTROLS` is enumerated rather than being all of
        // Unicode's format category: this character holds an emoji together.
        let out = sanitize_provider_message("\u{1f468}\u{200d}\u{1f4bb} failed");
        assert!(out.contains('\u{200d}'));
    }

    #[test]
    fn stripping_happens_before_truncation() {
        // A message padded with controls up to the cap, then followed by
        // readable text: strip-then-truncate keeps the readable text, and
        // truncate-then-strip would have returned an empty string.
        let raw = format!("{}{}", "\u{1b}".repeat(MAX_PROVIDER_MESSAGE), "visible");
        assert_eq!(sanitize_provider_message(&raw), "visible");
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // 'é' is two bytes, so a cap that is odd relative to the run forces a
        // cut mid-character unless the boundary is respected.
        let raw = "é".repeat(MAX_PROVIDER_MESSAGE);
        let out = sanitize_provider_message(&raw);
        assert!(out.len() <= MAX_PROVIDER_MESSAGE);
        assert!(
            out.chars().all(|c| c == 'é'),
            "a byte cut through a code point would not round-trip as chars"
        );
        assert_eq!(std::str::from_utf8(out.as_bytes()).unwrap(), out);
    }

    #[test]
    fn an_all_control_message_becomes_empty_rather_than_being_refused() {
        // Lossy on purpose: this module rewrites, it never refuses — see the
        // module docs on why a refusal would be the wrong answer here.
        assert_eq!(sanitize_provider_message("\u{1b}\u{7f}\u{202e}"), "");
    }
}
```

Add to `crates/hop-core/src/lib.rs`, keeping the list alphabetical:

```rust
pub mod aliases;
pub mod host;
pub mod learning;
pub mod pipeline;
pub mod provider;
pub mod rank;
pub mod router;
pub mod sanitize;
```

Note: `pub mod host;` will not compile until Task 3 creates that file. For this task, add **only** `pub mod sanitize;` and leave `host` for Task 3.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hop-core sanitize`
Expected: FAIL — `file not found for module` before the file exists, then compile errors resolving to passes once the implementation above is in place. Confirm each test name appears.

- [ ] **Step 3: Confirm the implementation satisfies them**

The implementation is in Step 1's file. Re-read `sanitize_provider_message` against `stripping_happens_before_truncation` and `truncation_never_splits_a_multi_byte_character` specifically — those two are the ones an "obvious" rewrite gets wrong.

- [ ] **Step 4: Run the gate**

```bash
cargo test -p hop-core sanitize
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/hop-core/src/sanitize.rs crates/hop-core/src/lib.rs
git commit -m "hop-core: bound and strip provider-supplied error text"
```

---

### Task 2: `Provider`'s future becomes genuinely spawnable

**Files:**
- Modify: `crates/hop-core/src/provider.rs`
- Modify: `crates/hop-core/src/pipeline.rs` (its two test providers only)

**Interfaces:**
- Produces: the new trait shape every later task and every future provider implements:
  ```rust
  pub trait Provider: Send + Sync + 'static {
      fn manifest(&self) -> ProviderManifest;
      fn query(self: Arc<Self>, q: Arc<RoutedQuery>, ctx: QueryCtx)
          -> impl Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static;
      fn execute(self: Arc<Self>, item_id: ItemId, action_id: ActionId)
          -> impl Future<Output = Result<ExecOutcome, ProviderError>> + Send + 'static;
  }
  ```
  and `ProviderManifest` deriving `PartialEq, Eq` so Task 4 can compare a captured manifest against a freshly-read one.

- [ ] **Step 1: Write the failing test**

Add to `crates/hop-core/src/provider.rs`'s `mod tests`:

```rust
    /// The criterion #29 exists for: a provider's future can be handed to
    /// `tokio::spawn`, which requires `'static` as well as `Send`. Under the
    /// old borrowed signature this did not compile, and the trait's own doc
    /// comment reached for the isolation it made unavailable.
    ///
    /// It is written as a spawn rather than an `assert_static` helper because
    /// spawning is the thing the host actually does, and a bound assertion
    /// would still pass if some later change made the future `'static` but
    /// un-spawnable for another reason.
    #[tokio::test]
    async fn a_provider_query_future_can_be_spawned_as_its_own_task() {
        let provider = Arc::new(FakeProvider);
        let routed = Arc::new(route("firefox"));
        let ctx = QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: Instant::now() + Duration::from_secs(1),
        };

        let handle = tokio::spawn(provider.query(routed, ctx));
        let items = handle.await.unwrap().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "firefox");
    }

    /// `ProviderManifest` has to be comparable for the host to detect a
    /// provider whose `manifest()` answers differently after registration —
    /// #32's interior-mutability abuse. Equality is the whole mechanism, so it
    /// is pinned here rather than assumed at the call site.
    #[test]
    fn two_manifests_with_the_same_fields_are_equal_and_a_changed_field_is_not() {
        let a = manifest(vec![Mode::Apps], 3);
        assert_eq!(a, manifest(vec![Mode::Apps], 3));
        assert_ne!(a, manifest(vec![Mode::Apps], 4));
        assert_ne!(a, manifest(vec![Mode::All], 3));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hop-core provider::tests::a_provider_query_future_can_be_spawned_as_its_own_task`
Expected: FAIL to compile — `provider.query(routed, ctx)` does not match the borrowed signature, and `tokio::spawn` rejects a non-`'static` future. The manifest test fails with `binary operation == cannot be applied`.

- [ ] **Step 3: Change the trait**

In `crates/hop-core/src/provider.rs`:

Derive equality on the manifest:

```rust
/// ...existing docs, plus:
///
/// `PartialEq`/`Eq` because a host compares the manifest it captured at
/// registration against what [`Provider::manifest`] answers later — see
/// [`ProviderHost`](crate::host::ProviderHost). That comparison is how a
/// manifest built from interior mutability is caught, so equality here is
/// load-bearing rather than a convenience: a field added to this struct and
/// left out of the comparison would be a field a provider could change
/// undetected, and deriving is what keeps the two in step automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderManifest {
```

Replace the trait, keeping every existing doc comment on `query` and `execute` and adding the ownership rationale:

```rust
/// The plugin seam: anything that can answer a routed query with items, and
/// execute an action on one of the items it produced.
///
/// The two async methods are written as `-> impl Future<...> + Send + 'static`
/// (native async-in-trait, stabilized without the bounds baked in) rather than
/// as bare `async fn`. A bare `async fn` in a public trait produces a future
/// type with no bounds at all, which would block the daemon from spawning it
/// onto a Tokio runtime. Writing the desugared form here, once, means every
/// implementor gets a spawnable future automatically instead of everyone
/// needing to route around the gap later. It also avoids the
/// `async_fn_in_trait` lint, which flags exactly this problem and which
/// `-D warnings` turns into a hard error.
///
/// # Why the arguments are owned
///
/// `tokio::spawn` requires `'static` as well as `Send`, and a future capturing
/// `&self`, `&RoutedQuery` or `&QueryCtx` is neither — so the borrowed
/// signature this trait shipped with made the panic isolation its own docs
/// reached for unavailable, and forced a host to poll every provider's future
/// in one task. That is issue #29, and closing it is a breaking change to this
/// seam that spec §6's 2026-07-31 amendment sanctions by name: the lock takes
/// effect when the extension store ships, not now, and #29 is one of the two
/// gaps that amendment says can only be closed by changing these types.
///
/// `Arc<Self>` rather than `Self` so one registered provider serves every
/// query without being cloned; `Arc<RoutedQuery>` so the same routed query
/// reaches every selected provider without one clone per provider on the
/// keystroke path; `QueryCtx` by value because it is two cheap fields, one of
/// them already `Arc`-backed.
///
/// `'static` on the trait itself is what `Arc<dyn ...>` erasure needs
/// downstream, and every provider is a long-lived registered object anyway.
pub trait Provider: Send + Sync + 'static {
    /// This provider's static description — see [`ProviderManifest`].
    ///
    /// **Stability is part of this contract: every call must return the same
    /// manifest.** A host calls this once — at registration, before any query
    /// has run — and treats the value as constant for the life of the
    /// provider. Returning a stored manifest, or rebuilding one fixed value
    /// per call, satisfies this; deriving any field from state that changes
    /// while the provider is alive does not, whatever the intent.
    ///
    /// Unlike when this comment was written, something does now check:
    /// [`ProviderHost`](crate::host::ProviderHost) compares its captured
    /// manifest against a fresh call before it accepts a provider's items, and
    /// refuses the answer on a mismatch. What that does *not* do is make the
    /// contract enforced everywhere —
    /// [`ProviderOutput::from_provider`](crate::pipeline::ProviderOutput::from_provider)
    /// still reads the manifest off the object it is handed, and a caller that
    /// is not the host still gets whatever the provider answers with. Read on
    /// for the abuse that recovers.
    ///
    /// [`ProviderOutput::from_provider`](crate::pipeline::ProviderOutput::from_provider)
    /// reads the manifest *after* [`Provider::query`] has returned, so a
    /// provider answering differently on two calls gets to choose what it is
    /// checked against once it has seen what it wants to return. Concretely,
    /// this is issue #31's exclusive-mode bypass rebuilt from honest-looking
    /// parts: declare `kinds: [Calculator]` at registration, before the host
    /// captures it as constant, return `Kind::Window` items from `query`, then
    /// answer `kinds: [Window]` when the check asks. Each answer is
    /// self-consistent in isolation, the kind check passes, and the Window
    /// items go on to survive a `w `-exclusive filter and inherit Window's
    /// ranking weight — which is the whole of what that check exists to
    /// prevent.
    fn manifest(&self) -> ProviderManifest;

    /// Answers a routed query with the items this provider can find.
    ///
    /// [KEEP the entire existing doc comment from `# The implementation is the
    /// escaping party for its own sink` through the end of `# The
    /// implementation also validates its own term` verbatim — none of it is
    /// affected by the ownership change.]
    fn query(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        ctx: QueryCtx,
    ) -> impl Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static;

    /// Executes `action_id` on `item_id`, both of which this provider must
    /// have produced from a prior [`Provider::query`] call.
    fn execute(
        self: Arc<Self>,
        item_id: ItemId,
        action_id: ActionId,
    ) -> impl Future<Output = Result<ExecOutcome, ProviderError>> + Send + 'static;
}
```

Update `provider.rs`'s own `FakeProvider` to match:

```rust
    impl Provider for FakeProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "fake",
                ..manifest(vec![Mode::All], 0)
            }
        }

        async fn query(
            self: Arc<Self>,
            q: Arc<RoutedQuery>,
            ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            if ctx.cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            Ok(vec![Item {
                id: ItemId::new("app:fake").unwrap(),
                kind: Kind::App,
                title: q.term.clone(),
                subtitle: None,
                icon: None,
                actions: vec![],
                default_action: ActionId::new("open").unwrap(),
                copy_text: None,
                append_to_end: false,
                provider: "fake".into(),
            }])
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }
```

Then fix the three existing tests in that module that call `provider.query(&routed, &ctx)`: wrap the provider in `Arc::new`, the routed query in `Arc::new`, and pass `ctx` by value. `a_providers_own_output_passes_its_own_manifests_checks` calls `ProviderOutput::from_provider(&provider, items)` — with an `Arc`, that becomes `ProviderOutput::from_provider(&*provider, items)`. Delete the old `provider_query_future_is_send` test: the new spawn test above subsumes it, since `tokio::spawn` requires `Send` too.

In `crates/hop-core/src/pipeline.rs`, update its `FakeProvider` the same way (its `query` returns `Ok(Vec::new())` and is never called; its `execute` returns `Ok(ExecOutcome::Done)`). Its `output()` helper calls `ProviderOutput::from_provider(&provider(id, kinds), items)` and needs no change, because that helper builds a bare `FakeProvider` rather than an `Arc`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p hop-core
```
Expected: PASS, including both new tests. Every pre-existing `hop-core` test must still pass — this task changes a signature, not a behavior.

- [ ] **Step 5: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green. `hopd` and `hop-cli` do not implement `Provider` yet, so nothing outside `hop-core` breaks.

- [ ] **Step 6: Commit**

```bash
git add crates/hop-core/src/provider.rs crates/hop-core/src/pipeline.rs
git commit -m "hop-core: make a provider's future spawnable and its manifest comparable"
```

---

### Task 3: The reporting vocabulary — attributed failures and the log seam

**Files:**
- Create: `crates/hop-core/src/host.rs`
- Modify: `crates/hop-core/src/lib.rs`

**Interfaces:**
- Consumes: `sanitize_provider_message`, `MAX_PROVIDER_MESSAGE` (Task 1).
- Produces, for Tasks 4–7:
  ```rust
  pub enum FailureKind { Timeout, Cancelled, Failed, Panicked }
  pub struct ProviderFailure { pub provider: String, pub kind: FailureKind, pub message: String, pub elapsed: Duration }
  impl ProviderFailure {
      pub fn from_error(provider: &str, error: ProviderError, elapsed: Duration) -> Self;
      pub fn panicked(provider: &str, elapsed: Duration) -> Self;
      pub fn budget_miss(provider: &str, elapsed: Duration) -> Self;
  }
  pub enum ProviderEvent<'a> {
      Answered { provider: &'a str, items: usize, elapsed: Duration },
      Failed(&'a ProviderFailure),
      BudgetMiss { provider: &'a str, budget: Duration, elapsed: Duration },
      Rejected { provider: &'a str, rejections: &'a [Rejection] },
      Skipped { provider: &'a str },
  }
  pub trait ProviderLog: Send + Sync + 'static { fn record(&self, event: ProviderEvent<'_>); }
  pub struct NoopLog;
  ```

- [ ] **Step 1: Write the failing tests**

Create `crates/hop-core/src/host.rs` with this content (Task 4 and Task 5 append to it):

```rust
//! The provider host: what owns registered providers, decides which of them a
//! keystroke reaches, runs their queries under a budget it enforces itself,
//! and contains their failures.
//!
//! This module is the enforcement point issues #28, #32 and #34 each found
//! missing. Before it, [`ProviderManifest::budget`] appeared nowhere outside
//! doc comments, [`should_query`] had no caller outside tests, and nothing in
//! the workspace recorded that a provider had failed at all.
//!
//! # What is enforced here rather than asked for
//!
//! A provider is untrusted code. Every guarantee below therefore holds without
//! its cooperation:
//!
//! - **The manifest is read once**, at registration, and every scheduling
//!   decision reads that captured copy. A provider that answers differently
//!   afterwards changes nothing about whether it is asked to run.
//! - **The budget is a host deadline, not a request.** Each provider's future
//!   runs as its own task and is abandoned when its budget expires, whether or
//!   not it ever polled [`QueryCtx::cancel`].
//! - **A panic is contained at the seam** and reported as a failure naming the
//!   provider, because the future runs under [`tokio::spawn`] and a panicking
//!   task surfaces as a [`JoinError`](tokio::task::JoinError) rather than
//!   unwinding into the daemon.
//! - **Provider-supplied text is rewritten before it can leave**, by
//!   [`sanitize_provider_message`](crate::sanitize::sanitize_provider_message).
//! - **One failing provider never empties a frame for the others**, which is
//!   spec §9's per-provider isolation rule: providers are separate tasks
//!   holding separate senders, so nothing about one provider's outcome is on
//!   another's path.
//!
//! # What is not enforced here, and where it goes instead
//!
//! Ranking. This module streams each provider's manifest-checked items as its
//! own batch, in the order providers answer, and never calls
//! [`Pipeline::assemble`](crate::pipeline::Pipeline::assemble) — see the
//! "Scope" section of `docs/superpowers/plans/2026-08-04-issue-56-provider-host.md`
//! for why wiring assembly needs a protocol answer about streaming that issue
//! #56 does not give.

use std::sync::Arc;
use std::time::Duration;

use crate::pipeline::Rejection;
use crate::provider::ProviderError;
use crate::sanitize::sanitize_provider_message;

/// Why a provider did not answer, as the host classifies it.
///
/// Deliberately not the same enum as [`ProviderError`]: that one is the
/// provider's own vocabulary, and this one adds the case a provider cannot
/// report about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The provider returned [`ProviderError::Timeout`] on its own, before its
    /// budget ran out. It noticed its deadline and gave up — cooperation, not
    /// enforcement. A host cut-off is [`ProviderEvent::BudgetMiss`] and is a
    /// different event on purpose; only the second one proves the host enforced
    /// anything.
    Timeout,
    /// The provider returned [`ProviderError::Cancelled`], or its task was
    /// abandoned after the query it belonged to went away.
    Cancelled,
    /// The provider returned [`ProviderError::Failed`]. Its text is in
    /// [`ProviderFailure::message`], sanitized.
    Failed,
    /// The provider's future panicked, and [`tokio::spawn`] contained it. No
    /// provider can report this about itself, which is why the variant exists
    /// on this enum and not on [`ProviderError`].
    Panicked,
}

/// One provider's failure, attributed and safe to render.
///
/// # Why the host builds this and a provider cannot
///
/// `provider` is read from the manifest the host **captured at
/// registration** — never from anything the failing provider said at failure
/// time. This is [`ProviderOutput`](crate::pipeline::ProviderOutput)'s argument
/// applied to errors instead of items: a value the untrusted party can name is
/// a value it can forge, so a provider that fails with the text
/// `"apps: index corrupt"` cannot make the daemon attribute its failure to the
/// apps provider. It is also why issue #34's "the error carries the producing
/// provider's id" is met here rather than by a field on [`ProviderError`],
/// which providers construct themselves.
///
/// `message` has been through
/// [`sanitize_provider_message`](crate::sanitize::sanitize_provider_message),
/// so it is within
/// [`MAX_PROVIDER_MESSAGE`](crate::sanitize::MAX_PROVIDER_MESSAGE) bytes and
/// carries no control or direction-override characters. Constructing a
/// `ProviderFailure` is the only way this type is built, and each constructor
/// sanitizes, so there is no path that produces one carrying raw provider text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    /// The [`ProviderManifest::id`](crate::provider::ProviderManifest::id) of
    /// the provider that failed, from the captured manifest.
    pub provider: String,
    /// How it failed.
    pub kind: FailureKind,
    /// Human-readable detail, sanitized. Empty for the kinds that carry no
    /// provider-supplied text.
    pub message: String,
    /// How long the host waited before this outcome was known.
    pub elapsed: Duration,
}

impl ProviderFailure {
    /// Classifies a [`ProviderError`] the provider returned, sanitizing its
    /// text.
    pub fn from_error(provider: &str, error: ProviderError, elapsed: Duration) -> Self {
        let (kind, message) = match error {
            ProviderError::Timeout => (FailureKind::Timeout, String::new()),
            ProviderError::Cancelled => (FailureKind::Cancelled, String::new()),
            ProviderError::Failed(text) => {
                (FailureKind::Failed, sanitize_provider_message(&text))
            }
        };
        ProviderFailure {
            provider: provider.to_string(),
            kind,
            message,
            elapsed,
        }
    }

    /// A provider whose future panicked. The message is the host's own words —
    /// a panic payload is provider-controlled text that has already escaped one
    /// boundary, and nothing needs it to render a failure.
    pub fn panicked(provider: &str, elapsed: Duration) -> Self {
        ProviderFailure {
            provider: provider.to_string(),
            kind: FailureKind::Panicked,
            message: "the provider panicked".to_string(),
            elapsed,
        }
    }

    /// A provider the host cut off at its budget. Reported as a timeout
    /// because that is what the client needs to know; the host-versus-provider
    /// distinction is carried by [`ProviderEvent::BudgetMiss`] on the log seam.
    pub fn budget_miss(provider: &str, elapsed: Duration) -> Self {
        ProviderFailure {
            provider: provider.to_string(),
            kind: FailureKind::Timeout,
            message: "the provider exceeded its budget".to_string(),
            elapsed,
        }
    }
}

/// What the host reports about one provider on one query.
///
/// Borrowed throughout, and constructed per event on the query path: an
/// implementation that wants to keep a record owns it itself, so a
/// [`NoopLog`] costs a call and no allocation. That matters because this is
/// the keystroke path spec §3 holds to 10 ms.
#[derive(Debug)]
pub enum ProviderEvent<'a> {
    /// A provider answered, with this many items, after this long.
    Answered {
        provider: &'a str,
        items: usize,
        elapsed: Duration,
    },
    /// A provider failed. Covers every [`FailureKind`], including a budget
    /// miss — a miss emits *both* this and [`ProviderEvent::BudgetMiss`],
    /// because one is the failure the client sees and the other is the
    /// enforcement fact only the host knows.
    Failed(&'a ProviderFailure),
    /// The host cut a provider off at its budget. Issue #34 names budget
    /// misses separately from failures, and spec §3 requires that a budget
    /// miss logs.
    BudgetMiss {
        provider: &'a str,
        budget: Duration,
        elapsed: Duration,
    },
    /// Items the manifest checks refused —
    /// [`CheckedItems::check`](crate::pipeline::CheckedItems::check)'s
    /// rejections, which had nowhere to go before this seam existed. This is
    /// the event that makes ignoring them a mistake rather than a one-character
    /// omission, which is what `pipeline.rs` said a logging seam would buy.
    Rejected {
        provider: &'a str,
        rejections: &'a [Rejection],
    },
    /// The pre-filter declined to run a provider for this query — its captured
    /// manifest did not list the routed mode, or the term was shorter than its
    /// minimum. The common case by design ("most keystrokes never reach most
    /// plugins", spec §6 rule 2), so an implementation that records this should
    /// expect volume.
    Skipped { provider: &'a str },
}

/// Where the host reports what providers did.
///
/// # Why a trait and not `tracing`
///
/// Spec §9 makes `tracing` the daemon's logging backend, and issue #34 puts
/// choosing a backend explicitly out of scope. A trait is what separates the
/// two: this crate defines *what* is worth recording, and the daemon decides
/// where it goes. It is also the only shape that delivers what `pipeline.rs`
/// promised a logging seam would — rejections stay ignorable "until there is a
/// logging seam (issue #34) that makes ignoring them a real mistake", and a
/// macro at a call site makes nothing unignorable while a `ProviderLog` the
/// host cannot be constructed without does.
pub trait ProviderLog: Send + Sync + 'static {
    /// Records one event. Called on the query path, so an implementation that
    /// blocks or allocates heavily spends the latency budget spec §3 sets.
    fn record(&self, event: ProviderEvent<'_>);
}

/// A [`ProviderLog`] that discards everything — for tests, and for any host
/// whose caller has not chosen a backend.
///
/// It exists so that "no logging configured" is a visible choice at the
/// construction site rather than an `Option<Arc<dyn ProviderLog>>` every call
/// site has to branch on.
pub struct NoopLog;

impl ProviderLog for NoopLog {
    fn record(&self, _event: ProviderEvent<'_>) {}
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::sanitize::MAX_PROVIDER_MESSAGE;
    use std::sync::Mutex;

    /// A [`ProviderLog`] that keeps what it was told, as owned strings — the
    /// recording impl every test below and in Task 5 asserts against.
    ///
    /// It formats each event into a short line rather than storing the borrowed
    /// event, because [`ProviderEvent`] borrows and a recorder has to outlive
    /// the call. The lines are what the assertions read.
    #[derive(Default)]
    pub(crate) struct RecordingLog {
        pub(crate) lines: Mutex<Vec<String>>,
    }

    impl RecordingLog {
        pub(crate) fn lines(&self) -> Vec<String> {
            self.lines.lock().expect("no test panics holding this").clone()
        }
    }

    impl ProviderLog for RecordingLog {
        fn record(&self, event: ProviderEvent<'_>) {
            let line = match event {
                ProviderEvent::Answered {
                    provider, items, ..
                } => format!("answered {provider} {items}"),
                ProviderEvent::Failed(failure) => format!(
                    "failed {} {:?} {}",
                    failure.provider, failure.kind, failure.message
                ),
                ProviderEvent::BudgetMiss { provider, .. } => format!("budget-miss {provider}"),
                ProviderEvent::Rejected {
                    provider,
                    rejections,
                } => format!("rejected {provider} {}", rejections.len()),
                ProviderEvent::Skipped { provider } => format!("skipped {provider}"),
            };
            self.lines
                .lock()
                .expect("no test panics holding this")
                .push(line);
        }
    }

    #[test]
    fn a_provider_failure_is_attributed_to_the_captured_id_not_the_error_text() {
        // The provider's text names another provider; attribution must ignore
        // it entirely.
        let failure = ProviderFailure::from_error(
            "calculator",
            ProviderError::Failed("apps: index corrupt".into()),
            Duration::from_millis(3),
        );
        assert_eq!(failure.provider, "calculator");
        assert_eq!(failure.kind, FailureKind::Failed);
        assert_eq!(failure.message, "apps: index corrupt");
    }

    #[test]
    fn provider_error_text_is_sanitized_when_the_failure_is_built() {
        let raw = format!("\u{1b}[31m{}", "x".repeat(MAX_PROVIDER_MESSAGE * 2));
        let failure = ProviderFailure::from_error(
            "apps",
            ProviderError::Failed(raw),
            Duration::from_millis(1),
        );
        assert_eq!(failure.message.len(), MAX_PROVIDER_MESSAGE);
        assert!(!failure.message.contains('\u{1b}'));
    }

    #[test]
    fn the_kinds_that_carry_no_provider_text_have_an_empty_message() {
        for error in [ProviderError::Timeout, ProviderError::Cancelled] {
            let failure = ProviderFailure::from_error("apps", error, Duration::ZERO);
            assert_eq!(failure.message, "");
        }
    }

    #[test]
    fn a_panic_failure_names_the_provider_and_carries_the_hosts_own_words() {
        let failure = ProviderFailure::panicked("apps", Duration::from_millis(2));
        assert_eq!(failure.provider, "apps");
        assert_eq!(failure.kind, FailureKind::Panicked);
        assert_eq!(failure.message, "the provider panicked");
    }

    #[test]
    fn a_budget_miss_reports_as_a_timeout_to_the_client() {
        let failure = ProviderFailure::budget_miss("slow", Duration::from_millis(50));
        assert_eq!(failure.kind, FailureKind::Timeout);
        assert_eq!(failure.provider, "slow");
    }

    #[test]
    fn a_provider_volunteered_timeout_and_a_host_cut_off_are_the_same_kind() {
        // Deliberate: the client learns "it timed out" either way. What tells
        // them apart is the log seam's `BudgetMiss`, asserted in Task 5.
        let volunteered =
            ProviderFailure::from_error("slow", ProviderError::Timeout, Duration::ZERO);
        let enforced = ProviderFailure::budget_miss("slow", Duration::ZERO);
        assert_eq!(volunteered.kind, enforced.kind);
        assert_ne!(
            volunteered.message, enforced.message,
            "and the message is what distinguishes them for a reader"
        );
    }

    #[test]
    fn the_noop_log_accepts_every_event_shape() {
        // Compile-and-run coverage that `ProviderEvent`'s borrows work for a
        // real implementation, which is what Task 5's call sites depend on.
        let failure = ProviderFailure::panicked("apps", Duration::ZERO);
        NoopLog.record(ProviderEvent::Answered {
            provider: "apps",
            items: 3,
            elapsed: Duration::ZERO,
        });
        NoopLog.record(ProviderEvent::Failed(&failure));
        NoopLog.record(ProviderEvent::BudgetMiss {
            provider: "apps",
            budget: Duration::ZERO,
            elapsed: Duration::ZERO,
        });
        NoopLog.record(ProviderEvent::Rejected {
            provider: "apps",
            rejections: &[],
        });
        NoopLog.record(ProviderEvent::Skipped { provider: "apps" });
    }

    #[test]
    fn the_recording_log_captures_what_it_is_told() {
        let log = RecordingLog::default();
        log.record(ProviderEvent::Answered {
            provider: "apps",
            items: 2,
            elapsed: Duration::ZERO,
        });
        log.record(ProviderEvent::Skipped { provider: "calc" });
        assert_eq!(log.lines(), vec!["answered apps 2", "skipped calc"]);
    }
}
```

Add `pub mod host;` to `crates/hop-core/src/lib.rs` (alphabetical, after `aliases`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hop-core host::`
Expected: FAIL — the module does not exist until Step 1's file is written; once written, every test name above must appear and pass.

- [ ] **Step 3: Run the gate**

```bash
cargo test -p hop-core
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/hop-core/src/host.rs crates/hop-core/src/lib.rs
git commit -m "hop-core: an attributed provider failure and a logging seam to report it through"
```

---

### Task 4: Registration — capture once, clamp, and pre-filter

**Files:**
- Modify: `crates/hop-core/src/host.rs`

**Interfaces:**
- Consumes: Task 3's `ProviderLog`, `NoopLog`, `ProviderEvent`; Task 2's `Provider`, `ProviderManifest`.
- Produces, for Task 5 and the daemon:
  ```rust
  pub const MAX_PROVIDER_BUDGET: Duration = Duration::from_millis(50);
  pub struct HostPolicy { pub max_budget: Duration, pub min_term_len_floor: usize }
  impl Default for HostPolicy
  pub enum RegistrationError { DuplicateId(String) }   // thiserror
  pub struct ProviderHost { /* private */ }
  impl ProviderHost {
      pub fn new(policy: HostPolicy, log: Arc<dyn ProviderLog>) -> Self;
      pub fn with_log(log: Arc<dyn ProviderLog>) -> Self;      // default policy
      pub fn register<P: Provider>(&mut self, provider: P) -> Result<(), RegistrationError>;
      pub fn manifests(&self) -> Vec<ProviderManifest>;        // the captured, clamped copies
      pub fn len(&self) -> usize;
      pub fn is_empty(&self) -> bool;
  }
  ```
  Task 5 adds `spawn_query` to the same `impl`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/hop-core/src/host.rs`'s `mod tests`:

```rust
    use crate::provider::{CancellationFlag, Provider, ProviderManifest, QueryCtx};
    use crate::router::{Mode, RoutedQuery, route};
    use hop_protocol::{ActionId, ExecOutcome, Item, ItemId, Kind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider whose manifest is whatever the test says it is, and whose
    /// `query` answers with a fixed list. Task 5's tests extend this file with
    /// providers that hang and panic; this one is the well-behaved baseline.
    pub(crate) struct ScriptedProvider {
        pub(crate) manifest: ProviderManifest,
        pub(crate) items: Vec<Item>,
        /// How many times `manifest()` has been called — the counter that
        /// proves capture happens once.
        pub(crate) manifest_calls: AtomicUsize,
    }

    impl ScriptedProvider {
        pub(crate) fn new(id: &'static str, kinds: Vec<Kind>, items: Vec<Item>) -> Self {
            ScriptedProvider {
                manifest: ProviderManifest {
                    id,
                    kinds,
                    modes: vec![Mode::All],
                    min_term_len: 0,
                    budget: Duration::from_millis(10),
                },
                items,
                manifest_calls: AtomicUsize::new(0),
            }
        }
    }

    impl Provider for ScriptedProvider {
        fn manifest(&self) -> ProviderManifest {
            self.manifest_calls.fetch_add(1, Ordering::Relaxed);
            self.manifest.clone()
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Ok(self.items.clone())
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// A provider whose `manifest()` answers one way the first time and
    /// another way afterwards — issue #32's interior-mutability abuse, built
    /// from honest-looking parts.
    pub(crate) struct ShiftyProvider {
        calls: AtomicUsize,
    }

    impl ShiftyProvider {
        pub(crate) fn new() -> Self {
            ShiftyProvider {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Provider for ShiftyProvider {
        fn manifest(&self) -> ProviderManifest {
            let first = self.calls.fetch_add(1, Ordering::Relaxed) == 0;
            ProviderManifest {
                id: "shifty",
                kinds: vec![Kind::App],
                modes: vec![Mode::Apps],
                // Declares a 3-character minimum while the host is looking,
                // then zero forever after — so an unscheduled provider would
                // start being dispatched on every keystroke.
                min_term_len: if first { 3 } else { 0 },
                budget: Duration::from_millis(10),
            }
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

    pub(crate) fn item(provider: &str, kind: Kind, id: &str, title: &str) -> Item {
        Item {
            id: ItemId::new(id).unwrap(),
            kind,
            title: title.to_string(),
            subtitle: None,
            icon: None,
            actions: vec![],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: provider.to_string(),
        }
    }

    fn host() -> ProviderHost {
        ProviderHost::with_log(Arc::new(NoopLog))
    }

    #[test]
    fn a_registered_providers_manifest_is_read_exactly_once() {
        let provider = Arc::new(ScriptedProvider::new("apps", vec![Kind::App], vec![]));
        let mut host = host();
        // Registration takes ownership, so the counter is read through a clone
        // of the Arc the test kept.
        host.register_arc(provider.clone()).unwrap();
        assert_eq!(
            provider.manifest_calls.load(Ordering::Relaxed),
            1,
            "registration reads the manifest once and captures it"
        );
        assert_eq!(host.len(), 1);
    }

    #[test]
    fn a_second_registration_under_an_id_already_in_use_is_refused() {
        // Load-bearing for boost correctness, not registry hygiene: two
        // providers sharing an id both pass `CheckedItems::check` and both
        // collect every alias boost tagged with it — see `APPS_PROVIDER_ID`'s
        // docs, which name rejecting this as the host's job.
        let mut host = host();
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        let err = host
            .register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .expect_err("a duplicate id must be refused");
        assert!(matches!(err, RegistrationError::DuplicateId(id) if id == "apps"));
        assert_eq!(host.len(), 1, "the duplicate must not be registered");
    }

    #[test]
    fn a_manifest_budget_over_the_ceiling_is_clamped() {
        let mut provider = ScriptedProvider::new("greedy", vec![Kind::App], vec![]);
        provider.manifest.budget = Duration::from_secs(3600);
        let mut host = host();
        host.register(provider).unwrap();
        assert_eq!(
            host.manifests()[0].budget, MAX_PROVIDER_BUDGET,
            "an hour-long budget is clamped to the host's ceiling"
        );
    }

    #[test]
    fn a_manifest_budget_under_the_ceiling_is_left_alone() {
        let mut host = host();
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        assert_eq!(host.manifests()[0].budget, Duration::from_millis(10));
    }

    #[test]
    fn the_host_can_raise_a_minimum_term_length_above_what_a_provider_declared() {
        let mut host = ProviderHost::new(
            HostPolicy {
                min_term_len_floor: 2,
                ..HostPolicy::default()
            },
            Arc::new(NoopLog),
        );
        // Declares 0 — "always run, including for the empty term".
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        assert_eq!(host.manifests()[0].min_term_len, 2);
    }

    #[test]
    fn the_floor_never_lowers_a_providers_own_minimum() {
        let mut provider = ScriptedProvider::new("apps", vec![Kind::App], vec![]);
        provider.manifest.min_term_len = 5;
        let mut host = ProviderHost::new(
            HostPolicy {
                min_term_len_floor: 2,
                ..HostPolicy::default()
            },
            Arc::new(NoopLog),
        );
        host.register(provider).unwrap();
        assert_eq!(
            host.manifests()[0].min_term_len, 5,
            "the floor raises, it never relaxes a provider's own stricter rule"
        );
    }

    #[test]
    fn scheduling_reads_the_captured_manifest_so_a_shifty_provider_changes_nothing() {
        // The whole of issue #32: `min_term_len: 3` at registration, `0`
        // afterwards. Scheduling must still refuse a 2-character term.
        let mut host = host();
        host.register(ShiftyProvider::new()).unwrap();

        let short = route("a hi");
        assert_eq!(short.term, "hi");
        assert!(
            host.selected_ids(&short).is_empty(),
            "the captured minimum of 3 governs, not the 0 it now answers with"
        );

        let long = route("a firefox");
        assert_eq!(
            host.selected_ids(&long),
            vec!["shifty"],
            "and a term over the captured minimum still reaches it"
        );
    }

    #[test]
    fn the_prefilter_declines_a_provider_whose_captured_modes_exclude_the_route() {
        let mut host = host();
        // `ScriptedProvider` declares `modes: [Mode::All]`, so an exclusive
        // windows route reaches nothing.
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        assert!(host.selected_ids(&route("w terminal")).is_empty());
        assert_eq!(host.selected_ids(&route("terminal")), vec!["apps"]);
    }

    #[test]
    fn a_skipped_provider_is_recorded_on_the_log_seam() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        host.selected_ids(&route("w terminal"));
        assert_eq!(log.lines(), vec!["skipped apps"]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hop-core host::`
Expected: FAIL to compile — `ProviderHost`, `HostPolicy`, `RegistrationError`, `MAX_PROVIDER_BUDGET`, `register`, `register_arc`, `manifests`, `len`, `selected_ids` are all undefined.

- [ ] **Step 3: Implement registration**

Append to `crates/hop-core/src/host.rs`, before `mod tests`:

```rust
use crate::provider::{Provider, ProviderManifest, should_query};
use crate::router::RoutedQuery;

/// The most a provider's per-query budget may be, whatever its manifest says.
///
/// # Why 50 ms
///
/// Spec §3 holds the whole keystroke path — every provider, plus ranking — to
/// 10 ms, and this ceiling is deliberately looser than that rather than equal
/// to it. The two bound different things: 10 ms is the target for the frame a
/// user sees, and this is the point past which the host stops waiting for one
/// provider that is already late. A ceiling at 10 ms would make the *first*
/// slow provider the thing that fails the latency contract, and a provider cut
/// off at its budget does not delay the frame at all — spec §3's rule is that
/// a budget miss "logs and isolates, never blocks the frame", and the other
/// providers' items have already streamed by then.
///
/// 50 ms is also what every manifest in the tree already declares, so this
/// ceiling clamps nothing that exists today and only bites a provider asking
/// for materially more. A provider that genuinely needs longer than this is a
/// provider doing I/O on the query path, which spec §3 forbids outright: the
/// network providers return "a cached-or-pending row synchronously and push an
/// update frame when the fetch lands", so their slow half is not a query at
/// all.
///
/// It is a constant rather than a knob because a per-provider override is
/// exactly the provider-authored policy issue #32 exists to remove.
/// [`HostPolicy::max_budget`] lets the *host* lower it, never a provider raise
/// it.
pub const MAX_PROVIDER_BUDGET: Duration = Duration::from_millis(50);

/// The host's own policy, applied to every manifest at registration.
///
/// This is the layer issue #32 found missing: before it, every input to a
/// scheduling decision came from the provider's own manifest, so the
/// declarative pre-filter spec §6 describes as the host's protection was
/// really a provider self-declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPolicy {
    /// Ceiling on a provider's budget. A manifest asking for more is clamped
    /// to this. Defaults to [`MAX_PROVIDER_BUDGET`]; a host may set it lower
    /// but nothing lets a provider raise it.
    pub max_budget: Duration,
    /// Floor under a provider's `min_term_len`. A manifest declaring less is
    /// raised to this, so the host can keep providers off short terms
    /// regardless of what they asked for. Defaults to `0`, which changes
    /// nothing.
    ///
    /// One direction only: a provider that declares a *higher* minimum keeps
    /// it. The floor exists to make providers cheaper to run, not to make a
    /// cautious provider run more often than it wanted.
    pub min_term_len_floor: usize,
}

impl Default for HostPolicy {
    fn default() -> Self {
        HostPolicy {
            max_budget: MAX_PROVIDER_BUDGET,
            min_term_len_floor: 0,
        }
    }
}

/// Why [`ProviderHost::register`] refused a provider.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    /// Another provider is already registered under this
    /// [`ProviderManifest::id`].
    ///
    /// Refusing is load-bearing for boost correctness rather than registry
    /// hygiene: [`APPS_PROVIDER_ID`](crate::provider::APPS_PROVIDER_ID)'s docs
    /// spell out that two providers sharing an id both pass
    /// [`CheckedItems::check`](crate::pipeline::CheckedItems::check) and both
    /// collect every alias boost tagged with that id — issue #31's boost theft,
    /// moved up one level from "which item" to "which provider" — and name
    /// enforcing uniqueness here as the M2 registry's job.
    #[error("a provider is already registered under the id `{0}`")]
    DuplicateId(String),
}

/// One registered provider: the manifest captured at registration, the
/// host-clamped copy scheduling reads, and the provider itself.
struct Registration {
    /// Exactly what [`Provider::manifest`] answered at registration, before
    /// any clamp. Kept so the host can compare it against a later call and
    /// catch a provider whose manifest shifts — clamping deliberately makes
    /// `effective` differ, so `effective` cannot serve as that baseline.
    declared: ProviderManifest,
    /// `declared` with [`HostPolicy`] applied. Every scheduling decision and
    /// the enforced budget read this, and nothing re-reads
    /// [`Provider::manifest`] to make one.
    effective: ProviderManifest,
    provider: Arc<dyn ErasedProvider>,
}

/// Owns registered providers and runs their queries.
///
/// See the module docs for what is enforced here without a provider's
/// cooperation.
pub struct ProviderHost {
    providers: Vec<Registration>,
    policy: HostPolicy,
    log: Arc<dyn ProviderLog>,
}

impl ProviderHost {
    /// A host with an explicit policy and log seam.
    pub fn new(policy: HostPolicy, log: Arc<dyn ProviderLog>) -> Self {
        ProviderHost {
            providers: Vec::new(),
            policy,
            log,
        }
    }

    /// A host with the default policy — every ceiling and floor at its
    /// documented value.
    pub fn with_log(log: Arc<dyn ProviderLog>) -> Self {
        ProviderHost::new(HostPolicy::default(), log)
    }

    /// Registers `provider`, reading its manifest **once** and keeping the
    /// value.
    ///
    /// From here on nothing this host does consults
    /// [`Provider::manifest`] to make a decision, so a provider that answers
    /// differently later changes neither whether it is asked to run nor what
    /// budget it gets. That is issue #32's criterion, and the reason the
    /// capture happens here rather than per query.
    ///
    /// Refuses a provider whose id is already registered — see
    /// [`RegistrationError::DuplicateId`].
    pub fn register<P: Provider>(&mut self, provider: P) -> Result<(), RegistrationError> {
        self.register_arc(Arc::new(provider))
    }

    /// [`ProviderHost::register`] for a provider the caller already holds
    /// behind an `Arc` — the same capture, the same refusals.
    pub fn register_arc<P: Provider>(
        &mut self,
        provider: Arc<P>,
    ) -> Result<(), RegistrationError> {
        let declared = provider.manifest();
        if self
            .providers
            .iter()
            .any(|r| r.effective.id == declared.id)
        {
            return Err(RegistrationError::DuplicateId(declared.id.to_string()));
        }

        let effective = ProviderManifest {
            budget: declared.budget.min(self.policy.max_budget),
            min_term_len: declared.min_term_len.max(self.policy.min_term_len_floor),
            ..declared.clone()
        };

        self.providers.push(Registration {
            declared,
            effective,
            provider,
        });
        Ok(())
    }

    /// The captured, clamped manifests, in registration order. What scheduling
    /// reads, exposed so a caller can see what the host actually accepted
    /// rather than what a provider asked for.
    pub fn manifests(&self) -> Vec<ProviderManifest> {
        self.providers.iter().map(|r| r.effective.clone()).collect()
    }

    /// How many providers are registered.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether no providers are registered. A host in this state answers every
    /// query with nothing at all, which is a real state during M2: providers
    /// arrive in later slices.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The registrations this routed query reaches, by captured manifest, and
    /// a [`ProviderEvent::Skipped`] on the seam for each one it does not.
    ///
    /// This is [`should_query`]'s caller — the thing issue #32 found it did
    /// not have outside tests, which is what left the codebase with no worked
    /// example of the intended enforcement point.
    fn selected(&self, q: &RoutedQuery) -> Vec<&Registration> {
        self.providers
            .iter()
            .filter(|r| {
                let run = should_query(&r.effective, q);
                if !run {
                    self.log.record(ProviderEvent::Skipped {
                        provider: r.effective.id,
                    });
                }
                run
            })
            .collect()
    }
}
```

Add a test-only accessor at the end of the same `impl` block, so the pre-filter tests can assert on ids without `Registration` being public:

```rust
    /// The ids [`ProviderHost::selected`] would run for `q`, in registration
    /// order. Test-only: production callers want the providers, not their
    /// names.
    #[cfg(test)]
    fn selected_ids(&self, q: &RoutedQuery) -> Vec<&str> {
        self.selected(q).iter().map(|r| r.effective.id).collect()
    }
```

Add the erasure, before `mod tests`:

```rust
/// A dyn-compatible view of a [`Provider`], so a host can hold providers of
/// different concrete types in one collection.
///
/// # Why erasure is needed, and why this trait stays private
///
/// [`Provider`] is dyn-incompatible by construction — its RPITIT methods make
/// it so — and
/// [`ProviderOutput`](crate::pipeline::ProviderOutput)'s docs rely on exactly
/// that: "not something `dyn Provider` can launder either". So the host cannot
/// hold `Arc<dyn Provider>`, and needs this.
///
/// What keeps erasure from reopening the hole is where
/// [`ErasedProvider::output`] is implemented: inside a blanket
/// `impl<P: Provider>`, where the concrete `P` is in hand, so
/// [`ProviderOutput::from_provider`](crate::pipeline::ProviderOutput::from_provider)
/// still receives the object that was asked rather than a manifest a caller
/// chose. Nothing an item claims about itself can select the manifest it is
/// checked against, before or after erasure.
///
/// It is private to this crate for the same reason: a public
/// dyn-compatible provider trait would be a second route to supplying a
/// manifest, and the blanket impl means every [`Provider`] already has one.
trait ErasedProvider: Send + Sync + 'static {
    fn manifest(&self) -> ProviderManifest;

    /// [`Provider::query`] with its future boxed, which is what makes the
    /// method dyn-compatible.
    fn query_erased(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        ctx: QueryCtx,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static>>;

    /// Pairs `items` with this provider's manifest the only way
    /// [`ProviderOutput`](crate::pipeline::ProviderOutput) allows — see the
    /// trait docs for why this method exists here rather than at the call site.
    fn output(&self, items: Vec<Item>) -> ProviderOutput;
}

impl<P: Provider> ErasedProvider for P {
    fn manifest(&self) -> ProviderManifest {
        Provider::manifest(self)
    }

    fn query_erased(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        ctx: QueryCtx,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static>> {
        Box::pin(Provider::query(self, q, ctx))
    }

    fn output(&self, items: Vec<Item>) -> ProviderOutput {
        ProviderOutput::from_provider(self, items)
    }
}
```

and extend the module's imports at the top of the file:

```rust
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hop_protocol::Item;

use crate::pipeline::{ProviderOutput, Rejection};
use crate::provider::{Provider, ProviderError, ProviderManifest, QueryCtx, should_query};
use crate::router::RoutedQuery;
use crate::sanitize::sanitize_provider_message;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p hop-core host::`
Expected: PASS, all of Task 3's and Task 4's tests.

- [ ] **Step 5: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green. Note `clippy::len_without_is_empty` — both are defined above, so it stays quiet.

- [ ] **Step 6: Commit**

```bash
git add crates/hop-core/src/host.rs
git commit -m "hop-core: capture a provider's manifest once, clamp it, and pre-filter on the captured copy"
```

---

### Task 5: Query execution — enforced budgets, contained panics, streamed batches

**Files:**
- Modify: `crates/hop-core/src/host.rs`

**Interfaces:**
- Consumes: everything from Tasks 3 and 4.
- Produces, for Task 6:
  ```rust
  impl ProviderHost {
      pub fn spawn_query(
          self: &Arc<Self>,
          q: Arc<RoutedQuery>,
          results: mpsc::Sender<Vec<Item>>,
      ) -> CancellationFlag;
  }
  ```
  One `tokio::spawn` per selected provider; each sends its checked items as one batch and drops its sender. The channel closes when the last provider finishes, which is what tells `hopd`'s connection driver the query is done. The returned `CancellationFlag` is shared by every provider's `QueryCtx`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/hop-core/src/host.rs`'s `mod tests`:

```rust
    use tokio::sync::mpsc;

    /// A provider that never returns — the non-cooperating case #28 is about.
    /// It does not poll `ctx.cancel` at all, so nothing but the host can end
    /// it.
    pub(crate) struct HangingProvider;

    impl Provider for HangingProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "hanging",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            // A yielding hang rather than a busy loop: `abort` takes effect at
            // a yield point, and a busy loop would pin a worker thread for the
            // whole test run. The provider still never checks cancellation,
            // which is the property under test.
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// A provider that panics inside its future.
    pub(crate) struct PanickingProvider;

    impl Provider for PanickingProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "panicking",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            panic!("a provider indexing an empty vec");
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// A provider that fails with attacker-shaped text: over the cap, opening
    /// with a terminal escape and a right-to-left override.
    pub(crate) struct NastyProvider;

    impl Provider for NastyProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "nasty",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Err(ProviderError::Failed(format!(
                "\u{1b}[31m\u{202e}{}",
                "x".repeat(MAX_PROVIDER_MESSAGE * 10)
            )))
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// Drains every batch a query produces, in arrival order.
    async fn drain(mut rx: mpsc::Receiver<Vec<Item>>) -> Vec<Item> {
        let mut all = Vec::new();
        while let Some(batch) = rx.recv().await {
            all.extend(batch);
        }
        all
    }

    /// Runs one query against `host` and returns everything it streamed.
    async fn run(host: Arc<ProviderHost>, raw: &str) -> Vec<Item> {
        let (tx, rx) = mpsc::channel(1);
        host.spawn_query(Arc::new(route(raw)), tx);
        drain(rx).await
    }

    #[tokio::test]
    async fn a_well_behaved_providers_items_are_streamed() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let items = run(Arc::new(host), "firefox").await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Firefox");
        assert_eq!(log.lines(), vec!["answered apps 1"]);
    }

    #[tokio::test]
    async fn the_channel_closes_once_every_selected_provider_has_finished() {
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(ScriptedProvider::new("a", vec![Kind::App], vec![]))
            .unwrap();
        host.register(ScriptedProvider::new("b", vec![Kind::App], vec![]))
            .unwrap();

        let (tx, mut rx) = mpsc::channel(1);
        Arc::new(host).spawn_query(Arc::new(route("x")), tx);
        // Both answer with no items, so nothing is sent and the only event is
        // the close — which is what `hopd`'s driver turns into `QueryDone`.
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_host_with_no_providers_closes_immediately() {
        let host = Arc::new(ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog)));
        let (tx, mut rx) = mpsc::channel(1);
        host.spawn_query(Arc::new(route("x")), tx);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_provider_that_never_completes_is_cut_off_at_its_budget_without_cooperating() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(HangingProvider).unwrap();

        let started = std::time::Instant::now();
        let items = run(Arc::new(host), "x").await;
        let waited = started.elapsed();

        assert!(items.is_empty());
        assert!(
            waited < Duration::from_secs(1),
            "the host stopped waiting on its own; it waited {waited:?}"
        );
        let lines = log.lines();
        assert!(
            lines.iter().any(|l| l == "budget-miss hanging"),
            "a budget miss must reach the seam: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "failed hanging Timeout the provider exceeded its budget"),
            "and be reported as a timeout: {lines:?}"
        );
    }

    #[tokio::test]
    async fn a_panicking_provider_yields_a_panic_shaped_failure_naming_it() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(PanickingProvider).unwrap();

        let items = run(Arc::new(host), "x").await;
        assert!(items.is_empty());
        assert_eq!(
            log.lines(),
            vec!["failed panicking Panicked the provider panicked"]
        );
    }

    #[tokio::test]
    async fn one_providers_panic_does_not_cost_another_provider_its_results() {
        // Spec §9's per-provider isolation rule, and #29's second criterion.
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(PanickingProvider).unwrap();
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let items = run(Arc::new(host), "firefox").await;
        assert_eq!(
            items.len(),
            1,
            "the surviving provider's items still reach the client"
        );
        assert_eq!(items[0].title, "Firefox");
        assert!(log.lines().iter().any(|l| l.starts_with("failed panicking")));
    }

    #[tokio::test]
    async fn a_hanging_provider_does_not_delay_a_fast_providers_batch() {
        // "No slowest-provider gate" (spec §3): the fast provider's items must
        // arrive well before the hanging one's budget expires.
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(HangingProvider).unwrap();
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let (tx, mut rx) = mpsc::channel(1);
        Arc::new(host).spawn_query(Arc::new(route("firefox")), tx);

        let first = tokio::time::timeout(Duration::from_millis(5), rx.recv())
            .await
            .expect("the fast provider's batch must not wait on the slow one")
            .expect("a batch, not a close");
        assert_eq!(first[0].title, "Firefox");
    }

    #[tokio::test]
    async fn provider_error_text_is_bounded_and_stripped_before_it_leaves() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(NastyProvider).unwrap();

        run(Arc::new(host), "x").await;
        let lines = log.lines();
        let line = lines.first().expect("one failure was recorded");
        assert!(line.starts_with("failed nasty Failed "));
        let message = line.trim_start_matches("failed nasty Failed ");
        assert_eq!(message.len(), MAX_PROVIDER_MESSAGE);
        assert!(!message.contains('\u{1b}'));
        assert!(!message.contains('\u{202e}'));
    }

    #[tokio::test]
    async fn items_that_fail_their_own_producers_manifest_are_refused_and_recorded() {
        // The manifest checks still run, and their rejections now have
        // somewhere to go. This provider declares `kinds: [App]` and returns a
        // Window item.
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![
                item("apps", Kind::App, "app:ok", "Fine"),
                item("apps", Kind::Window, "window:forged", "Forged"),
            ],
        ))
        .unwrap();

        let items = run(Arc::new(host), "x").await;
        assert_eq!(items.len(), 1, "the forged-kind item never reaches a client");
        assert_eq!(items[0].id.as_str(), "app:ok");
        assert!(log.lines().iter().any(|l| l == "rejected apps 1"));
    }

    #[tokio::test]
    async fn a_provider_whose_manifest_shifted_after_registration_has_its_answer_refused() {
        // The comparison `ProviderOutput::from_provider`'s docs ask a host to
        // make: captured versus fresh, refuse on mismatch. `ShiftyProvider`
        // answers `min_term_len: 3` once and `0` afterwards, so its second
        // call — the one the check makes — differs.
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ShiftyProvider::new()).unwrap();

        let items = run(Arc::new(host), "a firefox").await;
        assert!(items.is_empty());
        let lines = log.lines();
        assert!(
            lines.iter().any(|l| l.starts_with("failed shifty Failed")),
            "the mismatch is reported as a failure attributed to the provider: {lines:?}"
        );
    }

    #[tokio::test]
    async fn dropping_the_receiver_cancels_the_providers_still_running() {
        // The `ResultSource` contract `hopd` relies on: dropping the receiver
        // is cancellation. A provider that polls the flag sees it set.
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let (tx, rx) = mpsc::channel(1);
        let cancel = Arc::new(host).spawn_query(Arc::new(route("firefox")), tx);
        drop(rx);

        // The provider's send fails, and that is what sets the flag for every
        // sibling still running.
        for _ in 0..100 {
            if cancel.is_cancelled() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("a failed send must set the shared cancellation flag");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hop-core host::`
Expected: FAIL to compile — `spawn_query` is undefined.

- [ ] **Step 3: Implement execution**

Append to `ProviderHost`'s `impl` block in `crates/hop-core/src/host.rs`:

```rust
    /// Runs every provider this routed query reaches, each as its own task,
    /// each under the budget captured for it, streaming what each one answers
    /// as its own batch.
    ///
    /// Returns the [`CancellationFlag`] shared by every provider's
    /// [`QueryCtx`], so a caller that wants to cancel cooperatively can — but
    /// the flag is not how cancellation normally arrives. Dropping `results`
    /// is: a provider's send then fails, and that failure sets this flag for
    /// every sibling still running. That makes cancellation a property of the
    /// channel, matching `hopd`'s `ResultSource` contract, rather than a second
    /// mechanism a caller has to remember.
    ///
    /// # Why one task per provider
    ///
    /// It is what makes three separate guarantees hold at once, and no shape
    /// with fewer tasks delivers all three:
    ///
    /// - **Panic containment.** A panic inside a spawned task surfaces as
    ///   [`JoinError::is_panic`](tokio::task::JoinError::is_panic) rather than
    ///   unwinding into whatever polled it. Polling providers in one task, the
    ///   only option before issue #29 made the future `'static`, means one
    ///   provider's panic takes the query — and, on the connection driver's
    ///   task, the connection.
    /// - **A cut-off that needs no cooperation.** The task is timed out and
    ///   then aborted, so a provider that never polls
    ///   [`QueryCtx::cancel`] is still abandoned at its budget.
    /// - **No slowest-provider gate** (spec §3). Each task sends as soon as its
    ///   provider answers, so a fast provider's items are on the wire while a
    ///   slow one is still running.
    ///
    /// # What abort does and does not stop
    ///
    /// [`JoinHandle::abort`](tokio::task::JoinHandle::abort) takes effect at
    /// the task's next yield point. A provider awaiting anything is dropped
    /// promptly; a provider in a loop that never yields keeps a worker thread
    /// until it does. What the host guarantees regardless is its own
    /// behaviour: it stops waiting at the budget, reports the miss, and the
    /// frame is never blocked. Bounding a non-yielding provider needs
    /// process-level isolation, which issue #29 puts explicitly out of scope
    /// and the v3 sandbox tier (spec §6) is the answer to.
    pub fn spawn_query(
        self: &Arc<Self>,
        q: Arc<RoutedQuery>,
        results: mpsc::Sender<Vec<Item>>,
    ) -> CancellationFlag {
        let cancel = CancellationFlag::default();

        for registration in self.selected(&q) {
            let host = Arc::clone(self);
            let provider = Arc::clone(&registration.provider);
            let declared = registration.declared.clone();
            let effective = registration.effective.clone();
            let q = Arc::clone(&q);
            let results = results.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                host.run_one(provider, declared, effective, q, results, cancel)
                    .await;
            });
        }

        // Every task holds its own clone of `results`; this function's copy
        // going out of scope is what lets the last task's drop close the
        // channel. A host with no selected providers therefore closes it here,
        // which is how "nothing answered" reaches the driver as a clean
        // `QueryDone` rather than a hang.
        cancel
    }

    /// One provider's whole turn: run it under its budget, classify what came
    /// back, check its items against its own manifest, and send what survived.
    async fn run_one(
        &self,
        provider: Arc<dyn ErasedProvider>,
        declared: ProviderManifest,
        effective: ProviderManifest,
        q: Arc<RoutedQuery>,
        results: mpsc::Sender<Vec<Item>>,
        cancel: CancellationFlag,
    ) {
        let id = effective.id;
        let budget = effective.budget;
        let started = Instant::now();

        let ctx = QueryCtx {
            cancel: cancel.clone(),
            deadline: started + budget,
        };

        // The handle is kept rather than moved into `timeout`, so the task can
        // still be aborted after the budget expires. `JoinHandle` is `Unpin`,
        // which is what makes `&mut handle` a future.
        let mut handle = tokio::spawn(Arc::clone(&provider).query_erased(q, ctx));
        let outcome = match tokio::time::timeout(budget, &mut handle).await {
            Err(_elapsed) => {
                handle.abort();
                let elapsed = started.elapsed();
                self.log.record(ProviderEvent::BudgetMiss {
                    provider: id,
                    budget,
                    elapsed,
                });
                Err(ProviderFailure::budget_miss(id, elapsed))
            }
            Ok(Err(join_error)) if join_error.is_panic() => {
                Err(ProviderFailure::panicked(id, started.elapsed()))
            }
            Ok(Err(_cancelled_task)) => Err(ProviderFailure::from_error(
                id,
                ProviderError::Cancelled,
                started.elapsed(),
            )),
            Ok(Ok(Err(error))) => Err(ProviderFailure::from_error(
                id,
                error,
                started.elapsed(),
            )),
            Ok(Ok(Ok(items))) => Ok(items),
        };

        let items = match outcome {
            Ok(items) => items,
            Err(failure) => {
                self.log.record(ProviderEvent::Failed(&failure));
                return;
            }
        };

        // The comparison `ProviderOutput::from_provider`'s docs ask a host to
        // make, and which only a host can: a captured manifest cannot be
        // re-minted in response to what a provider decided to return, so a
        // provider whose `manifest()` now answers differently is caught here
        // and its whole answer refused. `declared` rather than `effective` is
        // the baseline, because clamping deliberately changes fields.
        let fresh = provider.manifest();
        if fresh != declared {
            let failure = ProviderFailure::from_error(
                id,
                ProviderError::Failed(
                    "the provider's manifest changed after registration".to_string(),
                ),
                started.elapsed(),
            );
            self.log.record(ProviderEvent::Failed(&failure));
            return;
        }

        // One `ProviderOutput`, from this provider alone, so each item is
        // checked against its own producer and nothing else — the property
        // `CheckedItems::check`'s loop comment warns against hoisting away.
        let checked = CheckedItems::check(vec![provider.output(items)]);
        if !checked.rejections().is_empty() {
            self.log.record(ProviderEvent::Rejected {
                provider: id,
                rejections: checked.rejections(),
            });
        }

        let items = checked.items().to_vec();
        self.log.record(ProviderEvent::Answered {
            provider: id,
            items: items.len(),
            elapsed: started.elapsed(),
        });

        if items.is_empty() {
            return;
        }

        // A failed send means the receiver is gone, which is this seam's
        // cancellation. Setting the flag is what carries that to the siblings
        // still running: they learn it from the flag rather than waiting to
        // discover their own send failing.
        if results.send(items).await.is_err() {
            cancel.cancel();
        }
    }
```

Extend the file's imports:

```rust
use std::time::Instant;

use tokio::sync::mpsc;

use crate::pipeline::CheckedItems;
use crate::provider::CancellationFlag;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p hop-core host::`
Expected: PASS. `a_panicking_provider_yields_a_panic_shaped_failure_naming_it` prints the panic message to stderr via tokio's default hook — that is expected output, not a failure.

- [ ] **Step 5: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/hop-core/src/host.rs
git commit -m "hop-core: run each provider under an enforced budget in its own isolated task"
```

---

### Task 6: Wire the host into the daemon

**Files:**
- Modify: `crates/hopd/Cargo.toml`
- Modify: `crates/hopd/src/source.rs`
- Modify: `crates/hopd/src/server.rs`
- Modify: `crates/hopd/src/lib.rs`

**Interfaces:**
- Consumes: `ProviderHost::{with_log, register, spawn_query}`, `ProviderLog`, `ProviderEvent`.
- Produces, for Task 7: `hopd::source::{HostSource, SkeletonProvider, StderrLog}`, and `server::serve_with` continuing to accept any `ResultSource` so tests can pass a host of their own.

- [ ] **Step 1: Write the failing test**

Add to `crates/hopd/src/source.rs`'s `mod tests`:

```rust
    use hop_core::host::{NoopLog, ProviderHost};
    use std::sync::Arc;

    #[tokio::test]
    async fn the_skeleton_provider_answers_through_the_host() {
        // The walking skeleton's item, reached the way every later provider
        // will be: registered with the host, selected by its captured
        // manifest, and streamed.
        let mut host = ProviderHost::with_log(Arc::new(NoopLog));
        host.register(SkeletonProvider).unwrap();
        let source = HostSource::new(Arc::new(host));

        let mut rx = source.start(QueryText::new("anything").unwrap());
        let batch = rx.recv().await.expect("one batch must arrive");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].title, "Hello from hopd");
        assert!(
            rx.recv().await.is_none(),
            "the channel closes once the one provider has finished"
        );
    }

    #[tokio::test]
    async fn the_skeleton_providers_own_item_passes_its_own_manifest() {
        // If it did not, the host would refuse it and the walking skeleton
        // would silently answer nothing — so this is what keeps the in-tree
        // worked example honest, the same guarantee `hop-core`'s
        // `a_providers_own_output_passes_its_own_manifests_checks` gives.
        let manifest = SkeletonProvider.manifest();
        let item = hardcoded_item();
        assert_eq!(item.provider, manifest.id);
        assert!(manifest.kinds.contains(&item.kind));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hopd source::`
Expected: FAIL to compile — `hop_core` is not a dependency, and `SkeletonProvider`, `HostSource` do not exist.

- [ ] **Step 3: Add the dependency**

In `crates/hopd/Cargo.toml`, under `[dependencies]`, keeping the existing comment about `version` alongside `path`:

```toml
hop-core = { path = "../hop-core", version = "0.1.0" }
```

- [ ] **Step 4: Implement the source**

Rewrite `crates/hopd/src/source.rs`'s module docs and replace `SkeletonSource`:

```rust
//! The seam between a connection and whatever answers its queries.
//!
//! A [`ResultSource`] answers one query with a stream of item batches behind
//! an `mpsc::Receiver`. The channel is the whole contract: batches arrive on
//! it, the source finishing closes it, and the *caller dropping it is
//! cancellation* — a source notices its next `send` fail and stops working.
//! That makes cancellation a property of the seam rather than a protocol
//! bolted onto it, and it is what issue #55's "a new query cancels the old
//! one server-side" hangs off.
//!
//! The one production source is [`HostSource`], over `hop-core`'s
//! [`ProviderHost`]: it routes the query text and hands the routed query to
//! the host, which runs every provider the query reaches under an enforced
//! budget. [`SkeletonProvider`] is the one provider registered today — the
//! walking skeleton's hardcoded item, re-expressed as a real provider so the
//! daemon's production path runs through the host from issue #56 onward rather
//! than waiting for #57's apps provider to make it real.
```

Replace the `SkeletonSource` struct and impl with:

```rust
/// The walking skeleton's item, as a real [`Provider`].
///
/// # Why this exists rather than a source that returns the item directly
///
/// It is what makes the provider host the daemon's *production* path in this
/// slice instead of a component only tests reach. Issue #32's criterion is that
/// the enforcement predicate has a caller outside tests, and a host nothing
/// registers with does not have one. Keeping the old direct source alongside
/// would also have meant `hop query` returning results that never passed a
/// manifest check, which is precisely the arrangement `hop-core`'s
/// [`CheckedItems`](hop_core::pipeline::CheckedItems) exists to make
/// unreachable.
///
/// It is also the tree's worked example of a well-behaved provider for issue
/// #57 to copy, so its manifest and the item it returns must agree — the item's
/// `provider` string equals this manifest's `id` and its kind is one this
/// manifest declares. Pinned by
/// `tests::the_skeleton_providers_own_item_passes_its_own_manifest`.
///
/// `budget` is 1 ms because the work is constructing one struct. A provider
/// asking for more than [`MAX_PROVIDER_BUDGET`](hop_core::host::MAX_PROVIDER_BUDGET)
/// would be clamped to it, and one asking for less is taken at its word.
#[derive(Clone)]
pub struct SkeletonProvider;

impl Provider for SkeletonProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: "skeleton",
            kinds: vec![Kind::Action],
            // `Mode::All` because this provider answers ordinary, unprefixed
            // search — a provider that omits it is never asked to run for a
            // query that did not route to one of its other modes.
            modes: vec![Mode::All],
            min_term_len: 0,
            budget: Duration::from_millis(1),
        }
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(vec![hardcoded_item()])
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        // Action dispatch is issue #59's slice; until then this provider
        // produces items nothing can act on, and says so rather than
        // pretending to have done something.
        Err(ProviderError::Failed(
            "action dispatch is not implemented yet".to_string(),
        ))
    }
}

/// The production [`ResultSource`]: a routed query handed to a
/// [`ProviderHost`].
///
/// `Clone` is cheap — the host sits behind an `Arc`, so every connection's
/// handle shares one registry rather than one per connection.
#[derive(Clone)]
pub struct HostSource {
    host: Arc<ProviderHost>,
}

impl HostSource {
    /// A source over `host`. The host is already built and its providers
    /// already registered: registration happens once at startup, which is what
    /// makes a captured manifest a startup-time fact rather than a per-query
    /// one.
    pub fn new(host: Arc<ProviderHost>) -> Self {
        HostSource { host }
    }
}

impl ResultSource for HostSource {
    fn start(&self, text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        // Capacity 1 for the reason this trait's docs give: what a source
        // buffers is daemon memory the retained-set cap does not see, so a
        // deeper channel would only let providers park items the cap never
        // counts.
        let (tx, rx) = mpsc::channel(1);
        // Routing happens here rather than inside the host because the host's
        // vocabulary is a `RoutedQuery` — the same value every provider sees,
        // shared rather than cloned per provider.
        let routed = Arc::new(route(text.as_str()));
        self.host.spawn_query(routed, tx);
        rx
    }
}

/// The daemon's [`ProviderLog`]: one line per event on stderr.
///
/// Deliberately the crudest thing that satisfies issue #34's criterion, and
/// consistent with how this crate already reports — [`crate::server::serve`]
/// logs accept and connection errors with `eprintln!` too. Spec §9's
/// `tracing` with an env-filter is the eventual backend, and the
/// [`ProviderLog`] seam is what lets it arrive without touching a call site.
pub struct StderrLog;

impl ProviderLog for StderrLog {
    fn record(&self, event: ProviderEvent<'_>) {
        match event {
            ProviderEvent::Answered { .. } => {}
            ProviderEvent::Failed(failure) => eprintln!(
                "hopd: provider {} failed ({:?}) after {:?}: {}",
                failure.provider, failure.kind, failure.elapsed, failure.message
            ),
            ProviderEvent::BudgetMiss {
                provider,
                budget,
                elapsed,
            } => eprintln!(
                "hopd: provider {provider} missed its {budget:?} budget after {elapsed:?}"
            ),
            ProviderEvent::Rejected {
                provider,
                rejections,
            } => eprintln!(
                "hopd: provider {provider} had {} item(s) refused by its own manifest",
                rejections.len()
            ),
            // Skipped is the common case by design — most keystrokes reach
            // most providers not at all — so logging it per keystroke would
            // bury everything above it.
            ProviderEvent::Skipped { .. } => {}
        }
    }
}
```

Replace the imports at the top of `source.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use hop_core::host::{ProviderEvent, ProviderHost, ProviderLog};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery, route};
use hop_protocol::{
    Action, ActionId, ActionKind, ExecOutcome, Item, ItemId, Kind, QueryText,
};
use tokio::sync::mpsc;
```

Delete the old `the_skeleton_source_yields_one_batch_then_finishes` test — the new
`the_skeleton_provider_answers_through_the_host` is the same behavior through the
real path. Keep `hardcoded_item` exactly as it is, and widen its visibility to
`pub(crate)` if it is not already.

- [ ] **Step 5: Wire it in `server.rs`**

Find where `serve` calls `serve_with(..., SkeletonSource)` and replace the source it builds. Add above it:

```rust
/// Builds the daemon's provider host: the registry every query runs through.
///
/// Registration failures are a programming error rather than an operating
/// condition — the only ids registered here are literals in this function, so
/// a duplicate means two lines in this file chose the same one. It is reported
/// and the provider skipped rather than panicking, because a daemon that
/// refuses to start over one misconfigured provider is worse than one that
/// serves the rest: spec §9's per-provider isolation rule applied to startup.
fn build_host() -> ProviderHost {
    let mut host = ProviderHost::with_log(Arc::new(StderrLog));
    if let Err(err) = host.register(SkeletonProvider) {
        eprintln!("hopd: could not register the skeleton provider: {err}");
    }
    host
}
```

and change the call to pass `HostSource::new(Arc::new(build_host()))`. Import
`hop_core::host::ProviderHost`, `std::sync::Arc`, and
`crate::source::{HostSource, SkeletonProvider, StderrLog}`.

- [ ] **Step 6: Update `lib.rs`'s module docs**

In `crates/hopd/src/lib.rs`, replace the "What it is not yet" paragraph:

```rust
//! What it is not yet: a daemon with real providers — the query router and the
//! provider host are wired ([`source`]), but the only provider registered is
//! the walking skeleton's, until issue #57 lands apps and #58 the calculator —
//! a result *assembly* step (ranking, boosts and the pinned tail are
//! `hop-core`'s [`pipeline`](hop_core::pipeline), still uncalled here), or
//! anything with a lifecycle beyond "runs until killed". Each of those gaps is
//! named where it applies, in [`runtime_dir`], [`server`] and [`source`].
```

- [ ] **Step 7: Run the tests**

```bash
cargo test -p hopd
cargo test -p hop-cli
```
Expected: PASS. `hop-cli`'s e2e tests assert on "Hello from hopd" and must still pass — the item is unchanged, only the path to it is.

- [ ] **Step 8: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: green. `cargo deny check` matters here: this task adds a workspace path dependency, and it carries `version` alongside `path` for the wildcard-ban reason the existing manifests document.

- [ ] **Step 9: Commit**

```bash
git add crates/hopd/Cargo.toml crates/hopd/src/source.rs crates/hopd/src/server.rs crates/hopd/src/lib.rs Cargo.lock
git commit -m "hopd: serve queries through the provider host"
```

---

### Task 7: The scripted fake-provider fixture and integration tests over a real socket

**Files:**
- Modify: `crates/hopd/tests/common/mod.rs`
- Create: `crates/hopd/tests/host.rs`

**Interfaces:**
- Consumes: `hopd::server::serve_with`, `hopd::source::HostSource`, `hop_core::host::{ProviderHost, HostPolicy, ProviderLog, ProviderEvent}`, and whatever `tests/common/mod.rs` already exposes for driving a socket (a client that sends `Hello`, `Query`, and reads frames).
- Produces: `common::{ScriptedProvider, Script, RecordingLog}` — the fixture spec §11 asks for ("hopd runs against scripted fake providers ... so integration tests are deterministic"), reusable by #57, #58 and #61.

- [ ] **Step 1: Read what `tests/common/mod.rs` already provides**

Run: `sed -n '1,200p' crates/hopd/tests/common/mod.rs`

Note the exact names of the helpers that bind a socket, connect, send a framed
`ClientMsg` and read a `DaemonMsg`, and reuse them — do not add a second client.
`crates/hopd/tests/lifecycle.rs` is the worked example of driving them.

- [ ] **Step 2: Write the fixture**

Append to `crates/hopd/tests/common/mod.rs`:

```rust
/// What a scripted provider does when it is asked to run — the fixture spec
/// §11 asks for, so an integration test's outcome is a property of the script
/// rather than of timing.
///
/// It lives here rather than in `hop-core` because only `hopd`'s integration
/// tests need it: exporting it from the library crate would mean a `testing`
/// feature or a permanently-compiled module, for a type no production caller
/// has any use for. Issues #57, #58 and #61 reuse it from here.
#[derive(Clone)]
pub enum Script {
    /// Answer with these items.
    Answer(Vec<Item>),
    /// Fail with this text — used for the bounding-and-stripping tests, so
    /// pass whatever hostile string is under test.
    Fail(String),
    /// Panic, to prove the host contains it.
    Panic,
    /// Never return, to prove the host cuts it off without cooperation. Yields
    /// while it waits, so `abort` can take effect and no worker thread is
    /// pinned for the test run.
    Hang,
}

/// A provider that does exactly what its [`Script`] says, and declares exactly
/// the manifest it was built with.
pub struct ScriptedProvider {
    manifest: ProviderManifest,
    script: Script,
}

impl ScriptedProvider {
    /// A provider answering to `id`, declaring `kinds`, serving `Mode::All`
    /// with no minimum term length, and running `script`.
    ///
    /// `budget` is 20 ms: comfortably above what an `Answer` or a `Fail` needs,
    /// and comfortably below the wait an integration test would notice, so a
    /// `Hang` resolves fast.
    pub fn new(id: &'static str, kinds: Vec<Kind>, script: Script) -> Self {
        ScriptedProvider {
            manifest: ProviderManifest {
                id,
                kinds,
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(20),
            },
            script,
        }
    }

    /// The same provider with a manifest field overridden — for the tests that
    /// need a specific budget, mode set or minimum.
    pub fn with_manifest(mut self, manifest: ProviderManifest) -> Self {
        self.manifest = manifest;
        self
    }
}

impl Provider for ScriptedProvider {
    fn manifest(&self) -> ProviderManifest {
        self.manifest.clone()
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        match &self.script {
            Script::Answer(items) => Ok(items.clone()),
            Script::Fail(text) => Err(ProviderError::Failed(text.clone())),
            Script::Panic => panic!("scripted provider panic"),
            Script::Hang => loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            },
        }
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        Ok(ExecOutcome::Done)
    }
}

/// A [`ProviderLog`] the tests can read back, so "a record was emitted" is an
/// assertion rather than an inspection of stderr.
#[derive(Default)]
pub struct RecordingLog {
    lines: Mutex<Vec<String>>,
}

impl RecordingLog {
    /// Every line recorded so far, in order.
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().expect("no test panics holding this").clone()
    }
}

impl ProviderLog for RecordingLog {
    fn record(&self, event: ProviderEvent<'_>) {
        let line = match event {
            ProviderEvent::Answered { provider, items, .. } => {
                format!("answered {provider} {items}")
            }
            ProviderEvent::Failed(failure) => format!(
                "failed {} {:?} {}",
                failure.provider, failure.kind, failure.message
            ),
            ProviderEvent::BudgetMiss { provider, .. } => format!("budget-miss {provider}"),
            ProviderEvent::Rejected { provider, rejections } => {
                format!("rejected {provider} {}", rejections.len())
            }
            ProviderEvent::Skipped { provider } => format!("skipped {provider}"),
        };
        self.lines
            .lock()
            .expect("no test panics holding this")
            .push(line);
    }
}

/// One item, well-formed and agreeing with `provider` — the fixture's honest
/// item, for tests that need results rather than failures.
pub fn scripted_item(provider: &str, kind: Kind, id: &str, title: &str) -> Item {
    Item {
        id: ItemId::new(id).expect("within bounds by construction"),
        kind,
        title: title.to_string(),
        subtitle: None,
        icon: None,
        actions: vec![],
        default_action: ActionId::new("open").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: provider.to_string(),
    }
}
```

Add the imports `tests/common/mod.rs` needs — `std::sync::{Arc, Mutex}`,
`std::time::Duration`, `hop_core::host::{ProviderEvent, ProviderLog}`,
`hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx}`,
`hop_core::router::{Mode, RoutedQuery}`, and the `hop_protocol` items above —
merged into whatever it already imports.

Add `hop-core` to `crates/hopd/Cargo.toml`'s `[dev-dependencies]` only if
Task 6's `[dependencies]` entry does not already make it available to tests (it
does — a `[dependencies]` entry is visible to integration tests, so no change is
needed; verify rather than assume).

- [ ] **Step 3: Write the integration tests**

Create `crates/hopd/tests/host.rs`:

```rust
//! The provider host over a real socket: what a client actually receives when
//! a provider hangs, panics, fails with hostile text, or answers honestly
//! alongside one that does not.
//!
//! `hop-core`'s own tests cover the host's units. These cover the daemon: the
//! frames that reach a peer, which is the only place "one failing provider
//! never empties a frame for the others" can actually be observed.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{RecordingLog, Script, ScriptedProvider, scripted_item};
use hop_core::host::{HostPolicy, ProviderHost};
use hop_protocol::{DaemonMsg, Kind};
use hopd::source::HostSource;

/// A daemon serving a host with `providers` registered, plus the log the test
/// reads back.
///
/// [FILL IN using `crates/hopd/tests/lifecycle.rs`'s existing harness: bind a
/// socket in a `tempfile::TempDir`, spawn `server::serve_with(dir, source)`,
/// and return whatever handle that file's tests use to connect and exchange
/// frames. Do not invent a second harness.]
fn daemon_with(
    providers: Vec<ScriptedProvider>,
    log: Arc<RecordingLog>,
) -> /* the harness type lifecycle.rs uses */ {
    let mut host = ProviderHost::new(HostPolicy::default(), log);
    for provider in providers {
        host.register(provider).unwrap();
    }
    let source = HostSource::new(Arc::new(host));
    // ... spawn `serve_with` with `source`, exactly as lifecycle.rs does
}

#[tokio::test]
async fn a_panicking_provider_does_not_empty_the_frame_for_the_others() {
    // Spec §9's per-provider isolation rule, observed where it matters: at the
    // client. Before issue #29 this panic would have unwound through the
    // connection driver and taken the connection with it.
    let log = Arc::new(RecordingLog::default());
    let mut client = daemon_with(
        vec![
            ScriptedProvider::new("panicking", vec![Kind::App], Script::Panic),
            ScriptedProvider::new(
                "apps",
                vec![Kind::App],
                Script::Answer(vec![scripted_item(
                    "apps",
                    Kind::App,
                    "app:firefox",
                    "Firefox",
                )]),
            ),
        ],
        log.clone(),
    );

    client.handshake().await;
    client.query(1, "firefox").await;

    let items = client.collect_results_until_done(1).await;
    assert_eq!(items.len(), 1, "the honest provider's item still arrives");
    assert_eq!(items[0].title, "Firefox");
    assert!(
        log.lines().iter().any(|l| l.starts_with("failed panicking")),
        "and the panic is reported: {:?}",
        log.lines()
    );
}

#[tokio::test]
async fn a_hanging_provider_is_cut_off_and_the_query_still_terminates() {
    // #28's criterion at the socket: the exchange reaches `QueryDone` without
    // the provider ever cooperating, and without the client waiting on it.
    let log = Arc::new(RecordingLog::default());
    let mut client = daemon_with(
        vec![ScriptedProvider::new(
            "hanging",
            vec![Kind::App],
            Script::Hang,
        )],
        log.clone(),
    );

    client.handshake().await;
    client.query(7, "anything").await;

    let done = tokio::time::timeout(Duration::from_secs(2), client.next_frame())
        .await
        .expect("the exchange must terminate on the host's budget, not the test's patience")
        .unwrap();
    assert!(matches!(done, DaemonMsg::QueryDone { query_id: 7 }));
    assert!(log.lines().iter().any(|l| l == "budget-miss hanging"));
}

#[tokio::test]
async fn a_providers_hostile_error_text_never_reaches_the_client() {
    // #34 at the boundary it is about: the text is bound for a UI label, so
    // what matters is that no frame carries it. This slice reports provider
    // failures on the log seam and sends no error frame for one, so the
    // assertion is that the exchange completes carrying no provider text at
    // all — and that what the seam recorded is bounded and stripped.
    let log = Arc::new(RecordingLog::default());
    let hostile = format!("\u{1b}[31m\u{202e}{}", "x".repeat(4096));
    let mut client = daemon_with(
        vec![ScriptedProvider::new(
            "nasty",
            vec![Kind::App],
            Script::Fail(hostile),
        )],
        log.clone(),
    );

    client.handshake().await;
    client.query(2, "anything").await;

    let done = client.next_frame().await.unwrap();
    assert!(
        matches!(done, DaemonMsg::QueryDone { query_id: 2 }),
        "a provider failure ends the exchange cleanly; it does not send the \
         provider's words to the client"
    );

    let lines = log.lines();
    let failure = lines
        .iter()
        .find(|l| l.starts_with("failed nasty"))
        .expect("the failure was recorded");
    assert!(!failure.contains('\u{1b}'));
    assert!(!failure.contains('\u{202e}'));
    assert!(
        failure.len() < 512,
        "the 4 KB message was bounded before it was recorded: {}",
        failure.len()
    );
}

#[tokio::test]
async fn a_fast_providers_items_arrive_before_a_slow_providers_budget_expires() {
    // "No slowest-provider gate", spec §3, observed as frame timing.
    let log = Arc::new(RecordingLog::default());
    let mut client = daemon_with(
        vec![
            ScriptedProvider::new("hanging", vec![Kind::App], Script::Hang),
            ScriptedProvider::new(
                "apps",
                vec![Kind::App],
                Script::Answer(vec![scripted_item(
                    "apps",
                    Kind::App,
                    "app:firefox",
                    "Firefox",
                )]),
            ),
        ],
        log,
    );

    client.handshake().await;
    client.query(3, "firefox").await;

    let frame = tokio::time::timeout(Duration::from_millis(15), client.next_frame())
        .await
        .expect("the fast provider's frame must not wait on the hanging one")
        .unwrap();
    match frame {
        DaemonMsg::Results { items, partial, .. } => {
            assert!(partial);
            assert_eq!(items[0].title, "Firefox");
        }
        other => panic!("expected a results frame first, got {other:?}"),
    }
}
```

Replace every `client.*` call and the `daemon_with` return type with the real
names from `crates/hopd/tests/lifecycle.rs`. The helper names above are
placeholders for whatever that file already calls them, and reusing them is the
point — a second harness in this file would be the duplication `tests/common`
exists to prevent.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hopd --test host`
Expected: PASS, all four. Panic output on stderr from the panicking provider is expected.

- [ ] **Step 5: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/hopd/tests/common/mod.rs crates/hopd/tests/host.rs
git commit -m "hopd: scripted provider fixture and host integration tests over a real socket"
```

---

### Task 8: Glossary, documentation alignment, and the follow-up issue

**Files:**
- Modify: `CONTEXT.md`
- Modify: `crates/hop-core/src/pipeline.rs` (two doc comments that this slice makes stale)
- Modify: `crates/hop-core/src/provider.rs` (one doc comment likewise)

**Interfaces:** none — documentation only.

- [ ] **Step 1: Extend `CONTEXT.md`'s glossary**

`docs/agents/domain.md` requires it, and `/review`'s Standards axis has pulled a prior slice up on exactly this omission. Add a `## Provider host` section after `## Result assembly`, defining the vocabulary this slice resolved. Use the file's existing style — a bolded term, then what it means, then what it is deliberately *not*:

- **Provider host** — what owns registered providers and runs their queries. Not a scheduler in the ranking sense: it decides *whether* and *for how long* a provider runs, never in what order its items appear.
- **Registration** — the one moment a provider's manifest is read. What is captured then is what every later decision consults.
- **Captured manifest** — the copy taken at registration. **Effective manifest** — that copy with host policy applied. Both are kept: the captured one is the baseline a shifted manifest is caught against, and clamping deliberately makes the effective one differ.
- **Clamp** — the host lowering a provider's budget to its ceiling, or raising a minimum term length to its floor. One direction each, and neither is negotiable by a provider.
- **Budget** — the host's deadline for one provider on one query. Distinguish **budget miss** (the host cut the provider off — enforcement) from a provider's own **timeout** (it gave up first — cooperation). Note that `CONTEXT.md` already defines **Bound**; say plainly how a budget differs (a bound is on a value, a budget is on time) rather than letting the two blur.
- **Panic isolation** — a provider's panic surfacing as a failure that names it instead of unwinding into the daemon.
- **Sanitize** — rewriting provider text so it is safe to render: control and direction-override characters removed, then truncated. Contrast with a **content rule**, which *refuses* a value; sanitizing is lossy and never refuses, because the value here is a diagnostic about a failure that already happened.
- **Log seam** — where the host reports what providers did. The thing `Rejection`'s docs promised would make ignoring a rejection a real mistake.

Cross-reference the existing **Truncate** and **Refusal** entries so the new terms sit inside the vocabulary rather than beside it; if either existing definition now reads as contradicting sanitizing, say so in place and reconcile — that is the same failure the prior slice's "truncate-and-terminate" reconciliation fixed.

- [ ] **Step 2: Retire the doc comments this slice made stale**

`grep -rn "issue #56\|#56\|no logging seam\|has no registry and no scheduler\|zero callers" crates/ CONTEXT.md` and fix each hit that is now false. At minimum:

- `pipeline.rs`'s `CheckedItems` docs: "Until there is a logging seam (issue #34) that makes ignoring them a real mistake" — the seam exists now, and the host records rejections through it. Rewrite to say the host does read them, and that `Assembly::rejections` remains ignorable by *other* callers.
- `pipeline.rs`'s `ProviderOutput::from_provider` docs: "`hop-core` has no registry and no scheduler, so there is no earlier, trusted manifest in this crate to compare against" — it has both now, and `ProviderHost::run_one` makes exactly the comparison that paragraph asks a host to make. Rewrite to point at it, keeping the warning that a caller-supplied manifest is still the hole this constructor must not open.
- `pipeline.rs`'s `Rejection` docs: "this codebase has no logging seam yet" — same fix.
- `provider.rs`'s `should_query` and `APPS_PROVIDER_ID` docs: `should_query` now has a caller, and the "whatever builds the M2 provider registry needs to enforce it" sentence about duplicate ids is now satisfied by `RegistrationError::DuplicateId` — point at it.
- `source.rs`'s `ResultSource` docs: "issue #56's provider host is the first implementation that can break them" — it is no longer future tense, so state which obligations `HostSource` honours and how (capacity 1; per-provider items are unbounded in count until #30 lands its cap, which is worth naming rather than leaving implied).

- [ ] **Step 3: File the follow-up issue for the assembly gap**

The scope section of this plan says wiring `Pipeline::assemble` is out of scope and needs a protocol answer. File that as an issue so it is tracked rather than assumed:

```bash
gh issue create -R pedrosousa13/hop \
  --title "Daemon: provider items reach the client unranked — assembly is never called" \
  --label needs-triage --milestone 2 \
  --body "$(cat <<'BODY'
> *This was generated by AI during implementation.*

Issue #56 landed the provider host, which streams each provider's
manifest-checked items as its own batch in the order providers answer.
`hop_core::pipeline::Pipeline::assemble` — routing's aliases, learning and
alias boosts, the exclusive-mode filter, ranking, inferred-mode promotion, the
pin budget and `max_results` — is still never called by the daemon.

No M2 slice currently owns wiring it, and #56's brief and acceptance criteria
mention none of it. #30 ("provider output enters ranking with no result-count
cap") presupposes ranking is wired, so this gap sits underneath it.

The reason it was not folded into #56 is that it needs a protocol answer first.
`assemble` is a whole-list pure function taking `max_results`, while the wire
streams append-only `Results { partial: true }` batches that a client appends
(#55). So "rank the streamed set" means one of:

- re-assemble on every provider's arrival and re-send the whole list, which
  needs a frame that *replaces* rather than appends; or
- gate on every provider before assembling once, which spec §3 forbids
  outright ("No slowest-provider gate"); or
- assemble per batch, which ranks each provider's items against each other
  only and gives the pin budget and `max_results` no coherent meaning.

Deciding between them is a protocol decision touching the frontend, which is
why it is filed rather than guessed at.
BODY
)"
```

- [ ] **Step 4: Run the full gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: all four green.

- [ ] **Step 5: Commit**

```bash
git add CONTEXT.md crates/
git commit -m "docs: provider-host vocabulary, and retire the comments this slice made stale"
```

---

## Acceptance criteria coverage (from issue #56)

| Criterion | Where |
| --- | --- |
| Providers register with the host, manifests read once at registration | Task 4 — `register_arc`, `a_registered_providers_manifest_is_read_exactly_once` |
| A manifest that changes its answers afterwards does not change scheduling (#32) | Task 4 — `scheduling_reads_the_captured_manifest_so_a_shifty_provider_changes_nothing`; Task 5 — `a_provider_whose_manifest_shifted_after_registration_has_its_answer_refused` |
| Host clamps a budget over its ceiling; can raise the minimum term length (#32) | Task 4 — `a_manifest_budget_over_the_ceiling_is_clamped`, `the_host_can_raise_a_minimum_term_length_above_what_a_provider_declared`, `the_floor_never_lowers_a_providers_own_minimum` |
| A never-completing provider is cut off at its budget and reported as a timeout, without cooperating (#28) | Task 5 — `a_provider_that_never_completes_is_cut_off_at_its_budget_without_cooperating`; Task 7 — `a_hanging_provider_is_cut_off_and_the_query_still_terminates` |
| A panicking provider yields a panic-shaped error naming it; other providers' results still reach the client (#29) | Task 5 — `a_panicking_provider_yields_a_panic_shaped_failure_naming_it`, `one_providers_panic_does_not_cost_another_provider_its_results`; Task 7 — `a_panicking_provider_does_not_empty_the_frame_for_the_others` |
| Provider error text truncated to a documented maximum and stripped of control and direction-override characters before it can leave the daemon (#34) | Task 1 (all tests); Task 3 — `provider_error_text_is_sanitized_when_the_failure_is_built`; Task 5 — `provider_error_text_is_bounded_and_stripped_before_it_leaves`; Task 7 — `a_providers_hostile_error_text_never_reaches_the_client` |
| Provider failures and budget misses emit records through a logging seam (#34) | Task 3 (the seam); Task 5 — the budget-miss and panic tests assert both events; Task 6 — `StderrLog` is the daemon's backend |
| A scripted fake-provider fixture exists and is used by the integration tests | Task 7 — `common::{Script, ScriptedProvider}`, used by all four tests in `tests/host.rs` |
| Enforcement point exported so a scheduler uses it rather than reimplementing it (#28) | Task 5 — `ProviderHost::spawn_query` is `pub`; Task 6 — `hopd` calls it rather than reimplementing |
| The pre-filter predicate has a caller outside tests (#32) | Task 4 — `ProviderHost::selected` calls `should_query`; Task 6 — `SkeletonProvider` makes that caller a production one |

## Self-review notes

- **Spec coverage.** §3's latency contract shapes `MAX_PROVIDER_BUDGET`'s documented reasoning and the "no slowest-provider gate" tests; §5's trait is the one changed in Task 2; §6 rule 2's declarative pre-filter is Task 4; §6 rule 3's per-plugin deadlines are Task 5; §9's per-provider isolation is Tasks 5–7 and its `tracing` backend is deferred behind Task 3's seam with that deferral written down; §11's scripted fake providers are Task 7. §13's `rt-multi-thread` requirement is already satisfied by `hopd::run` and needs no change — verified before planning.
- **Deliberate omission.** `Pipeline::assemble` wiring, with the reasoning in the Scope section and a follow-up issue filed in Task 8 Step 3 rather than left implicit.
- **Type consistency.** `ProviderFailure::{from_error, panicked, budget_miss}`, `ProviderEvent`'s five variants, `HostPolicy::{max_budget, min_term_len_floor}`, `ProviderHost::{new, with_log, register, register_arc, manifests, len, is_empty, spawn_query}` and `Registration::{declared, effective, provider}` are used under exactly these names in Tasks 3–7. `RecordingLog` is defined twice on purpose — once in `hop-core`'s unit tests and once in `hopd`'s `tests/common` — because a `#[cfg(test)]` type in a library crate is not reachable from another crate's integration tests.
- **Known placeholder.** Task 7's `daemon_with` and its `client.*` calls are marked FILL-IN against `crates/hopd/tests/lifecycle.rs`'s existing harness, whose helper names this plan does not restate. That is deliberate — inventing names for helpers that already exist would produce a second harness — and Task 7 Step 1 is the step that reads them.
