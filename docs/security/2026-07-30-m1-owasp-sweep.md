# OWASP Top 10 sweep — M1 — Core

**Date:** 2026-07-30
**Issue:** [#19](https://github.com/pedrosousa13/hop/issues/19)
**Milestone:** M1 — Core
**Verdict:** 29 findings, all filed as `needs-triage` issues. Nothing fixed here.

---

## Scope

The workspace as it stands at the end of M1 — Core: two library crates, 4 009
lines of Rust across 10 files.

| Crate | Files audited |
| --- | --- |
| `hop-protocol` | `wire.rs`, `item.rs`, `lib.rs` |
| `hop-core` | `provider.rs`, `pipeline.rs`, `learning.rs`, `aliases.rs`, `router.rs`, `rank.rs` |

Also read for context, not audited as code: `Cargo.toml` (workspace and both
crates), `Cargo.lock`, `.github/workflows/ci.yml`, `rust-toolchain.toml`,
`CONTEXT.md`, and the design spec at
`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`.

**Out of scope**, per the issue brief: fixing anything; auditing code that does
not exist yet (the daemon, the CLI, the GTK client, the v1 providers — each
gets its own sweep in its own milestone); standing up recurring dependency CVE
scanning.

### The shape of the thing being audited

M1 shipped **no binary and no daemon**. Nothing listens on a socket, no process
is spawned, no network request is made, and no code outside
`Learning::load`/`save` touches the filesystem. Every finding below is
therefore about one of two things:

1. **Code that is reachable today** if a caller drives it — the ranking and
   routing paths, and the learning store's load/save.
2. **A contract that is already fixed in code** and that M2/M3 will implement
   against — the IPC frame types and the provider trait.

Category 2 dominates. That is not padding: the design spec states the protocol
and the provider trait are "locked in v1's protocol so retrofit is never
needed", so a missing bound or a missing validating type is materially cheaper
to fix now than after two consumers and a plugin tier exist. Where a gap is
**explicitly deferred by the code's own doc comments** — budget enforcement and
parallel dispatch are documented as M2 daemon work — the finding is written as a
concern about the *shape being locked*, not as an accusation of oversight.

## Method

1. Four parallel read-only audits, one per attention area named in the brief:
   the `hop-protocol` trust boundary; the provider trait as a plugin seam; the
   learning engine's persistence; injection-shaped paths in query routing.
2. Every claim re-verified against the source before it was written up. Claims
   that survived: `Pattern::parse` vs `Pattern::new`; the unconditional
   parent-directory chmod; `unwrap_or_default()` on the save payload;
   `create(true)` rather than `create_new(true)` on the temp file; the
   unvalidated `version` and `count` read from disk; `truncate(max_results)`
   running last; the absence of `cargo audit`/`cargo deny` in CI; and zero
   non-test callers of `should_query`.
3. Two findings were reproduced by running code rather than reading it:
   - **[#45](https://github.com/pedrosousa13/hop/issues/45)** — a throwaway
     integration test against three items confirmed that `^`, `'`, `!` and
     `^ ^ ^` each return *every* candidate while the control term `zzzqxk`
     returns none, and that `!Firefox` returns everything *except* Firefox.
     This falsifies `rank.rs`'s documented contract that "an item that doesn't
     match at all is dropped". The test was deleted after use; the workspace is
     unchanged.
   - **[#46](https://github.com/pedrosousa13/hop/issues/46)** — wall-clock
     measurements of `Ranker::rank` and `Pipeline::assemble` under long queries
     and large candidate sets, up to 4.09 s for a 100 KB query over 5 000 items.

## Verdicts

Every category carries an explicit verdict. Nine are applicable with findings;
one is not applicable at this stage, with the reason stated.

| Category | Verdict | Issues |
| --- | --- | --- |
| **A01** Broken Access Control | Applicable — 2 findings | [#25](https://github.com/pedrosousa13/hop/issues/25), [#40](https://github.com/pedrosousa13/hop/issues/40) |
| **A02** Cryptographic Failures | Applicable — 1 finding | [#39](https://github.com/pedrosousa13/hop/issues/39) |
| **A03** Injection | Applicable — 2 findings | [#23](https://github.com/pedrosousa13/hop/issues/23), [#45](https://github.com/pedrosousa13/hop/issues/45) |
| **A04** Insecure Design | Applicable — 11 findings | [#21](https://github.com/pedrosousa13/hop/issues/21), [#22](https://github.com/pedrosousa13/hop/issues/22), [#24](https://github.com/pedrosousa13/hop/issues/24), [#28](https://github.com/pedrosousa13/hop/issues/28), [#30](https://github.com/pedrosousa13/hop/issues/30), [#32](https://github.com/pedrosousa13/hop/issues/32), [#33](https://github.com/pedrosousa13/hop/issues/33), [#46](https://github.com/pedrosousa13/hop/issues/46), [#47](https://github.com/pedrosousa13/hop/issues/47), [#48](https://github.com/pedrosousa13/hop/issues/48), [#49](https://github.com/pedrosousa13/hop/issues/49) |
| **A05** Security Misconfiguration | Applicable — 1 finding | [#36](https://github.com/pedrosousa13/hop/issues/36) |
| **A06** Vulnerable and Outdated Components | Applicable — 1 finding | [#35](https://github.com/pedrosousa13/hop/issues/35) |
| **A07** Identification and Authentication Failures | Applicable — 1 finding | [#26](https://github.com/pedrosousa13/hop/issues/26) |
| **A08** Software and Data Integrity Failures | Applicable — 7 findings | [#29](https://github.com/pedrosousa13/hop/issues/29), [#31](https://github.com/pedrosousa13/hop/issues/31), [#37](https://github.com/pedrosousa13/hop/issues/37), [#38](https://github.com/pedrosousa13/hop/issues/38), [#41](https://github.com/pedrosousa13/hop/issues/41), [#42](https://github.com/pedrosousa13/hop/issues/42), [#44](https://github.com/pedrosousa13/hop/issues/44) |
| **A09** Security Logging and Monitoring Failures | Applicable — 3 findings | [#27](https://github.com/pedrosousa13/hop/issues/27), [#34](https://github.com/pedrosousa13/hop/issues/34), [#43](https://github.com/pedrosousa13/hop/issues/43) |
| **A10** Server-Side Request Forgery | **Not applicable at this stage** | — |

### A01 — Broken Access Control · applicable

There is no authorization model in M1, and two places assume one exists
elsewhere. The protocol's `Execute { query_id, item_id, action_id }` accepts
peer-chosen ids with nothing in the type binding them to the results the daemon
actually delivered ([#25](https://github.com/pedrosousa13/hop/issues/25)) — the
rule that makes this safe lives only in a doc comment on `Provider::execute`.
On disk, the learning store's temp file is opened with `create(true)`, so a
pre-planted file or symlink at that path is written through rather than
rejected ([#40](https://github.com/pedrosousa13/hop/issues/40)).

### A02 — Cryptographic Failures · applicable

No cryptography exists in the workspace, so the algorithm-choice half of this
category has no surface. The data-at-rest half does:
`canonicalize_result_id` scrubs dynamic payloads from exactly two id prefixes
(`utility:`, `web-search:`) and lets every other id through verbatim into
plaintext JSON with a 90-day retention
([#39](https://github.com/pedrosousa13/hop/issues/39)). `Kind::File` already
exists, so a planned file provider's ids — full paths — hit the fail-open
branch. The file is mode 0600, which stops other local users but not backup
tools, sync clients, or offline reads of the home directory.

The counterweight is real and worth recording: **raw query text never reaches
disk.** The `selections` map is `#[serde(skip_serializing)]`, the persisted
struct has no such field, and a test asserts the round trip. That makes the id
channel the only leak, which is why closing it is achievable.

### A03 — Injection · applicable

No shell, SQL, path, or process sink exists in the workspace — there is no
`Command::new` anywhere, and no path is constructed from query text. What
*does* exist is a parser being fed raw user text:
`Ranker::rank` calls `Pattern::parse`, not `Pattern::new`, so `!`, `^`, `$` and
`'` are live metacharacters in the user's query
([#45](https://github.com/pedrosousa13/hop/issues/45)). Confirmed by running
it: `!Firefox` excludes Firefox, and `^` returns everything. The second path is
the protocol's `ExecOutcome::OpenUrl(String)`/`CopyText(String)` — the only two
variants in the contract that are instructions to act rather than data, both
unvalidated ([#23](https://github.com/pedrosousa13/hop/issues/23)).

### A04 — Insecure Design · applicable, and the dominant category

Eleven findings, and they share one root: **the boundaries are named but not
enforced.** The IPC contract has no frame codec and no size cap
([#21](https://github.com/pedrosousa13/hop/issues/21)), no field-length bounds
([#22](https://github.com/pedrosousa13/hop/issues/22)). The provider manifest
declares a `budget` no code reads
([#28](https://github.com/pedrosousa13/hop/issues/28)) and a pre-filter with
zero callers ([#32](https://github.com/pedrosousa13/hop/issues/32)). The
pipeline caps output but not work
([#30](https://github.com/pedrosousa13/hop/issues/30)), and ranking cost is
`O(atoms × items)` with no ceiling on either factor — measured at 4.09 s
([#46](https://github.com/pedrosousa13/hop/issues/46)). The raw-vs-routed query
distinction that `CONTEXT.md` defines carefully turns out to have no
enforcement and no escaping contract at the seam where it matters, and `raw` is
read by nothing at all ([#47](https://github.com/pedrosousa13/hop/issues/47)).

### A05 — Security Misconfiguration · applicable

`Learning::save` chmods its parent directory to 0700 on every call, whether or
not it created that directory, via a call that follows symlinks
([#36](https://github.com/pedrosousa13/hop/issues/36)). The designed path makes
this benign; a user-controlled `XDG_STATE_HOME` makes the blast radius somebody
else's directory.

### A06 — Vulnerable and Outdated Components · applicable

**No known-vulnerable dependency was found.** The pinned set (`serde 1.0.229`,
`serde_json 1.0.151`, `thiserror 2.0.19`, `tokio 1.53.1`, `regex 1.13.1`,
`nucleo-matcher 0.3.1`) is current and small. The finding is procedural: CI runs
fmt, clippy and tests and nothing else, so an advisory published against any of
these lands in the lockfile on the next `cargo update` with no gate that
notices ([#35](https://github.com/pedrosousa13/hop/issues/35)).

### A07 — Identification and Authentication Failures · applicable

No authentication exists, and for a local Unix-socket IPC that is a defensible
design — peer trust from the socket directory's mode. The finding is that this
is invisible from the contract: the handshake is an ordinary enum variant with
no required ordering, nothing prevents `Execute` as a first frame, and no peer
identity appears anywhere in the protocol
([#26](https://github.com/pedrosousa13/hop/issues/26)).

### A08 — Software and Data Integrity Failures · applicable

Two clusters. **The plugin seam**: no panic isolation and no error variant for
a panicking provider, with borrowed trait arguments that block the
`tokio::spawn` isolation the doc comment reaches for
([#29](https://github.com/pedrosousa13/hop/issues/29)); and item identity,
kind, and provenance all self-asserted by the provider and never checked,
yielding boost theft, dedupe eviction of the genuine item, and exclusive-mode
bypass from one crafted item
([#31](https://github.com/pedrosousa13/hop/issues/31)). **The store as
untrusted input**: unbounded read and deserialize
([#37](https://github.com/pedrosousa13/hop/issues/37)), no integrity check and
an unvalidated `version`, where a future timestamp disables decay and pins an
attacker-chosen id at position one
([#38](https://github.com/pedrosousa13/hop/issues/38)), plus three smaller
defects — a silent empty-store overwrite
([#41](https://github.com/pedrosousa13/hop/issues/41)), a missing parent-dir
fsync ([#42](https://github.com/pedrosousa13/hop/issues/42)), and a `u32`→`i32`
sign wrap saved only by a clamp three frames away
([#44](https://github.com/pedrosousa13/hop/issues/44)).

### A09 — Security Logging and Monitoring Failures · applicable

The workspace has **no logging dependency at all** — no `tracing`, no `log`, no
metrics. Consequently a provider failure, a slow provider, and a provider
silently returning nothing are all indistinguishable
([#34](https://github.com/pedrosousa13/hop/issues/34)), and a corrupt,
tampered, or simply absent learning store produce the identical value with no
signal ([#43](https://github.com/pedrosousa13/hop/issues/43)). Pointing the
other way, the `Debug` derive on `ClientMsg` means the planned
`tracing::debug!(?msg)` would write raw user keystrokes into the journal
([#27](https://github.com/pedrosousa13/hop/issues/27)).

### A10 — Server-Side Request Forgery · not applicable at this stage

**No code in the workspace makes, or can make, an outbound request.** There is
no HTTP client dependency, no socket, no DNS resolution, and no URL is
dereferenced anywhere — `ExecOutcome::OpenUrl` carries a string the *client*
would later hand to a URI handler, which is a local-handler concern (filed
under A03 as [#23](https://github.com/pedrosousa13/hop/issues/23)), not a
server-side fetch.

The category becomes applicable the moment a provider that performs network
I/O lands. The groundwork for that exposure is already visible and is filed
under A04: `Mode::Weather` and `Mode::WebSearch` route unvalidated user text to
providers with no escaping contract, so `wx Berlin&key=leak` hands a future
weather provider an attacker-chosen extra query parameter
([#47](https://github.com/pedrosousa13/hop/issues/47)). **The M4 sweep must
re-run A10 against the real providers** rather than inheriting this verdict.

## Checked and found sound

Recorded so a later sweep does not re-litigate them, and so the "applicable
with no finding" half of each verdict is legible.

- **No `unsafe` anywhere in the workspace.** Every finding is a resource,
  logic, or trust issue; none is a memory-safety issue.
- **No panicking `unwrap()`/`expect()`/slice indexing in non-test code.** The
  only `expect` outside tests is on a fixed-literal regex. Every
  `#![allow(clippy::unwrap_used)]` is inside a `#[cfg(test)]` module, and the
  workspace lints `unwrap_used = "warn"`.
- **No user-controlled regex compilation.** Exactly one `Regex::new` exists, on
  a string literal in a `LazyLock`. No ReDoS, no regex injection — and the
  `regex` crate has no backtracking regardless.
- **Byte-index slicing of user text is panic-safe.** `strip_prefix_ci` uses
  `q.get(0..n)?` and `strip_suffix_ci` guards with `checked_sub` plus
  `is_char_boundary`, so multi-byte queries cannot panic on a slice boundary.
- **Sorting cannot panic on NaN.** Every comparator uses `f32::total_cmp`, a
  total order, rather than `partial_cmp().unwrap()`.
- **Arithmetic on disk-derived values is saturating or capped throughout** —
  the one exception is the sign wrap filed as
  [#44](https://github.com/pedrosousa13/hop/issues/44).
- **Store growth during normal operation is bounded** by `MAX_QUERIES = 500`,
  `MAX_ITEMS_PER_QUERY = 20`, `MAX_GLOBAL_ENTRIES = 1000` and a 90-day
  retention purge, all enforced on every `record`. The gap is the *load* path
  ([#37](https://github.com/pedrosousa13/hop/issues/37)), not ordinary use.
- **Raw query text is never persisted** — see A02 above.
- **Learning boost is hard-capped below alias boost**, asserted against the
  constants rather than literals, so a tampered store cannot outrank an
  explicit user alias.
- **Alias rewrites cannot inject a mode prefix**: the pipeline routes the raw
  query before applying aliases and never re-routes the rewritten term.
- **Nesting-depth bombs are handled** by `serde_json`'s default 128-level
  recursion limit — noting that the future daemon must not disable it.
- **All protocol enums are closed sets** with no `#[serde(other)]` catch-all,
  so an unknown variant is a deserialization error rather than a silent
  default.
- **Unknown-field tolerance is a deliberate, tested forward-compat choice**,
  not an oversight, and unknown fields are discarded rather than retained — no
  smuggling channel.

## Follow-up

All 29 findings are open as `needs-triage` and are triaged like any other work;
this sweep reports and does not fix. Two are worth flagging for sequencing
rather than severity:

- **[#21](https://github.com/pedrosousa13/hop/issues/21)** (frame cap) and
  **[#28](https://github.com/pedrosousa13/hop/issues/28)** (budget enforcement)
  are M2 daemon work in everything but their filing. Whoever slices M2 should
  read them first — they are the two places where "the daemon will handle it"
  is currently the whole plan.
- **[#45](https://github.com/pedrosousa13/hop/issues/45)** is the only finding
  that is a live, reproducible defect in shipped M1 behavior rather than a
  contract concern. Typing `!chrome` today makes Chrome unfindable.
