# hop M3 frontend — design decisions

Output of the issue #80 design grill, run 2026-08-10 against the precedent
survey in `docs/research/2026-08-10-launcher-ui-survey.md`.

## What this is, and what it is not

This document records **eight decisions** and the reasoning behind each. It
does **not** restate or replace §8 and §8a of
`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`, which already
commit the frontend to GTK4 + libadwaita, a pre-built hidden window, all IPC
off the main thread, `GtkListView` with row recycling, a configurable keymap
from M3, `tokens.css` before pixels, dark-first with both themes, the six
designed states, and the motion and accessibility bars. Those stand.

The grill deliberately did **not** re-open them. Reading §3, §6 and §8a
against the survey's eight open questions showed that five were already
answered in writing — and answered with better provenance than a fresh
argument would produce, since most carry the specific predecessor defect they
exist to fix. Two examples, recorded because the survey document itself does
not know them:

- **The survey's "what happens to a slow provider" question is answered by
  §3.** Network providers "return a cached-or-pending row synchronously and
  push an update frame when the fetch lands," with timeouts *and* real
  cancellation. The per-provider budget governs the synchronous answer, not
  the fetch. The middle tier the survey asked for already exists.
- **The survey's "is hop the most render-constrained launcher" question is
  reframed by §6.** v2 Tier 1 is *trusted* TS extensions with a
  "Raycast-shaped TS SDK (`List`, `Detail`, `ActionPanel` catalog)". Rich
  rendering is not foreclosed by hop's trust model; it is scheduled at the
  trusted tier. See D3, which is what remained genuinely open.

Questions the survey raised that are settled by §8a and needed no ruling:
fixed-height reserved rows over a growing window (§8a, explicitly);
command-first with per-row action hints (§8a's "right-side action hints on
every row"); ranking legibility paired with user control (§8a's settings
window carries "learning controls + insights", and the empty-query view shows
recents/frequents).

---

## D1 — The 10ms budget stays; what proves it changes

**Decision.** Keep `< 10 ms` as written. Before M5 slices its files provider,
(a) narrow §3's wording to the segment actually measured, and (b) add a
files-shaped arm to the latency fixture.

**Why.** The gate currently measures **p95 = 2.29ms** (min 2.08, max 4.76) over
the 10 000-item fixture — 4.4× headroom, with even the worst sample under half
the budget. A budget met four times over is not the one to relax, and it is the
only contract in hop that no launcher in the survey has committed to: GNOME
Shell debounces 150ms before dispatching to providers at all, and PowerToys Run
ships a named settings category for slow plugins.

**But that number covers less than §3 claims.** §3 says "keystroke → ranked
results: < 10 ms". The gate times `Pipeline::assemble`, a pure function —
provider execution, IPC, serialization and render all sit outside it. Either
the claim narrows to what is proven, or arms are added for the rest. Narrow the
claim: an honest scope beats an unmeasured one.

**And the fixture models the easy case.** Its titles are deliberately short
("Firefox", "Chrome 2"). The test's own comment records that inflating titles
to ~45 bytes pushed p95 to **11.8ms — over budget**. So the measurement is
acutely sensitive to haystack length, and M5's files provider is precisely the
combination excluded: long path-like haystacks at six-figure counts.

**Cost of this decision.** If the files-shaped arm fails at 10ms, the fix is
architectural — prefix indexing, path-segment matching, candidate pre-filtering
— not a tweak. That is exactly why the arm belongs before M5's issues are
written rather than during them. Discovering it late is the expensive order.

---

## D2 — The view catalog belongs to the protocol, not to the tier

**Decision.** v3's sandboxed Tier 2 gets the **same** view catalog as v2's
trusted Tier 1. Rendering capability is a property of the protocol; the sandbox
constrains what a plugin may *reach*, never what it may *describe*.

**Why the technical objection does not hold.** §6 specifies Tier 1's rich
rendering as a **view-tree JSON** whose component names and props map 1:1 onto
Raycast's. A JSON view tree is data, and data crosses a process or sandbox
boundary without difficulty. The survey's finding that rich rendering always
costs the sandbox is true of every launcher it examined — Raycast's real
Node.js access, Flow's `Lazy<UserControl>` restricted to its in-process .NET
tier, PowerToys Run's `IconDelegate` — but those all ship a *live component
tree backed by in-process code*. That is the thing that cannot be sandboxed. A
declarative tree is not.

**Why it matters for adoption, not just purity.** If Tier 2 is less capable
than Tier 1, authors target Tier 1 and the sandboxed tier gets no ecosystem.
That is how good sandboxes die: not rejected, merely unused. v3 is described in
§6 as "the differentiator no launcher offers" — it cannot also be the
downgrade.

**What this obliges M3 to do.** Build a **view-tree renderer whose only node
type today is `Row`.** The dispatch point exists; nothing speculative sits
behind it. §8a already commits to `GtkListView` with a factory, and a factory
producing one row widget is a special case of a factory producing a widget per
node — so the general shape costs M3 very little, while the narrow shape taxes
v2 with a renderer retrofit, the expensive kind.

**Cost, accepted knowingly.** M3 carries structure it does not yet need, and
there is a real risk of over-abstracting against a catalog that does not exist.
The guard: build the **seam**, not the catalog. One node type. No second node
type until a real consumer asks for it.

---

## D3 — Mode signalling mirrors `exclusive`, and nothing else

**Decision.** When a route is **exclusive**, the frontend shows a quiet mode
label and highlights the consumed marker inside the typed text. When a route is
**inferred**, the frontend shows **nothing**.

**Why this split rather than a uniform indicator.** It falls directly out of
`CONTEXT.md`'s augment-not-hijack rule. An exclusive route *filtered results to
one mode's kinds and nothing else shows* — the user has lost results they
cannot see, so they are owed the reason. An inferred route filtered nothing;
the calculator answer was promoted and every app is still there. Announcing
"Calculator mode" over an unfiltered list would be a false claim about what the
window contains. **The UI signal must mean "results were withheld", because
that is the only thing the user cannot otherwise observe.**

This also disposes of the survey's framing, which treated marker *count* as the
argument for chrome. Count is not the reason. Loss is.

**Form.** A quiet label, not a chip — no launcher in the survey uses a
Material-style chip, and §8a's design language is a compact overlay, not a
widget showcase. Plus Albert's trick: highlight the marker inside the entry
text, which is the one precedent that disambiguates confusable pairs. hop needs
that specifically: `w ` and `wx ` reach different modes on one added character,
a confusability `router.rs` documents against itself.

**This is blocked on a protocol addition, and that is the real finding.**
`DaemonMsg::Results { query_id, partial, items }` and
`QueryDone { query_id }` carry no mode. Routing lives entirely in `hop-core`
inside the daemon, so **the frontend cannot currently know which mode
answered.** The two ways out:

1. Carry the routed mode and its `exclusive` flag on `Results`. Correct.
2. Re-implement `route()` client-side. Rejected — it duplicates the router,
   drifts from it, and puts two answers to "what mode is this" in one system.

§6's 2026-07-31 amendment states the seam "stays open to change throughout v1
development" and locks when the extension store ships. So this addition is
sanctioned now and expensive later. **It must land in M3.**

---

## D4 — `IconSpec` stays two arms in v1; a third is expected, not added

**Decision.** Do not add a third arm now. Record that a raw-bytes arm is
**expected** at the tier that needs it, gated by the api-version handshake.

**Why not now.** There is no consumer. `IconSpec`'s own doc comment says a
third arm would be breaking, and the api-version handshake (§6 rule 4) is
exactly the mechanism that makes adding one at v2 a version bump rather than a
break. Adding a variant with no caller invites it to be designed wrong.

**Why record the expectation.** Three of the nine launchers surveyed ended up
needing a form beyond name-or-path — GNOME's own `SearchProvider2` carries raw
pixel data specifically because in-process extensions sometimes hold a bitmap
Shell cannot resolve, and PowerToys Run and Flow both generate image objects
in-process. The pattern is consistent enough that "exactly two arms, forever"
should not be assumed by anything built in M3.

**Note the interaction with #93.** A path arm carries the allowed-roots problem
— documented but unenforced, which is #93. A bytes arm sidesteps both the
filesystem round-trip and the roots question. That is an argument for the bytes
arm's eventual shape, not for adding it early.

---

## D5 — Which GNOME HIG rules bind, stated explicitly

**Decision.** §8a's posture ("HIG-informed where it serves, deliberately
non-stock where identity demands") stays, and this is the list it was missing.

**Binding:**

- **Icon language** — symbolic for chrome, full-colour for content. The one HIG
  rule with independent cross-ecosystem support: PowerToys Run arrives at the
  same split for unrelated reasons.
- **Accessibility** — contrast-checked palette in both themes, screen-reader
  labels on rows and actions, system font scaling. Already §8a commitments;
  named here as HIG-derived rather than local taste.
- **Reduced motion** — honoured via the GTK setting.
- **Full keyboard operability** — structural in hop already.

**Deliberately broken:**

- **The window model.** GNOME Shell's own search is a fullscreen modal
  overview; hop is a ~400×500px overlay and "not an Adwaita dialog" (§8a).
  Worth stating plainly, because "GNOME-native" is easily misread as "matches
  GNOME Shell's search", and hop does not.
- **Stock widget styling.** `tokens.css` governs, not Adwaita defaults.
- **The accent colour.** One committed brand accent on a disciplined dark
  neutral scale, not the desktop accent. §8a treats this as the
  first-in-class-versus-AI-template separator.

---

## D6 — Honesty-critical UI is not themeable

**Decision.** User themes may restyle anything **except** a designated set of
honesty-critical elements, which a higher-priority style provider always wins.

**Why.** §8a already identifies the threat: "a theme is untrusted input" —
GTK CSS executes no code, but it can restyle or hide the very labels §5 relies
on to stay honest (the "as of" timestamp on cached rates, the pending-row
skeleton, the offline indicator). A theme that makes stale data look fresh
defeats "never fabricates freshness". §8a says this is "tracked as its own
issue against M3".

**It was never filed.** M3 contained only #80, #93 and closed #24 at the time
of this grill. Filing it is part of this document's follow-up.

**Shape.** A `.hop-honesty` class set — cached-data "as of" labels, pending
skeleton rows, the offline indicator, error rows — rendered from locked tokens
by a provider installed *above* `GTK_STYLE_PROVIDER_PRIORITY_USER`, so user
CSS loses on those properties specifically. Testable: load a hostile theme
that sets `opacity: 0` and `display: none` equivalents on every honesty class
and assert the elements still render legibly. This is the honesty analogue of
`CheckedItems::check` — a rule enforced at a seam rather than trusted to
authors.

---

## D7 — A dev instance beside a real one is first-class

**Decision.** `hopd` grows a real `--socket` override, and the clients grow the
matching one. Constrained: the path must resolve **inside `$XDG_RUNTIME_DIR`**,
and the 0700-directory / 0600-socket bounds still apply.

**Why now, when #122 deliberately deferred it.** M3 changes the calculus. Until
now the only need was occasional testing, served adequately by relocating
`XDG_RUNTIME_DIR` wholesale. M3 means running a dev frontend against a dev
daemon continuously, while a working launcher stays available — on at least one
development machine that working launcher is a different build entirely. A
wholesale environment override is a workable trick for a test and poor
ergonomics for a daily loop.

**Why the constraint is what makes it safe.** A socket path is threat-model
surface, not ergonomics. An unconstrained `--socket` lets an operator place the
socket somewhere without the 0700 parent that the same-uid boundary depends on.
Requiring the path to sit under `$XDG_RUNTIME_DIR` keeps the directory bound
that `runtime_dir::resolve` already enforces, and turns the flag into "which
socket under my runtime dir", not "any path".

**It becomes a new arm of `hopd::parse`** — the function #124 added, whose
`Invocation::Usage` refusal exists precisely so that a flag which does not
exist cannot be silently ignored. This is the flag that refusal was
anticipating.

---

## D8 — One spec, not two, for now

#80 asks for a second spec covering the provider-authoring contract "if the
plugin-DX half resolves cleanly". It resolved to a **single decision** (D2)
rather than a contract: rendering capability belongs to the protocol, and M3
must build the seam that keeps that true. An authoring contract needs a real
authoring tier to be written against, and the earliest is v2 Tier 1. Writing it
now would be speculative in the way D2 explicitly guards against.

Defer the second spec to v2 design, with D2 as its fixed point.

---

## What M3 must not foreclose

Collected, because these are the retrofit costs the decisions above are shaped
to avoid:

1. **A renderer that can only ever draw a row** (D2).
2. **A protocol whose result frames cannot say which mode answered** (D3) —
   and the window for changing it closes when the extension store ships.
3. **An icon representation assumed to have exactly two arms forever** (D4).
4. **Hardcoded key handlers** — already covered by §8a's 2026-07-31 amendment,
   restated because it has the same retrofit shape.
5. **A theme system that trusts themes** (D6).

## Slice-ready work

Enough to cut M3 issues from, in dependency order:

1. **Protocol: carry the routed mode and `exclusive` on `Results`** (D3). Comes
   first — the frontend's mode signalling depends on it, and it is a v1 seam
   change that gets more expensive over time.
2. **Latency: narrow §3's claim; add a files-shaped fixture arm** (D1). Before
   M5 slices, independent of everything else here.
3. **Theme trust: the `.hop-honesty` locked-token contract** (D6), including the
   hostile-theme test. This is the issue §8a promised and nobody filed.
4. **Frontend: view-tree renderer with `Row` as its only node** (D2).
5. **Frontend: mode label and consumed-marker highlight** (D3), depends on 1.
6. **Frontend: the HIG conformance list as a reviewable checklist** (D5).
7. **Daemon + clients: constrained `--socket` override** (D7). Independent;
   pick it up whenever the dev loop starts hurting.

`IconSpec` (D4) gets no issue by design — the decision is to record an
expectation, not to build anything.
