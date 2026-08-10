# Issue #72 — a provider dimension for the learning store

Closes the half of issue #31's boost-theft criterion that #31 deliberately left
open, and lands the manifest opt-in half of threat model Decision 2 that #39
deferred here.

## The vulnerability

`Learning` keys its boosts on the bare item id. A provider that declares itself
honestly — manifest `id: "evil"`, `kinds: [App]` — and returns
`Item { id: ItemId("app:firefox"), kind: Kind::App, provider: "evil" }` passes
both of #31's manifest checks and still collects every learning boost the
genuine Firefox earned on `app:firefox`. A boost decides which item sorts first,
and the item that sorts first is what Enter dispatches.

Two `// DECISION:` comments record the gap and must be removed when it closes:
`crates/hop-core/src/pipeline.rs:912-925` and `crates/hop-core/src/rank.rs:258-265`.

## Decisions this slice inherits

- **Key shape (session decision).** The provider is folded into the key string;
  `global_frequency` stays a `HashMap<String, LearningEntry>`. A nested map or a
  tuple key would change the stored shape, and `learning.rs`'s version probe
  refuses a mismatch outright rather than migrating, so either would cost every
  user their whole store. This mirrors what `Boosts::by_provider_item`
  (`rank.rs:274-278`) already does semantically for alias boosts, without paying
  JSON's price for a tuple key.
- **Legacy entries (maintainer, option A).** Plaintext `app:` entries are
  re-attributed to the apps provider. Everything else is dropped. Legacy hashed
  keys cannot match once the provider joins the hash input, so they are dead
  weight under every option.
- **Manifest opt-in (maintainer).** It lands here, as a **required field with no
  default**, so a manifest that omits it does not compile. There are three
  production manifests to update.
- **Composition (session decision).** The manifest flag is the authority for
  plaintext persistence: a provider that opts in persists its ids in the clear,
  one that does not is hashed. This answers the question Decision 2 left open —
  "whether a built-in provider is covered by a shape, by the manifest flag, or
  by both" — with: by the flag. The shape rule guessed at what a provider knows
  for itself, and the flag is that knowledge stated directly.

## Global Constraints

- **No provider can collect another's learned boosts.** This is the issue's
  whole point. It must hold for both halves of `boost_for` — `frequency_boost`
  from the persisted map and `query_boost` from the in-memory `selections` map.
  #31's criterion is currently half-met; it must be fully met when this lands.
- **The stored shape does not change.** `global_frequency` stays a
  `HashMap<String, LearningEntry>` and `STORE_VERSION` stays 1. If either has to
  change, stop and report rather than bumping the version — a bump discards
  every user's store.
- **Learning survives a restart**, exactly as #39 required: the key a launch is
  recorded under is the key a later lookup computes, across save and load.
- **The partition #39 established still holds.** A persisted key is either
  plaintext or a hash, and no id a provider can mint may land in the wrong half.
  The provider dimension must not create a way to forge one shape into the
  other — consider what happens if a provider id contains the separator
  character.
- `unsafe_code = "deny"`; `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo deny check` and `cargo test --workspace`
  must all pass.
- No AI attribution in commits.

## Task 1 — thread provider identity to the store, and key on it

**Files:** `crates/hop-core/src/learning.rs`, `crates/hop-core/src/pipeline.rs`,
`crates/hopd/src/source.rs`, `crates/hopd/src/connection.rs`, and the test
doubles that implement `ResultSource`.

**Signatures.** Add the provider to the write and read paths. Both call sites
already hold it, so nothing needs to be plumbed further up than the signature:

- `ResultSource::record_launch` (`source.rs:182`) takes the provider. Note
  `ResultSource::execute` two lines above already takes `provider: &str` —
  match its parameter order and naming rather than inventing a new convention.
  Update `HostSource` and every test double (`connection.rs`'s
  `ScriptedSource`, `hopd/tests/exec.rs`, three no-ops in
  `hopd/tests/lifecycle.rs`).
- `connection.rs:433` passes the `provider` already bound at line 424.
- `Learning::record_launch`, `Learning::record`, `Learning::boost_for`,
  `Learning::query_boost` and `Learning::frequency_boost` all take the provider.
- `pipeline.rs:927` passes `item.provider`. It is already in scope — the alias
  loop above it uses it.

**The key.** `persistence_key` becomes a function of the provider *and* the raw
id. Compose them so that no provider id and no item id can collide across the
boundary between them: the provider id is bounded at
`MAX_PROVIDER_ID` (64 bytes, `hop-protocol/src/limits.rs:136`) but its contents
are otherwise unconstrained, so a separator a provider can put in its own id is
not safe. Choose a composition that cannot be forged, and prove it in the doc
comment — length-prefixing is one way; there are others. Do not assume a
character is unusable just because it is unusual.

`selections` — the in-memory per-query map — gets the same treatment. It is not
persisted, but `query_boost` reads it and the vulnerability applies there too.

**Legacy entries, per option A.** On load, an entry whose key is a plaintext
`app:` shape is re-attributed to the apps provider id
(`APPS_PROVIDER_ID`, `provider.rs:46`). Every other legacy entry is dropped.
Extend #39's existing `rekeyed_global_frequency` rather than adding a second
migration pass; its doc comment already explains the load-time re-key contract
and must be updated to describe what this adds.

**Tests.** At minimum:

- The issue's own scenario: a provider `"evil"` presenting `app:firefox` gets
  **no** boost from launches the apps provider earned on `app:firefox`. Assert
  on `boost_for`, and assert the genuine provider still gets its boost.
- The same for `query_boost`'s in-memory path, not only `frequency_boost`.
- A provider id containing whatever separator the composition uses cannot forge
  another provider's key.
- Record → save → load → lookup still finds the entry, for an opted-in provider
  and a hashed one.
- A legacy plaintext `app:` entry is re-attributed to the apps provider and
  keeps its count; a legacy `calc:` or hashed entry is dropped.
- The existing boost-behavior tests keep passing, updated for the new
  signatures.

## Task 2 — the manifest opt-in

**Files:** `crates/hop-core/src/provider.rs`, the three production manifests
(`hopd/src/apps.rs:2444`, `hopd/src/calculator.rs:225`,
`hopd/src/source.rs:212`), `crates/hop-core/src/learning.rs`, and the test
fixtures that construct a `ProviderManifest`.

Add the field to `ProviderManifest` (`provider.rs:84-121`). **No default and no
`Default` derive** — a manifest that omits it must not compile. Name it so a
provider author reads it as a claim about their ids rather than a feature
toggle, and document on the field what setting it wrongly costs the user:
`CheckedItems::check` holds an item to its producer's manifest, but nothing
validates the claim that an id is safe to persist in the clear.

Built-ins:

- **apps opts in.** Its ids are `app:<desktop-entry-id>` — enumerable, not
  user-authored.
- **calculator does not.** Its ids are `calc:{term}`, minted from raw routed
  query text (`calculator.rs:178`). This is the case #39 was filed about.
- **skeleton** (`source.rs:212`): decide from what it actually produces and say
  why in the diff.

**Honoring it.** The flag decides plaintext versus hash, per the composition
decision above. Work out how the flag reaches the key computation — the store
does not hold manifests today, and how it learns the answer is part of this
task. Whatever the route, a provider that never registered must not be able to
get plaintext persistence by default.

Update the `Consequence — providers opt in to plaintext persistence via their
manifest` section's status in the threat model, and the two `// DECISION:`
comments named at the top of this plan — the gap they describe is closed here.

## Task 3 — documentation

**Files:** `CONTEXT.md`, `docs/security/2026-08-02-m2-socket-boundary-threat-model.md`.

`CONTEXT.md` carries the **Persistence key** entry #39 added. Update it for the
provider dimension and the manifest opt-in, keeping the register terse and
definitional. State what the flag means and what it does not promise.

Amend the threat model per its own convention (`Status:` line carries the date,
an `**Amendment, <date>.**` block names what changed and why, each changed
passage marked in place, falsified claims annotated rather than rewritten).
Decision 2's manifest half is now implemented; its "What the implementing slice
must still settle" bullets on the manifest field and on which shapes count as
known-safe are answered; the salt and empty-query-view bullets remain open.
