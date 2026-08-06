# Threat model — the hopd socket boundary

Date: 2026-08-02
Issue: [#53](https://github.com/pedrosousa13/hop/issues/53)
Milestone: M2 — Daemon
Status: Recorded; amended 2026-08-04
Decisions by: Pedro Sousa

Two design forks are settled here —
[#25](https://github.com/pedrosousa13/hop/issues/25) and
[#39](https://github.com/pedrosousa13/hop/issues/39) — and the code they govern
is not written yet.

**How this document is amended.** It describes code that does not exist yet —
Decision 2's manifest field is the clearest case — so it is a document that
will be corrected as the M2 slices land, not a record of one audit taken at a
moment. It therefore follows the amendment convention of the v1 design spec
(`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`): the `Status:`
line above carries the amendment dates, each amendment opens with an
**Amendment, `<date>`.** block below this one naming the sections it changed
and why, and every changed passage is marked **[Amended `<date>`]** in place so
a reader sees which sentence moved. The other in-repo precedent, the M1 OWASP
sweep (`docs/security/2026-07-30-m1-owasp-sweep.md`), uses `Verdict:` and is
left untouched after it lands — that is the right shape for a sweep of code
that exists, and the wrong shape for this.

**Amendment, 2026-08-04.** Amended when issue #56 landed the provider host.
Four places changed: the item-count-not-bytes passage under Decision 1's
retained-set discussion, which said the byte figure "stops being moot" once
#56 lands and would enforce per-item bounds — #56 has landed and deliberately
did not add that enforcement, so the passage is corrected to say what
actually landed and that per-item field-length enforcement remains open with
issue #30 owning it; the Follow-up table's #55 row, which made the same
"until #56" claim and is corrected the same way; a new #56 row added to the
Follow-up table, in the style of the existing **Landed.** rows; and the
actors table's provider row, which still described `CheckedItems::check` as
the whole of the in-process provider check and now also names the
captured-manifest comparison, the enforced budget, panic containment and
error-text bounding #56 added, plus issue #104's residual. This document's
"What this model does not cover" section still defers the daemon↔provider
boundary in full to the M2 OWASP sweep; none of these four changes model it
beyond correcting what was said. Each change is marked **[Amended
2026-08-04]** in place.

---

## What this is

The v1 design spec (§13, M2) requires a threat model for the daemon's Unix
socket boundary **before the read loop exists**. This is that document. It
describes a boundary that no code in the workspace implements today, against a
wire contract that does exist and is already fixed in types.

Two things follow from that, and both are load-bearing for how this document
should be read:

1. Where it describes a rule, a bound or a check, it names the file that
   carries it. A claim about behaviour with no file behind it is a claim about
   code that has not been written, and is marked as such.
2. The M2 slices are what turn the rest of it into behaviour. When they land,
   the M2 OWASP sweep ([#52](https://github.com/pedrosousa13/hop/issues/52))
   audits the real code rather than inheriting this model's verdicts.

### The shape of the thing being modelled

The workspace at the time of writing holds two library crates, `hop-protocol`
and `hop-core` (`Cargo.toml`, `[workspace] members`). There is no `hopd`, no
`hop-cli`, no `hop-gtk` and no shim. Checks run for this document:

| Question | Check | Answer |
| --- | --- | --- |
| Does any code open a socket? | `grep -rn "UnixListener\|UnixStream\|tokio::net" crates/` | No hits. `tokio`'s workspace features are `sync`, `time`, `macros`, `rt` — `net` is not among them. |
| Does any code compute the runtime-dir path? | `grep -rn "XDG_RUNTIME_DIR" crates/` | No hits. |
| Does any code spawn a process? | `grep -rn "Command::new" crates/` | Two hits, both `std::process::Command::new("mkfifo")` inside `learning.rs`'s `#[cfg(test)]` module (which begins at `learning.rs`:1704). No non-test code spawns anything. |
| Is there a frame codec or a frame-size cap? | `grep -rn "FRAME" crates/hop-protocol/src/` | Matches outside doc prose are `MAX_ITEMS_PER_RESULTS_FRAME` and its uses — an item-count bound, not a byte cap. [#21](https://github.com/pedrosousa13/hop/issues/21) is open. |
| Is a peer credential consulted? | `grep -rni "peercred\|peer_cred" .` over `.rs`, `.md`, `.toml` | No hits outside this document. |
| Is there a logging seam? | `tracing`/`log` in either crate's `Cargo.toml` | Neither is a dependency. The full list, so the claim is checkable rather than gestured at: `hop-protocol` takes `serde`, `serde_json`, `thiserror` and, under `[target.'cfg(unix)'.dependencies]`, `libc` — for the `O_NONBLOCK` flag on `IconPath::open_regular_file` — plus `tempfile` as a dev-dependency. `hop-core` takes those same three, `hop-protocol` itself, `nucleo-matcher`, `regex` and `tokio` (`sync`, `time`, `macros`, `rt`), plus `tempfile`. No logging crate is among them. |
| Does `hop doctor` exist? | `grep -rn "doctor" --include=*.rs .` | No hits. It is specified (§9, §11) and not implemented. |

So the socket in the title is a socket the spec describes and issue
[#54](https://github.com/pedrosousa13/hop/issues/54) builds. What exists today
is the contract that will travel over it.

---

## The boundary

`$XDG_RUNTIME_DIR/hop/hopd.sock`, inside a directory at mode 0700 (spec §3).
Persistent connections carrying length-prefixed JSON frames.

Neither the directory nor the socket is created by any code in the workspace
yet. Three properties of the boundary are therefore *design intent* at this
point, and #54 is where they become behaviour:

- **The directory's mode.** 0700 withholds traverse permission from other
  uids. There is an in-repo precedent for creating it correctly:
  `learning.rs`'s `persist_atomically` (crates/hop-core/src/learning.rs:1491–1559)
  passes the mode as an argument to `mkdir(2)` through `fs::DirBuilder`, so the
  directory is born at 0700 with no window at a wider mode to race, and a
  pre-existing path is left as found rather than chmodded. The same reasoning
  applies to the runtime dir.
- **The socket file's own mode and owner.** The spec fixes the directory's
  mode and says nothing about the socket's. Connecting needs traverse on the
  directory *and* write on the socket, so the directory carries the control
  as designed — but leaving the socket's mode unstated means it will be
  whatever the umask makes it. #54 should decide it rather than inherit it.
- **`$XDG_RUNTIME_DIR` is an environment variable the user controls.** It is
  0700 and user-owned on a systemd session, which is the case the spec
  assumes; it is not a guarantee the daemon can make about a value handed to
  it. This is the same shape as the reasoning already written down in
  `learning.rs`:1493–1501 about `XDG_STATE_HOME` — a path derived from
  user-controlled environment is not a path the process can reason about
  unaided.

---

## Assets behind the boundary

What a peer that reaches the socket gets access to, in rough order of what
would be worst to lose.

| Asset | What it is | Where it lives | Exists today? |
| --- | --- | --- | --- |
| **Action execution** | The daemon acts on the user's behalf: launching applications, focusing and closing windows, opening URLs. This is the asset that makes the boundary worth modelling. | Spec §5's provider table; `ExecOutcome` in `wire.rs`:102–110 | No. No provider is implemented and no code spawns a process. |
| **The item stream** | Titles, subtitles, icon paths and ids describing the user's installed applications, open windows and — from M5 — files. The items a query answers with are a description of the user's machine. | `Item` in `item.rs`:242–302 | The type exists; nothing produces real items. |
| **Query text** | Keystrokes typed into the launcher overlay, which can be a pasted credential. | `QueryText` in `redaction.rs`:136–138 on the wire; the same text again as `RoutedQuery.term`, a plain `String`, once `hop-core` has routed it (`router.rs`:257–258) | Both types exist. Only the first redacts — see T9 and [#83](https://github.com/pedrosousa13/hop/issues/83). |
| **The learning store** | Per-item launch frequency, persisted. Reveals what the user launches and, through ids, what they launch it *on*. | `$XDG_STATE_HOME/hop/learning.json`, mode 0600, 90-day retention (`learning.rs`) | The store exists; no code computes its path yet ([#60](https://github.com/pedrosousa13/hop/issues/60)). |
| **The client's clipboard and URL handler** | `ExecOutcome::CopyText` and `ExecOutcome::OpenUrl` are instructions to a client, not reports. | `content.rs` | Types and content rules exist; no client. |
| **The daemon's availability** | A resident, socket-activated process the user's launcher depends on. Losing it loses the launcher. | Spec §3, [#62](https://github.com/pedrosousa13/hop/issues/62) | No. |
| **Provider-held state** | From M5, network providers hold cached rates, geocoded locations, and the endpoints they talk to. | Spec §5 | No. A10 (SSRF) was recorded not-applicable by the M1 sweep for this reason and re-runs at M5. |

Two things that are **not** behind this boundary, recorded so a later reader
does not go looking: the config file (read-only load is
[#60](https://github.com/pedrosousa13/hop/issues/60), not implemented), and
anything on the network — no HTTP client is a dependency of either crate.

---

## Actors that can reach the boundary

| Actor | How it reaches the socket | What stands in its way |
| --- | --- | --- |
| **The real client** (`hop-gtk`, `hop-cli`) | Opens the socket as the session user | Nothing. It is the intended peer. |
| **Any other process running as the same uid** | Same path, same permissions. A shell script, a compromised application, an editor plugin, anything the user runs | The 0700 directory does not distinguish it from the real client. The protocol does not either — see the next section. |
| **root**, and a process holding `CAP_DAC_OVERRIDE` | Directory mode does not apply | Nothing at this boundary. A root-equivalent adversary is outside the model. |
| **A process that inherits an open descriptor** | Does not traverse the directory at all | Directory mode is checked at open, not per write. Whatever the daemon or a client passes on, it passes on. |
| **The daemon, toward a client** | The reverse direction of the same socket | Bounds on `DaemonMsg`. `wire.rs`:59–64 states the reason: "A client trusts its daemon no more than the daemon trusts its clients." |
| **A provider, in-process** | Not across the socket — but a provider supplies the values that cross it | **[Amended 2026-08-04]** `CheckedItems::check` (`pipeline.rs`:393) holds each item to the manifest of the provider that produced it, called through `ProviderHost::run_one` (`hop-core`'s `host.rs`, issue #56), which also compares the manifest it captured at registration against a fresh call before accepting a provider's answer, runs the provider under a host-enforced budget it aborts on a miss, contains a panicking provider's failure at the `tokio::spawn` seam, and bounds and strips a provider's error text before it can leave. `content.rs`:1–9 states the residual: "the daemon is trusted" degrades to "every installed provider is trusted". A further residual #56 left open, owned by issue #104: a panic *payload* still reaches the daemon's stderr through Rust's default panic hook, unsanitized, before the host's own failure classification runs. |
| **A remote network actor** | No route today | Neither crate depends on an HTTP client, and nothing listens on a TCP socket. This changes at M5. |

The row that matters is the second one. It is the boundary's actual shape: the
control is *which uid*, and there is no finer distinction available.

---

## Entry points

### Frames from a client

| Frame | Fields | Enforced today, and where | Not enforced |
| --- | --- | --- | --- |
| `hello` | `api_version: u32` | Fixed-width integer; nothing to bound. `wire.rs`:33–35 | That it arrives first — nothing in the type requires it ([#26](https://github.com/pedrosousa13/hop/issues/26)) |
| `query` | `id: u64`, `text: QueryText` | `MAX_QUERY_TEXT` = 1 024 bytes, applied by `QueryText`'s `Deserialize` (`redaction.rs`:140–149) through its constructor (`redaction.rs`:167–171) | How many query frames arrive, and how fast — no read loop exists to bound either |
| `cancel` | `id: u64` | Fixed-width | That the id names a live query |
| `execute` | `query_id: u64`, `item_id: ItemId`, `action_id: ActionId` | Length bounds only — `MAX_ITEM_ID` = 4 096, `MAX_ACTION_ID` = 128 (`item.rs`:43–47, 86–90) | That the ids name anything the daemon delivered. This is [#25](https://github.com/pedrosousa13/hop/issues/25), settled below |

### Frames from the daemon

| Frame | Fields | Enforced today, and where |
| --- | --- | --- |
| `hello_ack` | `api_version: u32` | Fixed-width. Negotiates no capability set (`wire.rs`:68–70) |
| `results` | `query_id`, `partial: bool`, `items: Vec<Item>` | `MAX_ITEMS_PER_RESULTS_FRAME` = 1 000, applied at the parse (`limits.rs`:515–519) and refusing on the element past the maximum without reserving capacity for a peer-claimed length (`limits.rs`:300–316, 446–468) |
| `query_done` | `query_id` | — |
| `executed` | `query_id`, `outcome: ExecOutcome` | `CopyText` and `OpenUrl` are validating newtypes carrying content rules as well as bounds (`content.rs`) |
| `error` | `query_id: Option<u64>`, `error: ProtoError` | `MAX_ERROR_MESSAGE` = 1 024 on the message (`limits.rs`:505–507) |

### Entry points that are not frames

- **The length prefix.** The framing the spec mandates gives the peer control
  of an allocation size. No codec and no byte cap exist
  ([#21](https://github.com/pedrosousa13/hop/issues/21)); #54's acceptance
  criteria require the cap to be a constant exported by `hop-protocol` and
  checked before any allocation sized by the prefix.
- **The connection itself.** Accept rate, concurrent connection count and
  per-connection memory are unmodelled because no accept loop exists. They
  belong to #54 and #55.
- **The socket path.** Creating, binding to and unlinking a path inside a
  directory the daemon may not have created. #54.
- **Socket activation.** systemd passes a listening descriptor the daemon did
  not create ([#62](https://github.com/pedrosousa13/hop/issues/62)). The unit
  file's socket mode becomes part of the boundary at that point.
- **The learning store on disk.** Not a socket entry point, but untrusted
  input reaching the same process. The M1 sweep filed **four** gaps against
  this load path — [#37](https://github.com/pedrosousa13/hop/issues/37),
  [#38](https://github.com/pedrosousa13/hop/issues/38),
  [#43](https://github.com/pedrosousa13/hop/issues/43) and
  [#44](https://github.com/pedrosousa13/hop/issues/44) — and all four are now
  closed. The load is bounded and checked: `Learning::load`
  (`learning.rs`:879–881), over `Learning::load_reporting`
  (`learning.rs`:911–943), stats for a regular file before the open, reads
  through `Read::take` against `MAX_STORE_BYTES` rather than trusting the
  file's reported length, drops keys over `MAX_ITEM_ID` after the parse and
  applies the entry cap to what was parsed (#37, **closed** by `96d5713`); it
  refuses a `version` that is not `STORE_VERSION`, and `purge_and_bound` clamps
  a future-dated `last_ms` back to the load instant (#38, **closed** by
  `59fd5fe`); the version is read through a minimal probe ahead of either full
  parse, which is #43's work and not #38's — `056893e` replaced #38's two
  per-branch checks, which could only fire on a document that still had v1's
  shape, with one probe that depends on neither shape — and every fallback
  names its condition through `LoadReport` (#43, **closed** by `056893e`); and
  a persisted `count` is saturated where it crosses into signed arithmetic
  rather than relying on a caller's clamp three frames away (#44, **closed** by
  `edb8258`).

  **Every one of those mitigations is partial, and each says so in place**, and
  two of them left a *pair* of residuals rather than one:

  - #37 leaves the TOCTOU window between the stat and the open, which can cost
    a blocked load but never unbounded memory. `96d5713` names a second thing
    it left alone in the same breath: **eviction's preference for a
    future-dated entry**. #38's clamp did not close that half and `59fd5fe`
    says so outright — "`min` is monotonic, so it cannot change which honest
    entry survives". `learning.rs`:855–878 prices it: a clamped entry is
    stamped at the load instant, which makes it the newest stamp in the map
    and so the last one `evict_lru_map` drops, so a forged store still holds
    one of `MAX_GLOBAL_ENTRIES`' slots against real learning. What the clamp
    removed is the permanence, not the preference.
  - #38 leaves the integrity check itself: a *plausible* forged store — a
    recent timestamp, the right version, the right shape — passes every guard
    above.

  Both residuals need the same missing thing, and it is
  [#88](https://github.com/pedrosousa13/hop/issues/88), which is open.

---

## Where peer trust comes from

**From the socket's ownership and the mode of the directory containing it.
The protocol supplies none.**

That is not a summary of something the types express — it is a description of
their silence, and the silence is checkable:

- `ClientMsg::Hello` carries `api_version: u32` and no other field
  (`wire.rs`:33–35). `DaemonMsg::HelloAck` carries `api_version: u32` and no
  other field (`wire.rs`:68–70). Neither carries a credential, a token, a
  nonce, or a peer identifier.
- Both are ordinary variants of the message enums rather than a distinct
  pre-session type, so nothing in the types prevents `execute` as a first
  frame. Whether that is refused depends on daemon-side state the protocol
  does not ask for. This is
  [#26](https://github.com/pedrosousa13/hop/issues/26), folded into #54 as an
  acceptance criterion.
- `SO_PEERCRED` does not appear in the workspace's Rust, Markdown or TOML
  (checked by grep). There is no connection-handling code to consult it in.
- `API_VERSION` (`lib.rs`:16) is a compatibility marker. It is not an
  authorization value and nothing treats it as one.

### What that means for everything below

A peer that can open the socket is fully authorized. The consequence worth
stating plainly, because it changes how the rest of this document should be
read: **the bounds, content rules and binding rules in the protocol constrain
a confused, buggy or careless peer, and constrain resource consumption. They
are not an access-control layer over a hostile peer, because a hostile peer
that reached the socket is already inside the boundary.**

A future consumer reading only `hop-protocol` has no way to learn this from
the types, which is the substance of #26. The documentation half of that issue
is what puts the sentence above where a plugin-tier implementer will read it.

### Trust directions, stated

- The daemon does not trust a client: `ClientMsg`'s fields are bounded at the
  parse (`wire.rs`:10–29).
- A client does not trust the daemon: `DaemonMsg`'s fields are bounded for the
  same reason (`wire.rs`:59–64).
- The daemon does not fully trust a provider: `CheckedItems::check` holds each
  item to its own producer's manifest, and `CONTEXT.md` names what survives
  that check **checked items**. The residual is recorded in
  [#72](https://github.com/pedrosousa13/hop/issues/72) — the learning store
  keys on a bare item id, so an honestly-declared hostile provider can still
  collect another provider's learned boosts.

---

## What the contract enforces today

Recorded so the M2 sweep does not re-derive it, and so the decisions below sit
on a stated baseline.

**Size budget** (`limits.rs`). Enumerating the variable-length fields of
`ClientMsg`, `DaemonMsg`, `Item` and `Action`, each carries a bound: either a
`deserialize_with` target (`limits.rs`:470–519) or a validating newtype that
applies one. That enumeration is the whole of the claim — it says nothing
about a field added later. The constants:

| Constant | Value | Field |
| --- | --- | --- |
| `MAX_QUERY_TEXT` | 1 024 | `ClientMsg::Query.text` |
| `MAX_ITEM_ID` | 4 096 | `ItemId`, sized to `PATH_MAX` |
| `MAX_ACTION_ID` | 128 | `ActionId` |
| `MAX_TITLE` / `MAX_SUBTITLE` | 1 024 each | `Item.title`, `Item.subtitle` |
| `MAX_ACTION_LABEL` | 128 | `Action.label` |
| `MAX_PROVIDER_ID` | 64 | `Item.provider` |
| `MAX_ICON_NAME` / `MAX_ICON_PATH` | 256 / 4 096 | `IconSpec` |
| `MAX_COPY_TEXT` | 65 536 | `Item.copy_text`, `ExecOutcome::CopyText` |
| `MAX_OPEN_URL` | 8 192 | `ExecOutcome::OpenUrl` |
| `MAX_ERROR_MESSAGE` | 1 024 | `ProtoError.message` |
| `MAX_ACTIONS_PER_ITEM` | 32 | `Item.actions` |
| `MAX_ITEMS_PER_RESULTS_FRAME` | 1 000 | `DaemonMsg::Results.items` |

Bounds are counted in bytes, refuse rather than truncate, and are applied at
the parse rather than at a later read (`limits.rs`:1–25).

**Validating newtypes.** `ItemId` and `ActionId` (`item.rs`), `CopyText`,
`OpenUrl`, `IconName` and `IconPath` (`content.rs`), `QueryText`
(`redaction.rs`) — the seven `CONTEXT.md` names. Each wraps a private `String`
whose constructor applies the rules, and whose `Deserialize` hands the parsed
string to that same constructor — one gate rather than two that happen to
agree.

**Content rules** (`content.rs`). `OpenUrl` requires a scheme from
`ALLOWED_URL_SCHEMES` (`content.rs`:159 — `http`, `https`, `mailto`), refuses
a leading `-`, and refuses ASCII space and any `Cc` control character.
`CopyText` refuses `Cc` controls outside `ALLOWED_COPY_TEXT_CONTROLS`
(`content.rs`:170 — tab and newline). Both arms of an icon carry rules too,
since [#24](https://github.com/pedrosousa13/hop/issues/24) closed: `IconPath`
must be absolute, free of any `..` component, free of NUL and free of control
characters (`content.rs`:634–681), and `IconName` must be non-empty, free of
`/` and free of control characters (`content.rs`:526–552) — the `/` rule being
the one that keeps the two arms apart, so `name` cannot become a second channel
for a path-shaped value that passed none of `IconPath`'s rules.
`content.rs`:102–123 states what these rules do not close, and
that statement holds here too: an accepted URL is still an arbitrary web
address, accepted copy text is still arbitrary text, and an accepted icon path
names *somewhere* rather than somewhere an icon belongs. #24 closing is
therefore only half of the icon story — the unenforced-roots half is
[#93](https://github.com/pedrosousa13/hop/issues/93) and is open, and it is
recorded under "What the contract does not enforce" below.

**Structural rules that need no validator.** `IconSpec` is an externally tagged
enum (`item.rs`:216–221) whose two arms are `Name(IconName)` and
`Path(IconPath)`, so an icon carrying both a name and a path, and an icon
carrying neither, are values no frame can express — the shape refuses them at
the parse rather than a check having to.

**Redaction** (`redaction.rs`). `QueryText`'s `Debug` prints
`QueryText(<redacted, N bytes>)` (`redaction.rs`:199–203), so formatting a
`query` frame does not reproduce the keystrokes. The redaction travels with
the value rather than with the frame's `Debug`. It discloses a byte count,
and `redaction.rs`:73–117 prices that disclosure rather than filing it under
"something about the value".

**Query text is not written by `Learning`'s persistence path.**
`Learning::save` writes a `PersistedLearningStore`, which has no `selections`
field (`learning.rs`:356–360), and the in-memory `selections` map is
`#[serde(default, skip_serializing)]` (`learning.rs`:348–349). The test
`save_and_load_round_trip_without_persisting_query_keys` asserts the saved
file does not contain the query key. That is a statement about this path in
this module, not about code that does not exist yet.

**Closed enums.** `Kind`, `ActionKind`, `ErrorCode`, `ExecOutcome` and the
message enums carry no `#[serde(other)]` catch-all, so an unknown variant is a
deserialization error rather than a silent default.

---

## What the contract does not enforce

Stated as gaps, with the issue that owns each.

- **No frame-size cap.** [#21](https://github.com/pedrosousa13/hop/issues/21).
  A peer-supplied length prefix with no ceiling is the canonical
  `vec![0u8; n]` allocation bomb. Folded into #54.
- **Field bounds apply after buffering.** Both message enums are internally
  tagged, so serde buffers the whole JSON value before dispatching on `type`
  and handing fields to the bounded deserializers (`limits.rs`:27–39). A
  200 MB `query` frame is refused — after 200 MB has been held. The frame cap
  is what closes this; the field bounds complement it.
- **The bounds do not compose to a usable frame ceiling.** `limits.rs`:41–72
  works it out: one item on every one of its bounds is 84 160 bytes, and at
  `MAX_ITEMS_PER_RESULTS_FRAME` that is roughly 84 MB in a single `results`
  frame, entirely within every bound in the module. A test recomputes the
  figure from the constants.
- **Nothing bounds a query's total across frames.** `MAX_ITEMS_PER_RESULTS_FRAME`
  bounds one frame; a daemon may send several partial `results` frames for one
  `query_id` (`wire.rs`:74–77). This matters directly to Decision 1 below.
- **`Item.copy_text` carries no content rules** — it is a bounded `String`,
  and reaches the same clipboard as `ExecOutcome::CopyText` by a different
  route ([#78](https://github.com/pedrosousa13/hop/issues/78)).
- **`IconSpec.path` is validated, and validation is not containment.**
  [#24](https://github.com/pedrosousa13/hop/issues/24) is **closed** (`6e428f7`):
  the path arm is now the validating newtype `IconPath`, which must be absolute,
  `..`-free, NUL-free and control-free, and the enum's shape settles the
  both-set and neither-set cases. **Partial by design, and the module says so
  rather than claiming otherwise. The residual is not closed with #24 — it is
  [#93](https://github.com/pedrosousa13/hop/issues/93), which is open**, split
  out of #24 for exactly this half. The icon roots are documented, not enforced
  — they depend on `XDG_DATA_DIRS` and on whether Flatpak or Snap is installed,
  so enforcing them inside the wire contract would make a frame valid on one
  machine and invalid on another — and a symlink under a root still leads out.
  So a regular file outside every root still opens: `/proc/self/mem` passes
  every rule #24 added, and `a_procfs_file_is_opened_because_it_is_a_regular_file`
  pins that so the documentation and the behaviour cannot drift apart. #93 puts
  the root check in the component that resolves the path — the client — which is
  the only process in a position to compute the roots and to make the check and
  the open refer to the same file. The filesystem check,
  `IconPath::open_regular_file`, is a method a caller runs explicitly rather
  than part of `Deserialize`, because a stat per item on the results path would
  break `CONTEXT.md`'s query-path rule and would still race the client's own
  open; it is also `#[cfg(unix)]`, so a client on any other platform inherits
  the value rules and nothing more. What the rules buy is a path somebody else
  can check against a root, which is what this boundary needs from them.
- **`ItemId` carries a length bound and no shape rule.** `ItemId::new`
  (`item.rs`:43–47) checks `MAX_ITEM_ID` and nothing else, so a provider
  chooses freely what goes in an id. This matters directly to Decision 2.
- **No audit trail.** Neither crate depends on `tracing` or `log`, so nothing
  records what crossed the boundary
  ([#34](https://github.com/pedrosousa13/hop/issues/34), open). A daemon that
  adds one inherits [#27](https://github.com/pedrosousa13/hop/issues/27)'s
  hazard, which `QueryText`'s `Debug` pre-empts for the one field that carries
  keystrokes — **in `hop-protocol` only**. #27 is closed and the redaction
  stops at the crate boundary: `hop-core`'s `route` takes a `&str`, and the
  `RoutedQuery` it returns (`router.rs`:257–258) derives `Debug` over a plain
  `String` term, so the same keystrokes format verbatim one crate downstream.
  That half is [#83](https://github.com/pedrosousa13/hop/issues/83) and is
  open; `router.rs`:249–256 says so in place. The nearest thing to a signal
  today is narrow, and deliberately:
  [#43](https://github.com/pedrosousa13/hop/issues/43) is **closed**
  (`056893e`), and what it produced is `LoadReport` (`learning.rs`:466–544) —
  seven variants naming one condition each, `Loaded` plus the six a
  learning-store load can fall back on, returned beside the store by
  `Learning::load_reporting`. That is a report about one file read at startup.
  It is not a log, it has no consumer yet by design, and it records nothing
  that crossed the socket.

---

## Threats, by entry point

The column that matters is the third one: what has to be true when the code
lands, since for most rows nothing stands in the way today because nothing
exists yet.

| # | Threat | Entry point | Today | What must hold when the code lands |
| --- | --- | --- | --- | --- |
| T1 | Allocation driven by a peer-supplied length prefix | Framing | No codec exists | Cap checked before allocation, from a `hop-protocol` constant (#21, #54) |
| T2 | Memory amplification below the cap, via tagged-enum buffering | Any frame | Bounds apply post-buffer (`limits.rs`:27–39) | Frame cap sized against the 84 MB figure in `limits.rs`:41–72 |
| T3 | **Unbounded retained item set.** Decision 1 has the daemon retain what it delivered per query id, accumulating across frames; `MAX_ITEMS_PER_RESULTS_FRAME` bounds one frame, and the protocol permits several partial frames per query, so absent a total the retained set would have no ceiling. Reachable by a **well-behaved** client, not only a hostile one | `results`, and Decision 1's registry | **Bounded, by item count, since #55 — amended 2026-08-06 for #103's replace-frame assembly.** The daemon no longer accumulates a retained set across frames. Under the replace rule (#103), `connection.rs`'s `Exchange::delivered` holds only the **last assembled list** for a query id, replaced whole by each `results` frame, and `forward_batch` enforces `MAX_ITEMS_PER_RESULTS_FRAME` = 1 000 on it — defensively, since the **result source** is untrusted — truncating an over-long arrival to fit and ending the exchange with `QueryDone` (truncate-and-terminate). `MAX_ITEMS_PER_QUERY` = 5 000 is no longer a per-connection binding; it now bounds the daemon-side accumulator in the result source (`source.rs`, `HostSource::start`), where the growth happens, still by truncating the arrival that crossed the line. Truncation of the undelivered remainder, never eviction of what was delivered — the two are named differently on purpose, because only one of them is visible to the client (see Decision 1's overflow paragraph). What is **not** bounded is bytes — see "count or bytes" under Decision 1 below | A documented per-query total cap on retained items. [#85](https://github.com/pedrosousa13/hop/issues/85) is the standalone record of this gap; #55 (the state) and #59 (the binding that retains it) carry it as acceptance criteria, amended 2026-08-03. #55 has landed the cap; #59 still has to resolve `execute` against the capped set, and to make an item lost to the cap distinguishable from one the daemon never emitted — which the terminal `QueryDone` does not do today |
| T4 | A frame acted on before the handshake | `execute`, `query` | Nothing in the types requires ordering (#26) | Connection loop refuses pre-handshake frames (#54) |
| T5 | `execute` naming an item the daemon never delivered | `execute` | Length bounds only | Decision 1, implemented by #59 |
| T6 | `execute` naming an action the item does not carry | `execute` | Nothing ties `action_id` to `Item.actions` | Decision 1's second half — see below |
| T7 | Query-path cost amplification | `query` | `MAX_QUERY_TEXT` bounds one query's bytes; ranking is `O(atoms × items)` with 4.09 s measured (#46), and `boost_for` lowercases per candidate item (#75) | Input caps decided in M2, gated by #61's adversarial arm |
| T8 | A provider aiming a command-shaped outcome at a client | `executed`, and `Item.icon` on `results` | Content rules on `CopyText`/`OpenUrl` (#23, closed) and on `IconName`/`IconPath` (#24, closed) | Residual on both halves, and an **open** issue owns each: `Item.copy_text` still reaches the clipboard as a bare bounded string ([#78](https://github.com/pedrosousa13/hop/issues/78), open), and an icon path is validated but not contained — the roots are documented, not enforced, so a regular file outside them still opens ([#93](https://github.com/pedrosousa13/hop/issues/93), open, split out of #24 for this half) |
| T9 | Keystrokes reaching the journal, then a shared bundle | Logging | No logging dependency. `QueryText` redacts (#27, closed) — **in `hop-protocol` only**. `route` takes a `&str` and `RoutedQuery` (`router.rs`:257–258) derives `Debug` over a plain `String` term, so the same text formats verbatim in `hop-core`; `router.rs`:249–256 says not to treat one as safe to log ([#83](https://github.com/pedrosousa13/hop/issues/83), open) | Any added logging keeps the redacting type at the field, and #83 carries the redaction across the crate boundary rather than stopping at `route` |
| T10 | The learning store as untrusted input on load | Disk | Read and parse are bounded (#37, closed by `96d5713`); the `version` is refused on mismatch and a future-dated timestamp clamped (#38, closed by `59fd5fe`); the version probe and the per-condition `LoadReport` are #43's (closed by `056893e`, which replaced #38's two per-branch checks); a persisted `count` is saturated at the boundary (#44, closed by `edb8258`). **Two residuals, one owner**: still no integrity check, so a plausible forged store passes all of it — and eviction still prefers a clamped future-dated entry, which `96d5713` left open and `59fd5fe` explicitly did not close (#88, open) | #88's integrity check, which is what lets a forged entry be *refused* rather than clamped, sequenced with #72 and with Decision 2 on the same load path |
| T11 | The learning store as a disclosure at rest | Disk | Fail-open id scrubbing (`learning.rs`:694–708) | Decision 2 |
| T12 | Cross-provider boost theft | Provider seam | `CheckedItems::check` closes provenance forgery; the store keys on a bare id (#72) | A provider dimension in the store key |
| T13 | Connection flood / socket occupancy | Accept loop | An accept loop exists (#54) and spawns one unbounded task per peer (`server.rs`, `serve_with`); nothing caps concurrent connections, aggregate memory across them, or the accept rate, and `read_frame` has no read timeout, so a peer that sends a valid length prefix and then stalls holds a task and its payload buffer open indefinitely (`connection.rs` says so in place) | Belongs to [#98](https://github.com/pedrosousa13/hop/issues/98); this document does not settle it. #54 and #55 have both landed and neither took it: #55 bounded per-*query* retained state (T3) and deliberately left every connection-level bound to #98 |
| T14 | A provider opts in to plaintext persistence for ids that carry user content | Manifest, under Decision 2's consequence | The opt-in field does not exist yet (`provider.rs`:62–78). `CheckedItems::check` verifies an item's kind and provider id, and inspects nothing about what the id *contains* | Documentation a provider author reads before setting the field, and the extension store's PR review (spec §6) as the gate. No code check can verify the claim |

---

## Exposure paths off the machine

Where data behind this boundary leaves the machine without an attacker. This
is the route the phrase "accidental disclosure" refers to in Decision 2, and
it is worth naming concretely rather than gesturing at.

- **A diagnostics bundle.** The spec gives `hop doctor` a bundling role (§9:
  "`hop doctor` bundles diagnostics"; §11: `hop doctor --json`), and
  [#27](https://github.com/pedrosousa13/hop/issues/27) names it as the path by
  which journal content reaches whoever is helping. **It is not implemented** —
  `grep -rn "doctor" --include=*.rs .` returns nothing, and no CLI crate
  exists. When it is built, whatever it collects becomes a thing users paste
  into issue trackers, and what it may collect is a decision that slice has to
  make rather than inherit.
- **Backups and sync clients.** `learning.json` is mode 0600, which withholds
  it from other local users. It does not withhold it from a backup agent, a
  file-sync client, or an offline read of the home directory, all of which run
  as the user or with the user's data in hand.
- **The system journal.** No logging dependency exists today. When one lands,
  a journal is readable by more than the process that wrote it and travels in
  the bundle above.

---

## Decision 1 — execute frames bind to the live item set

**Settles:** [#25](https://github.com/pedrosousa13/hop/issues/25) ·
**Implemented by:** [#59](https://github.com/pedrosousa13/hop/issues/59) ·
**Decided:** 2026-08-02, by the maintainer.

### The rule

The daemon retains the items it has delivered for a query id, and refuses any
`execute` frame naming an item or an action that is not among them.

- The daemon retains, per query id, the items it delivered under that id,
  together with each item's action ids. **Delivered, not last sent**: a query
  is answered by several partial `results` frames (`wire.rs`:74–77), and every
  item in every one of them stays executable until the retained set is
  released. A rule that kept only the most recent frame would break Enter on
  anything a client is still showing from an earlier one, and it is the reason
  the retained total needs a cap of its own — see T3 and the cap requirement
  below.
- An `execute` frame is served only if its `item_id` appears in that retained
  set and its `action_id` appears in that item's `actions`. Anything else is
  refused rather than dispatched.
- Refusals use the error codes the contract already carries:
  `ErrorCode::UnknownItem` and `ErrorCode::UnknownAction` (`wire.rs`:125–131).
  No new code, no new variant.
- **No new wire field.** `ClientMsg::Execute` (`wire.rs`:52–56) is unchanged,
  and so is `Item`.

### Why

- **No extra bytes on any results frame**, and no token table in the daemon
  carrying its own lifetime and expiry rules.
- **It rides on state the daemon must keep anyway.** Per-query state is
  already required for server-side cancellation and stale-frame drop (spec §3;
  [#55](https://github.com/pedrosousa13/hop/issues/55)). The binding is a use
  of that state rather than a second mechanism beside it. **That argument holds
  only while the state is bounded**, and #55 is what bounds it:
  `MAX_ITEMS_PER_QUERY` = 5 000 items per query id, per connection, enforced by
  truncating the undelivered remainder rather than evicting delivered ones —
  see T3 and the settled answers below. An unbounded registry would not be "state the daemon
  needs anyway"; it would be new state with a new failure mode, and the
  reasoning for this decision would not survive leaving it uncapped.
- **It puts no obligation on a future plugin ABI.** The host resolves the ids
  before dispatch, so `Provider::execute`'s existing prose contract
  (`provider.rs`:257–258, on `Provider::execute` — "both of which this provider
  must have produced from a prior `Provider::query` call") becomes a guarantee
  the host makes to a provider, rather than a rule each plugin author has to
  remember. A token scheme would have to be carried through that ABI by every
  implementer.

### What it does not do

**This binds a buggy or confused client. It does not bind a hostile one.** A
hostile peer is already inside the boundary, because peer trust derives from
socket ownership and the 0700 directory mode, not from anything in the
protocol — see "Where peer trust comes from" above. Such a peer can issue a
query, read the items it is handed, and execute any of their ids. The rule
removes *unsolicited* execution, which is the failure #25 describes; it is not
a defence against a malicious peer and should not be cited as one.

### Rejected alternative — an opaque per-query execution token

An opaque token minted with each `results` frame and required on the matching
`execute` frame.

Stated fairly: it is **stronger and more explicit**. The right to act becomes a
thing the client holds rather than a rule the daemon enforces, which is a
better shape — it fails closed at the type rather than at a lookup somebody
has to write, and it generalises if execution ever crosses a process or a
plugin boundary, where a rule enforced by one host's discipline does not
travel.

Its costs are bytes on every results frame (or on every item, if the binding
is per item rather than per frame — the per-item form also changes `Item`,
which is the plugin seam), and a token table in the daemon with lifetime and
expiry rules of its own.

**Rejected as disproportionate for v1**, given the trust that socket ownership
already confers on any peer that reaches the boundary. Worth recording for
whoever revisits this: a token is easier to add later than to remove later. If
execution ever crosses a process or plugin boundary, this is the decision to
reopen first.

### What the implementing slice settled — and what #59 still owns

The three questions this section posed were left to the slice that built the
retained set. [#55](https://github.com/pedrosousa13/hop/issues/55) has landed
and answered all three; the answers are recorded here because they are what
#59 inherits, and a reader auditing #59 against this document should not have
to reconstruct them from `connection.rs`.

- **When the retained set is dropped: replaced whole by the next `Query`, and
  not before.** Enter arrives *after* `query_done`, so the set cannot be
  released there — and it is not. `hopd` holds it across the exchange's
  terminal frame and across a `Cancel`, on the rule that an item the daemon
  has already shown must stay resolvable until the client visibly moves on.
  The only things that release it are a new `Query` on the same connection,
  which replaces it whole, and the connection closing, which drops it with
  everything else. So the retained set is exactly the most recent query id's,
  it is at most one per connection, and a stale id stays live precisely as
  long as the client leaves it as the latest one.
  A consequence worth stating: this makes reusing a query id on a connection
  a client-side hazard rather than a daemon-side one — the second `Query`
  replaces the first round's retained items while the client still holds them
  under the same label. `ClientMsg::Query`'s doc states the uniqueness rule
  and what the daemon does when it is broken.
- **Item count or total bytes: count only.** The cap is
  `hop_protocol::limits::MAX_ITEMS_PER_QUERY` = 5 000, a documented constant
  rather than a by-product of whatever `max_results` a caller happens to
  pass. **Bytes were deliberately not capped**, for a reason that is also its
  limitation: a count composes with this crate's per-item field bounds into a
  byte figure without a second constant (84 160 bytes per item worst case ×
  5 000 ≈ 421 MB, ~1 MB honest-shaped), and a second byte-denominated
  constant would have to be justified against the same arithmetic while
  bounding nothing the count does not already bound. That composition holds
  **only for items whose per-item bounds were actually applied**, and those
  bounds are applied at the *parse* — so they hold for every item that arrived
  over a socket and for no item a daemon constructs in-process. **[Amended
  2026-08-04]** The provider host
  ([#56](https://github.com/pedrosousa13/hop/issues/56)) has landed and is
  the first code to take items from outside the process without parsing
  them, and it deliberately did not add that enforcement —
  `hop-protocol/src/limits.rs` and `hopd/src/source.rs` both say so in place,
  and `ProviderHost`'s per-provider turn (`hop-core`'s `host.rs`) checks an
  item's `kind` and `provider` against its producer's manifest and nothing
  about its field lengths. `hopd`'s `ResultSource` trait still records the
  obligation on a source; closing it is issue #30's, not #56's, and until #30
  lands the byte figure above remains an argument about the wire and not a
  bound on the daemon's memory.
- **Per connection or per daemon: per connection.** The retained set lives in
  the connection driver's own state (`connection.rs`, `Exchange`), and there
  is no cross-connection registry for a query id to reach into. Client-chosen
  ids being guessable and non-unique across connections is therefore a
  non-issue by construction rather than by a check.

**Overflow behaviour, and the half of #85's question that is not settled.**
At the cap the daemon truncates the source batch that crossed the line,
delivers what fit, sends `QueryDone`, and drops the source — **truncate-and-terminate**,
in `CONTEXT.md`'s terms. Nothing already delivered is evicted, and nothing that
was not delivered is retained — the delivered set and what the client holds
stay in agreement, which is the property Decision 1 needs. The undelivered
remainder is a **truncation** and not a refusal, because nothing on the wire
names it; the client half of the same cap *is* a **refusal** in `CONTEXT.md`'s
sense, because it names the cap it declined on: `hop-cli` errors out and prints
nothing when a daemon streams past it.

What the daemon does **not** do is tell the client any of that happened. A
capped exchange and a completed one both terminate with the same
`QueryDone { query_id }`, and no field on any frame says items were dropped.
#85 asks for overflow to be a refusal or a rejection rather than a silent
truncation, and on the daemon's side it is still the silent one: what was
dropped is invisible to the peer it was dropped on behalf of. Changing that
needs a wire signal that does not exist today, so **this half is not settled
by #55** — it stays with #59, which the follow-up table already charges with
"an item lost to that cap distinguishable from one the daemon never emitted".
See T3.

---

## Decision 2 — unknown ids are hashed before persistence

**Settles:** [#39](https://github.com/pedrosousa13/hop/issues/39) ·
**Decided:** 2026-08-02, by the maintainer.

### The rule

Ids matching a known-safe shape persist as plaintext. Ids that do not are
persisted as a hash of the id.

Frecency needs only equality and counting — `Learning` stores a count and a
timestamp per key (`learning.rs`, `LearningEntry`) and looks keys up by
equality — so learning keeps working on a hashed id exactly as it does on a
plaintext one.

### Why

**Third-party providers keep learning at all, and that is the deciding
factor.** The roadmap makes the `Provider` trait and the `hop-protocol` frames
the plugin seam every later extension tier adapts to (spec §6), and third-party
providers will mint ids this code has never heard of. Under the rejected
alternative those items earn no learning and the author has no way to discover
why their items never rise — the failure is silent, and it lands on exactly the
people least able to diagnose it.

### What it does not do

**A hash of a low-entropy input is brute-forceable by anyone holding the
store.** A file path is drawn from a candidate set an attacker with the same
filesystem can enumerate; hashing it does not hide it from someone who can
generate the candidates. This protects against **accidental disclosure** — a
diagnostics bundle, a backup, a synced folder, all named under "Exposure paths
off the machine" above — and not against a determined attacker who already has
the file. **It does not make the ids private**, and the document does not claim
it does.

### Rejected alternative — an allowlist of safe id shapes

Only ids matching a registered shape persist; an unrecognised id is not
persisted in any form.

Stated fairly: it is **genuinely stronger on privacy**, and by construction
rather than by work factor — an unrecognised id never reaches disk, so there
is nothing there to brute-force, and the caveat above simply does not apply to
it.

**Rejected because the cost falls entirely on third-party providers, and falls
silently.** A plugin author whose ids match no registered shape gets a launcher
that quietly never learns their items, with no error, no diagnostic, and
nothing in the store to inspect.

### Consequence — providers opt in to plaintext persistence via their manifest

This exists **because** Decision 2 chose hashing. It is a constraint on later
slices, not a decision of its own, and it would not arise under the rejected
alternative.

Hashing keeps learning working for every provider, but it takes something with
it: a hashed key cannot be turned back into an item, so it cannot be rendered.
`Learning::recent_launches` (`learning.rs`:1296–1305) and
`Learning::frequent_launches` (`learning.rs`:1309–1319) both return the stored
keys directly, and spec §8 designs the empty-query view around
"recent/frequent items from learning". Under
hashing alone, a third-party provider's items would be learned — and so would
rank correctly — but would be missing from that screen, while built-ins
appeared on it.

**The rule.** A provider declares in its manifest that its ids are safe to
persist in plaintext. The daemon honours that declaration: ids from a provider
that opted in persist in the clear and its items appear in the recents view
like built-ins. Ids from a provider that did not opt in are hashed, and are
learned but not renderable in the empty-query view.

**Why it is the right shape.** It puts the choice with the party that knows
what its ids contain — a provider author knows whether its id embeds a path, a
window title or a contact, and the daemon does not. And it keeps third-party
providers at parity with built-ins on the recents screen rather than leaving
them visibly second-class, which matters because the plugin developer
experience is an explicit project goal (spec §1 positioning, §6).

**What it costs, stated plainly.** It means a provider decides what lands on
the user's disk in the clear. That trust is narrower than it sounds:
`CheckedItems::check` (`pipeline.rs`) already holds each item to the manifest
of the provider that produced it, so a provider cannot forge another's
provenance, and `CONTEXT.md` names what survives that check **checked items**.
But **the
provenance check does not validate the claim that an id is safe to persist** —
it checks kind and provider id, and nothing in it inspects what the id
contains. A provider that opts in wrongly, through carelessness or
misunderstanding, writes plaintext to `learning.json`, and no check in the
workspace catches that.

**It needs a manifest field that does not exist yet.** `ProviderManifest`
(`provider.rs`:62–78) carries `id`, `kinds`, `modes`, `min_term_len` and
`budget` — there is no field for this and no code reads one. Adding it is a
change to the plugin seam (spec §6), so it should land while the seam is still
open to change rather than after the extension store ships. It also interacts
with [#72](https://github.com/pedrosousa13/hop/issues/72), which wants a
provider dimension in the store key: both add a provider-shaped fact to how
learning is stored, and they should be designed together.

#### Rejected alternative — accepting the gap

Add no manifest field. Recents shows only the ids kept in plaintext under
Decision 2's known-safe shapes, and a third-party provider's items are simply
absent from that screen.

Stated fairly: it is **the smaller change by a wide margin**, and it is the
only one of the two that leaves the plugin seam alone. `ProviderManifest` is
untouched, so nothing has to land before the extension store ships, nothing
has to be co-designed with #72, and no provider author is handed a field they
can set wrongly — which removes the cost recorded above, that a provider
decides what lands on the user's disk in the clear, in full rather than
narrowing it. It also fails safe in the direction that matters: everything not
recognised stays hashed, with no opt-out.

**Rejected because the gap is silent and lands on third-party providers.** It
is the same failure mode #39's own rejected alternative was rejected for,
displaced one screen: under the allowlist a plugin author's items never learn,
and under this one they learn, rank, and then do not appear on the empty-query
view, with nothing to distinguish that from the provider being broken. It
would leave third-party items visibly second-class on the one screen the user
sees before typing, against the plugin developer experience being an explicit
project goal (spec §1, §6).

### Where today's code stands

`canonicalize_result_id` (`learning.rs`:694–708) strips dynamic payloads for
two prefixes, `utility:` and `web-search:`. An id that matches neither falls
through to `result_id.to_string()` (`learning.rs`:707) and is written into
plaintext JSON with the 90-day retention `PERSIST_RETENTION_MS` sets. So does
an id that carries one of the two prefixes with an empty first segment after
it — `utility:` alone takes the same fall-through, since the guard requires a
non-empty segment.

Two facts about that worth carrying into the implementing slice:

- **The two prefixes are inherited from the retired GNOME extension's id
  scheme.** `CONTEXT.md` records that `utility` was split into the four kinds
  `Calculator`, `Currency`, `Timezone` and `Weather`, and today's `Kind` set
  (`item.rs`:110–123) has no `utility` variant. No provider is implemented, so
  no non-test code in the workspace produces an id with either prefix. The
  allowlist as it stands is keyed to a naming scheme the current kind set has
  dropped.
- **Nothing constrains what a provider puts in an id.** `ItemId::new`
  (`item.rs`:43–47) applies `MAX_ITEM_ID` and no shape rule.

### What the implementing slice must still settle

- **The manifest opt-in field**, per the consequence recorded above: its name,
  its default, and the documentation a provider author reads before setting it.
  It is a change to `ProviderManifest` and therefore to the plugin seam. **The
  default is open, and this document does not close it** — the ruling settled
  that providers opt in and that the field does not exist yet, and said nothing
  about what a silent manifest means. The trade is worth stating so the slice
  decides it rather than inherits it: a default of "do not persist in
  plaintext" makes a provider that says nothing hashed, which fails closed on
  privacy and costs that provider the recents view until it acts; a default of
  the opposite fails open on the very content Decision 2 exists to keep off
  disk. There is also a third answer — no default at all, with the field
  required, so a manifest that omits it does not build.
- **Which shapes count as known-safe**, and how that list relates to the opt-in
  — whether a built-in provider is covered by a shape, by the manifest flag, or
  by both.
- **The hash function, and the dependency it brings.** Neither crate lists a
  hashing or cryptographic dependency today (`crates/*/Cargo.toml`), so this
  adds one. The gate it will meet now exists:
  [#35](https://github.com/pedrosousa13/hop/issues/35) is **closed**
  (`0168107`), and `cargo deny check` runs advisories, bans, licenses and
  sources against `deny.toml` as its own CI job. A hashing crate therefore has
  to clear three separate checks, not one: the `[licenses]` allow-list — today
  GPL-3.0-only, MIT, MPL-2.0 and Unicode-3.0, with `exceptions` empty
  (`deny.toml`:124-138) — plus `[bans]`'s empty `deny` list (`deny.toml`:158)
  and `[advisories]`'s empty `ignore` list (`deny.toml`:75). These are a
  constraint on the choice rather than a reason not to make it.

  **What #35 closed is narrower than the issue it was filed under, and this
  document has to say so**, because provider trust is this model's central
  residual and #35 was framed around "a workspace whose roadmap is hosting
  third-party plugin code". The gate reads the **lockfile**: it checks the
  crates a `cargo update` pulls in against advisories, licenses, bans and
  sources. It does not read a third-party provider's code, and nothing in the
  workspace does — the extension store's PR review (spec §6) is still the only
  gate on that, which is the same gate T14 names and the same one that cannot
  verify a manifest's plaintext-persistence claim. The second limit is in
  `Cargo.toml`'s own comment beside the lint: `unsafe_code = "deny"` covers
  normal compilation of both members, `src/` and `#[cfg(test)]` alike, and
  **does not reach doc tests** — rustdoc compiles each fenced block as a
  separate crate and does not forward the flag, so an `unsafe` block in a doc
  comment passes `cargo test --workspace` with nothing for a grep to find. Both
  limits are recorded in place rather than claimed away; neither has an issue
  of its own, which is why they are stated here.
- **Whether the hash is salted per install.** Unsalted, the same path hashes
  identically on every machine, so one precomputed table serves every user.
  Salted, that is closed, but the salt has to live somewhere — and if it lives
  beside the store it travels in the same backup, which returns the property to
  roughly what it was. This is a real trade and the slice should make it
  deliberately.
- **The stored-format version bump, and what happens to existing files.** This
  is the same load path [#37](https://github.com/pedrosousa13/hop/issues/37)
  and [#38](https://github.com/pedrosousa13/hop/issues/38) have already
  changed, that [#43](https://github.com/pedrosousa13/hop/issues/43) and
  [#44](https://github.com/pedrosousa13/hop/issues/44) have changed since, and
  that [#72](https://github.com/pedrosousa13/hop/issues/72) and
  [#88](https://github.com/pedrosousa13/hop/issues/88) still target. It is no
  longer a free bump: #38 landed `STORE_VERSION` and the refusal of a mismatch
  in *both* directions, an older store and a newer one alike, so bumping the
  version discards every existing file rather than migrating it. The **probe**
  that reads the version ahead of either full parse is #43's, not #38's —
  `056893e` replaced #38's two per-branch checks precisely because a v2 that
  changed shape, which is the reason to bump a version at all, reported
  `Malformed` under them. That matters to this bullet directly: the probe is
  what makes a shape-changing bump report `UnrecognizedVersion` rather than
  corruption. #72's third question asks precisely for this to be sequenced
  rather than run in parallel.
- **How the empty-query view behaves for a provider that did not opt in.** The
  consequence above settles the rule — those items are learned and not
  renderable there — but not what the view *shows* in their place: a gap, a
  built-ins-only list, or something that tells the user learning is working
  even though the row is absent. M3 builds that screen (spec §8), and it should
  arrive knowing this rather than discovering it.

---

## What this model does not cover

- **The client side.** `hop-gtk`'s handling of items, icons, clipboard writes
  and URL opens is M3/M4 work with its own sweep (spec §13).
- **The provider seam beyond what crosses the socket.** Panic isolation
  ([#29](https://github.com/pedrosousa13/hop/issues/29)), budget enforcement
  ([#28](https://github.com/pedrosousa13/hop/issues/28)) and the boost-theft
  residual ([#72](https://github.com/pedrosousa13/hop/issues/72)) are in-process
  concerns that the M2 sweep covers.
- **Network providers.** None exist. A10 (SSRF) was recorded not-applicable by
  the M1 sweep for that reason and re-runs at M5 against real providers.
- **Connection-level denial of service.** An accept loop exists now (#54), but
  connection count, aggregate memory across connections, accept rate and read
  timeouts are [#98](https://github.com/pedrosousa13/hop/issues/98)'s and are
  modelled there, not here — T13 records the exposure and names the owner.
- **A root-equivalent adversary**, and anything reachable by inheriting an open
  descriptor from the user's own processes.

## Follow-up

What has to be true for this model to describe reality rather than intent:

| Slice or issue | What it must establish |
| --- | --- |
| [#54](https://github.com/pedrosousa13/hop/issues/54) | Socket and directory created with a decided mode; frame cap from a `hop-protocol` constant, checked before allocation (#21); handshake-first ordering enforced (#26) |
| [#55](https://github.com/pedrosousa13/hop/issues/55) | **Landed.** Per-query state, server-side cancellation and client-side stale-frame drop, and the retained item set Decision 1 rides on: one set per connection, holding the most recent query id's delivered items, replaced whole by the next `Query` and released when the connection closes. Capped by `hop_protocol::limits::MAX_ITEMS_PER_QUERY` = 5 000 — by item **count**, not bytes — enforced by truncating the undelivered remainder, never by evicting delivered ones (T3, and [#85](https://github.com/pedrosousa13/hop/issues/85), which owns the cap). Decision 1's "rides on state the daemon needs anyway" reasoning therefore holds. Two things #55 deliberately did **not** take: bytes, which rest on per-item bounds — **[Amended 2026-08-04]** #56 landed the provider host without adding that enforcement, and issue #30 owns it — and connection-level bounds, which are #98's (T13) |
| [#56](https://github.com/pedrosousa13/hop/issues/56) | **[Amended 2026-08-04] Landed.** The provider host: each provider's manifest captured once at registration and compared against a fresh call before its answer is accepted, catching a provider that answers differently after registration; a host-enforced per-provider budget that aborts a non-cooperating provider's task; panic containment via `tokio::spawn`/`JoinError`; and provider error text bounded and stripped (`hop-core`'s `sanitize` module) before it can leave. Two things #56 deliberately did **not** take: per-item field-length enforcement on items a provider returns in-process, which remains open with issue #30 owning it; and a panic hook, so a panicking provider's payload still reaches the daemon's stderr through Rust's default hook, unsanitized, before the host's own failure classification runs — issue #104 owns that decision |
| [#59](https://github.com/pedrosousa13/hop/issues/59) | Decision 1's binding, including the action check, refusing with the existing error codes — and enforcing the retained-set cap #55 sets, with an item lost to that cap distinguishable from one the daemon never emitted (#85) |
| [#85](https://github.com/pedrosousa13/hop/issues/85) | The per-query total cap itself, as the standalone record #55 and #59 carry as acceptance criteria: the number and its reasoning, whether it bounds item count or total bytes or both, and whether overflow is a refusal or a **rejection** — never a silent truncation. #55 answered the first two — 5 000, item count only, reasoning in `hop_protocol::limits` — and left the third half-answered: the daemon truncates the undelivered remainder rather than evicting what it delivered, but says nothing on the wire that lets a client tell a capped exchange from a completed one, so its half is still a truncation and not a refusal. See Decision 1's settled answers |
| [#60](https://github.com/pedrosousa13/hop/issues/60) | A real state directory, which is where `learning.json`'s path stops being hypothetical |
| [#62](https://github.com/pedrosousa13/hop/issues/62) | Socket activation, which moves socket creation into a unit file |
| [#39](https://github.com/pedrosousa13/hop/issues/39) | Decision 2's rule, sequenced with #72 and #88 on the load path #37, #38, #43 and #44 have already changed — plus the `ProviderManifest` opt-in field the recents consequence needs, which does not exist today (`provider.rs`:62–78) and changes the plugin seam. The field's **default is an open question**, not something this model settles |
| [#57](https://github.com/pedrosousa13/hop/issues/57), M5 providers | Whatever the manifest field's default turns out to be, applied to each built-in provider: either each one declares whether its ids are safe to persist in plaintext, or the default covers those that say nothing. Open until #39 decides the default |
| M3 (spec §8) | The empty-query view's behaviour for a provider that did not opt in — learned, ranked, and absent from that screen |
| [#93](https://github.com/pedrosousa13/hop/issues/93) | The icon-root check #24 deliberately left out, and the open half of T8's pair: allowed roots computed at startup from `XDG_DATA_DIRS` and the icon theme spec's locations, enforced by whatever resolves the path, and checked against what the path resolves to rather than against the string |
| [#83](https://github.com/pedrosousa13/hop/issues/83) | The open half of T9's pair: `RoutedQuery` holds the term as a plain `String` under a derived `Debug`, so the redaction `QueryText` applies in `hop-protocol` stops at `route`, which takes a `&str` |
| [#88](https://github.com/pedrosousa13/hop/issues/88) | The open half of T10: an integrity check on the store, which is what closes both residuals #37 and #38 left — the forged-but-plausible store, and eviction's preference for the entry #38's clamp stamps at the load instant |
| [#52](https://github.com/pedrosousa13/hop/issues/52) | The M2 sweep, auditing the code rather than inheriting this document's verdicts |
