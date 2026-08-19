# Threat model — the hopd socket boundary

Date: 2026-08-02
Issue: [#53](https://github.com/pedrosousa13/hop/issues/53)
Milestone: M2 — Daemon
Status: Recorded; amended 2026-08-04, 2026-08-06, 2026-08-10, 2026-08-17, 2026-08-18, 2026-08-19
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

**Code citations.** Pointers into source name a stable item (normally with its
file or module), never a current line number; commit SHAs remain the evidence
mechanism. **[Amended 2026-08-18]**

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

**Amendment, 2026-08-06.** T3's row picked up an inline note — "amended
2026-08-06 for #103's replace-frame assembly" — when issue #103 replaced the
daemon's accumulate-across-frames model with the replace-frame rule, but no
`Amendment,` block was written for it and the `Status:` line above was never
updated to carry the date. That is the same lapse issue #102 was filed to
fix, one slice earlier than the commits it names. This entry exists so the
convention's own record is honest about it: T3's row is correct as written,
and it stands as this date's amendment on its own account, folded into the
2026-08-10 entry below because it was never given one of its own at the time.

**Amendment, 2026-08-10.** Issue #102's audit, covering everything #55's
landing (2026-08-04's amendment) did not reach and the 2026-08-06 lapse just
above, plus the two sites #99 named that #102 had not yet reached when this
entry was first written. Three kinds of change, in three passes.

First, a sweep corrected drifted `file.rs:NN` references throughout — mostly
in `wire.rs`, whose module doc grew a "Where peer trust comes from" section
that pushed everything below it down, and in `limits.rs`, `content.rs`,
`learning.rs`, `provider.rs`, `pipeline.rs` and `lib.rs`, all of which grew
substantially over M2's slices. These are pure pointer maintenance — no
claim changed, only where to find the thing it was always claiming — so each
is corrected in place and marked `[Amended 2026-08-10]` without further
comment. The full before/after list is in this issue's closing report rather
than repeated here.

Second, six sites stated a claim that later code falsified outright: the
"Entry points that are not frames" bullets on the length prefix ("No codec
and no byte cap exist (#21)"), the connection itself ("no accept loop
exists... they belong to #54 and #55" — the exact sentence #55 was raised to
correct, corrected in T13 and in "What this model does not cover" but left
standing here), the socket path and socket activation (both written
forward-looking); "The boundary"'s opening ("Neither the directory nor the
socket is created by any code in the workspace yet"); and "Exposure paths
off the machine"'s diagnostics-bundle bullet ("no CLI crate exists," which
also self-contradicted the document's own Actors table and Decision 1). A
threat model is a record of an analysis made at a point in time, and
rewriting a falsified claim destroys the record of what the model was drawn
against — so none of these six were rewritten. Each keeps its original
wording and gets a `[Amended 2026-08-10]` annotation immediately after,
naming what has since landed and the commit, in the shape the learning-store
section already uses for `96d5713`/`59fd5fe`/`056893e`/`edb8258`: #21 and #54
by the same commit, `ac782c7` (the walking skeleton PR built the frame codec
and cap alongside the accept loop and socket handling); #62 by `294836b`.

Third, the same treatment applies to every place a claim changed for reasons
other than pointer drift: the Assets table's "Exists today?" column for
action execution, the item stream, the learning store and the daemon's
availability (#57 `da5f65f`, #58 `3b53a7a`, #60 `6ef926d`, #54/#62 above);
the query-text asset row, the "No audit trail" bullet, T9, and the #83
Follow-up row, all of which described `RoutedQuery.term` as an unredacted
plain `String` with #83 open (#83 closed by `8bd6550`, giving `RoutedText`
the same redacting shape `QueryText` has); T5, T6, Decision 1's "not settled"
paragraph, and the #59/#85 Follow-up rows, which posed "distinguishable from
one the daemon never emitted" as open (#59 closed by `4c1aff4`, and settled
it by ruling the two are *not* worth distinguishing — the opposite of what
this document expected, which the annotations say plainly); Decision 1's
byte-figure paragraph and the #55/#56 Follow-up rows, which charged issue
#30 with the field-length gap (#30 closed by `80b7ffd`, via
`CheckedItems::check`'s `FailedCheck::FieldTooLong`); T7 (#61 also closed by
`80b7ffd`, whose same PR introduced `rank.rs`'s `MAX_TERM_CHARS` truncation;
#46 closed separately, by `85b4c2f`, which turned that truncation into a
configurable knob and added `hopd`'s loader enforcement); and Decision 2's "Where today's
code stands" bullet on provider ids (same `da5f65f`/`3b53a7a`). The "No audit
trail" bullet also named #34 as open; #34 is closed (`ad038d5`), but for the
provider seam's own logging, a different crossing from the one that bullet
is about. Every one of these keeps its original claim intact and carries the
annotation after it, not instead of it — including where the claim was only
partly falsified, the same way #85's residual is carried past #59's landing.

**Amendment, 2026-08-17.** Issue #88's authenticated learning-store slice
landed in `hop-core` and closes T10's store-integrity residual. The v2
envelope carries the version, provider-scoped learning entries and an
HMAC-SHA256 tag over a deterministic sorted serialization of that payload.
The key is a 32-byte sibling `learning.key`, initialized when the first save
reaches key creation after parent-directory setup, with exclusive creation at
mode 0600 on Unix and reused without rotation. Once fully written and synced,
it remains durable if that save's later store replacement fails, and later
saves reuse it. Loading bounds the store before consulting the key, then
verifies the tag before applying count bounds, timestamp clamping or
retention. A missing, unreadable or wrong-length key is reported separately
from an HMAC mismatch; both fail closed to empty learning.
Unsigned v1 stores are refused as an unrecognized version rather than treated
as trusted state.

This is option A's deliberate boundary: a store-only writer, or a store copied
without its sibling key, is detected; an attacker or process that can also
read `learning.key` can compute a valid tag and remains outside the guarantee.
The store remains plaintext and this change does not claim confidentiality or
protection from a process that can read the state directory.

This document's remaining residual findings — #25, #34's provider-seam
descendants, #72, #78, #85's wire-signal half, #93, #104 — are unchanged by
this pass: closing a stale note is not the same claim as closing a gap, and
nothing here asserts the latter where the code does not. **[Amended
2026-08-18]** #98 is no longer a residual: the connection-level bounds below
are implemented as same-uid robustness controls, not as a hostile-peer
security boundary.

**Amendment, 2026-08-18.** Issue #75's learning-lookup optimization changes
T7's query-path cost claim. `MAX_QUERY_TEXT` remains the daemon's wire cap;
direct embedders own upstream lookup-cost bounds; `Pipeline::assemble` prepares
the routed term once and reuses it across candidates; and issue #22's bound on
stored normalized `selections` keys remains a separate storage rule.

**Amendment, 2026-08-18.** Issue #146's citation sweep updates source pointers
and the code-citation convention only, not claims. Numeric source-line
pointers now name stable code items; commit-SHA evidence, residual statuses and
pre-existing amendment annotations are unchanged.

**Amendment, 2026-08-10.** A second, later amendment sharing this document's
date with the one above — issue #102's audit and this one are different
events that happen to fall on the same calendar day. This one records issue
#39's own implementation landing (`193dc4d`, `e83c373`), Decision 2's shape
half: `persistence_key` (`hop-core`'s `learning.rs`) now decides, at
`Learning::record` and every `global_frequency` lookup, whether a raw id
persists in the clear or as `sha256:<hex>`. Four things follow.

First, the passage under "What the contract enforces today" headed "Query
text is not written by `Learning`'s persistence path" is falsified as a
claim about the whole persistence path, though it is exactly true of what it
is actually about, `selections`: the same path also writes
`global_frequency`, and the calculator provider (#58, `3b53a7a`) mints an
item id as `calc:{term}` straight from the routed query text
(`crates/hopd/src/calculator.rs`'s `build_item` [Amended 2026-08-18]), so every
launched calculation reached
`learning.json` verbatim, as its id, from #58's landing until this round's
fix. Per this document's rule, the claim is not rewritten — it is annotated
in place.

Second, T11's "Today" column and Decision 2's "Where today's code stands"
both describe `canonicalize_result_id`'s fall-through — an id matching
neither `utility:` nor `web-search:` written to disk unchanged — as the live
behaviour. It no longer is: `persistence_key` sits between that fall-through
and the disk now. Both passages were accurate when written and are
annotated in place rather than rewritten.

Third, "What the implementing slice must still settle" posed six open
questions. Three are answered by this landing and marked so: which shapes
count as known-safe (exactly `app:`, `utility:<kind>` and
`web-search:<service>`, independent of any manifest flag), the hash function
and its dependency (`sha2`, clean against `cargo deny check`), and the
stored-format version bump (none — `STORE_VERSION` stays 1, and a legacy
entry is re-keyed as it loads instead). Three remain open and are marked as
still open rather than left to look resolved by proximity: the manifest
opt-in field, still absent from `ProviderManifest` and riding with #72;
whether the hash is salted, left unsalted deliberately this round because
issue #88's decided `learning.key` sibling file is the natural future home
for a salt, and a second key file before that one exists was not worth
inventing; and the empty-query view's behaviour for a provider that did not
opt in, still M3's.

Fourth, one accepted residual this landing introduces and documents rather
than closes: a legacy store already holding a plaintext key shaped exactly
like `sha256:` plus 64 lowercase hex digits cannot be told apart, on load,
from a key this module hashed itself — `stored_key_needs_no_rekeying`'s own
doc comment (`learning.rs`) states why nothing in the v1 format distinguishes
the two. No passage in this document claimed otherwise, so nothing needed
annotating for it; it is recorded here so a reader of Decision 2 knows it
exists.

Each changed passage below is marked **[Amended 2026-08-10]** in place, the
same shape this document's other amendments already use.

**Amendment, 2026-08-10.** A third amendment sharing this document's date
with the two above — issue #72's own implementation landing (`4f5acf9`,
`0c50a98`, `9a595bb`), the provider dimension in the store key and the
manifest half of Decision 2's consequence. Unlike the two amendments above,
this one does not merely correct pointers or note landings elsewhere: it
supersedes part of what the second 2026-08-10 amendment recorded, because
#72's landing did not simply add to #39's shape half — it retired it. Four
things follow.

First, `persistence_key` no longer looks at a raw id's shape at all.
`is_known_safe_shape` and `canonicalize_result_id`'s known-safe machinery
(`learning.rs`) are deleted, not merely bypassed, and `ProviderManifest`
gained `ids_are_safe_to_persist_in_the_clear` (`provider.rs`) — a required
field with no default, so a manifest literal that omits it does not compile.
That flag, read at `Learning::record` and every `global_frequency` lookup via
`Learning::sync_plaintext_providers`, is now the sole authority deciding
plaintext versus hash; no shape, built-in or otherwise, is special-cased. A
provider absent from the synced set — one that never registered, or one this
process has not learned about yet — hashes by the same default, meeting
issue #72's fail-closed requirement. The second amendment's claim that "which
shapes count as known-safe" was settled by #39's landing was true of what
#39 shipped; it is not rewritten, per this document's rule, and is annotated
in place below, at Decision 2's "Where today's code stands" and "What the
implementing slice must still settle," and at T11.

Second, the store key gained a provider dimension. `persistence_key` now
takes the producing provider alongside the raw id, and folds both into one
key by a composition proven injective in `provider_scoped_key`'s own doc
comment (`learning.rs`): no two distinct `(provider, id)` pairs can ever
produce the same key, so a provider cannot forge another provider's
persisted key merely by choosing what it puts in its own id or provider
string. This closes T12, "cross-provider boost theft," and the residual
named under "Trust directions, stated" — both annotated in place below. The
identical dimension was added to `rank.rs`'s `Boosts::by_item_id`, closing
the same gap one layer up: before that fix, an honestly-declared hostile
provider whose item shared a genuine provider's id inherited whatever the
genuine item had already added to a bare-`ItemId`-keyed slot, even though the
persisted side was by then already provider-scoped. Both fixes together are
what issue #31 asked for and left half met — see the plan
(`docs/superpowers/plans/2026-08-10-issue-72-provider-dimension.md`) and
`provider.rs`'s own doc comment on `APPS_PROVIDER_ID` for that history.

Third, the legacy-store migration this document already described changed
again. The second amendment recorded `rekeyed_global_frequency` as migrating
"a legacy entry to its persistence key as the store loads." Under #72's
landing it does something narrower: a legacy `app:`-prefixed key is
re-attributed to `APPS_PROVIDER_ID` — the one legacy shape with a single
honest owner, since no other provider has ever minted an `app:` id — and
every other legacy shape, including the `sha256:`-shaped entry the second
amendment's own residual paragraph named, is dropped rather than carried
forward: a hash taken without the provider that earned it can never match a
fresh, provider-scoped lookup regardless of what this pass did with it.
`STORE_VERSION` still does not bump. The residual the second amendment
recorded — a legacy plaintext key indistinguishable, on load, from this
module's own hash output — no longer needs distinguishing, since both are
dropped now along with every other unrecognized legacy shape.

Fourth, two of the six questions "What the implementing slice must still
settle" posed are now answered and marked so below: the manifest field's
name and default (`ids_are_safe_to_persist_in_the_clear`, required, no
default), and which shapes count as known-safe (none — there is no shape
list left to relate the opt-in to). The salt question and the empty-query
view's behaviour are unchanged by this landing and remain open, exactly as
the second amendment left them.

Each changed passage is marked **[Amended 2026-08-10]** in place below, the
same shape the two amendments above already use.

**Amendment, 2026-08-19.** Issue #158 changed "The socket path" bullet under
"Entry points that are not frames": the standalone branch of
`acquire_listener` (`server.rs`) no longer removes whatever sits at the
socket path unconditionally before binding. It now probes the path with a
real connect attempt (`probe_socket_liveness`, `server.rs`) and refuses
outright — `ListenerError::AlreadyListening`, no `remove_file`, no `bind` —
when that probe finds a live listener already answering there; the
unconditional `remove_file` only still runs on the two outcomes that mean
nothing live is at the path (`ECONNREFUSED`, `ENOENT`). The bullet is left
as originally written and annotated in place, per this document's own rule,
rather than rewritten.

Two things #158 does not change, both worth stating because a narrower fix
could plausibly have undone them. First, the reason the original removal
tolerated only `NotFound` — no `exists()`-then-`remove_file` TOCTOU window,
no blind spot for a dangling symlink `exists()` reports as absent — is
exactly the reasoning #158's own connect probe relies on too: it asks the
kernel the liveness question directly, the same way a real client's connect
already does, rather than stat-ing first and racing what it saw. #158 adds a
liveness question ahead of the removal; it does not relax what the removal
itself tolerates once it runs. Second, the socket file's 0600 mode, narrowed
by `set_permissions` right after `bind`, is untouched — #158's new branch
returns before `bind` is ever reached on the live-listener path, so it has
no interaction with mode at all.

This is a lifecycle and availability control, not a new authentication
boundary: "Where peer trust comes from" already treats a hostile same-uid
process as inside the boundary, and #158 does not change who is trusted to
reach the socket — only whether a second `hopd` may silently take over a
live listener's pathname out from under the clients already depending on it.
`acquire_listener`'s own "# A live listener's pathname is never replaced
(#158)" doc section (`server.rs`) says the same thing from the
implementation side: the probe closes the specific failure #158 was filed
for — an established, serving daemon losing its name to a second, later
start — and leaves ordinary `bind`-time contention between two daemons
racing to claim a path neither has bound yet exactly as unarbitrated as it
was before, which is a starting-order coin flip with no established victim
rather than the displacement #158 closes.

Each changed passage below is marked **[Amended 2026-08-19]** in place, the
same shape this document's other amendments already use.

**Amendment, 2026-08-19.** A second amendment sharing this document's date
with the one above — issues #85 and #162 landed the same day #158 did, and
neither was covered when that entry was written. Five passages change.

First, Decision 1's overflow paragraph and T3's row both described the
daemon-side half of #85's question as still a silent truncation. It no
longer is: `absorb_capped` (`source.rs`), which `HostSource::start`'s
accumulator runs each arrival through, now folds one
`FailedCheck::TooManyItemsPerQuery` rejection in alongside the items it
keeps — by way of `hop-core`'s
`CheckedItems::truncate_items_recording_overflow` (`pipeline.rs`), where the
rejection is minted — whenever an arrival crosses `MAX_ITEMS_PER_QUERY`. The peer-visibility
half is unchanged and stays true: nothing on the wire distinguishes a capped
exchange from a completed one, so the rejection is recorded internally but
never signalled to the client. Both passages are split rather than struck,
per this document's rule, so the sentence that conflated the two halves
still stands with the settled half marked apart from the one that is not.

Second, the Follow-up table's #85 row presented #85 itself as open. It has
since landed (`e0c295e`) and, per the first change above, closed the
daemon-side half of the question it was filed to answer. The wire-signal
half is not part of what #85 closed and stays open exactly as Decision 1's
settled answers already record it, and as the follow-up table's own #59 row
poses the distinguishability question it is charged with.

Third, the Assets table's "Action execution" row said the apps provider
launches applications via `std::process::Command`. Issue #162 (`f0784db`)
changed that to `tokio::process::Command` (`apps.rs`, `SystemLauncher::launch`),
which is what gives a spawned child a reaping path — see that struct's own
doc comment for why.

Fourth, the check table's "Does any code spawn a process?" row is corrected
rather than left as a point-in-time scan, per this document's own preamble
and its two prior amendments: it already disagreed with the Assets table row
above it before #162 ever landed, since the apps provider has spawned
processes since #57/#58. The row's stated grep, re-run against the current
tree, now returns nineteen hits rather than two, one of them the apps
provider's production spawn; "no non-test code spawns anything" no longer
holds.

Each changed passage below is marked **[Amended 2026-08-19]** in place, the
same shape this document's other amendments already use.

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
| Does any code spawn a process? | `grep -rn "Command::new" crates/` | Two hits, both `std::process::Command::new("mkfifo")` inside `learning.rs`'s `#[cfg(test)]` module (the first scan, now the same module [Amended 2026-08-10] [Amended 2026-08-18]). No non-test code spawns anything. **[Amended 2026-08-19]** The same grep, re-run against the current tree, now returns nineteen hits — the pattern already covers `tokio::process::Command::new` as well as `std::process::Command::new`, since both end in the literal substring searched for. Most of the growth is test harnesses (`hop-cli`'s and `hopd`'s integration tests spawning the built binaries under `tests/`) plus the same two `learning.rs` `#[cfg(test)]` hits; two sit outside a test, and only one of those is code — the apps provider's `SystemLauncher::launch` (`apps.rs`) spawns via `tokio::process::Command`, live since #57/#58 (`da5f65f`/`3b53a7a`) and switched from `std::process::Command` by #162 (`f0784db`) — see the Assets table's "Action execution" row; the other is a comment in `apps.rs` quoting the pattern, which the grep cannot tell from a call and which a later re-run of this row should expect to find. Non-test code does spawn a process. |
| Is there a frame codec or a frame-size cap? | `grep -rn "FRAME" crates/hop-protocol/src/` | Matches outside doc prose are `MAX_ITEMS_PER_RESULTS_FRAME` and its uses — an item-count bound, not a byte cap. [#21](https://github.com/pedrosousa13/hop/issues/21) is open. **[Amended 2026-08-10]** #21 is now **closed** (`ac782c7`) — `hop-protocol`'s `framing` module and `MAX_FRAME_BYTES` answer this; see "Entry points that are not frames". |
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
point, and #54 is where they become behaviour. **[Amended 2026-08-10]** #54
is now **closed** (`ac782c7`): `crates/hopd/src/runtime_dir.rs` and
`server.rs` create both — the directory at 0700, the socket at 0600 — so the
three properties below are behaviour rather than intent. The bullets are left
as written, since they still name the file that carries each rule correctly;
only the "yet" above and the "should decide" below are what landed since.

- **The directory's mode.** 0700 withholds traverse permission from other
  uids. There is an in-repo precedent for creating it correctly:
  `learning.rs`'s `persist_atomically` [Amended 2026-08-10] [Amended 2026-08-18]
  passes the mode as an argument to `mkdir(2)` through `fs::DirBuilder`, so the
  directory is born at 0700 with no window at a wider mode to race, and a
  pre-existing path is left as found rather than chmodded. The same reasoning
  applies to the runtime dir.
- **The socket file's own mode and owner.** The spec fixes the directory's
  mode and says nothing about the socket's. Connecting needs traverse on the
  directory *and* write on the socket, so the directory carries the control
  as designed — but leaving the socket's mode unstated means it will be
  whatever the umask makes it. #54 should decide it rather than inherit it.
  **[Amended 2026-08-10]** It did: `acquire_listener` (`server.rs`, `ac782c7`)
  narrows the socket to 0600 with `set_permissions` right after `bind`.
- **`$XDG_RUNTIME_DIR` is an environment variable the user controls.** It is
  0700 and user-owned on a systemd session, which is the case the spec
  assumes; it is not a guarantee the daemon can make about a value handed to
  it. This is the same shape as the reasoning already written down in
  `learning.rs`'s `persist_atomically` [Amended 2026-08-10] [Amended 2026-08-18]
  about `XDG_STATE_HOME` — a path derived from user-controlled environment is
  not a path the process can reason about unaided.

---

## Assets behind the boundary

What a peer that reaches the socket gets access to, in rough order of what
would be worst to lose.

| Asset | What it is | Where it lives | Exists today? |
| --- | --- | --- | --- |
| **Action execution** | The daemon acts on the user's behalf: launching applications, focusing and closing windows, opening URLs. This is the asset that makes the boundary worth modelling. | Spec §5's provider table; `ExecOutcome` in `wire.rs` [Amended 2026-08-10] [Amended 2026-08-18] | No. No provider is implemented and no code spawns a process. **[Amended 2026-08-10]** #57 (`da5f65f`) and #58 (`3b53a7a`) are closed: the apps provider launches applications via `std::process::Command`, and the calculator provider answers real queries; window focus/close is not yet a provider. **[Amended 2026-08-19]** #162 (`f0784db`) is closed: the apps provider now launches via `tokio::process::Command` (`apps.rs`, `SystemLauncher::launch`), which is what gives a spawned child a reaping path. |
| **The item stream** | Titles, subtitles, icon paths and ids describing the user's installed applications, open windows and — from M5 — files. The items a query answers with are a description of the user's machine. | `Item` in `item.rs` [Amended 2026-08-18] | The type exists; nothing produces real items. **[Amended 2026-08-10]** #57 and #58 (`da5f65f`, `3b53a7a`) are closed: the apps and calculator providers now produce real items from the user's machine; windows and files remain unimplemented. |
| **Query text** | Keystrokes typed into the launcher overlay, which can be a pasted credential. | `QueryText` in `redaction.rs` on the wire; the same text again as the pre-#83 `RoutedQuery.term` field, a plain `String`, once `hop-core` has routed it (the pre-#83 `RoutedQuery::term` in `router.rs`) [Amended 2026-08-18] | Both types exist. Only the first redacts — see T9 and #83. **[Amended 2026-08-10]** #83 (`8bd6550`) is closed: `RoutedQuery.term`/`raw` are now `RoutedText` (`RoutedQuery` in `router.rs`), which redacts under `Debug` the same way `QueryText` does — see T9. |
| **The learning store** | Per-item launch frequency, persisted. Reveals what the user launches and, through ids, what they launch it *on*. | `$XDG_STATE_HOME/hop/learning.json`, mode 0600, 90-day retention (`learning.rs`) | The store exists; no code computes its path yet ([#60](https://github.com/pedrosousa13/hop/issues/60)). **[Amended 2026-08-10]** #60 (`6ef926d`) is closed: `hopd/src/state_dir.rs` computes the real path and `HostSource::record_launch` persists to it. |
| **The client's clipboard and URL handler** | `ExecOutcome::CopyText` and `ExecOutcome::OpenUrl` are instructions to a client, not reports. | `content.rs` | Types and content rules exist; no client. |
| **The daemon's availability** | A resident, socket-activated process the user's launcher depends on. Losing it loses the launcher. | Spec §3, [#62](https://github.com/pedrosousa13/hop/issues/62) | No. **[Amended 2026-08-10]** #54 and #62 (`ac782c7`, `294836b`) are closed: `hopd` is a resident accept loop (`server.rs`) startable standalone or via systemd socket activation. |
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
| **The daemon, toward a client** | The reverse direction of the same socket | Bounds on `DaemonMsg`. `DaemonMsg` in `wire.rs` [Amended 2026-08-10] [Amended 2026-08-18] states the reason: "A client trusts its daemon no more than the daemon trusts its clients." |
| **A provider, in-process** | Not across the socket — but a provider supplies the values that cross it | **[Amended 2026-08-04, 2026-08-10]** `CheckedItems::check` (`pipeline.rs`) [Amended 2026-08-18] holds each item to the manifest of the provider that produced it, called through `ProviderHost::run_one` (`hop-core`'s `host.rs`, issue #56), which also compares the manifest it captured at registration against a fresh call before accepting a provider's answer, runs the provider under a host-enforced budget it aborts on a miss, contains a panicking provider's failure at the `tokio::spawn` seam, and bounds and strips a provider's error text before it can leave. `content.rs` module docs [Amended 2026-08-18] state the residual: "the daemon is trusted" degrades to "every installed provider is trusted". A further residual #56 left open, owned by issue #104: a panic *payload* still reaches the daemon's stderr through Rust's default panic hook, unsanitized, before the host's own failure classification runs. |
| **A remote network actor** | No route today | Neither crate depends on an HTTP client, and nothing listens on a TCP socket. This changes at M5. |

The row that matters is the second one. It is the boundary's actual shape: the
control is *which uid*, and there is no finer distinction available.

---

## Entry points

### Frames from a client

| Frame | Fields | Enforced today, and where | Not enforced |
| --- | --- | --- | --- |
| `hello` | `api_version: u32` | Fixed-width integer; nothing to bound. `ClientMsg::Hello` in `wire.rs` [Amended 2026-08-10] [Amended 2026-08-18] | That it arrives first — nothing in the type requires it ([#26](https://github.com/pedrosousa13/hop/issues/26)) |
| `query` | `id: u64`, `text: QueryText` | `MAX_QUERY_TEXT` = 1 024 bytes, applied by `QueryText`'s `Deserialize` implementation in `redaction.rs` [Amended 2026-08-18] through its constructor (`QueryText::new` in `redaction.rs`) [Amended 2026-08-18] | How many query frames arrive, and how fast — no read loop exists to bound either |
| `cancel` | `id: u64` | Fixed-width | That the id names a live query |
| `execute` | `query_id: u64`, `item_id: ItemId`, `action_id: ActionId` | Length bounds only — `MAX_ITEM_ID` = 4 096, `MAX_ACTION_ID` = 128 (`ItemId::new` and `ActionId::new` in `item.rs`) [Amended 2026-08-18] | That the ids name anything the daemon delivered. This is [#25](https://github.com/pedrosousa13/hop/issues/25), settled below |

### Frames from the daemon

| Frame | Fields | Enforced today, and where |
| --- | --- | --- |
| `hello_ack` | `api_version: u32` | Fixed-width. Negotiates no capability set (`DaemonMsg::HelloAck` in `wire.rs` [Amended 2026-08-10] [Amended 2026-08-18]) |
| `results` | `query_id`, `partial: bool`, `items: Vec<Item>` | `MAX_ITEMS_PER_RESULTS_FRAME` = 1 000, applied at the parse (`BoundedVec::visit_seq` in `limits.rs` [Amended 2026-08-10] [Amended 2026-08-18]) and refusing on the element past the maximum without reserving capacity for a peer-claimed length (`BoundedVec::visit_seq` in `limits.rs` [Amended 2026-08-10] [Amended 2026-08-18]) |
| `query_done` | `query_id` | — |
| `executed` | `query_id`, `outcome: ExecOutcome` | `CopyText` and `OpenUrl` are validating newtypes carrying content rules as well as bounds (`content.rs`) |
| `error` | `query_id: Option<u64>`, `error: ProtoError` | `MAX_ERROR_MESSAGE` = 1 024 on the message (`limits::de_error_message` in `limits.rs`) [Amended 2026-08-10] [Amended 2026-08-18] |

### Entry points that are not frames

- **The length prefix.** The framing the spec mandates gives the peer control
  of an allocation size. No codec and no byte cap exist
  ([#21](https://github.com/pedrosousa13/hop/issues/21)); #54's acceptance
  criteria require the cap to be a constant exported by `hop-protocol` and
  checked before any allocation sized by the prefix.
  **[Amended 2026-08-10]** #21 and #54 are now **closed**, by the same
  commit (`ac782c7`, the walking skeleton PR): `hop-protocol`'s `framing`
  module is the codec, and `MAX_FRAME_BYTES` (`limits.rs`) is the byte cap,
  applied by `framing::payload_len` — the pre-allocation gate — before a
  caller reads or allocates the payload the prefix describes, and wired into
  `hopd`'s `read_frame` (`connection.rs`), which calls it on every frame,
  refusing a length prefix over the cap (`ErrorCode::FrameTooLarge`) on the
  four prefix bytes alone, before this connection reads a byte of the
  payload it names.
- **The connection itself.** The accept loop's connection-level resource
  bounds are deliberate same-uid robustness controls, not a security boundary
  against a hostile peer. **[Amended 2026-08-18]** #98 now enforces a 64-owned-
  permit cap acquired before `accept`, a 64 KiB client-to-daemon pre-allocation
  ceiling, and a 10-second timeout only for completing a payload after its
  prefix. There is deliberately no idle timeout between frames and no
  accept-rate limiter; the existing 50 ms accept-error sleep remains only a
  hot-spin floor. The 64 admitted connections compose to at most 4 MiB of
  inbound payload buffers plus 64,000 retained bounded items.
- **The socket path.** Creating, binding to and unlinking a path inside a
  directory the daemon may not have created. #54.
  **[Amended 2026-08-10]** #54 (`ac782c7`) is closed: `acquire_listener`
  (`server.rs`) removes whatever sits at the socket path unconditionally
  before binding — tolerating only `NotFound`, which is what makes
  restarting after a crash work without a TOCTOU window or a
  dangling-symlink blind spot — and narrows the socket file to 0600 with
  `set_permissions` right after `bind`, decided rather than inherited from
  the umask. **[Amended 2026-08-19]** The "unconditionally" above is no
  longer true, and #158 (`64d319d`) is why: the standalone branch of
  `acquire_listener` now calls `probe_socket_liveness` (`server.rs`) before
  it touches the path at all — a real connect attempt, the same one a
  client would make. A successful connect (`SocketLiveness::Live`) means a
  live `hopd` is already answering, and `acquire_listener` returns
  `ListenerError::AlreadyListening` immediately: no `remove_file`, no
  `bind`. Only the two outcomes that mean nothing live is there —
  `ECONNREFUSED` (`SocketLiveness::Stale`) and `ENOENT`
  (`SocketLiveness::Absent`) — still reach the `remove_file` call this
  bullet describes, unconditionally within *that* narrower case. The
  TOCTOU and dangling-symlink reasoning just above for why the removal
  tolerates only `NotFound` is unchanged and still applies to that same
  `remove_file` call; what changed is only whether the removal runs at
  all. The socket file's 0600 mode, narrowed right after `bind`, is also
  unchanged — the live-listener path returns before `bind` is ever
  reached. See `acquire_listener`'s own "# A live listener's pathname is
  never replaced (#158)" doc section (`server.rs`) for the full reasoning,
  including the residual race #158 does not close (two daemons racing to
  bind a path neither has bound yet) and why this is a same-uid lifecycle
  and availability control rather than a change to "Where peer trust comes
  from" below.
- **Socket activation.** systemd passes a listening descriptor the daemon did
  not create ([#62](https://github.com/pedrosousa13/hop/issues/62)). The unit
  file's socket mode becomes part of the boundary at that point.
  **[Amended 2026-08-10]** #62 (`294836b`) is closed: under activation,
  `acquire_listener` takes the inherited descriptor directly and never
  binds, removes or `chmod`s anything at the runtime dir; the socket's mode
  (0600) and its directory's (0700) are carried instead by
  `contrib/systemd/hopd.socket`'s `SocketMode=`/`DirectoryMode=`, pinned by
  `server.rs`'s `systemd_unit_tests`.
- **The learning store on disk.** Not a socket entry point, but untrusted
  input reaching the same process. The M1 sweep filed **four** gaps against
  this load path — [#37](https://github.com/pedrosousa13/hop/issues/37),
  [#38](https://github.com/pedrosousa13/hop/issues/38),
  [#43](https://github.com/pedrosousa13/hop/issues/43) and
  [#44](https://github.com/pedrosousa13/hop/issues/44) — and all four are now
  closed. The load is bounded and checked: `Learning::load`
  (`Learning::load` [Amended 2026-08-10] [Amended 2026-08-18]), over `Learning::load_reporting`
  (`Learning::load_reporting` [Amended 2026-08-10] [Amended 2026-08-18]), stats for a regular file before the open, reads
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
    entry survives". `Learning::purge_and_bound` [Amended 2026-08-10] [Amended 2026-08-18] prices it: a clamped entry is
    stamped at the load instant, which makes it the newest stamp in the map
    and so the last one `evict_lru_map` drops, so a forged store still holds
    one of `MAX_GLOBAL_ENTRIES`' slots against real learning. What the clamp
    removed is the permanence, not the preference.
  - #38 leaves the integrity check itself: a *plausible* forged store — a
    recent timestamp, the right version, the right shape — passes every guard
    above.

  Both residuals need the same missing thing, and it is
  [#88](https://github.com/pedrosousa13/hop/issues/88), which is open.

**[Amended 2026-08-17]** #88 has landed. The authenticated v2 envelope now
rejects the plausible forged store before clamping or eviction, so a
store-only writer cannot consume an eviction slot. The future-stamp eviction
preference remains a defensive behavior for authenticated data (and for a
process that can read the key), not an integrity bypass; option A does not
cover that key-reading process.

---

## Where peer trust comes from

**From the socket's ownership and the mode of the directory containing it.
The protocol supplies none.**

That is not a summary of something the types express — it is a description of
their silence, and the silence is checkable:

- `ClientMsg::Hello` carries `api_version: u32` and no other field
  (`ClientMsg::Hello` [Amended 2026-08-10] [Amended 2026-08-18]). `DaemonMsg::HelloAck` carries
  `api_version: u32` and no other field (`DaemonMsg::HelloAck` [Amended
  2026-08-10] [Amended 2026-08-18]). Neither carries a credential, a token, a nonce, or a peer
  identifier.
- Both are ordinary variants of the message enums rather than a distinct
  pre-session type, so nothing in the types prevents `execute` as a first
  frame. Whether that is refused depends on daemon-side state the protocol
  does not ask for. This is
  [#26](https://github.com/pedrosousa13/hop/issues/26), folded into #54 as an
  acceptance criterion.
- `SO_PEERCRED` does not appear in the workspace's Rust, Markdown or TOML
  (checked by grep). There is no connection-handling code to consult it in.
- `API_VERSION` (`hop-protocol`'s `lib.rs`) [Amended 2026-08-10] [Amended 2026-08-18] is a compatibility marker.
  It is not an authorization value and nothing treats it as one.

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
  parse (`ClientMsg` [Amended 2026-08-10] [Amended 2026-08-18]).
- A client does not trust the daemon: `DaemonMsg`'s fields are bounded for the
  same reason (`DaemonMsg` [Amended 2026-08-10] [Amended 2026-08-18]).
- The daemon does not fully trust a provider: `CheckedItems::check` holds each
  item to its own producer's manifest, while `ItemTitle` and `ItemSubtitle`
  enforce their own bounded single-line display invariants before an item can
  exist. `CONTEXT.md` names what survives that check **checked items**. The
  residual is recorded in
  [#72](https://github.com/pedrosousa13/hop/issues/72) — the learning store
  keys on a bare item id, so an honestly-declared hostile provider can still
  collect another provider's learned boosts.
  **[Amended 2026-08-10]** #72 (`4f5acf9`, `0c50a98`, `9a595bb`) is now
  **closed**: the learning store and the ranker's in-memory boost maps both
  key on `(provider, id)`, so an honestly-declared hostile provider collects
  nothing from an id it did not itself earn a boost on — see T12 and
  Decision 2, "Where today's code stands."

---

## What the contract enforces today

Recorded so the M2 sweep does not re-derive it, and so the decisions below sit
on a stated baseline.

**Size budget** (`limits.rs`). Enumerating the variable-length fields of
`ClientMsg`, `DaemonMsg`, `Item` and `Action`, each carries a bound: either a
`deserialize_with` target (the bounded deserializers in the `limits` module [Amended 2026-08-10] [Amended 2026-08-18]) or a
validating newtype that applies one. That enumeration is the whole of the
claim — it says nothing about a field added later. The constants:

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
the parse rather than at a later read (`limits` module docs [Amended 2026-08-18]).

**Validating newtypes.** `ItemId` and `ActionId` (`item.rs`), `CopyText`,
`OpenUrl`, `IconName` and `IconPath` (`content.rs`), `QueryText`
(`redaction.rs`) — the seven `CONTEXT.md` names. Each wraps a private `String`
whose constructor applies the rules, and whose `Deserialize` hands the parsed
string to that same constructor — one gate rather than two that happen to
agree.

**Content rules** (`content.rs`). `OpenUrl` requires a scheme from
`ALLOWED_URL_SCHEMES` (`content.rs` [Amended 2026-08-18] — `http`, `https`, `mailto`), refuses
a leading `-`, and refuses ASCII space and any `Cc` control character.
`CopyText` refuses `Cc` controls outside `ALLOWED_COPY_TEXT_CONTROLS`
(`ALLOWED_COPY_TEXT_CONTROLS` [Amended 2026-08-18] — tab and newline). Both arms of an icon carry rules too,
since [#24](https://github.com/pedrosousa13/hop/issues/24) closed: `IconPath`
must be absolute, free of any `..` component, free of NUL and free of control
characters (`IconPath` [Amended 2026-08-10] [Amended 2026-08-18]), and `IconName` must
be non-empty, free of `/` and free of control characters (`IconName` [Amended 2026-08-10] [Amended 2026-08-18]) — the `/` rule being
the one that keeps the two arms apart, so `name` cannot become a second channel
for a path-shaped value that passed none of `IconPath`'s rules.
`content.rs` module docs [Amended 2026-08-18] state what these rules do not close, and
that statement holds here too: an accepted URL is still an arbitrary web
address, accepted copy text is still arbitrary text, and an accepted icon path
names *somewhere* rather than somewhere an icon belongs. #24 closing is
therefore only half of the icon story — the unenforced-roots half is
[#93](https://github.com/pedrosousa13/hop/issues/93) and is open, and it is
recorded under "What the contract does not enforce" below.

**Structural rules that need no validator.** `IconSpec` is an externally tagged
enum (`IconSpec` [Amended 2026-08-18]) whose two arms are `Name(IconName)` and
`Path(IconPath)`, so an icon carrying both a name and a path, and an icon
carrying neither, are values no frame can express — the shape refuses them at
the parse rather than a check having to.

**Redaction** (`redaction.rs`). `QueryText`'s `Debug` prints
`QueryText(<redacted, N bytes>)` (`QueryText`'s `Debug` implementation [Amended 2026-08-18]), so formatting a
`query` frame does not reproduce the keystrokes. The redaction travels with
the value rather than with the frame's `Debug`. It discloses a byte count,
and `QueryText`'s type docs [Amended 2026-08-18] prices that disclosure rather than filing it under
"something about the value".

**Query text is not written by `Learning`'s persistence path.**
`Learning::save` writes a `PersistedLearningStore`, which has no `selections`
field (`PersistedLearningStore` [Amended 2026-08-10] [Amended 2026-08-18]), and the in-memory
`selections` map is `#[serde(default, skip_serializing)]`
(`Learning::selections` [Amended 2026-08-10] [Amended 2026-08-18]). The test
`save_and_load_round_trip_without_persisting_query_keys` asserts the saved
file does not contain the query key. That is a statement about this path in
this module, not about code that does not exist yet.
**[Amended 2026-08-10]** True of `selections`, and only of `selections` — the
paragraph above is about the query key specifically, not about every string
the persistence path writes. `global_frequency`, the map `Learning::record`
also writes on the same path, kept a raw item id verbatim until this round:
the calculator provider (#58, `3b53a7a`) mints an id as `calc:{term}`
straight from the routed query text (`crates/hopd/src/calculator.rs`'s `build_item` [Amended 2026-08-18]),
so every launched calculation reached `learning.json` in the clear, as its
id, from #58's landing until issue #39's fix (`193dc4d`, `e83c373`) — see
Decision 2, "Where today's code stands." The claim about `selections` was
correct and is left as written; it never extended to the id channel, and
this document should not be read as though it did.

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
and handing fields to the bounded deserializers (`limits` module docs [Amended 2026-08-18]). A
  200 MB `query` frame is refused — after 200 MB has been held. The frame cap
  is what closes this; the field bounds complement it.
- **The bounds do not compose to a usable frame ceiling.** The `limits` module docs [Amended 2026-08-18]
  works it out: one item on every one of its bounds is 84 160 bytes, and at
  `MAX_ITEMS_PER_RESULTS_FRAME` that is roughly 84 MB in a single `results`
  frame, entirely within every bound in the module. A test recomputes the
  figure from the constants.
- **Nothing bounds a query's total across frames.** `MAX_ITEMS_PER_RESULTS_FRAME`
  bounds one frame; a daemon may send several `results` frames for one
  `query_id`, each replacing the last in full under the replace-frame rule
(`DaemonMsg::Results` [Amended 2026-08-10] [Amended 2026-08-18]). This matters directly to Decision 1
  below.
- **`Item.copy_text` carries no content rules** — it is a bounded `String`,
  and reaches the same clipboard as `ExecOutcome::CopyText` by a different
  route ([#78](https://github.com/pedrosousa13/hop/issues/78)).
  **[Amended 2026-08-10]** #78 is now **closed** (PR [#133](https://github.com/pedrosousa13/hop/pull/133)): `Item.copy_text`
  is `Option<content::CopyText>` (`CopyText::new_named` [Amended 2026-08-18]), whose only
  constructors validate both length and content — the same rules
  `ExecOutcome::CopyText` already carried — for every value that exists,
  in-process or off the wire, so this bullet's premise no longer holds. The
  two sinks still differ in one respect: which wire field a refusal names,
  `ExecOutcome::CopyText` for an outcome and `Item.copy_text` for an item —
  `content.rs`'s module docs and `CopyText::FIELD`'s own doc comment state
  why.
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
  (`ItemId::new` [Amended 2026-08-18]) checks `MAX_ITEM_ID` and nothing else, so a provider
  chooses freely what goes in an id. This matters directly to Decision 2.
- **No audit trail.** Neither crate depends on `tracing` or `log`, so nothing
  records what crossed the boundary
  ([#34](https://github.com/pedrosousa13/hop/issues/34), open). A daemon that
  adds one inherits [#27](https://github.com/pedrosousa13/hop/issues/27)'s
  hazard, which `QueryText`'s `Debug` pre-empts for the one field that carries
  keystrokes — **in `hop-protocol` only**. #27 is closed and the redaction
  stops at the crate boundary: `hop-core`'s `route` takes a `&str`, and the pre-#83
  `RoutedQuery` it returns (its pre-#83 `RoutedQuery::term` field [Amended 2026-08-18]) derives `Debug` over a plain
  `String` term, so the same keystrokes format verbatim one crate downstream.
  That half is [#83](https://github.com/pedrosousa13/hop/issues/83) and is
  open; `RoutedQuery`'s type docs [Amended 2026-08-18] say so in place.
  **[Amended 2026-08-10]** #34 is now **closed** (`ad038d5`), but for the
  provider seam's own logging (`ProviderLog`/`ProviderEvent`, `hop-core`'s
  `host.rs`) — a different crossing from the one this bullet is about, which
  it leaves exactly as bare. #83 is also **closed** (`8bd6550`): `RoutedQuery`'s
  `term` and `raw` are now `RoutedText` (`RoutedQuery` [Amended 2026-08-18]), which redacts
  under `Debug` the way `QueryText` does, so the same keystrokes no longer
  format verbatim one crate downstream; `RoutedQuery`'s type docs [Amended 2026-08-18] state the change
  in place. The nearest thing to a signal today is narrow, and deliberately:
  [#43](https://github.com/pedrosousa13/hop/issues/43) is **closed**
  (`056893e`), and what it produced is `LoadReport` (`LoadReport` [Amended 2026-08-10] [Amended 2026-08-18]) —
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
| T2 | Memory amplification below the cap, via tagged-enum buffering | Any frame | Bounds apply post-buffer (`limits` module docs [Amended 2026-08-18]) | Frame cap sized against the 84 MB figure in the `limits` module docs [Amended 2026-08-18] |
| T3 | **Unbounded retained item set.** Decision 1 has the daemon retain what it delivered per query id, accumulating across frames; `MAX_ITEMS_PER_RESULTS_FRAME` bounds one frame, and the protocol permits several partial frames per query, so absent a total the retained set would have no ceiling. Reachable by a **well-behaved** client, not only a hostile one | `results`, and Decision 1's registry | **Bounded, by item count, since #55 — amended 2026-08-06 for #103's replace-frame assembly.** The daemon no longer accumulates a retained set across frames. Under the replace rule (#103), `connection.rs`'s `Exchange::delivered` holds only the **last assembled list** for a query id, replaced whole by each `results` frame, and `forward_batch` enforces `MAX_ITEMS_PER_RESULTS_FRAME` = 1 000 on it — defensively, since the **result source** is untrusted — truncating an over-long arrival to fit and ending the exchange with `QueryDone` (truncate-and-terminate). `MAX_ITEMS_PER_QUERY` = 5 000 is no longer a per-connection binding; it now bounds the daemon-side accumulator in the result source (`source.rs`, `HostSource::start`), where the growth happens, still by truncating the arrival that crossed the line. **[Amended 2026-08-19]** Incomplete rather than wrong: the accumulator truncates *and* records a rejection — `absorb_capped` folds one `FailedCheck::TooManyItemsPerQuery` (`hop-core`'s `pipeline.rs`) in alongside the items it keeps, whenever an arrival overflows the cap (#85, `e0c295e`). Truncation of the undelivered remainder, never eviction of what was delivered — the two are named differently on purpose, because only one of them is visible to the client (see Decision 1's overflow paragraph). What is **not** bounded is bytes — see "count or bytes" under Decision 1 below | A documented per-query total cap on retained items. [#85](https://github.com/pedrosousa13/hop/issues/85) is the standalone record of this gap; #55 (the state) and #59 (the binding that retains it) carry it as acceptance criteria, amended 2026-08-03. #55 has landed the cap; #59 still has to resolve `execute` against the capped set, and to make an item lost to the cap distinguishable from one the daemon never emitted — which the terminal `QueryDone` does not do today |
| T4 | A frame acted on before the handshake | `execute`, `query` | Nothing in the types requires ordering (#26) | Connection loop refuses pre-handshake frames (#54) |
| T5 | `execute` naming an item the daemon never delivered | `execute` | Length bounds only | Decision 1, implemented by #59. **[Amended 2026-08-10]** #59 is now **closed** (`4c1aff4`): `connection.rs`'s Execute arm resolves `item_id` against `Exchange::delivered`, the retained set, and refuses with `ErrorCode::UnknownItem` (`ErrorDetail::Item`) otherwise — an id lost to the per-query cap and one the daemon never emitted are refused identically, deliberately (see Decision 1, "What the implementing slice settled") |
| T6 | `execute` naming an action the item does not carry | `execute` | Nothing ties `action_id` to `Item.actions` | Decision 1's second half — see below. **[Amended 2026-08-10]** #59 (`4c1aff4`) is closed: the same arm checks `action_id` against the resolved item's `actions` and refuses with `ErrorCode::UnknownAction` (`ErrorDetail::Action`) if it is not among them |
| T7 | Query-path cost amplification | `query` | `MAX_QUERY_TEXT` bounds one query's bytes at the daemon wire boundary; ranking is `O(atoms × items)` with 4.09 s measured (#46). `Learning::record` separately bounds stored normalized `selections` keys, while direct embedders own the upstream bound for lookup cost. **[Amended 2026-08-18]** | `Pipeline::assemble` prepares the routed term once and reuses the normalized learning lookup across candidate items, so there is no per-candidate lowercase copy. The wire cap remains `MAX_QUERY_TEXT`; direct embedders own lookup-cost bounds. **[Amended 2026-08-18]** #61 is now **closed** (`80b7ffd`), whose PR introduced `rank.rs`'s `MAX_TERM_CHARS` (256), truncating the term before `Pattern::new` is built. #46 is closed separately, by `85b4c2f`, which turned that fixed truncation into a configurable knob and added `hopd`'s loader enforcement (`config.rs`'s `validate_max_term_chars`). |
| T8 | A provider aiming a command-shaped outcome at a client | `executed`, and `Item.icon` on `results` | Content rules on `CopyText`/`OpenUrl` (#23, closed) and on `IconName`/`IconPath` (#24, closed) | Residual on both halves, and an **open** issue owns each: `Item.copy_text` still reaches the clipboard as a bare bounded string ([#78](https://github.com/pedrosousa13/hop/issues/78), open), and an icon path is validated but not contained — the roots are documented, not enforced, so a regular file outside them still opens ([#93](https://github.com/pedrosousa13/hop/issues/93), open, split out of #24 for this half). **[Amended 2026-08-10]** #78 is now **closed** (PR [#133](https://github.com/pedrosousa13/hop/pull/133)): `Item.copy_text` is `Option<content::CopyText>`, and `CopyText`'s only constructors apply the same content rules #23 already put on `ExecOutcome::CopyText` — refused control characters and the length bound — to every value that exists, in-process or off the wire. `content.rs`'s module docs name the one thing that still differs between the two routes: which field a refusal names. The icon-path half is untouched by this — #93 remains open |
| T9 | Keystrokes reaching the journal, then a shared bundle | Logging | No logging dependency. `QueryText` redacts (#27, closed) — **in `hop-protocol` only**. `route` takes a `&str` and the pre-#83 `RoutedQuery` (its pre-#83 `RoutedQuery::term` field [Amended 2026-08-18]) derives `Debug` over a plain `String` term, so the same text formats verbatim in `hop-core`; `RoutedQuery`'s type docs [Amended 2026-08-18] say not to treat one as safe to log ([#83](https://github.com/pedrosousa13/hop/issues/83), open). **[Amended 2026-08-10]** #83 is now **closed** (`8bd6550`): `RoutedQuery` (`RoutedQuery` [Amended 2026-08-18]) carries `term` and `raw` as `RoutedText`, which redacts under `Debug` the same way `QueryText` does, so the same text no longer formats verbatim in `hop-core`; `RoutedQuery`'s type docs [Amended 2026-08-18] state the change in place | Any added logging keeps the redacting type at the field, and #83 carries the redaction across the crate boundary rather than stopping at `route`. **[Amended 2026-08-10]** Landed — see the Today column |
| T10 | The learning store as untrusted input on load | Disk | Read and parse are bounded (#37, closed by `96d5713`); the `version` is refused on mismatch and a future-dated timestamp clamped (#38, closed by `59fd5fe`); the version probe and the per-condition `LoadReport` are #43's (closed by `056893e`, which replaced #38's two per-branch checks); a persisted `count` is saturated at the boundary (#44, closed by `edb8258`). **Two residuals, one owner**: still no integrity check, so a plausible forged store passes all of it — and eviction still prefers a clamped future-dated entry, which `96d5713` left open and `59fd5fe` explicitly did not close (#88, open) **[Amended 2026-08-10]** #72 (`4f5acf9`, `0c50a98`, `9a595bb`) has since landed on this same load path — provider-scoped keys, manifest-gated plaintext — without touching either residual named here **[Amended 2026-08-17]** #88 has landed: the v2 envelope verifies an HMAC-SHA256 before bounds, clamping, retention or boosts, and unsigned v1 is refused as `UnrecognizedVersion`. The fixed sibling `learning.key` is initialized when the first save reaches key creation after parent setup; once fully written and synced, it remains durable across a later store-write failure and later saves reuse it. Missing, unreadable or wrong-length keys and mismatched tags fail closed with distinct integrity reports; option A detects store-only writes and stores copied without their key, while a process that can read the key remains outside this guarantee. | #88's integrity check, which is what lets a forged entry be *refused* rather than clamped, sequenced with #72 and with Decision 2 on the same load path. **[Amended 2026-08-10]** #72 has landed; #88's integrity check is still what's needed for either residual **[Amended 2026-08-17]** #88 has landed; this integrity-check requirement is satisfied. See the adjacent Today cell for the implemented envelope, key boundary and verification order. |
| T11 | The learning store as a disclosure at rest | Disk | Fail-open id scrubbing (the pre-#39 `canonicalize_result_id` path, replaced by `persistence_key` in #39 and retired with `is_known_safe_shape` by #72) [Amended 2026-08-10] [Amended 2026-08-18]. **[Amended 2026-08-10]** Decision 2's shape half has landed (#39, `193dc4d`, `e83c373`): `persistence_key` (`learning.rs`) now sits between that scrubbing and disk — an id outside the three known-safe shapes persists as `sha256:<hex>` rather than unchanged. **[Amended 2026-08-10]** The shape half described above is retired, not merely extended: issue #72 (`4f5acf9`, `0c50a98`, `9a595bb`) deleted the known-safe-shape check outright and made `ProviderManifest::ids_are_safe_to_persist_in_the_clear` the sole authority — an id from an opted-in provider persists in the clear regardless of its shape, and an id from any other provider, including one presenting an `app:`-shaped id, hashes. | Decision 2. **[Amended 2026-08-10]** Shape half done; the manifest opt-in half is still open, riding with #72 **[Amended 2026-08-10]** Landed: the field exists, required with no default, and every production manifest (`apps.rs`, `calculator.rs`, `hopd::source::SkeletonProvider`) states it explicitly |
| T12 | Cross-provider boost theft | Provider seam | `CheckedItems::check` closes provenance forgery; the store keys on a bare id (#72) **[Amended 2026-08-10]** #72 (`4f5acf9`, `0c50a98`, `9a595bb`) is now **closed**: the store keys on `(provider, id)`, folded by `provider_scoped_key` into a composition proven injective in its own doc comment, so a provider cannot forge another provider's persisted key by choosing its own id or provider string; and `rank.rs`'s `Boosts::by_item_id` gained the identical provider dimension, closing the in-memory half of the same gap for a query where the genuine and impostor items both appear before anything is ever persisted | A provider dimension in the store key. **[Amended 2026-08-10]** Landed — see the Today column |
| T13 | Connection flood / socket occupancy | Accept loop | **[Amended 2026-08-18]** #98 enforces 64 concurrent connection tasks with an owned semaphore permit acquired before `accept`, so the 65th peer waits in the listener backlog. `hopd` rejects client-to-daemon prefixes over `MAX_INBOUND_FRAME_BYTES` = 65,536 before allocating, while the shared `MAX_FRAME_BYTES` = 268,435,456 ceiling remains available for daemon-to-client frames. After a complete prefix, the payload read has a 10-second completion timeout; the prefix read itself and idle time between frames are deliberately untimed. The existing 50 ms accept-error sleep is a hot-spin floor, not an accept-rate limiter. These controls provide same-uid robustness against buggy or runaway local clients, not a security boundary against a hostile peer. | **[Amended 2026-08-18]** The implemented controls compose to at most 4 MiB of inbound payload buffers and 64,000 retained bounded items across 64 admitted connections. The connection cap is the chosen backpressure; no token bucket or other accept-rate limiter is part of the design. |
| T14 | A provider opts in to plaintext persistence for ids that carry user content | Manifest, under Decision 2's consequence | The opt-in field does not exist yet (the pre-#72 `ProviderManifest` [Amended 2026-08-10] [Amended 2026-08-18]). `CheckedItems::check` verifies an item's kind and provider id, and inspects nothing about what the id *contains* **[Amended 2026-08-10]** The field exists now: `ProviderManifest::ids_are_safe_to_persist_in_the_clear` (#72, `4f5acf9`, `0c50a98`, `9a595bb`), required with no default. `CheckedItems::check` still verifies only kind and provider id — the claim itself remains unverified, exactly as this row already said | Documentation a provider author reads before setting the field, and the extension store's PR review (spec §6) as the gate. No code check can verify the claim. **[Amended 2026-08-10]** Still true: the field's own doc comment (`ProviderManifest`) [Amended 2026-08-18] is that documentation, but nothing checks the claim it asks a provider author to make |

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
  **[Amended 2026-08-10]** `crates/hop-cli` now exists (#54, `ac782c7`), with
  a working `exec` subcommand (#59, `4c1aff4`) — this document already
  references `hop-cli` as existing elsewhere (the Actors table; Decision 1's
  overflow paragraph), which this sentence contradicted. `hop doctor` itself
  remains unimplemented: `grep -rn "doctor" --include=*.rs .` still returns
  nothing, and the rest of this bullet holds.
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
  is answered by several `results` frames (`DaemonMsg::Results` [Amended
  2026-08-10] [Amended 2026-08-18]), and every
  item in every one of them stays executable until the retained set is
  released. A rule that kept only the most recent frame would break Enter on
  anything a client is still showing from an earlier one, and it is the reason
  the retained total needs a cap of its own — see T3 and the cap requirement
  below.
- An `execute` frame is served only if its `item_id` appears in that retained
  set and its `action_id` appears in that item's `actions`. Anything else is
  refused rather than dispatched.
- Refusals use the error codes the contract already carries:
  `ErrorCode::UnknownItem` and `ErrorCode::UnknownAction` (`ErrorCode::{UnknownItem, UnknownAction}` [Amended 2026-08-10] [Amended 2026-08-18]). No new code, no new variant.
- **No new wire field.** `ClientMsg::Execute` (`ClientMsg::Execute` [Amended
  2026-08-10] [Amended 2026-08-18]) is unchanged, and so is `Item`.

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
  (`Provider::execute` [Amended 2026-08-10] [Amended 2026-08-18] — "both of which this provider
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
  **[Amended 2026-08-10]** [#30](https://github.com/pedrosousa13/hop/issues/30)
  is now **closed** (`80b7ffd`): `CheckedItems::check` (`CheckedItems::check` [Amended 2026-08-18])
  rejects an item whose action `label` or action count is over the same bound
  `limits.rs` applies at the parse (`FailedCheck::FieldTooLong`), at the one seam every provider's
  answer must cross — `ProviderHost::run_one` calls it before an answer
  reaches assembly. The byte figure above is therefore a real bound on the
  daemon's memory for a provider's answer, not only an argument about the
  wire. It narrows rather than disappears, and `limits.rs`'s own doc says so
  in place: a caller that builds and consumes items without ever reaching
  `CheckedItems::check` — `Ranker::rank` taken directly, say — still gets no
  field-length enforcement at all.
  **[Amended 2026-08-10]** `copy_text` is no longer among the fields
  `CheckedItems::check` rejects on length — not because its gap reopened,
  but because issue #78 (PR [#133](https://github.com/pedrosousa13/hop/pull/133)) closed it a different way: `Item.copy_text`
  is now `Option<content::CopyText>`, and `CopyText`'s own constructor
  enforces the same length bound, and the content rules besides, on every
  value that exists, in-process or off the wire. There is no longer a state
  `CheckedItems::check` could catch that construction had not already
  refused, so checking it there again would be the second gate `limits.rs`'s
  own docs on `validated` argue against. **[Amended 2026-08-18]** `ItemTitle`
  and `ItemSubtitle` now enforce their byte bounds and single-line control
  character rule on every construction path, while retaining bare-string wire
  forms. `CheckedItems::check` therefore checks only action labels and action
  count for field length; title and subtitle checks are no longer duplicated
  there. Apps and calculator providers sanitize their display text before
  constructing these types.
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
See T3. **[Amended 2026-08-19]** The sentence above conflates two halves,
and only one of them has since moved, so it is split here rather than
struck. "On the daemon's side it is still the silent one" is now **false**:
#85 (`e0c295e`) has `HostSource::start`'s accumulator (`source.rs`) fold one
`FailedCheck::TooManyItemsPerQuery` rejection (`hop-core`'s `pipeline.rs`)
in alongside the items it keeps, whenever an arrival overflows the cap — the
drop is recorded, not silent, on the daemon's own side. "What was dropped is
invisible to the peer it was dropped on behalf of" is **still true**: #85
did not add a wire signal, so nothing distinguishes a capped exchange from a
completed one for the client, and this half stays open with #59 exactly as
the untouched sentence above already says. See T3.

**[Amended 2026-08-10]** [#59](https://github.com/pedrosousa13/hop/issues/59)
is now **closed** (`4c1aff4`), and it answers the question above
differently from what this document expected of it: not by making a
cap-truncated id distinguishable from one the daemon never emitted, but by
ruling that they are not worth distinguishing. `connection.rs`'s Execute arm
resolves `item_id` against `Exchange::delivered` and refuses anything absent
from it as `ErrorCode::UnknownItem`, and its own doc comment states the
reasoning in place: whether the daemon never emitted the id, a later frame
replaced it away, or the per-query cap dropped it, all three leave the
client in the same state — an id it cannot execute — and inventing a fourth
code for "lost to the cap" would name a distinction the retained set no
longer carries any evidence for, since nothing survives past the
replace-frame rule to tell a capped id from a superseded one. #85's other
half — a wire signal that an exchange was truncated at all, as against
`QueryDone`'s silence — is a different question from this one and remains
open; it is not something #59's landing settles.

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

**[Amended 2026-08-10]** This rule's shape half is implemented: issue #39's
landing (`193dc4d`, `e83c373`) added `persistence_key` (`hop-core`'s
`learning.rs`), which `Learning::record` and every `global_frequency` lookup
now go through — see `CONTEXT.md`'s **Persistence key** entry for the term,
and "Where today's code stands" and "What the implementing slice must still
settle" below for what changed and what is still open.
**[Amended 2026-08-10]** The manifest half has since landed too, and it did
not merely complement the shape half — it replaced it: issue #72
(`4f5acf9`, `0c50a98`, `9a595bb`) deleted the known-safe-shape check and made
`ProviderManifest::ids_are_safe_to_persist_in_the_clear` the sole authority
`persistence_key` consults. "The rule" above, as originally stated, no
longer describes how the code decides — see the third 2026-08-10 amendment
at the top of this document for the full account.

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
`Learning::recent_launches` (`Learning::recent_launches` [Amended 2026-08-10] [Amended 2026-08-18]) and
`Learning::frequent_launches` (`Learning::frequent_launches` [Amended 2026-08-10] [Amended 2026-08-18]) both return the stored
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

**It needs a manifest field that does not exist yet.** The pre-#72
`ProviderManifest` (`ProviderManifest` [Amended 2026-08-10] [Amended 2026-08-18]) carries `id`, `kinds`, `modes`, `min_term_len` and
`budget` — there is no field for this and no code reads one. Adding it is a
change to the plugin seam (spec §6), so it should land while the seam is still
open to change rather than after the extension store ships. It also interacts
with [#72](https://github.com/pedrosousa13/hop/issues/72), which wants a
provider dimension in the store key: both add a provider-shaped fact to how
learning is stored, and they should be designed together.
**[Amended 2026-08-10]** It exists now: issue #72's landing (`4f5acf9`,
`0c50a98`, `9a595bb`) added `ProviderManifest::ids_are_safe_to_persist_in_the_clear`
(`provider.rs`), designed together with the store's provider dimension exactly
as this paragraph anticipated. It is required, with no default — the struct
derives no `Default`, so a manifest literal that omits the field does not
compile, rather than silently inheriting `true` or `false`. See "What the
implementing slice must still settle" below for how this answers that
bullet's open questions.

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

`canonicalize_result_id` (the pre-#72 helper, retired when `persistence_key` became the live path [Amended 2026-08-10] [Amended 2026-08-18]) strips dynamic payloads for
two prefixes, `utility:` and `web-search:`. An id that matches neither falls
through to `result_id.to_string()` (`canonicalize_result_id`'s historical fall-through, now replaced by `persistence_key` [Amended 2026-08-10] [Amended 2026-08-18]) and is written into
plaintext JSON with the 90-day retention `PERSIST_RETENTION_MS` sets. So does
an id that carries one of the two prefixes with an empty first segment after
it — `utility:` alone takes the same fall-through, since the guard requires a
non-empty segment.

**[Amended 2026-08-10]** The fall-through above no longer reaches disk
unchanged. Issue #39's fix (`193dc4d`, `e83c373`) added `persistence_key`
(`learning.rs`), which every write and lookup on `global_frequency` now goes
through: `canonicalize_result_id`'s own behaviour is unchanged, and its
output still falls through unchanged for `calc:` and for any id this code
has never heard of — but `persistence_key` is what that fallen-through
string reaches next, and it replaces it with `sha256:<hex>` of the raw id
before anything is written to disk, rather than the plaintext this paragraph
describes.
**[Amended 2026-08-10]** `canonicalize_result_id` itself is gone now, not
merely bypassed: issue #72's landing (`4f5acf9`, `0c50a98`, `9a595bb`)
deleted it along with `raw_id_proves_a_known_safe_shape` (`learning.rs`),
since the shape check they implemented no longer decides anything.
`persistence_key` no longer inspects `raw_id`'s shape at all — it takes a
plain `persist_plaintext: bool`, computed by its caller
(`Learning::record`/`Learning::frequency_boost`) from the provider's own
manifest flag via `Learning::sync_plaintext_providers`. A provider absent
from the synced set — one that never registered, or one whose registration
this process has not learned about yet — hashes by the same default, which
is issue #72's fail-closed requirement.

Two facts about that worth carrying into the implementing slice:

- **The two prefixes are inherited from the retired GNOME extension's id
  scheme.** `CONTEXT.md` records that `utility` was split into the four kinds
  `Calculator`, `Currency`, `Timezone` and `Weather`, and today's `Kind` set
  (`Kind` [Amended 2026-08-18]) has no `utility` variant. No provider is implemented, so
  no non-test code in the workspace produces an id with either prefix. The
  allowlist as it stands is keyed to a naming scheme the current kind set has
  dropped.
  **[Amended 2026-08-10]** The apps and calculator providers are implemented
  now (#57 `da5f65f`, #58 `3b53a7a`, both closed), and neither produces an id
  with either prefix — apps ids are `app:`-namespaced, calculator ids
  `calc:`-namespaced (`APPS_PROVIDER_ID`, `hopd/src/apps.rs` and
  `hopd/src/calculator.rs`) — so the conclusion above still holds, on a
  different premise.
- **Nothing constrains what a provider puts in an id.** `ItemId::new`
  (`ItemId::new` [Amended 2026-08-18]) applies `MAX_ITEM_ID` and no shape rule.

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
  **[Amended 2026-08-10]** Still unsettled: issue #39's landing (`193dc4d`,
  `e83c373`) implemented Decision 2's shape half only. `ProviderManifest`
  (`provider.rs`) carries no such field today, and every question this
  bullet poses is unchanged — it rides with #72.
  **[Amended 2026-08-10]** Settled by issue #72's landing (`4f5acf9`,
  `0c50a98`, `9a595bb`): the field is
  `ProviderManifest::ids_are_safe_to_persist_in_the_clear`, and the third
  answer this bullet named — no default, required — is the one the
  maintainer took: the struct derives no `Default`, so a manifest literal
  omitting the field does not compile. `provider.rs`'s doc comment on the
  field states what setting it wrongly costs in each direction.
- **Which shapes count as known-safe**, and how that list relates to the opt-in
  — whether a built-in provider is covered by a shape, by the manifest flag, or
  by both.
  **[Amended 2026-08-10]** The shape half is settled by issue #39's landing
  (`193dc4d`, `e83c373`): exactly three prefixes, `app:`, `utility:<kind>`
  and `web-search:<service>` (`raw_id_proves_a_known_safe_shape`, the raw-id
  proof introduced by #145 and removed with `is_known_safe_shape` when #72's
  manifest authority landed in #148 [Amended 2026-08-18]), checked
  independently of any manifest flag. The opt-in half of this bullet's
  question is unanswered, since the field itself still does not exist — see
  the manifest-field bullet above.
  **[Amended 2026-08-10]** Superseded, not merely completed: issue #72's
  landing (`4f5acf9`, `0c50a98`, `9a595bb`) removed the shape check outright
  — `is_known_safe_shape` and `canonicalize_result_id`'s known-safe machinery
  no longer exist in `learning.rs` — rather than layering the opt-in on top
  of it. `persistence_key` now decides plaintext-versus-hash purely from
  `ProviderManifest::ids_are_safe_to_persist_in_the_clear`; no shape,
  built-in or otherwise, is special-cased. This bullet's question, how the
  shape list relates to the opt-in, is answered by there being no shape list
  left to relate anything to.
- **The hash function, and the dependency it brings.** Neither crate lists a
  hashing or cryptographic dependency today (`crates/*/Cargo.toml`), so this
  adds one. The gate it will meet now exists:
  [#35](https://github.com/pedrosousa13/hop/issues/35) is **closed**
  (`0168107`), and `cargo deny check` runs advisories, bans, licenses and
  sources against `deny.toml` as its own CI job. A hashing crate therefore has
  to clear three separate checks, not one: the `[licenses]` allow-list —
  today GPL-3.0-only, ISC, MIT, MPL-2.0 and Unicode-3.0, with `exceptions`
  empty (`[licenses].allow` in `deny.toml` [Amended 2026-08-10] [Amended 2026-08-18]); ISC was added since, by
  `da5f65f` (#57), for `inotify`/`inotify-sys`, the apps provider's
  filesystem watcher) — plus
  `[bans]`'s empty `deny` list (`[bans].deny` in `deny.toml` [Amended 2026-08-10] [Amended 2026-08-18]) and
  `[advisories]`'s empty `ignore` list (`[advisories].ignore` in `deny.toml` [Amended 2026-08-18]). These are a
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
  **[Amended 2026-08-10]** Settled by issue #39's landing (`193dc4d`,
  `e83c373`): `sha2` (`hop-core/Cargo.toml`, default features), the digest
  `persistence_key` (`learning.rs`) formats as `sha256:<lowercase hex>`.
  `cargo deny check` is clean on all four sub-checks against the gate this
  bullet anticipated.
- **Whether the hash is salted per install.** Unsalted, the same path hashes
  identically on every machine, so one precomputed table serves every user.
  Salted, that is closed, but the salt has to live somewhere — and if it lives
  beside the store it travels in the same backup, which returns the property to
  roughly what it was. This is a real trade and the slice should make it
  deliberately.
  **[Amended 2026-08-10]** Decided, deliberately, by issue #39's landing:
  unsalted. Issue #88's decided `learning.key` sibling file is the natural
  future home for a salt, and inventing a second key file before that one
  exists was judged not worth it this round — the trade above is made, not
  merely deferred, and #88 is where it would be revisited.
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
  **[Amended 2026-08-10]** Settled by issue #39's landing (`193dc4d`,
  `e83c373`): no bump. `STORE_VERSION` stays 1, and `rekeyed_global_frequency`
  (`learning.rs`) migrates a legacy entry to its persistence key as the store
  loads, so an existing file keeps working rather than being refused wholesale
  the way a version mismatch would refuse it. One residual this introduces,
  documented rather than closed: a legacy store already holding a plaintext
  key shaped exactly like this module's own hash output cannot be told apart,
  on load, from a key this module hashed itself —
  `stored_key_needs_no_rekeying`'s own doc comment states why nothing in the
  v1 format distinguishes the two.
  **[Amended 2026-08-10]** Issue #72's landing (`4f5acf9`, `0c50a98`,
  `9a595bb`) changed the migration rule again, this time by provider rather
  than by shape: a legacy `app:`-prefixed key is re-attributed to
  `APPS_PROVIDER_ID` — the one legacy shape with a single honest owner, since
  no other provider has ever minted an `app:` id — and every other legacy
  shape, including the `sha256:`-shaped entry the residual above describes,
  is dropped rather than carried forward: a hash taken without the provider
  that earned it can never match a fresh, provider-scoped lookup regardless.
  `STORE_VERSION` still does not bump. The residual above no longer needs
  distinguishing, since both the ambiguous plaintext key and this module's
  own hash output are dropped now, along with every other unrecognized
  legacy shape.
- **How the empty-query view behaves for a provider that did not opt in.** The
  consequence above settles the rule — those items are learned and not
  renderable there — but not what the view *shows* in their place: a gap, a
  built-ins-only list, or something that tells the user learning is working
  even though the row is absent. M3 builds that screen (spec §8), and it should
  arrive knowing this rather than discovering it.
  **[Amended 2026-08-10]** Still open: issue #39's landing implemented
  Decision 2's shape half only, and did not touch `recent_launches` /
  `frequent_launches` beyond having them return persistence keys rather than
  raw ids for a non-safe-shaped id (`learning.rs`) — a behaviour change with
  no visible effect yet, since nothing surfaces either function to a user
  today. This bullet's question is unchanged and remains M3's.

---

## What this model does not cover

- **The client side.** `hop-gtk`'s handling of items, icons, clipboard writes
  and URL opens is M3/M4 work with its own sweep (spec §13).
- **The provider seam beyond what crosses the socket.** Panic isolation
  ([#29](https://github.com/pedrosousa13/hop/issues/29)), budget enforcement
  ([#28](https://github.com/pedrosousa13/hop/issues/28)) and the boost-theft
  residual ([#72](https://github.com/pedrosousa13/hop/issues/72)) are in-process
  concerns that the M2 sweep covers.
  **[Amended 2026-08-10]** #72's boost-theft residual is closed — see T12;
  panic isolation and budget enforcement remain the M2 sweep's, as this
  bullet already said.
- **Network providers.** None exist. A10 (SSRF) was recorded not-applicable by
  the M1 sweep for that reason and re-runs at M5 against real providers.
- **Hostile-peer denial of service beyond the connection controls.** #98's
  64-connection cap, inbound pre-allocation ceiling and payload-only timeout
  are modelled above as same-uid robustness controls. They are not a security
  boundary against a hostile peer; root-equivalent adversaries and inherited
  open descriptors remain outside this model.
- **A root-equivalent adversary**, and anything reachable by inheriting an open
  descriptor from the user's own processes.

## Follow-up

What has to be true for this model to describe reality rather than intent:

| Slice or issue | What it must establish |
| --- | --- |
| [#54](https://github.com/pedrosousa13/hop/issues/54) | **[Amended 2026-08-10] Landed.** Socket and directory created with a decided mode (`server.rs`'s `acquire_listener`); frame cap from a `hop-protocol` constant, checked before allocation (#21, closed — `framing::payload_len`); handshake-first ordering enforced (#26, closed — `connection.rs`'s `HandshakeState`) |
| [#55](https://github.com/pedrosousa13/hop/issues/55) | **Landed.** Per-query state, server-side cancellation and client-side stale-frame drop, and the retained item set Decision 1 rides on: one set per connection, holding the most recent query id's delivered items, replaced whole by the next `Query` and released when the connection closes. Capped by `hop_protocol::limits::MAX_ITEMS_PER_QUERY` = 5 000 — by item **count**, not bytes — enforced by truncating the undelivered remainder, never by evicting delivered ones (T3, and [#85](https://github.com/pedrosousa13/hop/issues/85), which owns the cap). Decision 1's "rides on state the daemon needs anyway" reasoning therefore holds. Two things #55 deliberately did **not** take: bytes, which rest on per-item bounds — **[Amended 2026-08-04]** #56 landed the provider host without adding that enforcement, and issue #30 owns it — and connection-level bounds, which are #98's (T13). **[Amended 2026-08-10]** #30 is now **closed** too (`80b7ffd`), via `CheckedItems::check`'s field-length rejections — see Decision 1 |
| [#56](https://github.com/pedrosousa13/hop/issues/56) | **[Amended 2026-08-04] Landed.** The provider host: each provider's manifest captured once at registration and compared against a fresh call before its answer is accepted, catching a provider that answers differently after registration; a host-enforced per-provider budget that aborts a non-cooperating provider's task; panic containment via `tokio::spawn`/`JoinError`; and provider error text bounded and stripped (`hop-core`'s `sanitize` module) before it can leave. Two things #56 deliberately did **not** take: per-item field-length enforcement on items a provider returns in-process, which remains open with issue #30 owning it; and a panic hook, so a panicking provider's payload still reaches the daemon's stderr through Rust's default hook, unsanitized, before the host's own failure classification runs — issue #104 owns that decision. **[Amended 2026-08-10]** #30 is now **closed** (`80b7ffd`) — see Decision 1 |
| [#59](https://github.com/pedrosousa13/hop/issues/59) | Decision 1's binding, including the action check, refusing with the existing error codes — and enforcing the retained-set cap #55 sets, with an item lost to that cap distinguishable from one the daemon never emitted (#85). **[Amended 2026-08-10]** **Landed** (`4c1aff4`), but not as this row expected: `connection.rs`'s Execute arm refuses both an id lost to the cap and one the daemon never emitted as the same `ErrorCode::UnknownItem`, deliberately not distinguishing them — see Decision 1, "What the implementing slice settled" |
| [#85](https://github.com/pedrosousa13/hop/issues/85) | The per-query total cap itself, as the standalone record #55 and #59 carry as acceptance criteria: the number and its reasoning, whether it bounds item count or total bytes or both, and whether overflow is a refusal or a **rejection** — never a silent truncation. #55 answered the first two — 5 000, item count only, reasoning in `hop_protocol::limits` — and left the third half-answered: the daemon truncates the undelivered remainder rather than evicting what it delivered, but says nothing on the wire that lets a client tell a capped exchange from a completed one, so its half is still a truncation and not a refusal. **[Amended 2026-08-10]** #59 (`4c1aff4`) has since landed and settled the sub-question this row used to pose to it — a cap-truncated id and a never-emitted one are deliberately not distinguished — without touching this remaining half: no wire signal for a capped exchange exists yet, and none of the closed M2 slices added one. See Decision 1's settled answers. **[Amended 2026-08-19]** #85 is now **closed** (`e0c295e`): the daemon-side third-half question is answered — the accumulator's truncation now also records a `FailedCheck::TooManyItemsPerQuery` rejection (`source.rs`'s `HostSource::start`, folded in by `absorb_capped`), rather than only truncating silently. The wire-signal half above — no field on any frame says items were dropped — is not what #85 closed and remains open, exactly as Decision 1's settled answers already record it |
| [#60](https://github.com/pedrosousa13/hop/issues/60) | **[Amended 2026-08-10] Landed.** A real state directory, which is where `learning.json`'s path stops being hypothetical |
| [#62](https://github.com/pedrosousa13/hop/issues/62) | **[Amended 2026-08-10] Landed.** Socket activation, which moves socket creation into a unit file (`contrib/systemd/hopd.socket`) |
| [#39](https://github.com/pedrosousa13/hop/issues/39) | Decision 2's rule, sequenced with #72 and #88 on the load path #37, #38, #43 and #44 have already changed — plus the `ProviderManifest` opt-in field the recents consequence needs, which does not exist today (the pre-#72 `ProviderManifest` [Amended 2026-08-10] [Amended 2026-08-18]) and changes the plugin seam. The field's **default is an open question**, not something this model settles. **[Amended 2026-08-10]** The shape half has **landed** (`193dc4d`, `e83c373`) — see Decision 2, "Where today's code stands." The manifest opt-in field is unchanged by that landing and still does not exist; it rides with #72, as before **[Amended 2026-08-10]** The manifest field has since landed too, under #72 rather than #39 — see the #72 row below |
| [#57](https://github.com/pedrosousa13/hop/issues/57), M5 providers | Whatever the manifest field's default turns out to be, applied to each built-in provider: either each one declares whether its ids are safe to persist in plaintext, or the default covers those that say nothing. **[Amended 2026-08-10]** Not "open until #39 decides the default": #39 landed the shape half only and explicitly deferred the manifest default to #72 (see the #39 row above) — open until #72 decides it **[Amended 2026-08-10]** #72 decided it: no default, the field required. Every built-in manifest states it explicitly — `apps.rs` and `hopd::source::SkeletonProvider` opt in, `calculator.rs` does not |
| [#72](https://github.com/pedrosousa13/hop/issues/72) | **[Amended 2026-08-10] Landed.** The provider dimension the store key was missing (`4f5acf9`): `persistence_key` now folds `(provider, id)` into one key, by a composition proven injective in `provider_scoped_key`'s own doc comment, and `rank.rs`'s `Boosts::by_item_id` (`9a595bb`) carries the identical dimension in the ranker — closing T12 on both the persisted and in-memory sides. Alongside it, Decision 2's manifest half landed too (`0c50a98`): `ProviderManifest::ids_are_safe_to_persist_in_the_clear`, required with no default, is now the sole authority `persistence_key` consults, retiring the known-safe-shape check #39 landed rather than layering on top of it |
| M3 (spec §8) | The empty-query view's behaviour for a provider that did not opt in — learned, ranked, and absent from that screen |
| [#93](https://github.com/pedrosousa13/hop/issues/93) | The icon-root check #24 deliberately left out, and the open half of T8's pair: allowed roots computed at startup from `XDG_DATA_DIRS` and the icon theme spec's locations, enforced by whatever resolves the path, and checked against what the path resolves to rather than against the string |
| [#83](https://github.com/pedrosousa13/hop/issues/83) | The open half of T9's pair: `RoutedQuery` holds the term as a plain `String` under a derived `Debug`, so the redaction `QueryText` applies in `hop-protocol` stops at `route`, which takes a `&str`. **[Amended 2026-08-10]** **Closed** (`8bd6550`): `RoutedQuery`'s `term` and `raw` are now `RoutedText`, which redacts under `Debug` the way `QueryText` does — see T9 |
| [#88](https://github.com/pedrosousa13/hop/issues/88) | **[Amended 2026-08-17] Landed.** `hop-core` v2 learning envelopes carry an HMAC-SHA256 over the sorted version and entries, with a fixed sibling `learning.key`; verification precedes bounds and timestamp handling. Store-only writes and stores copied without the key fail closed; a process that can read the key remains outside option A's guarantee |
| [#52](https://github.com/pedrosousa13/hop/issues/52) | The M2 sweep, auditing the code rather than inheriting this document's verdicts |
