# Issue #39 — hash unknown ids before persistence

Implements threat model Decision 2
(`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "Decision 2 —
unknown ids are hashed before persistence") for the half that does not need
provider identity on the learning write path.

## Scope, as decided by the maintainer (2026-08-10)

Decision 2 has two halves. The **shape half** — ids matching a known-safe shape
persist as plaintext, everything else persists as a hash — lands here. The
**manifest half** — a provider declaring that its ids are safe to persist in the
clear — does **not**: honoring it requires the provider id to reach
`Learning::record_launch`, and today it does not
(`crates/hopd/src/connection.rs:424` holds `provider` and drops it at line 433;
`ResultSource::record_launch` at `crates/hopd/src/source.rs:182` takes only
`query` and `item_id`). Threading it through is
[#72](https://github.com/pedrosousa13/hop/issues/72)'s design surface, and the
threat model says the two should be designed together. The manifest opt-in rides
with #72.

## Why this is not hypothetical

`crates/hopd/src/calculator.rs:178` mints item ids as `calc:{term}`, where `term`
is the raw routed query text. `calc:` matches neither prefix
`canonicalize_result_id` allowlists, so it falls through to
`result_id.to_string()` (`crates/hop-core/src/learning.rs:720`) and every
launched calculation is written to `learning.json` verbatim for the 90-day
`PERSIST_RETENTION_MS` window. The threat model's credit line — that raw query
text never reaches disk, making the id channel the only leak — is falsified by
the calculator provider (#58, `3b53a7a`). The id channel *is* the query-text
channel today.

## Global Constraints

- **The store's key set is partitioned, and the partition must be provable.** A
  persisted key is either a known-safe plaintext shape or a hash. The two sets
  must be disjoint by construction, so that no id can be minted that lands in
  the wrong one.
- **Learning must survive a restart.** Whatever key a launch is recorded under
  must be the key a later lookup computes, across save and load. A design that
  hashes only on the way to disk silently loses every hashed provider's learning
  on reload.
- **No existing store is discarded.** `STORE_VERSION` stays at 1. Legacy
  plaintext entries are re-keyed as they load (see Task 1), which migrates them
  in place; #38's refusal of a version mismatch in both directions means a bump
  would discard the file instead of migrating it.
- **`unsafe_code = "deny"` and `cargo clippy --workspace --all-targets -- -D
  warnings`** both bind, as does `cargo fmt --all --check`.
- The new dependency must keep `cargo deny check` green on all four sub-checks.
  `sha2` 0.10 has been verified against advisories, bans, licenses and sources
  on this workspace; `blake3` fails the license allow-list.
- Tests live in the in-file `#[cfg(test)] mod tests` at the bottom of
  `learning.rs`, use `tempfile::tempdir()` for anything touching disk, and are
  named as snake_case sentences.

## Task 1 — the persistence key

**Files:** `crates/hop-core/src/learning.rs`, `crates/hop-core/Cargo.toml`,
`Cargo.lock`.

Add `sha2` 0.10 to `hop-core` (default features; it is already added to
`crates/hop-core/Cargo.toml` and `Cargo.lock` on this branch with
`--no-default-features` — switch it to default features, which brings `std`).

Introduce a single function that maps a raw item id to the key the store uses.
Replace `canonicalize_result_id`'s role with it; keep that function as the
payload-stripping step feeding it.

**The rule:**

1. Strip dynamic payloads exactly as `canonicalize_result_id` does today, for
   `utility:` and `web-search:`. This step is unchanged, and its existing tests
   must keep passing.
2. If the stripped id matches a **known-safe shape**, it persists as that
   plaintext string. The known-safe shapes are exactly:
   - `app:<rest>` — a desktop-entry id, enumerable from the system's
     application directories and not user-authored.
   - `utility:<kind>` and `web-search:<service>` — the two payload-stripped
     forms step 1 produces, which by construction carry no payload.
3. Every other id — `calc:` included, and every id a provider this code has
   never heard of mints — persists as `sha256:` followed by the lowercase hex
   SHA-256 of the **raw** id (the id as it arrived, not the stripped form).

The partition is provable from this rule: a plaintext key always begins `app:`,
`utility:` or `web-search:`, and a hashed key always begins `sha256:`, so no
minted id can collide across the two sets. An id that literally begins
`sha256:` is not a known-safe shape and is therefore itself hashed, never
written through verbatim. Pin this with a test.

**Where the key is applied — this is the load-bearing part.** Today
canonicalization runs only at save time (`canonicalized_global_frequency`,
`learning.rs:731-746`, called from `Learning::save` at `learning.rs:1147`), and
`global_frequency` holds raw ids in memory. Hashing at save time alone would
break learning across a restart: the reloaded map would be keyed by hash while
every lookup still keys by raw id, so a hashed provider's boosts would silently
stop applying. Move the key computation to the boundary where an id enters the
store instead:

- `Learning::record` (`learning.rs`, reached from `record_launch` at
  `learning.rs:1193-1195`) keys `global_frequency` by the persistence key.
- Every read path that consults `global_frequency` — `query_boost`,
  `frequency_boost`, and anything else keying on `item_id.as_str()` — computes
  the same key before looking up. Find them all; do not assume this list is
  complete.
- `selections` is in-memory only (`#[serde(default, skip_serializing)]`,
  `learning.rs:361-362`) and keeps its present keying.
- On **load**, re-key every entry through the same function, so a legacy v1
  store's plaintext `calc:` entries become hashed as they are read. Two source
  entries can collide onto one key (a raw and an already-canonical form of the
  same id): merge them by summing `count` — saturating, matching
  `deserialize_saturating_count`'s posture — and keeping the later `last_ms`.
- With load re-keying in place, `Learning::save` no longer needs its own
  canonicalization pass, since the in-memory keys are already the persisted
  ones. Verify that claim against the code rather than assuming it, and remove
  the pass only if it holds.

**Tests** (in-file, `tempfile` for disk):

- A `calc:` id with an embedded expression never appears verbatim in the
  written JSON — assert on the file's bytes, not on the in-memory map.
- Record → save → load → the same raw id still receives its boost. This is the
  restart-survival constraint, and it must fail if the key is applied at save
  time only.
- An `app:` id round-trips as plaintext.
- The existing `utility:`/`web-search:` stripping tests keep passing.
- An id beginning `sha256:` is hashed rather than written through.
- A legacy store written with plaintext `calc:` keys, loaded, then saved, no
  longer contains the plaintext, and the entry's count survives the re-keying.
- Two legacy entries that re-key onto one key merge by summing count and taking
  the later timestamp.

## Task 2 — record the rule where it will be read

**Files:** `CONTEXT.md`,
`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`.

`CONTEXT.md` is this Project's domain document and carries the vocabulary; there
is no provider-authoring reference in `docs/` today, so this is where the rule
goes. Add the persistence-key rule to it in the register the surrounding prose
uses — what a **persistence key** is, which shapes are known-safe, that
everything else is hashed, and that the hash is not confidentiality against
someone holding the store, only against accidental disclosure. Say plainly that
the manifest opt-in is not implemented and rides with #72, so nobody reads the
section as describing a field that exists.

Amend the threat model per its own stated convention (the `Status:` line carries
amendment dates, an `**Amendment, <date>.**` block names what changed and why,
and each changed passage is marked `**[Amended <date>]**` in place). Three
things changed:

- Decision 2's shape half is implemented; the manifest half is not, and #72
  owns it.
- The document's claim that raw query text never reaches disk is falsified by
  the calculator provider (#58, `3b53a7a`); annotate it in place rather than
  rewriting it, the same treatment the 2026-08-10 amendment gave its six
  falsified claims.
- "Where today's code stands" and "What the implementing slice must still
  settle" both describe a fall-through that no longer exists; annotate what
  landed, and which bullets remain open (the manifest field, salting, and the
  empty-query view's behavior).

Do not restate Decision 2's reasoning in `CONTEXT.md` — link to it.
