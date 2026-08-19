# hop — GNOME HIG conformance checklist

Status: normative for M3 review
Source: decision D5 in
[`docs/superpowers/specs/2026-08-10-hop-m3-frontend-design.md`](superpowers/specs/2026-08-10-hop-m3-frontend-design.md),
and §8a of
[`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`](superpowers/specs/2026-07-30-hop-launcher-v1-design.md)

## What this is

D5 settled which GNOME Human Interface Guidelines rules bind on hop and which
are deliberately broken, and why. A decision recorded in a design document is
not something a reviewer can run against a build — this document is that
decision turned into a walk: one item per rule, each with what to look at and
what a pass looks like, so two reviewers looking at the same build reach the
same verdict.

This document does not re-argue D5. Where a reason is owed, it cites D5 or
§8a and gives the reasoning compactly, rather than restating either at
length.

**The deliberately-broken half is not an appendix here — it earns equal
billing with the binding half, on purpose.** "GNOME-native" is easily misread
as "matches GNOME Shell's own search UI", and hop does not match it: a
~400×500px overlay is not GNOME Shell's fullscreen modal overview, `tokens.css`
governs styling rather than Adwaita's defaults, and hop's accent is one
committed brand colour rather than the desktop's own. A reviewer walking this
document without reading the deliberately-broken section first will file each
of those three as a bug. They are not.

## How to use this

Walk each item below against a real build (or, where marked capture-verifiable,
against `hop-gtk --screenshot` output — see
[`apps/hop-gtk/tests/headless_smoke.rs`](../apps/hop-gtk/tests/headless_smoke.rs)
for how those captures are produced in CI). Each item states a **pass
condition** specific enough that two reviewers should not be able to reach
different verdicts from the same evidence.

**On what a capture proves.** A `--screenshot` PNG is a single rendered
frame. It can show colour, layout, text content, and geometry. It cannot show
timing, an accessibility tree, a live GTK setting's effect, or anything about
input handling — `headless_smoke.rs`'s own comments record this distinction
carefully for the mode-label/marker-highlight case, and this document holds
every item marked "capture-verifiable" to that same standard: named
specifically, not claimed as a general proof of correctness.

**Status legend**, applied honestly rather than aspirationally — this
document's value depends on a reviewer being able to trust a "satisfied"
claim:

| Status | Meaning |
| --- | --- |
| **Satisfied, verified** | Checked against the build at the commit cited; holds. |
| **Partially satisfied** | Part of the item holds, part does not or is unbuilt; the gap is named. |
| **Not yet satisfied** | Checked; does not hold yet. Named as a gap, not filed as a bug — M3's remaining slices own closing it. |
| **Unknown** | Not checked, or checkable only by a human/setting this pass did not have. Recorded as unknown rather than guessed. |

Every status below was determined by reading the build at commit `7a6f99b`
(`hop-gtk: mode label and consumed-marker highlight`, #192) — by source
inspection, by running the existing test suite, and in a few cases by
producing a `--screenshot` capture. Where a status could not be determined
this way, it says so rather than guessing.

---

## Binding rules

These four are HIG rules hop keeps. Each states what a reviewer looks at and
what a pass looks like.

### 1. Icon language — symbolic for chrome, full-colour for content

**Rule.** Chrome (window controls, UI affordances) uses symbolic
(single-tone) icons; content (an app's own icon, a file type, a weather
condition) renders in full colour. D5: "the one HIG rule with independent
cross-ecosystem support — PowerToys Run arrives at the same split for
unrelated reasons."

**Check.** Take a results-state capture with at least one icon-bearing row
(`hop-gtk --screenshot out.png --query <a query that returns app results>`).
For every icon-bearing element on screen, classify it chrome or content, and
confirm: every chrome icon renders single-tone (symbolic), and every content
icon renders in its source's own colour (an app icon, a favicon, a weather
glyph). Pair the visual read with a source check of which icon name or
`IconSpec` arm each element actually uses — a coincidentally single-colour
content icon and a genuinely symbolic one look the same in a screenshot but
are not the same thing. **Pass condition:** no content icon renders symbolic,
and no chrome icon renders full-colour.

**Verifiability.** Partially capture-verifiable (colour classification is
visible in a PNG) but not fully — telling deliberate symbolic rendering apart
from an icon that merely happens to be one colour needs a source read
alongside the capture, not the capture alone.

**Status: not yet satisfied — no subject exists to check.** `ui/row.rs`'s row
widget is a single `gtk::Label` populated with only a title; there is no icon
widget anywhere in the row layout, chrome or content (verified by reading
`apps/hop-gtk/src/ui/row.rs` in full at `7a6f99b`). This item has nothing to
pass or fail yet.

---

### 2. Accessibility — contrast, screen-reader labels, font scaling

**Rule.** A contrast-checked palette in both themes, screen-reader labels on
rows and actions, and system font scaling. D5: "already §8a commitments;
named here as HIG-derived rather than local taste." The concrete numbers and
structural requirements are in
[`2026-08-19-hop-m3-visual-design.md`](superpowers/specs/2026-08-19-hop-m3-visual-design.md)'s
"Accessibility floor" section.

This rule has three independently checkable parts.

#### 2a. Contrast-checked palette, both themes

**Check.** For each text and non-text token in `assets/tokens.css`'s neutral
ramps and accent block, confirm the ratio recorded in its adjacent comment
meets the visual design spec's accessibility floor table (4.5:1 for text,
3:1 for non-text/UI elements — selection indicator, dimmed hints), computed
against the surface the token **actually composites onto**, not a flat
neutral (the spec calls out two ratios that failed exactly this check on
first draft: `--hop-neutral-500` and `--hop-neutral-600-light`, both
corrected and marked `WAS` in `tokens.css`). **Pass condition:** every token
used for text meets ≥4.5:1 against its real rendering surface in both the
dark and light ramps; every non-text token meets ≥3:1.

**Verifiability.** Human-verifiable by arithmetic today; automatable in
principle (WCAG relative-luminance contrast from two hex values is a pure
function) but no such script exists in this repo. Not capture-verifiable on
its own — see 2's overall status below for why a capture of the *running
app* does not currently prove this.

**Status: unknown whether it reaches the screen, though the numbers on paper
check out.** The ratios recorded in `tokens.css`'s comments are consistent
with the visual design spec's own account of the 2026-08-19 pass (two
corrected failures, one false-positive investigated and dismissed, all
documented in place) — I did not re-derive every ratio myself, but found no
internal inconsistency between the two documents. Separately, and more
important for a reviewer: **no `gtk::CssProvider` is installed anywhere in
`hop-gtk`** (`apps/hop-gtk/src` has zero hits for `CssProvider`,
`add_provider_for_display`, or `STYLE_PROVIDER_PRIORITY` outside of comments
describing its absence). `tokens.rs`'s own doc comment confirms `tokens.css`
is parsed today only for a handful of structural pixel values, not loaded as
a stylesheet. The one place a token colour is actually painted —
`ui/mode_label.rs`, via direct Pango attributes from `tokens::MODE_LABEL_RGB`
— bypasses the stylesheet entirely, and that file's own comment names a real
`CssProvider` as separately-scoped future work. So: the palette is
contrast-checked on paper; whether the running window paints with it at all
is a separate, mostly-open question — see item 6 below, which this
duplicates rather than restates.

#### 2b. Screen-reader labels on rows and actions

**Check.** With Orca (or an AT-SPI introspection tool / GTK Inspector's
Accessibility tab) against a live, non-headless build, confirm: each result
row exposes one concatenated accessible name covering title, subtitle, and
its default action; each row exposes `SET_SIZE`/`POS_IN_SET` (so the reader
can say "2 of 47"); each row's full action set is exposed via
`DESCRIBED_BY`; the query entry has `GTK_ACCESSIBLE_ROLE_SEARCH_BOX` with
`ACTIVE_DESCENDANT` tracking the selected row, and rows are never
`grab_focus()`ed; a coalesced result-count announcement fires via a polite
`status` node once the stream quiesces, and errors/offline fire via an
assertive `alert` node — never per-row, which would flood the reader.
**Pass condition:** all of the above hold across at least the results,
no-results, error, and offline states.

**Verifiability.** Not capture-verifiable — a PNG carries no accessibility
tree. Needs either a human with a screen reader, or a not-yet-written
automated AT-SPI check (GTK4's own bindings expose `gtk_test_accessible_*`,
noted in `ui/window.rs`'s test-module comments, but nothing in this repo
calls them for this purpose today).

**Status: not yet satisfied.** No `set_accessible_role`, `update_property`,
or any GTK accessibility API call exists anywhere in `apps/hop-gtk/src`
(verified by grep — zero hits). `ui/row.rs` carries only a title; there is
no subtitle, no action hint, and no accessible-name assembly to check. The
query entry's role and `ACTIVE_DESCENDANT` wiring do not exist.

#### 2c. System font scaling

**Check.** With `org.gnome.desktop.interface text-scaling-factor` (GNOME's
"Large Text") set above 1.0, drive each of the six states and confirm text
grows, rows grow in height rather than clip, the window grows in height
(capped at ~80% of the monitor) to preserve row count, width stays fixed at
400px with titles ellipsizing and action hints collapsing to icon-only, and
row height is computed from measured content rather than a hardcoded
constant — all per the visual design spec's decision 3. **Pass condition:**
at 200% scaling, no text clips or truncates vertically, and the window grows
rather than compresses.

**Verifiability.** Human-verifiable only today — the headless smoke test
does not currently drive `text-scaling-factor`, and broadway's own handling
of that setting for a headless capture is unverified, so this is not
currently capture-verifiable even in principle without more work.

**Status: not yet satisfied, with a concrete conflict on record.**
`ui/row.rs::build()` calls `label.set_height_request(*tokens::ROW_HEIGHT_PX)`
— a fixed height reserved *before* any content is known, by that module's
own design, to stop an async result from shifting layout when it lands. That
is the opposite of "row height is computed from measured content", which the
visual design spec commits to specifically for elevated scaling. Separately,
`ui/mode_label.rs`'s typography is set via
`pango::AttrSize::new_size_absolute`, which that function's own doc comment
says yields "a device pixel count rather than a value further scaled by the
display's own point-to-pixel conversion" — by the file's own account, that
text does not participate in the display's own scaling conversion. Whether
`text-scaling-factor` specifically still reaches it through some other layer
is not something I determined; recorded as a real tension for a reviewer to
weigh, not as a settled verdict either way.

---

### 3. Reduced motion — honoured via the GTK setting

**Rule.** Motion respects `gtk-enable-animations` (GNOME:
`org.gnome.desktop.interface enable-animations`). D5, citing §8a.

**Check.** With the setting off, drive each transition in the visual design
spec's Motion table (window open/close, selection move, skeleton→resolved,
state change, action hint) and confirm each substitutes its documented
reduced form (e.g. window open/close becomes opacity-only at ~85ms with no
scale or translate; selection move becomes an instant reposition;
skeleton→resolved and state change become instant swaps) rather than either
skipping the transition's *purpose* (the state still changes, just without
the animated path) or ignoring the setting and playing the full animation
anyway. With the setting on, confirm the full form plays as specified.
**Pass condition:** every transition in the table honours the reduced form
when the setting is off and the full form when it is on.

**Verifiability.** Only the endpoints of a transition are
capture-verifiable — a screenshot shows a settled frame, never the animation
path or its timing, which is the actual content of this rule. Real
verification needs a human watching a live build with the setting toggled,
or an automated check reading the applied CSS transition/animation
properties against the setting; neither exists in this repo today.

**Status: not yet satisfied — no subject exists to check.** No animation,
transition, or read of `gtk-enable-animations` exists anywhere in
`apps/hop-gtk/src` (verified by grep). The six states and their motion are
fully specified in the visual design spec but the window currently presents
and dismisses with no implemented open/close transition at all. There is
nothing yet to honour or violate the setting.

---

### 4. Full keyboard operability

**Rule.** Every action is reachable by keyboard alone. D5: "already
structural in hop."

**Check.** From a cold launch, using no mouse input at all, confirm: results
navigate (Up/Down/Page Up/Page Down/Home/End), the selected result executes
(Enter), a row's secondary action is reachable (the configured
`SecondaryAction` binding, `Menu` by default), a prefix completes (Tab by
default), and the window dismisses (Escape). Confirm the keymap is
configurable via `config.toml` rather than hardcoded (issue #182), per D5's
"already structural" claim. **Pass condition:** every action above is
reachable from the query entry with keyboard alone, without ever needing to
click or tab onto a specific row.

**Verifiability.** Partially automated-test-verifiable, with a documented
gap. `keymap.rs`'s unit tests and `ui/window.rs`'s
`keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window` test
exercise `Keymap::resolve` and `HopWindow::dispatch_action` directly — real
proof that the *mapping* from action to effect is correct — but neither
drives a real `GdkEvent` through the real `EventControllerKey`. That test's
own comment, in `ui/window.rs`'s test module, explains why at length:
GTK4 removed `gtk_test_widget_send_key` with no replacement, and GDK4 exposes
no constructor for a synthetic key event on any backend, broadway included —
confirmed there by grepping the installed GTK4 headers against the GTK3
ones. So "does a physical keypress actually move the selection" is not
exercisable by any automated test in this environment; it needs a human at a
keyboard on a live build. Not capture-verifiable at all — a screenshot is one
frame, and can at best show the *result* of a keypress a human already made.

**Status: likely satisfied, not independently verified end-to-end.** The
keymap covers all ten actions with defaults, is attached at
`PropagationPhase::Capture` so it can intercept before GTK's own default
bindings, and is loaded from `config.toml`. I did not personally drive the
live app keyboard-only through every action (in particular, confirming
`SecondaryAction`'s dispatched effect end-to-end) — recorded as unknown
rather than assumed satisfied, per the verifiability gap above.

---

## Deliberately broken

These three are HIG rules hop does not follow, on purpose. Each item exists
to stop a reviewer filing the deliberate shape as a defect — the check below
confirms the shape matches what D5 committed to, not that it matches GNOME
Shell.

### 5. The window model

**The break.** hop is a ~400×500px overlay, not GNOME Shell's fullscreen
modal overview, and it is deliberately not an Adwaita dialog. D5, "The
window model": "worth stating plainly, because 'GNOME-native' is easily
misread as 'matches GNOME Shell's search', and hop does not."

**Why a reviewer should not flag this.** hop's platform integration
(layer-shell where the compositor supports it, a desktop entry, respecting
`XDG_RUNTIME_DIR`) is what "GNOME-native" refers to here — not visual or
behavioral parity with GNOME Shell's own search overview. A compact,
non-fullscreen popup is the intended shape, not an unfinished one.

**Check.** Confirm: (1) the window's default size is 400×500px (width fixed;
height may grow per accessibility item 2c's decision 3, capped at ~80% of
the monitor) rather than fullscreen; (2) the window's type in source is
`adw::ApplicationWindow` — an ordinary top-level or layer-surface window —
never `adw::Dialog`, `adw::AlertDialog`, or any Adwaita dialog type; (3) it
does not block the rest of the desktop the way a modal overview does.
**Pass condition:** the window is a compact, non-fullscreen,
non-Adwaita-dialog window at the specified geometry.

**Verifiability.** Capture-verifiable for geometry — a `--screenshot` PNG's
own pixel dimensions show the window's actual size directly. Source-verifiable
for widget type — grep `ui/window.rs` for `adw::ApplicationWindow` versus any
`adw::Dialog`/`adw::AlertDialog` construction. Both are cheap, mechanical
checks with no ambiguity.

**Status: satisfied, verified.** `ui/window.rs::HopWindow::build` constructs
an `adw::ApplicationWindow` (never a dialog type — grep confirms no
`adw::Dialog`/`adw::AlertDialog` reference anywhere in the crate).
`tokens.rs`'s own unit test pins `*tokens::WINDOW_SIZE_PX` — parsed live out
of `assets/tokens.css` — to `(400, 500)`. `layer_shell.rs` configures the
overlay layer with no anchors ("a centered popup rather than an
edge-anchored panel") where the compositor supports `zwlr_layer_shell_v1`,
and falls back to an ordinary centered top-level window — not fullscreen —
everywhere else, GNOME/Mutter included, by design rather than as a degraded
case.

---

### 6. Stock widget styling

**The break.** `assets/tokens.css` governs visual styling; Adwaita's stock
defaults do not. D5, "Stock widget styling."

**Why a reviewer should not flag this.** Rows, chrome, and controls are
expected to look different from a default Adwaita/GNOME application — that
divergence is the point of the token system, not an unstyled placeholder a
reviewer should ask to be "fixed" toward Adwaita's look.

**Check.** For each themeable surface — window background, row
background/hover, the selection indicator, typography — confirm its rendered
colour and font trace to a `--hop-*` token in `assets/tokens.css` rather than
to Adwaita's own stylesheet defaults (Adwaita's default blue accent, its own
neutral grays, its default font). **Pass condition:** no visible surface in a
captured state falls back to an un-tokenized Adwaita default where the token
contract specifies a hop value.

**Verifiability.** Capture-verifiable where the divergence is unambiguous (an
amber selection indicator versus Adwaita's default blue is visually obvious
in a screenshot) but not exhaustively — some coincidental resemblance is
possible, and font family in particular (Iosevka/Inter versus a system
fallback) can be subtle at a small capture size and is safer confirmed by
also checking that the bundled-font GResource path is actually reached.

**Status: not yet satisfied — a specific, current gap.** There is no
`gtk::CssProvider` installed anywhere in `hop-gtk` (same grep as item 2a:
zero hits for `CssProvider`, `add_provider_for_display`, or
`STYLE_PROVIDER_PRIORITY` in `apps/hop-gtk/src` outside comments describing
the absence). `tokens.rs`'s own doc comment states this outright:
`assets/tokens.css` is parsed today only for a handful of structural pixel
values (row height, window size); it is not loaded as a GTK stylesheet, and
"a real stylesheet pass that hardcodes literal values out of it is
explicitly named as future work, not this issue's to start." The one place a
token-derived value is actually painted is `ui/mode_label.rs`, which sets
Pango attributes directly from `tokens::MODE_LABEL_RGB`/`MODE_LABEL_FONT` —
bypassing CSS, and by that file's own comment, a stand-in until the real
provider lands. So as of `7a6f99b`, the running window is still
substantially painted with Adwaita's stock defaults — window chrome, row
background, the selection indicator's fill — and only the mode label's
colour and typography are token-derived, by a mechanism other than the one
the contract eventually describes. This is a real, present conformance gap
against this item, not a hypothetical one; recording it here per this
issue's scope, not fixing it.

---

### 7. The accent colour

**The break.** One committed brand accent (`#E3A83B` dark / `#875C0F`
light), used only for the selection indicator, focus ring, and action
hints — never for body text, and never the desktop's own accent colour. D5,
"The accent colour": §8a treats this as "the first-in-class-versus-AI-template
separator."

**Why a reviewer should not flag this.** hop intentionally does not read
`org.gnome.desktop.interface accent-color` (or any other desktop's accent
setting). A launcher whose brand colour changed with the user's desktop
theme would not have a brand colour.

**Check.** Confirm: (1) no code path reads a desktop accent-color setting;
(2) every place the accent renders — selection indicator, focus ring, action
hints — uses `--hop-accent`/`--hop-accent-light` (or their hover/subdued/ring
variants), not a system-supplied colour; (3) the accent never colours body
text (title, subtitle, path text). **Pass condition:** the accent renders
identically regardless of the user's desktop accent-color setting, and never
appears as body-text colour.

**Verifiability.** Capture-verifiable for "does the accent change with the
desktop setting" (two captures under two different desktop accent settings,
diffed — not yet produced) and for "is the accent ever used on body text"
(visual inspection of any capture). Source-verifiable for "is
`accent-color` ever read at all" (grep).

**Status: partially satisfied, mostly unbuilt.** No code in `apps/hop-gtk`
reads a desktop accent-color setting (grep found nothing), so "never follows
the desktop" holds — trivially, since there's nothing yet that could follow
it. But the accent itself is barely wired in: `ui/window.rs` adds the
`hop-selection-indicator` CSS class to the selection indicator widget, but
per item 6 above there is no stylesheet installed to give that class its
amber fill — so today the selection indicator does not yet render the
committed accent colour at all. The commitment is real and precise in
`tokens.css`; its render path does not exist yet.

---

## Summary, at commit `7a6f99b`

| # | Item | Kind | Status |
| --- | --- | --- | --- |
| 1 | Icon language | Binding | Not yet satisfied — no icons exist to check |
| 2a | Contrast-checked palette | Binding | Unknown whether it reaches the screen (values check out on paper) |
| 2b | Screen-reader labels | Binding | Not yet satisfied |
| 2c | System font scaling | Binding | Not yet satisfied — active conflict on record |
| 3 | Reduced motion | Binding | Not yet satisfied — no motion exists to check |
| 4 | Full keyboard operability | Binding | Likely satisfied — not independently verified end-to-end |
| 5 | Window model | Deliberately broken | Satisfied, verified |
| 6 | Stock widget styling | Deliberately broken | Not yet satisfied — no stylesheet installed |
| 7 | Accent colour | Deliberately broken | Partially satisfied — never follows the desktop; not yet rendered |

None of the above is this issue's to fix — see #183's own scope. Recorded so
the reviewer of the next frontend slice knows exactly what it inherits.
