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

Since issue #228, the smoke test also *defends* the two capture-readable
claims that nothing else can pin: every capture's dimensions are asserted
against the token-declared window size from the PNG header, and the results
capture's selected-row composited fill is decoded and sampled against
`--hop-accent-subdued`'s documented composite — so those specific claims are
regression-defended on every test run, not point-in-time prose. The
limitation above is unchanged: a capture still cannot show timing,
accessibility, live settings, or input handling, and flat token colours
(the row ground, the hint-chip background) remain pinned at the declaration
level by the token-resolution tests rather than by pixel assertions.

**Status legend**, applied honestly rather than aspirationally — this
document's value depends on a reviewer being able to trust a "satisfied"
claim:

| Status | Meaning |
| --- | --- |
| **Satisfied, verified** | Checked against the build at the commit cited; holds. |
| **Partially satisfied** | Part of the item holds, part does not or is unbuilt; the gap is named. |
| **Not yet satisfied** | Checked; does not hold yet. Named as a gap, not filed as a bug — M3's remaining slices own closing it. |
| **Unknown** | Not checked, or checkable only by a human/setting this pass did not have. Recorded as unknown rather than guessed. |

Every status below was determined by reading the build at commit `0fc1c92`
(`docs: correct theme contract's presence claim for GTK`, #211) — by source
inspection, by running the existing test suite, and by producing several
real `--screenshot` captures against a live `hopd` serving this machine's own
installed applications (not a stub or a fixture — see item 1 for how that was
done and what it did and did not settle). Where a status could not be
determined this way, it says so rather than guessing. This is a full re-pass:
every item below was re-checked against this commit, not left quoting an
earlier one, per the issue that requested this refresh (#202).

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

**Status: satisfied, verified — for the one icon category this UI currently
has.** Issue #190 turned `ui/row.rs`'s row widget from a bare `gtk::Label`
into a `gtk::Box` carrying a leading `gtk::Image` (`ui::row::build`,
`ui::row::resolve_icon`); `bind` resolves it from `item.icon: Option<IconSpec>`
in the three-way split `resolve_icon`'s own doc comment names — `None` clears
the widget, `IconSpec::Name` hands the lookup to
`gtk::Image::set_icon_name`, `IconSpec::Path` decodes the file itself via
`load_path_texture` (gated by `icon_roots::ALLOWED_ICON_ROOTS`, issue #93)
and falls back to the literal `"image-missing"` icon name on any failure.

A results-state capture was produced end to end, against a real, unmodified
`hopd` serving this machine's actual installed applications (the `apps`
provider, `crates/hopd/src/apps.rs`, indexing real `.desktop` files under
`/usr/share/applications` and `~/.local/share/applications` — not a fixture
or a stub host): `gtk4-broadwayd :42` as the headless backend,
`hop-gtk --screenshot out.png --query <text>` exactly as the CI smoke test
and this item's own Check both call for. Three queries were captured and
inspected as PNGs:

- `--query code` returned two real rows. "Visual Studio Code" renders its
  actual brand icon (a blue/teal glyph) via `IconSpec::Name("vscode")` — a
  successful theme lookup, confirmed by reading `code.desktop`'s own
  `Icon=vscode` line. "T3 Code" (a locally installed AppImage,
  `~/.local/share/applications/t3code.desktop`, `Icon=t3code`) also resolves
  through `IconSpec::Name`, but the name lookup fails — this machine's icon
  cache was never updated for the user icon directory that actually holds
  `t3code.png` — and GTK's own built-in fallback glyph renders instead: a
  cream/tan document-with-warning-triangle icon, two flat colours, not a
  `-symbolic`-suffixed lookup and not recoloured to match text the way a
  true symbolic icon is. It is a resolution failure, not a violation of this
  item's rule: the element is still content (an app's own icon slot), and
  GTK's non-symbolic fallback for a missing name is itself non-symbolic.
- `--query gitkraken` returned "GitKraken", whose `Icon=/usr/share/pixmaps/gitkraken.png`
  line makes it an `IconSpec::Path` case — `/usr/share/pixmaps` is one of
  `icon_roots::ALLOWED_ICON_ROOTS`' permitted roots, confirmed by reading
  that module. The captured row shows the real, full-colour kraken logo,
  proving the `Path` arm's decode-and-permit path renders a genuine
  full-colour content icon end to end, not only that the code compiles.

Pairing the visual read with the source read: every icon-bearing element
across all three captures is content (an item's own icon), none renders
`-symbolic` or single-tone-by-construction, and the one fallback case is a
resolution failure that still renders non-symbolically. **No chrome icon
exists anywhere in this UI to test the rule's other half against** — a grep
across `apps/hop-gtk/src` for `gtk::Image`/`set_icon_name`/`icon-name`
construction finds exactly one call site, `ui::row::build`'s leading icon;
the query entry, the mode label, the hint chips, and the selection indicator
carry no icon of their own. So "no chrome icon renders full-colour" holds
because there is no chrome icon yet to violate it, not because one was
checked and found compliant — recorded plainly rather than glossed over.

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

**Status: partially satisfied — the palette now reaches the screen, and one
concrete contrast failure was found in it.** The ratios recorded in
`tokens.css`'s comments are still consistent with the visual design spec's
own account of the 2026-08-19 pass (two corrected failures, one
false-positive investigated and dismissed, all documented in place) — I did
not re-derive every ratio myself, so that half remains plausible rather than
independently confirmed.

What changed since `7a6f99b` is the other half: issue #193 installed
`style::install`'s `gtk::CssProvider`, and this pass confirmed by pixel
sampling a live capture (`--query code`, dark palette, the default this
environment resolves to — see item 7 below for why the light palette could
not be forced live) that several tokens really do reach the screen at their
documented values, not merely in theory: an unselected row's background
sampled exactly `#121214`, matching `--hop-bg`/`--hop-neutral-950`; the
selected row's composited fill sampled exactly `#2f2719`, matching
`--hop-accent-subdued`'s own comment ("composites to #2f2719") to the byte;
a hint chip's background sampled exactly `#202024`, matching `--hop-bg-hover`.
That is real, not plausible, confirmation for the tokens it covers.

Of these three samples, the composited selection fill is no longer a
one-off: issue #228 committed a regression for it — `headless_smoke.rs`
decodes its own results capture and asserts the fill equals the composite
`--hop-accent-subdued` documents, computed live from the committed token
values — so that claim is re-proven on every test run. The row ground and
hint-chip samples deliberately did not get pixel regressions: those flat
token colours are already pinned at the declaration level by the
token-resolution tests, which is the cheaper and earlier place to catch a
drift. The light palette remains unverified by any capture (see item 7
below), manually or otherwise.

A genuine, previously-unrecorded contrast failure turned up while checking
the rest; it has since been closed by issue #214 — the finding is kept as
recorded, with its closure at the end of this paragraph:
`assets/stylesheet.css`'s `.hop-row-hint-key` rule set
`color: {{hop-accent}}` — the bare, palette-invariant ramp literal
(`#e3a83b`), not `{{hop-sel-bar}}` or any other name `.hop-theme-light`
redeclares. Reading `apps/hop-gtk/src/tokens.rs`'s `raw_from`: a
`Palette::Light` lookup checks `.hop-theme-light`'s overlay first, but that
overlay only redeclares the SEMANTIC LAYER names (`--hop-bg`,
`--hop-sel-bar`, and the rest — twelve today, issue #214's palette-aware
`--hop-hint-accent` alias being the twelfth; eleven at this pass's own
snapshot) — `--hop-accent` itself is not among them, so
a light-palette resolution of `--hop-accent` falls through to the same dark
literal a dark-palette resolution gets. The result: under the light palette,
the hint-key glyph ("Enter") would render in the *dark* accent, `#e3a83b`,
against the light paper background (`#faf9f6`), rather than the committed
light accent `#875c0f` item 7 below describes. Computing WCAG contrast from
those two hex values (the same relative-luminance formula this item's own
Verifiability note treats as "automatable in principle") gives ≈2.0:1 —
against the 4.5:1 text floor this item's own pass condition sets, and against
`--hop-accent-light`'s own documented "5.13:1 on paper" the rule intends
instead. This was checked by arithmetic, not by a live light-palette capture:
this pass tried to force one (`gsettings set
org.gnome.desktop.interface color-scheme prefer-light`, then re-captured)
and got a byte-identical PNG to the dark-palette capture — this sandboxed,
portal-less headless environment does not appear to let
`adw::StyleManager::default().is_dark()` see that setting at all (`style.rs`'s
own doc comment already names dependence on desktop portal/GSettings
plumbing this crate does not itself provide), so a live light-mode render
was not achievable here; see item 7's own note on the same limitation. The
arithmetic itself does not depend on that capture, though — it follows
directly from the hex values `tokens.css` and `resolve`'s own logic commit
to. That follow-up was filed and closed as issue #214:
`.hop-row-hint-key` now asks for `{{hop-hint-accent}}` — a semantic-layer
alias declared `var(--hop-accent)` under the dark palette and redeclared in
`.hop-theme-light` as `var(--hop-accent-light)`, with regression tests
pinning the glyph's resolved colour to different values per palette — so the
glyph resolves to the committed light accent under the light palette and the
≈2.0:1 failure computed above cannot occur. What the closure does not change:
the environment limitation above still stands — no portal/GSettings plumbing
exists to force a live light-palette capture, so the light palette's
on-screen rendering remains confirmed by arithmetic and by the regression
tests, not by a capture of the running app.

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
(verified by grep — zero hits, unchanged from `7a6f99b`). This is worth
re-deriving rather than carrying forward, because what there now is to *not*
expose has grown: issue #196 gave the row a real subtitle
(`ui::row::resolve_subtitle`, hidden rather than reserved when absent — see
that module's "The absent case" section) and issue #197 gave it a real
right-aligned action hint pairing the item's default-action label with
`Keymap::activate_binding_display`'s key glyph. Both render correctly on
screen (confirmed in item 1's captures above — every row shows "Open" +
"Enter"), but neither is wired into any accessible name: there is still no
concatenated accessible name covering title, subtitle, and default action,
no `SET_SIZE`/`POS_IN_SET`, no `DESCRIBED_BY` for the row's action set. The
query entry's `GTK_ACCESSIBLE_ROLE_SEARCH_BOX` and `ACTIVE_DESCENDANT`
wiring still do not exist either. So this item moves from "nothing exists to
expose" to "real content exists and none of it is exposed" — a different,
more concrete gap than the one recorded at `7a6f99b`, not the same one
restated.

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
`ui/row.rs::build()` still calls `container.set_height_request(*tokens::ROW_HEIGHT_PX)`
— a fixed height reserved on the row's outer `gtk::Box` *before* any content
is known, by that module's own design (now covering the icon, text column,
and hint too, not only a title label), to stop an async result from shifting
layout when it lands. That is the opposite of "row height is computed from
measured content", which the visual design spec commits to specifically for
elevated scaling — unchanged in substance from `7a6f99b`, re-confirmed by
reading the current file in full. No code anywhere in the crate grows the
window's height, or reads `text-scaling-factor`/`org.gnome.desktop.interface`
at all (grep for both finds nothing) — the ~80%-of-monitor growth cap the
visual spec commits to is unbuilt, not merely unverified.

One piece of the previous pass's evidence does not survive re-reading and is
retracted rather than carried forward: it cited `ui/mode_label.rs` setting
its typography via `pango::AttrSize::new_size_absolute`, which that
function's own doc comment said bypassed the display's scaling conversion.
Issue #193 removed that Rust-side Pango code outright — `ui/mode_label.rs`'s
own doc comment now (section "CSS supersedes the Pango stand-in") explains
why: keeping both would have made the equivalent CSS rule permanently dead,
since GTK applies a label's `set_attributes` on top of, not instead of, its
resolved CSS style. The mode label's typography is `.hop-mode-label`'s CSS
`font: {{font:hop-text-section}}` rule now, a plain pixel size in a
`gtk::CssProvider`-loaded stylesheet — a structurally different mechanism
from the removed Pango attribute, and whether GTK's CSS `px` font sizing
itself scales with `text-scaling-factor` is a real, open question this pass
did not determine either way (the same environment limitation item 2a and
item 7 name — no reachable portal/GSettings plumbing to drive the setting
live here) rather than something to assume settled by the old, now-incorrect
citation. Recorded as unknown for that specific question, not as a bypass
that no longer exists in the code.

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

**Status: partially satisfied — a real subject now exists for exactly one of
the six motion-table rows; the other five remain unbuilt.** Issue #207 gave
this crate its first motion: `apps/hop-gtk/src/style.rs` reads
`gtk::Settings::default()`'s `gtk-enable-animations` property at startup
(before the action hint's first paint, from the same `connect_startup`
handler that installs the stylesheet) and subscribes to its
`notify::gtk-enable-animations` signal, so a live flip of the setting
re-resolves and reloads the installed `gtk::CssProvider` with no restart —
the same live-subscription shape `style.rs` already used for the palette
axis, now proven for motion too by `apps/hop-gtk/tests/motion_setting.rs`,
which drives the setting through its own public setter and confirms the
*same* installed provider's serialized CSS changes. This is GTK's own
setting, read directly — no portal call, no direct read of
`org.gnome.desktop.interface`'s `enable-animations` key — matching
`assets/tokens.css`'s own `@media` comment naming `gtk-enable-animations` as
the source of truth.

The one row this issue wires motion into is the action hint's entrance fade:
`assets/stylesheet.css`'s `.hop-row-hint.hop-row-hint-shown` rule declares
`transition: opacity {{motion:hop-duration-fast}} {{motion:hop-ease-out}}
{{motion:hop-delay-hint}};`, every value resolved through
`apps/hop-gtk/src/tokens.rs`'s `resolve_motion` rather than hand-copied.
Under the full-motion setting this resolves to 80ms, `--hop-ease-out`, and a
40ms delay (confirmed via `gtk::CssProvider::to_str()`'s own serialized
`transition-duration: 80ms;`/`transition-delay: 40ms;` in
`stylesheet.rs`'s and `motion_setting.rs`'s own tests); under reduced motion
the delay collapses to `0ms` (one of the six `@media` overrides
`assets/tokens.css`'s own block names) while the 80ms duration and the
easing curve survive unchanged — the fade still plays, opacity only, just
without the anti-flicker delay, exactly the "reduced motion is not shorter
everywhere" rule this document's own item-3 check already named.
`apps/hop-gtk/src/ui/row.rs`'s `bind` plays that fade only on a genuine
not-shown-to-shown transition for the row currently on screen — never on a
bare recycle, regardless of whether a recycled row's new item's hint text
differs from the old one's — a distinction `tests/view_tree_renderer.rs`'s
own "issue #207" section exercises directly against
`ui::row::HINT_SHOWN_CLASS`'s presence on a real widget.

**What this status does not claim:** the other five motion-table rows —
window open/close, selection move, skeleton→resolved, state change — are
exactly as unbuilt as they were before this issue. None of #193, #196, #197,
or #207 gave the window an open/close transition, the selection indicator a
movement transition, or the skeleton/state crossfade its own transition;
`apps/hop-gtk/src/ui/window.rs` still presents and dismisses with no
animation of any kind. The mechanism issue #207 built — `tokens::Motion`,
`tokens::resolve_motion`, `stylesheet.rs`'s `{{motion:name}}` placeholder,
and `style.rs`'s live subscription — is reusable for all five, but none of
them consume it yet, and this item's status is not to be read as "reduced
motion is honoured" in general, only as "one real, live-verified subject now
exists to check, where before this issue there was none." Per this item's
own **Verifiability** note above, only the endpoints of that one subject's
transition are capture-verifiable; no test in this repo claims to have
proven its path or timing, only its resolved duration/delay/easing values
and the correctness of when it is triggered.

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

**Status: unknown, not independently verified end-to-end.**
Re-confirmed at this commit: the keymap still covers all ten actions with
defaults (`Action::ALL`, unchanged), is still attached at
`PropagationPhase::Capture` (`ui::window::HopWindow::wire_keyboard`) so it
can intercept before GTK's own default bindings, and is still loaded from
`config.toml` (`Keymap::load`). Issue #197 added `Keymap::binding_for` and
`Keymap::activate_binding_display` — read for this pass — but both are pure
lookups over the same resolved binding table `resolve`/`dispatch_action`
already used at `7a6f99b`; neither changes what the keymap covers or how it
dispatches, only what a caller can ask it after the fact. I did not
personally drive the live app keyboard-only through every action this pass
either (in particular, confirming `SecondaryAction`'s dispatched effect
end-to-end) — recorded as unknown rather than assumed satisfied, per the
verifiability gap above, which itself still holds: GTK4 still exposes no
synthetic-key-event constructor on any backend, broadway included.

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

**Status: satisfied, verified.** Re-confirmed at this commit:
`ui/window.rs::HopWindow::build` still constructs an `adw::ApplicationWindow`
(never a dialog type — grep confirms no `adw::Dialog`/`adw::AlertDialog`
reference anywhere in the crate, unchanged). `tokens.rs`'s own unit test
still pins `*tokens::WINDOW_SIZE_PX` — parsed live out of `assets/tokens.css`
— to `(400, 500)`, and this pass's own `--screenshot` captures (item 1 above)
independently confirm it: every PNG produced this pass measures exactly
400×500 pixels. `layer_shell.rs` still configures the overlay layer with no
anchors ("a centered popup rather than an edge-anchored panel") where the
compositor supports `zwlr_layer_shell_v1`, and falls back to an ordinary
centered top-level window — not fullscreen — everywhere else, GNOME/Mutter
included, by design rather than as a degraded case. No titlebar/`HeaderBar`
is set on the window either (`adw::ApplicationWindow::builder()` never calls
`.titlebar(...)`, and every capture this pass produced shows no window-chrome
bar) — consistent with "a compact popup", not an unfinished dialog waiting
for its header.

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

**Status: partially satisfied — the cause named at `7a6f99b` is closed, and
a different, more specific set of gaps replaces it.** Issue #193 installed a
real `gtk::CssProvider` (`style::install`, called from both `run_interactive`
and `run_screenshot`'s `connect_startup`), loading `assets/stylesheet.css`
resolved against `tokens.css` at `gtk::STYLE_PROVIDER_PRIORITY_APPLICATION`.
This pass confirmed the provider is not merely installed but actually
governing pixels, the same pixel-sampling this item's own Verifiability note
calls "visually obvious": a resting row's background sampled exactly
`#121214` (`--hop-bg`), a selected row's composited fill sampled exactly
`#2f2719` (`--hop-accent-subdued`'s own documented composite), and a hint
chip's background sampled exactly `#202024` (`--hop-bg-hover`) — three
surfaces genuinely tracing to their tokens, not stock Adwaita, confirmed
against a live capture rather than assumed from the stylesheet's text alone.

Since issue #228, the selected-row sample among these is regression-defended:
`headless_smoke.rs` decodes its own results capture on every test run and
asserts the composited fill equals `--hop-accent-subdued`'s documented
composite, computed from the committed token values — no longer dependent on
this pass's one-off manual sampling. The row-ground and hint-chip samples
stay declaration-level (token-resolution tests), by design; the remaining
stock surfaces named below are unchanged.

But "a provider now exists" is not "every themeable surface passes", and
this pass's own captures surfaced several concrete surfaces that fell back to
an un-tokenized Adwaita default, each checked against this item's own pass
condition rather than waved through on the provider's existence. One of them
— the window/listview base background, immediately below — has since been
closed by issue #215, and the declared brand fonts' bundling half by issue
#198; the rest remain stock:

- **Most of the window, in the state a user sees most often — closed by
  issue #215.** An empty-query capture (`--screenshot out.png`, no
  `--query`) originally showed almost the entire window body — everywhere
  the `gtk::ListView` widget's own base CSS node is visible, not covered by
  an actual `row` child — filled with a flat `rgb(30, 30, 30)`, sampled
  consistently across a wide area (bottom-left, bottom-right, and directly
  below the last real row in the results capture). That value matched
  neither `--hop-bg` (`#121214` = `rgb(18, 18, 20)`) nor `--hop-bg-hover`
  (`#202024` = `rgb(32, 32, 36)`) — it read as libadwaita's own stock dark
  `window`/`view` background, showing through because `assets/stylesheet.css`
  styled `window.background` and `listview > row` but never the `listview`
  node itself, so any area the list view owns but no realized row covers
  still painted Adwaita's default. Since the empty-query state is the
  window's default, most-often-seen state, this was not a cosmetic corner
  case. Issue #215 closed it: `assets/stylesheet.css` now also declares
  `listview { background-color: {{hop-bg}}; }`, resolved against the same
  window-ground token `window.background` above already paints, confirmed
  red/green with a live capture — the same area now samples `rgb(18, 18,
  20)`, matching `--hop-bg` exactly, not libadwaita's stock default.
- **The query entry.** The zoomed capture (item 1's `--query code` PNG)
  shows the entry's background as `rgb(40, 40, 42)` — a fourth colour,
  matching none of `--hop-bg`/`--hop-bg-hover`/`--hop-fg` — and its focus
  outline as a violet-blue ring, GTK/Adwaita's own stock `entry:focus`
  colour, not `--hop-accent`/`--hop-sel-ring` in any form. No `entry`
  selector exists anywhere in `assets/stylesheet.css` — grep confirms it —
  so this is not a rendering bug, it is an un-styled surface exactly as the
  pass condition describes.
- **The row title's own typography.** `assets/tokens.css` declares
  `--hop-text-title: 500 14px/20px var(--hop-font-sans)` specifically for
  this label, but no selector in `assets/stylesheet.css` targets
  `ui::row::TITLE_CHILD_NAME`/`hop-row-title` at all — only `.hop-row-subtitle`,
  `.hop-row-hint-label`, and `.hop-row-hint-key` exist for row text. The
  title inherits `window.background`'s `color`/`font-family` (so it is at
  least the right colour and typeface stack, not wrong) but its weight,
  exact size, and any tracking are whatever GTK's default label styling
  gives a plain `gtk::Label` — `--hop-text-title` is declared and unused.
- **The window's own shape.** `--hop-radius-lg` ("the window panel") and
  `--hop-shadow-window`/`--hop-shadow-window-light` are declared in
  `tokens.css` but referenced by no selector in `assets/stylesheet.css` —
  the rounded corner visible in every capture this pass produced is
  Adwaita's own default client-side-decoration shape, not a hop value.
- **The declared brand fonts, on this machine — the bundling half closed by
  issue #198.** At this pass's snapshot, `fc-list` on the machine this pass
  ran on showed neither "Inter" nor "Iosevka"/"Iosevka Term" installed, and
  `apps/hop-gtk` bundled no GResource of its own (no
  `gresource`/`register_resource`/font-map call anywhere in the crate) — this
  item's own Verifiability note already flagged font family as needing
  exactly that check. So every capture this pass produced, including the
  ones confirming correct *colour* above, rendered every label in a
  fallback system font, not hop's declared identity type; the CSS
  `font-family` chain was correctly wired (confirmed by reading it) but its
  actual on-screen effect for typeface, specifically, was not something this
  pass could observe. Issue #198 closed the bundling half: the five faces now
  compile into a GResource (`assets/hop-gtk.gresource.xml`, registered by
  `fonts::bundle` before Pango constructs its first font map, with fontconfig
  told about the materialized directory). Better, the check this item's
  Verifiability note called for is no longer hypothetical:
  `apps/hop-gtk/tests/font_resolution.rs` asks Pango for `"Inter"` and
  `"Iosevka Term"` through a real `pango::Context` and asserts each loaded
  font's own family echoes the request back — the bundled-font GResource path
  is confirmed actually reached, cited here as the evidence rather than
  re-derived. What remains open from this bullet is unchanged: whether a
  *captured* frame on a given machine renders the bundled faces is still the
  separate, manual-capture proof the Verifiability note describes, and the
  query entry, the row title's typography, and the window's own shape remain
  stock Adwaita.

Row background and row-hover, the selection indicator's fill, the hint
chips' colours and backgrounds, the subtitle's colour, and the mode label's
full typography (weight/size/family/tracking/colour, all now via
`.hop-mode-label`'s CSS rule rather than the removed Pango stand-in — see
item 2c) do trace to tokens, confirmed by source and, where a live example
existed to capture, by pixel sampling. The gaps above are named as what they
are: real and specific, not a residue of "no provider yet." The window/listview
background gap dominated the most commonly seen state and was, as this pass
suggested, worth a follow-up issue; that follow-up was filed and closed as
issue #215 (see above). The query entry, the row title's typography, and the
window's own shape remain open, not filed here per this pass's own scope;
the font-family gap's bundling half has since been closed by issue #198
(above), leaving only the capture-side confirmation that the bundled
typefaces are what actually renders on screen.

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

**Status: partially satisfied — real progress since `7a6f99b`, one clause
still fully unbuilt.** No code in `apps/hop-gtk` reads a desktop accent-color
setting (grep found nothing, unchanged), so "never follows the desktop"
still holds trivially. What changed is the middle clause: at `7a6f99b`, the
selection indicator's CSS class existed with no stylesheet to give it a
fill; issue #193 closed that. This pass's own pixel sample of a selected row
(item 6 above) reads exactly `#2f2719` — `--hop-accent-subdued`'s own
documented composite, to the byte — so the selection indicator now genuinely
renders the committed accent, confirmed live rather than assumed. The hint
chip's key glyph (`.hop-row-hint-key`, issue #197) also renders in the
accent — `--hop-hint-accent`, issue #214's palette-aware alias that resolves
to `--hop-accent` under the dark palette — visible as amber "Enter"/"Open"
text in every capture this
pass produced, and per that rule's own comment this is a deliberate use of
the accent's reservation ("action hints"), not an exception to
"never for body text" — a key glyph is a short badge naming a physical key,
not prose.

Since issue #228 that sample is no longer the only evidence: the committed
regression in `headless_smoke.rs` re-reads the composited fill from a fresh
capture on every test run and asserts it against `--hop-accent-subdued`'s
documented composite, so "the selection indicator renders the committed
accent" is defended by a test, not by this pass's prose.

The focus ring clause is not partially built, it is entirely absent: grep
for "focus" across `apps/hop-gtk/src` finds no CSS class, no widget, and no
`--hop-sel-ring`/`--hop-accent-ring` reference anywhere outside `tokens.css`
itself — `assets/stylesheet.css`'s own `.hop-selection-indicator` comment
says so directly ("`--hop-sel-ring`... deliberately NOT used here yet...
Left for whichever follow-up adds a real border/ring-width token"). This
pass's own zoomed capture of the focused query entry shows a violet-blue
outline — GTK/Adwaita's stock `entry:focus` ring, not hop's accent in any
form — which is the concrete, on-screen shape of that absence, not only a
grep result.

One more thing surfaced while checking this item, cross-referenced rather
than repeated in full: item 2a above found that `.hop-row-hint-key`'s
`color: {{hop-accent}}` resolved to the *dark* accent literal even under the
light palette (`.hop-theme-light` did not then redeclare `--hop-accent`), so
a light-mode hint glyph would have rendered `#e3a83b`, not the committed
light accent `#875c0f` this item's own "the break" section names. Issue #214
has since closed it: the rule now asks for the palette-aware
`--hop-hint-accent` alias instead, which `.hop-theme-light` redeclares as
`var(--hop-accent-light)`, so the glyph resolves to the committed light
accent under the light palette and this slip no longer exists — see item 2a's
annotation for the detail. It was a contrast failure first and an
accent-fidelity slip second — item 2a carries the arithmetic and why a live
light-palette capture could not be forced in this environment.

---

## Summary, at commit `0fc1c92`

| # | Item | Kind | Status |
| --- | --- | --- | --- |
| 1 | Icon language | Binding | Satisfied, verified — for content icons, the only kind this UI has; no chrome icon exists yet to test the rule's other half |
| 2a | Contrast-checked palette | Binding | Partially satisfied (updated by #214, after this table's own `0fc1c92` snapshot) — several tokens confirmed reaching the screen at their exact documented values; the hint-key glyph's light-palette contrast failure (≈2.0:1) is closed, the glyph now resolving through the palette-aware `--hop-hint-accent` alias; the remaining gap (recorded ratios plausible rather than independently re-derived; no reachable portal/GSettings plumbing for a live light-palette capture) is unchanged — see item 2a's own section above for the full account |
| 2b | Screen-reader labels | Binding | Not yet satisfied — real subtitle and hint content now exists (#196, #197) and none of it is exposed |
| 2c | System font scaling | Binding | Not yet satisfied — the row's fixed-height reservation still conflicts with it; the previous mode-label evidence is retracted (that code was removed by #193) |
| 3 | Reduced motion | Binding | Partially satisfied (updated by #207, after this table's own `0fc1c92` snapshot) — a real, live-verified subject exists for the action hint's entrance fade; the other five motion-table rows (window open/close, selection move, skeleton→resolved, state change) remain exactly as unbuilt as at `0fc1c92` — see item 3's own section above for the full account |
| 4 | Full keyboard operability | Binding | Unknown — not independently verified end-to-end |
| 5 | Window model | Deliberately broken | Satisfied, verified |
| 6 | Stock widget styling | Deliberately broken | Partially satisfied (updated by #215 and #198, after this table's own `0fc1c92` snapshot) — the provider (#193) exists and several surfaces confirmed tokenized; the window/listview base background (most of the default empty state) is now tokenized too (#215), closing that surface, and the declared brand fonts are bundled in a GResource whose path Pango is test-confirmed to reach (#198); the query entry, the row title's typography, and the window's own shape remain stock Adwaita — see item 6's own section above for the full account |
| 7 | Accent colour | Deliberately broken | Partially satisfied (updated by #214, after this table's own `0fc1c92` snapshot) — never follows the desktop; the selection indicator and hint-key glyph now genuinely render the accent (pixel-confirmed), and the glyph's light-palette slip is closed by #214's palette-aware alias; the focus ring remains entirely unbuilt, confirmed stock Adwaita blue on screen — see item 7's own section above for the full account |

None of the above is this issue's to fix — see #183's own scope, and #202's
(the issue that requested this refresh pass). Recorded so the reviewer of
the next frontend slice knows exactly what it inherits.
