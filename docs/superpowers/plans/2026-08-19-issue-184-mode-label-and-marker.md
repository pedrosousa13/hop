# Issue #184 — mode label and consumed-marker highlight

Spec: GitHub issue **#184**, slice item 5 of the #80 grill's spec
(`docs/superpowers/specs/2026-08-10-hop-m3-frontend-design.md`, decision D3).
Visual direction settled by `docs/superpowers/specs/2026-08-19-hop-m3-visual-design.md`
and `assets/tokens.css`. The issue body is the binding authority.

## What the issue asks for, verbatim

1. A `Results` frame reporting an exclusive route shows the mode label naming
   that mode; a non-exclusive route shows no label at all.
2. The consumed marker is visually distinguished from the term within the query
   field.
3. Both are driven by what the frame reports, never by re-parsing the query text
   in the frontend — #127 put this on the wire precisely so the frontend does
   not re-implement routing.
4. Every colour, size and spacing value comes from a token in
   `assets/tokens.css`.
5. The label's appearance and disappearance do not shift the results list —
   reserved space or an overlay, consistent with §8a's no-layout-shift rule.
6. The mode label is exposed to assistive technology as text, not as decoration.
7. A headless capture (#179's `--screenshot`) covers both an exclusive route and
   a non-exclusive one.

Out of scope: changing the router, the marker vocabulary, or which markers are
exclusive; live filtering by mode; any second view-tree node type.

## The gap this plan closes, and the maintainer's decision

Criteria 2 and 3 cannot both be met against the protocol as it stands.
`DaemonMsg::QueryRouted` carries `{ query_id, mode, exclusive }` and nothing
else. The consumed marker's extent is **not on the wire**, so a frontend that
highlighted it would have to work out what the router consumed — exactly what
criterion 3 forbids and what #127 existed to prevent.

**The maintainer chose to extend the frame**: the daemon reports the consumed
marker's span on the `QueryRouted` frame it already sends. `API_VERSION` is 2
and, per its own doc comment, has never left this repo, so a version bump is
cheap now and expensive later.

## Global Constraints

- **The frontend never routes.** It renders what the frame reports. If a
  question can only be answered by inspecting the query text, the answer belongs
  on the wire instead.
- **The router reports; it does not change.** No query may route to a different
  mode, carry a different `term`, or change its `exclusive` flag as a result of
  this work.
- **Every visual value comes from `assets/tokens.css`.** No ad-hoc colours,
  sizes or spacing. Note `assets/tokens.css` is consumed by
  `apps/hop-gtk/src/tokens.rs` via `include_str!`, with tests asserting window
  size and row height — a token edit that changes those fails `cargo test` by
  design.
- The accent (`--hop-accent`) is otherwise reserved for the selection indicator,
  focus ring and action hints. Using it here is permitted but must be
  deliberate, not decorative.
- `unsafe_code = "deny"`, no new `unsafe`; `clippy::unwrap_used = "warn"`.
- No `std::env::set_var` anywhere, including tests.
- Doc comments argue *why*, at length, in place, and are self-contained.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check` all pass. `layer-shell` stays off.

## Design decisions

**D1 — the router reports the span; a diff of `raw` against `term` will not do.**
The obvious shortcut is to compute the consumed marker in the daemon by
subtracting `term` from `raw`. It is wrong, and the router's own docs say why:
an alias-matched timezone route carries "the canonical form of that key
(lowercased, whitespace runs collapsed to `_`) rather than the spelling that was
typed", so `term` is not always a substring of `raw`. A diff would silently
produce a wrong span on exactly the routes that are hardest to eyeball.

So `hop_core::router::RoutedQuery` gains a field recording the span its own
matching consumed — the one place that knows it exactly.

This is not "changing the router", which the issue puts out of scope: no query
may route to a different mode, carry a different `term`, or flip `exclusive`.
The guard is a test-level one — every existing router test's outcome must be
unchanged — and it should be stated plainly in the report.

**D2 — the span is a byte range into the raw query, not the marker's text.**
Send offsets, not a substring. The client already holds the text it typed;
echoing part of it back adds a second copy of user input to the wire, and a
second disclosure surface, to tell the client something it can derive from two
integers.

The range is `Option`-shaped: a route that consumed no marker reports `None`.
That covers the `Mode::All` fallback and every inferred route that matched a
*shape* rather than a marker.

Byte offsets into UTF-8 must land on character boundaries, and a frame arriving
from a hostile peer must not be able to panic a client by slicing mid-character.
Validate on parse, in `hop-protocol`, the way that crate already validates its
other bounded wire values — a client should be unable to observe an invalid
range at all.

**D3 — `API_VERSION` 2 → 3.** A conforming daemon now sends a field it did not
before. The handshake exists precisely so that this is a version bump rather
than a break, and `API_VERSION`'s doc comment records that it has never left
this repo. Update that comment to say what changed at 3.

**D4 — the span is a priced disclosure.** A byte range reveals where in the
user's query the marker sat and how long it was. `CONTEXT.md`'s Conventions
require that a redaction disclosing a fact about a value carries a
`# What ... costs` heading, and `QueryText`'s `# What reporting the length
costs` is the worked example. Wherever this range is disclosed — its `Debug`
above all — it needs the same treatment. Read `QueryText`'s heading before
writing this one.

**D5 — the mode's human label belongs to the frontend.** `Mode` is a wire enum
whose spellings are snake_case contract, and it has no `Display`. A label like
"Weather" is presentation — and a localization surface later. Map `Mode` to its
label in `hop-gtk`, not in `hop-protocol`. Cover every variant explicitly rather
than with a catch-all, so a new mode cannot ship without someone choosing its
label.

**D6 — no layout shift (criterion 5).** The label appears and disappears as the
route changes between keystrokes. Reserve its space or overlay it; do not let
the results list move. This crate already has the discipline — `ui/row.rs`
reserves row height before content exists — so follow that shape and say so.

**D7 — confusability is the point.** The issue names the risk: `w ` and `wx `
reach different modes on one added character, and the signal exists to make that
visible *before* the user commits. Both signals must be legible at a glance.
Where a token choice trades subtlety against legibility, choose legibility and
record why.

## Tasks

### Task 1 — the span, from the router to the wire

**`crates/hop-core/src/router.rs`**: `RoutedQuery` gains the consumed-marker
span, set by each matching branch to what that branch actually consumed, and
`None` where nothing was. Read the module's own docs first — the alias-canonical
case in D1 is documented there, and the branches differ in whether the marker
led or trailed (`zurich weather` is routed by a trailing marker).

**`crates/hop-protocol`**: `DaemonMsg::QueryRouted` gains the field, validated on
parse per D2, with the `# What ... costs` heading per D4. Bump `API_VERSION` to
3 and update its doc comment.

**`crates/hopd`**: send it. The daemon already builds `QueryRouted` from the
router's result; carry the new field through.

**Tests**: every existing router test's `mode`/`term`/`exclusive` outcome
unchanged — that is the guard for "reports, does not change". New tests for the
span itself on a leading-marker route, a trailing-marker route, the
alias-canonical timezone route that motivated D1, and a route that consumed
nothing. On the wire: a round-trip, and a rejected out-of-bounds or inverted
range.

**Correction, made during Task 1's review.** An earlier draft of this line asked
for a *mid-character* range to be rejected on the wire. That is unsatisfiable by
construction, and the reason is D2's own choice: the frame carries offsets and
not the text they index into, so character-boundary validity is a relationship
between the span and a string that never travels with it. What the wire can and
does check is `start <= end` and `end` within the query-text bound. The
mid-character hazard is closed one layer down, where the text finally exists, by
an accessor that returns nothing rather than panicking on a split. Recorded here
so a later reader does not reopen it as a defect.

### Task 2 — the frontend

**`apps/hop-gtk`**: consume `QueryRouted` (today `ipc` may drop it — check), and
render:

- **The mode label**, shown only when the frame reports an exclusive route,
  naming the mode per D5. `--hop-text-section` with `--hop-tracking-section`.
  Exposed to assistive technology as text (criterion 6). No layout shift (D6).
- **The consumed-marker highlight** inside the query field, from the reported
  span, distinguished from the term. The field is mono
  (`--hop-text-input`); `--hop-accent` is available but reserved elsewhere, so
  use it deliberately.

**Tests**: the label appears for an exclusive route and is absent for a
non-exclusive one; the highlight covers exactly the reported span; nothing
re-parses the query text. Plus criterion 7's headless captures via
`--screenshot`, covering both an exclusive and a non-exclusive route — follow
`tests/headless_smoke.rs`'s `gtk4-broadwayd` + `GDK_BACKEND=broadway` recipe.

Note that no test can drive a real synthesized key press: GTK4 removed
`gtk_test_widget_send_key` and GDK4 exposes no event constructor. That is
recorded in `apps/hop-gtk/src/ui/window.rs`'s test module — read it rather than
rediscovering it.

## Verification

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all --check`
- `cargo check -p hop-gtk`
