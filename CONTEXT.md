# CONTEXT

The vocabulary this codebase uses. When naming something — a type, a test, an
issue title, a comment — use the term as defined here rather than a synonym.

Seeded at the end of M1, from the terms the core crates actually settled on,
and extended through M2 with the framing and query-lifecycle terms the daemon
and its clients settled on. It describes what exists; extend it as later
milestones resolve new terms.

## Items and actions

**Item** — one result. Never "result", "row", "hit" or "entry" in code or
prose. An item carries its id, kind, title, optional subtitle and icon, its
available actions, which of them is default, optional copy text, the id of the
provider that produced it, and its `append_to_end` flag.

**Kind** — what an item *is*: `App`, `Window`, `File`, `Calculator`,
`Currency`, `Timezone`, `Weather`, `Emoji`, `WebSearch`, `Action`. A closed
set, defined in `hop-protocol`.

Two kinds from the old GNOME extension are deliberately gone. `recent` has no
equivalent — there is no recents concept in the protocol. `utility` split into
the four kinds `Calculator`, `Currency`, `Timezone` and `Weather`. Ported tests
that referenced either carry a `// DIVERGENCE:` comment saying so.

**Action** — something you can do to an item: open, focus, copy, run, close a
window, move to a workspace, open a URL. An item's **default action** is the
one Enter runs.

**Provider** — a source of items. Describes itself with a **manifest** (its id,
the kinds it produces, the modes it serves, a minimum term length, a per-query
budget, and whether its ids are safe to persist in the clear) and answers
queries. This is the plugin seam; every later extension tier adapts to it.

## Queries

Three different strings travel together, and confusing them causes real bugs:

**Raw query** — exactly what the user typed, untouched, whitespace and all.
Carried as `RoutedQuery::raw`.

**Term** — the raw query with any explicit prefix stripped and trimmed, plus,
where routing matched a known key rather than just a shape, the canonical form
of that key: an alias-matched timezone query carries the alias key it matched
(`sao_paulo`), not the spelling that was typed. This is what search means by
"the query". Carried as `RoutedQuery::term`.

**Effective term** — the term after an alias rewrite, if one applied. This is
what ranking scores against. It is *not* what learning is keyed on: learning
keys on the term, before any rewrite — an alias rewrite is a ranking
substitution the user never typed. The term is not always the typed spelling
either, though: where routing canonicalized it, `sao paulo` and `SAO PAULO`
learn as one key rather than two.

**Mode** — how a query should be interpreted. One of `All`, `Windows`, `Apps`,
`Files`, `Emoji`, `Timezone`, `Currency`, `Calculator`, `Weather`, `Actions`,
`WebSearch`. Distinct from `Kind`: a mode is an interpretation of a query, a
kind is a property of an item. Every mode except `All` maps to exactly one
kind today, which is a fact about the current mapping and not a guarantee.

**Routing** — deciding a raw query's mode. Pure, and runs on every keystroke.

## Exclusive, inferred, and augment-not-hijack

**Exclusive** — the query named its mode with an explicit prefix (`w `, `$`,
`=`, a trailing ` weather`…). Results are filtered to that mode's kinds and
nothing else shows.

Filtering is the whole of the contract: an exclusive route carries **no**
guarantee about the shape of the **term**. Whatever named the mode — a prefix,
a sigil, or the trailing ` weather` — matches before any inference predicate
runs and **routing** returns immediately, so no predicate ever sees the term it
leaves behind, whichever end of the query that term sat at: `zurich weather` is
as unchecked as `w zurich`. `$١٠٠ usd to eur` is an exclusive `Currency` route
whose numeric portion is not an `f64`, and `=٢+٢` the same for `Calculator` —
both correct, because typing the sigil is the user asking for that mode
whatever comes after it. Validating a term is the receiving **provider**'s job:
a provider parses a routed term defensively or not at all, and never on the
strength of the mode it arrived under. That obligation is not new — `100 xyz to
abc` passes the currency shape check and still names no real currency pair, so
inference never promised semantic validity either.

Shape-checking the sigil path was considered and rejected (issue #67).
**Routing** runs on every keystroke while the currency shape check only matches
a *complete* conversion, so a checked sigil would drop the user back to general
results for `$`, `$1`, `$100` and `$100 usd`, snapping into `Currency` only on
the final character. Avoiding that flicker would need a weaker check on the
sigil path than on the inferred one, at which point `Currency` means two
different things and the change has lost its only advantage.

**Inferred** — the mode was deduced from the shape of the query rather than
declared: a bare sum, a bare currency conversion, a bare city name. Exclusivity
stays **off**.

Inferred is where shape-checking happens, but it does not follow that an
inferred **term** is checked. What each predicate guarantees is its own, and
only one of them is about parseability: an inferred `Currency` route matched
`^[0-9]+(\.[0-9]+)?…` on ASCII digits, so its numeric portion parses as an
`f64`. An inferred `Timezone` route constrains the term to the alias set on
only two of its five branches — the `time in `, `time ` and `now in ` phrase
prefixes forward whatever was typed after them, unchecked. And the `Mode::All`
fallback is `exclusive: false` while being deduced from nothing at all: it is
what routing returns when every predicate declined, so it is the fallback
rather than an inference, and it carries the least of any route.

**Augment, not hijack** — the rule that inferred modes add to results instead
of replacing them. Typing `2+2` puts the calculator answer first and still
shows the app you might have been reaching for. This fixes an audited defect in
the old extension, where a bare city name or sum hid everything else. It is why
mode filtering is conditional on `exclusive` and why promotion is conditional
on its negation — the two are mutually exclusive by construction.

**Promotion** — moving an inferred mode's items to the front of the ranked
list, stably, without removing anything.

## Scoring

**Score** = fuzzy match score + kind **weight** + **boost**. Weights break ties
between comparable matches; boosts are strong enough to override match quality.

**Weight** — a per-kind constant expressing which kinds matter more: windows
30, actions and web search 25, apps 20, files 12, emoji 8, the four utility
kinds 6.

**Boost** — an additive nudge for one specific item, from one of two sources,
and their ordering is load-bearing:

- **Alias boost** (`ALIAS_BOOST`, 180.0) — an explicit user instruction.
- **Learning boost** (capped at `LEARNING_BOOST_CAP`, 85.0) — inferred from
  what the user has launched before.

The cap sits strictly below the alias boost so an explicit alias always beats
learned behavior. Both constants are public so that relationship can be
asserted by referencing them rather than repeating their values.

**Frecency** — the learning engine's model: how often an item was launched for
a query, decayed by how long ago. Not "history", not "MRU".

**Persistence key** — the string `Learning::record` writes `global_frequency`
under, and every `global_frequency` lookup keys on; computed from a provider id
and a raw item id by `persistence_key` (`hop-core`'s `learning.rs`), which
folds both into one key so a provider cannot collect another provider's
boosts by presenting its item id — the identical guarantee `rank.rs`'s
`Boosts::by_item_id` carries at the ranker, not just at the store.

Whether the id-part persists in the clear or as `sha256:<hex>` (the unsalted
digest of the raw id) is decided by one thing only: the producing provider's
own manifest, `ProviderManifest::ids_are_safe_to_persist_in_the_clear`
(`hop-core`'s `provider.rs`) — a required field with no default, so a manifest
that omits it does not compile. A provider that opts in persists every id it
mints in the clear; every other provider's ids hash, including one this
process has never registered a manifest for. The flag is a claim only the
provider can make about its own ids' content; nothing checks it, so a
provider that opts in wrongly writes plaintext to disk that this decision
otherwise keeps off it. The hash is not confidentiality against someone who
already holds the store: it has no secret input, so a targeted guess is
checked by hashing it and comparing. What it defends is accidental
disclosure — a backup, a synced folder, a support bundle — where a plaintext
`calc:2+2` is legible on sight and a hex digest is not. See the threat model's
Decision 2 (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`) for
the full reasoning.

**Revocation** — a provider leaving the opted-in set, whether by flipping its
manifest flag off or by dropping out of the registry entirely. The store
reacts on its next sync, not on retention's schedule: a revoked provider's
plaintext entries are hashed immediately rather than waiting to age out. The
reaction is one-directional — a hash, once written, cannot become plaintext
again if the provider opts in later, since the hash cannot be reversed to
recover the id it was computed over.

**Load report** — what one load of the learning store noticed: that it loaded,
or which single fallback it took instead — absent, not a regular file,
unreadable, over the byte ceiling, malformed, or on a store version this code
does not write. One variant per condition, never two conditions sharing one.
Returned as data beside the store by `Learning::load_reporting`, never logged —
what a caller does with one is not decided yet; `Learning::load` is the same
call with the report dropped, and still degrades to an empty store. A report
names what the load *detected*, so a store forged to be plausible reports a
successful load like any other. Distinct from a **refusal**, which is a gate
declining to build a value, and from a **rejection**, which is an item assembly
declined.

## Result assembly

**Checked items** — provider output that has been checked against the
manifest of the provider that actually produced it: every item's kind is one
its producer declared, and every item's provider string is its producer's
manifest id. Assembly accepts nothing else, so an item's self-description is
never taken on trust.

**Rejection** — one item assembly declined, and why. `CheckedItems::check`
produces four of the six reasons `FailedCheck` distinguishes. Two are the
**manifest checks**: the item's `kind` is not one its producer declared, or
its `provider` string is not its producer's manifest id — the two ways an
item's self-description can be a lie. A third, `FailedCheck::FieldTooLong`,
is not a manifest check and not evidence of one: it means an action label or
the action count was over the same length bound `hop-protocol`'s own parse
already applies. Item titles and subtitles cannot reach this check: their
validating newtypes enforce their bounds on every construction path. A
fourth, `FailedCheck::TooManyItems`, is not about any one item at all — it
records a whole provider answer over the per-answer item cap, decided before
any item in it was inspected. The fifth reason, `FailedCheck::PinBudget`, is
minted only later, inside `Pipeline::assemble`, for a pinned item the **pin
budget** could not afford even though it passed every check above. The sixth,
`FailedCheck::TooManyItemsPerQuery` (issue #85), is minted later still and
outside `check` entirely: `hopd::source`'s per-query accumulator mints it,
over the **result source**'s own `MAX_ITEMS_PER_QUERY` cap, following
`TooManyItems`'s own precedent one layer up — one aggregate rejection per
overflowing arrival, never one per dropped item. Returned as data alongside
the assembled items rather than logged directly, because `Pipeline::assemble`
runs on every keystroke and may not have side effects. That used to mean
rejections went unlogged everywhere, full stop; it no longer does. The
**provider host** now reads everything `CheckedItems::check` itself can
produce — the four reasons above it mints, tallied truthfully by cause rather
than as one undifferentiated count (a `TooManyItems` rejection stands for its
whole dropped excess, not one item) — through its **log seam**, every time it
runs `CheckedItems::check` on `ProviderHost::run_one`'s path. What still goes
unlogged are the pin-budget and per-query-cap reasons, both minted after that
path: the daemon *does* call `assemble` (as `assemble_checked`) now — the
**result source** does, on every provider arrival, over the accumulated
**checked items**, itself already carrying any per-query-cap rejection the
accumulator minted before `assemble_checked` ever ran. What that returns
travels to `hopd::connection` over the **result source**'s own channel — the
caller that discards the rejections, the four logged reasons, and the two
unlogged ones, arriving together but never reaching the wire (see
**truncate-and-terminate**). Only the two manifest checks mean a provider
lied; a rejection names which reason it was, so the six are never confused
for one another.

**Ranked body** — the scored, ordered items.

**Pinned tail** — the items flagged `append_to_end` that a query honors, which
always follow the ranked body regardless of score. Web-search actions are the
motivating case. They are split off before ranking and never scored. Flagging
an item asks for the tail rather than joining it: how many of the requests a
query honors is the **pin budget**, and the rest become rejections.

**Pin budget** — how many pinned items assembly honors for one query:
`MAX_PINNED_ITEMS_PER_PROVIDER` from any one producing provider and
`MAX_PINNED_ITEMS_PER_QUERY` in all, both in `hop-core`'s `pipeline`. Spent in
provider-supplied order, so a provider that sets the flag on everything it
returns gets its one row rather than the list — and, because the share is per
producer, cannot take the whole tail away from another provider by answering
first, though answering first does spend one of the shared slots.
It counts pins, never deciding which providers may pin; that is a capability
check nothing has built yet. Not a **bound** in this glossary's sense: it
restricts neither the size nor the content of a wire value, and it is one
process's assembly decision rather than a maximum both peers apply — which is
why it lives with the assembly that spends it rather than in `hop-protocol`'s
`limits`.

**Cap** — the maximum result count, applied to the concatenated body and tail
together. A body that alone fills the cap squeezes the tail out; the old
extension reserved room for the tail instead, and that difference is a recorded
divergence.

## Provider host

**Provider host** — what owns registered **providers** and runs their
queries: `hop-core`'s `ProviderHost`. Not a scheduler in the ranking sense —
it decides *whether* a provider runs for a query and *for how long*, never in
what order its items appear. Ordering that is `Pipeline::assemble`'s job, and
the host still does not call it — the **result source** does, on every provider
arrival, over the accumulated **checked items** (issue #103 made that true).

**Registration** — the one moment a provider's manifest is read:
`Provider::manifest`, called once, at `ProviderHost::register`. What is
captured then is what every later scheduling decision consults; nothing after
registration re-reads the provider to make one.

**Captured manifest** — the manifest exactly as registration read it, kept as
the baseline a later call is checked against, so a provider whose live
`Provider::manifest()` answer has since shifted is caught rather than trusted.
**Effective manifest** — the captured manifest with host policy applied: its
budget **clamped** to the host's ceiling, its minimum term length raised to
the host's floor where that is higher. Both are kept because they answer
different questions: scheduling and the enforced budget read the effective
manifest, while the captured one is what a later call is compared against —
clamping deliberately makes the two differ, which is exactly why the effective
manifest cannot serve as that baseline itself.

**Clamp** — the host lowering a provider's budget to its ceiling, or raising
its minimum term length to its floor. One direction each: a clamp can only
shorten a budget or raise a floor, never move a value the other way, and
neither is negotiable by the provider it clamps.

**Budget** — the host's deadline for one provider on one query: how long it
may run before the host stops waiting and reports an outcome, cut off or not.
Distinguish a **budget miss** — the host's own act of cutting a provider off
once its budget expires, enforcement — from a provider's own **timeout**
(`ProviderError::Timeout`) — the provider noticing its own deadline and giving
up first, cooperation. A client sees the same failure kind either way; only
the **log seam** tells the two apart. Not a **bound**: a bound constrains how
large a value may be, a budget constrains how long a provider may run — one is
about size, the other about time, and a manifest's `budget` field is measured
in neither bytes nor elements. Also distinct from the **pin budget** above,
which counts items rather than measuring time; the two share a name and
nothing else.

**Panic isolation** — a provider's panic surfacing as a failure that names the
provider, rather than unwinding into the daemon. Each provider's query runs as
its own spawned task, so a panic there becomes a `JoinError` the host reads,
not a crash the connection driver inherits.

**Sanitize** — rewriting provider-supplied text so it is safe to render:
stripped of control and direction-override characters, then truncated to a
documented maximum. That truncation is the same act this glossary already
names under **Refusal**, above — a shortened value with nothing said about
what was cut — applied here to a provider's error text rather than to an
assembled list or a delivered batch; the two are consistent, not competing
senses of the word. Contrast with a **content rule**, which *refuses* a value
that breaks a rule outright: sanitizing never refuses, because the value here
is a diagnostic about a failure that has already happened, and refusing it
would replace the reason a provider failed with the reason its explanation was
unacceptable. The same holds against an ordinary **bound** — one enforced, as
most are, by the **validating newtype** that carries it: a value over the
bound is refused outright and never exists to be rendered at all. Sanitizing
takes the opposite path everywhere: it never refuses, keeping a rewritten,
lossy version instead of the value that broke a rule, rather than declining
to build one. Where a bound or a content rule is applied is a separate
question from what either does to a value that breaks it — see **Bound** —
and it is the latter, not the former, that sanitizing departs from.

**Log seam** — where the **provider host** reports what providers did:
`ProviderLog` and the `ProviderEvent`s it records. This is the seam
**Rejection**, above, promised would make ignoring a rejection a real mistake
— and now does, for the host: `ProviderHost::run_one` reads every rejection
`CheckedItems::check` produces and records it as `ProviderEvent::Rejected`.
It does not make `Assembly::rejections` unignorable for every caller, only for
the host's own path — see **Rejection** for the half that still is not
reached.

**Desktop entry** — a `.desktop` file under an XDG application directory
(freedesktop.org's Desktop Entry Specification), the source the apps
provider indexes.

**App id** — the desktop entry's file name with its trailing `.desktop`
removed (`firefox.desktop` → `firefox`); the apps provider's items carry it
as `app:<app id>`, which is what `hop-core`'s alias table also synthesizes
for an `app` alias boost — see `APPS_PROVIDER_ID`'s own docs for why the two
must agree. A file name ending in `.desktop` is only a *candidate* app id;
`scan_apps` claims it only once the file is *understood*, i.e. parsed into
one of `DesktopEntryOutcome`'s three outcomes — valid (claims the id,
contributes an item), occluded (claims the id, contributes nothing, for a
deliberate `Hidden=true`/`NoDisplay=true` entry suppressing a
lower-precedence one on purpose), or malformed (claims nothing, leaving the
id free for a lower-precedence root to supply a working entry).

## Frames

**Frame** — one message on the socket: a four-byte big-endian payload length
followed by that many bytes of JSON. It is the unit everything on the wire is
counted and refused in — `MAX_FRAME_BYTES` bounds one, a **refusal** off the
parse sinks one, and an `ErrorCode` names why one was refused. The shape is written down
once, in `hop-protocol`'s `framing`, so the tokio daemon and the blocking CLI
read the same bytes the same way instead of each carrying its own copy of the
prefix arithmetic.

**Payload** — the JSON half of a frame, exclusive of the length prefix. What
`MAX_FRAME_BYTES` counts, what `payload_len` returns the length of, and what a
`ClientMsg` or `DaemonMsg` is parsed from. Never a synonym for the frame: the
prefix's four bytes are not payload, and a transport reads them on their own
precisely so it can decide about the payload before it holds any of it.

**Pre-allocation gate** — `framing::payload_len`: the check a transport runs on
a frame's length prefix before it allocates a buffer sized by the number the
peer put there. A prefix over `MAX_FRAME_BYTES` is refused on those four bytes
alone, so the payload it describes is never read in order to be reported. It is
a gate a caller has to *call* — a transport that allocates first and checks
after has undone it — which is why both transports say in place that they apply
this gate rather than re-implement it.

## The query lifecycle

**Handshake state** — where a connection sits in the gate every frame passes
through: `AwaitingHello` until a `Hello` carrying this `API_VERSION` arrives,
`Ready` after. It moves once and never back, so a second `Hello` on a `Ready`
connection is refused rather than read as a re-handshake. Per connection, and
held by the connection's driver: nothing about it is shared between peers.

**Exchange** — one query id's life on a connection: the source still producing
for it, and the items delivered under it. At most one is live per connection.
An exchange *ends* when its source ends — naturally, at the per-query cap, or
on a `Cancel` — and outlives that end, because what it delivered stays
resolvable until the next query replaces it.

**Result source** — the seam between a connection and whatever answers its
queries: one query in, a stream of item batches out behind a channel. The
channel is the whole contract — batches arrive on it, the source finishing
closes it, and the caller dropping it *is* the cancellation, which makes
cancellation a property of the seam rather than a protocol bolted beside it.
This is where the **provider host** plugs in, and it now does: the one
production source routes the query text and hands the routed query to the host,
which answers from the providers registered with it. Only the walking
skeleton's own provider is registered until issues #57 and #58 land the apps
and calculator ones, so what comes back is still a single hardcoded item — but
it arrives through the host, having passed the **manifest checks**, rather than
bypassing it. As of issue #85, the channel carries **checked items**
(`hop_core::pipeline::CheckedItems`) rather than a bare item list: that type's
mint sites — `CheckedItems::check`, `Pipeline::assemble_checked`, and the
combinators over values `check` already produced — are all private to
`hop-core`, and every one of them either runs `check` itself or repackages
items that already went through it, so an implementation of this seam cannot
hand the daemon an item that skipped the per-item field-bound check — the
trait's own type is the enforcement, not a paragraph asking an implementor to
remember it.

**Retained set** — the **last assembled list** an exchange has sent, replaced
whole by each `results` frame under the replacement-frames rule, kept so that
a later `execute` resolves against what the client was actually shown (issue
#59). One per connection, holding the most recent query id's list: a new
`Query` replaces it whole, and the connection closing drops it. It survives
the exchange's **terminal frame** and a `Cancel`; an item the daemon has since
replaced away is no longer resolvable, which is the decision issue #103
recorded. Bounded by `MAX_ITEMS_PER_RESULTS_FRAME` — `connection.rs`'s
`forward_batch` truncates one list to it before retaining — not
`MAX_ITEMS_PER_QUERY`, which now bounds the daemon-side accumulator in the
**result source** instead. Its lifetime is a rule, ruled explicitly by issue
#85: it expires on the next query for that id, and on nothing else — no timer,
no idle eviction. The only thing that ever ends one early is a new `Query`
replacing the whole `Exchange` it lives in; see `hopd::connection::Exchange`'s
own docs for why that makes the rule true by construction rather than by
discipline.

**Replacement frame** — a `results` frame carrying a query's complete current
list, which a client swaps its held list for in whole, never an increment. A
daemon never splits one list across frames; there is one frame per provider
arrival, each a fresh re-ranked list over everything received so far. What a
client does with one is the **Retained set**; what it does with a frame for a
query it is no longer waiting on is the **Stale-frame drop**; the frame that
ends the exchange is the **Terminal frame**.

**Truncate-and-terminate** — what the daemon does at a cap: it truncates the
batch that crossed the line, delivers what fit, ends the exchange with its
terminal frame, and drops the source. Two caps now sit on the daemon; the
accumulator's `MAX_ITEMS_PER_QUERY`, which bounds what the
**result source** accumulates in `source.rs`, and the connection's
`MAX_ITEMS_PER_RESULTS_FRAME`, which bounds one assembled list in
`connection.rs`'s `forward_batch`. The two halves of that are
different things in this glossary's terms and are worth keeping apart. Nothing
delivered is ever **evicted** — that is what the retained set exists for, and
what keeps a delivered item resolvable. The undelivered remainder is dropped
with nothing on the wire naming it, so that half is a **truncation** and not a
**refusal**, however deliberate it is: a capped exchange and a completed one
carry the same terminal frame. A client's only guard against an oversized list
is the frame cap at the parse — `de_results_items` refuses a `results` frame
over `MAX_ITEMS_PER_RESULTS_FRAME` — so that half of the cap *is* a refusal,
and the two sides of one cap are named differently on purpose.

As of issue #85, "nothing on the wire naming it" stays true for the *client*
— truncate-and-terminate is a wire behavior, not a protocol change — but it
stopped meaning nothing is named at all. The accumulator's own truncation
(`hopd::source`'s `absorb_capped`, over `MAX_ITEMS_PER_QUERY`) now records
what it drops as a **rejection**
(`hop_core::pipeline::FailedCheck::TooManyItemsPerQuery`), riding alongside
the surviving items in the same `CheckedItems` batch the **result source**
hands the connection, following `FailedCheck::TooManyItems`'s own precedent
one layer up. Never a silent truncation and never a refusal of the whole
set — the maintainer's ruling on this issue in those exact terms. The
connection's own `MAX_ITEMS_PER_RESULTS_FRAME` truncation is unchanged by
this: it stays a wire-level truncation with nothing recorded, because it
bounds a different thing (one frame, not the query's whole accumulated set)
at a different layer, and this issue's ruling was scoped to the per-query cap.

**Terminal frame** — the one frame that ends an exchange: `DaemonMsg::QueryDone`,
sent when the source finishes, at the per-query cap, or in answer to a matching
`Cancel`. Never a `partial: false` results frame — `partial` is advisory, and a
client keys on the terminal frame instead. A query-scoped `DaemonMsg::Error` is
terminal in its place, and the two never both arrive for one id;
**supersession** and the connection ending produce neither.

**Supersession** — a new `Query` replacing the one before it on the same
connection. Dropping the previous exchange's source is the server-side
cancellation, and its retained set is replaced along with it. No frame follows
for the superseded id, not even a terminal one: the client that superseded it
has moved on and would drop one as stale. A `Cancel` is the same mechanism
acknowledged — the canceller is still waiting on that id, so it gets the
terminal frame.

**Stale-frame drop** — a client discarding a frame whose `query_id` names a
query it is no longer waiting on, rather than rendering it or treating it as an
error. The client half of the lifecycle contract, and what makes supersession's
silence safe.

## Content rules

**Command-shaped outcome** — an `ExecOutcome` variant that tells a client to
*act* rather than reporting what happened: `CopyText` and `OpenUrl`. Both come
from a provider, so neither is trusted. `Done` is not one, and an item's
`copy_text` is not one either — it reaches the same clipboard, but by way of an
item rather than an outcome.

**Bound** — a maximum on how large a wire value may be: how many bytes a string
may hold, or how many elements a sequence may. Bounds live in `hop-protocol`'s
`limits`, one per variable-length field, declared once for both peers. Most are
applied at the deserialization boundary, by the field's own `deserialize_with`
or by the **validating newtype** that carries it. Two are not, because no
single frame can break them, and each says so where it is defined:
`MAX_FRAME_BYTES` is applied by a transport to a frame's length prefix ahead of
the parse — the **pre-allocation gate** — and `MAX_ITEMS_PER_QUERY` is applied
in the daemon's result source (`source.rs`), where the **checked items** a
query accumulates across every provider arrival — work no single frame can
exceed — are capped and truncated, the excess recorded as a **rejection**
rather than dropped silently (issue #85; see **truncate-and-terminate**).
Where a bound is applied is a fact about what it bounds; being applied at the
parse is not what makes one a bound.

**Content rule** — a restriction on what a wire value may *contain*, as against
a **bound**, which restricts how large it may be. Content rules live in
`hop-protocol`'s `content` module, bounds in its `limits` module; a content
rule and the bound on the same value are both applied at the deserialization
boundary, and the bound is applied first.

**Validating newtype** — a type wrapping a private `String` whose only
constructor applies every rule, and whose `Deserialize` hands the parsed string
to that same constructor. One gate, not two: a value that exists has passed the
rules however it was made. `ItemId`, `ActionId`, `ItemTitle`, `ItemSubtitle`,
`CopyText`, `OpenUrl`, `IconName`, `IconPath` and `QueryText`.

**Allowed scheme** — a URL scheme an `OpenUrl` may carry, from the allow-list
`ALLOWED_URL_SCHEMES`. An allow-list, never a deny-list: a scheme that is not on
it is refused, so a handler the contract has never heard of cannot be reached by
installing a provider that names it.

**Refusal** — a value either gate would not build, named for the rule it broke:
a constructor returns it as an error, and off the parse it becomes an error that
sinks the whole frame. Distinct from a **rejection**, which is an *item* that
assembly declined: a rejection is data returned alongside the items that
survived, so a query with one still answers. Neither is ever a truncation, a
normalization, or a silent fix.

The shape generalises past the two gates: a client refusing a stream that runs
past `MAX_ITEMS_PER_QUERY` declines to produce the assembled list at all, names
the cap that was broken, and prints nothing — an error where a shortened list
would have been a truncation.

A truncation in that sense is keeping a shortened version of a value and saying
nothing — the shortened id `limits` refuses to produce. Refusing a whole item
and reporting it is not one, however short the surviving list gets, which is
why the **pin budget** rejects rather than truncates. Within assembly the
**cap** is the truncation: it shortens the assembled list to `max_results`
silently, naming nothing it dropped. The daemon's half of the per-query cap is
the other one — see **truncate-and-terminate**.

## Redaction

**Redaction** — printing a marker in place of the value, optionally with a
bounded fact about it such as its byte length. Redaction applies to
*formatting*, not to transport: a redacted value is still serialized and sent
whole. A bound restricts how long a value may be, a content rule what it may
contain, and a redaction what formatting it discloses; the three live in
`hop-protocol`'s `limits`, `content` and `redaction` modules. What the disclosed
fact costs is priced on the type that discloses it, under the `# What ... costs`
heading in Conventions, never assumed to be free.

**Redacting newtype** — a newtype that carries its own `Debug`, so the
redaction travels with the value: a field formatted on its own prints the same
marker it prints inside its frame, and a field added to a frame later is
redacted by having the type. Often *also* a validating newtype, but the two are
independent and one instance of each exists. `QueryText`, the type of
`ClientMsg::Query.text`, is both: it holds keystrokes typed into the launcher
overlay and enforces `MAX_QUERY_TEXT`. `hop-core`'s `RoutedText`, the type of
`RoutedQuery`'s `term` and `raw`, redacts without validating — it carries those
same keystrokes past the wire and asserts no bound, because `Pipeline::assemble`
builds one from an alias rewrite target, which is config-file text and not
wire-bound (#83). Routing, assembly, and the learning lookup path add no
query-cost bound; direct embedders own that upstream. `hopd` gets the wire
bound from `hop_protocol::limits::MAX_QUERY_TEXT` before routing. Separately,
`Learning::record_launch` refuses a normalized `selections` key over that
constant for storage.

## Conventions

**`// DIVERGENCE:`** — the literal marker on any test where behavior
deliberately differs from the old extension's, with the reason inline. Grep for
it to audit every place this codebase knowingly departs from what it ported.
Comments must be self-contained: never defer the justification to a document
outside the repo.

**`# What ... costs`** — the doc-heading form under which a type prices a
decision: what is given up, and the alternative that was rejected instead.
Only `# What` and `costs` are fixed, so the audit is
`grep -rnE '^\s*/// # What .* costs'` rather than a grep for one literal string
— and it is the heading that is greppable, not the wording between. The anchor
is what keeps the output to the headings themselves: without it the pattern also
returns this file's own mentions of the form. A redaction that
discloses a fact about the value **must** carry one, so that every priced
disclosure is one grep away; anything else that gives something up **may**.
`QueryText`'s `# What reporting the length costs` is the worked example, and
`content`'s `# What refusing a carriage return costs` is the permission taken
up — the same heading spent on a rule's cost rather than a disclosure's.

The form prices what a gate or a redaction gives up. That is mostly wire values,
so it is mostly `hop-protocol`'s `limits`, `content` and `redaction` — plus
`hop-core`'s `router`, where `RoutedText`'s `# What reporting the length costs`
prices what its redacted `Debug` discloses (#83). A redaction that discloses a
fact about the value carries the heading wherever the type lives; that module
list is where such types happen to sit, not a boundary. Prose that prices
something else is outside the form rather than missing it: `hop-core`'s
`pipeline` heading `## When the manifest is read, and what that costs` prices
when a check runs, not what a gate discloses or refuses, and the grep above does
not match it.

**Query path** — the code that runs on every keystroke: routing, alias
application, ranking, learning lookup, assembly. Nothing on it may touch disk,
spawn a subprocess, or make a network call. Learning state moves to and from
disk only on explicit `load` and `save`.

**`unsafe`, declared rather than absent** — enforced rather than promised:
`unsafe_code = "deny"` in the root `Cargo.toml`'s `[workspace.lints.rust]`,
which each member inherits with `[lints] workspace = true`, so a new member that
omits that line sits outside the gate. So do doc tests, which rustdoc compiles
as crates of their own that the lint does not reach — the comment beside
`unsafe_code` in the root `Cargo.toml` carries the detail, and it is the reason
this rule is stated about compiled code rather than about the whole tree. What
the lint guarantees is not zero `unsafe`, but that every block is declared: as
of issue #182 there are seven in the tree, two of them in production code —
`hopd::server`'s `OwnedFd::from_raw_fd`, taking ownership of a
systemd-activated socket descriptor (issue #62), and `hop-gtk`'s `ui::window`,
setting `XDG_ACTIVATION_TOKEN` immediately before the `present()` that reads it
back, on the GTK main thread (issue #179) — and five test-only
`libc::mkfifo`/`pre_exec` calls, in `hop-protocol::content`, `hop-protocol`'s
own `config_file` (promoted out of `hopd::config` by issue #182), `hopd::config`,
`hopd`'s `tests/activation.rs`, and `hopd::apps`. Each carries its own narrow
`#[expect(unsafe_code)]` on the statement rather than
`#[allow]` on the module, so a second `unsafe` beside it still fails, and the
exception warns itself out of existence once its call goes — which CI's
`-D warnings` turns into an error. `deny` and not `forbid`, so a genuine FFI
need can annotate one call with a `SAFETY:` comment instead of weakening the
line for the whole workspace.
