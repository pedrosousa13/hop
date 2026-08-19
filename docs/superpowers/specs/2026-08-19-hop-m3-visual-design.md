# hop M3 — visual design pass

Status: approved by the maintainer 2026-08-19

Output of the §8a design pass, run 2026-08-19 against the precedent survey in
`docs/research/2026-08-10-launcher-ui-survey.md` and the eight decisions in
`docs/superpowers/specs/2026-08-10-hop-m3-frontend-design.md`.

§8a of `docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md` requires
that "before GTK implementation in M3, a static design pass produces the visual
direction (mock frames of the 6 key states, iterated with Pedro until
approved)". This document, plus `assets/tokens.css`, is that pass. The
interactive mock frames it was approved from rendered all six states at 1:1
with live motion.

This document records the visual direction and the four decisions the
maintainer made. It does **not** re-open D1–D8, and it does not restate §8 or
§8a — those stand.

## The direction: editorial dark, mono as signature

A proportional face carries titles and subtitles. **Monospace is reserved for
text that is literally machine text**: what you typed, what was computed, where
a file lives, and when data was fetched. That is what §8a means by "a launcher
brand lives in its mono" — the mono is load-bearing rather than total, and it
sits exactly where the eye lands first.

Rejected alternatives, both coherent, neither chosen:

- **Terminal-adjacent** — mono everywhere, ~32px rows, near-zero chrome.
  Distinctive and unambiguous about its audience, but it is a well-worn look,
  which cuts against the "first in class, not AI-template" bar §8a sets.
- **GNOME-sympathetic** — Adwaita-adjacent proportions carrying hop's own
  accent. Lowest risk, but D5 already rules the window model and stock widget
  styling deliberately broken, so it fights decisions already made.

## Maintainer decisions, 2026-08-19

**1. The accent is amber-gold, `#E3A83B` (dark) / `#875C0F` (light).**
Committed, not derived from the desktop and not derived from the wallpaper. It
clears 8.85:1 on the dark ground and 5.13:1 on paper, and it avoids every major
desktop's default blue, Ubuntu's orange-red (hue ~15°), and the violet that has
become AI tooling's own default. It is used **only** for the selection
indicator, the focus ring, and action hints — never for body text. That
restraint is what keeps the UI legible when a careless theme desaturates
everything around it.

**2. Both typefaces are bundled** — Iosevka Term and Inter, via GResource,
~1.3MB. Iosevka is narrow by construction, roughly 15% more characters per line
than JetBrains Mono at the same size, which is the difference between a path
reading whole and a path eliding inside a ~380px content width. Bundling is
what stops the identity element falling back silently to generic `monospace` on
a fresh install. The accepted cost is a bounded, intentional off-family seam
next to the GTK file-chooser hop launches. Cantarell was considered and
rejected: it is free and removes the seam, but re-attaches hop to the Adwaita
default look D5 spends a whole decision separating from.

**3. At elevated text scaling the window grows in height, never clips.**
`56px × 5 rows` in a 500px window is scale-1.0-only arithmetic; at 200% a
two-line row needs ~90–100px. The window may grow in height, capped at ~80% of
the monitor, to preserve the row count. **Width stays fixed at 400px**: long
titles ellipsize, and the action hint collapses to icon-only before it would
ever be pushed off-window. Row height is computed from measured content, not
hardcoded.

**4. The empty state's prefix cheatsheet lives inline in the query bar**,
right-aligned and muted, rather than in a panel of its own. It rides in space
the input already owns, so recents keep every row. This is closer to Alfred's
placeholder approach than to PowerToys Run's separate "Plugins overview" panel,
which would compete for the same vertical space recents need.

## The six states

Row anatomy: 26px icon · title (14px proportional) · subtitle (12.5px
proportional, or 11.5px mono for a path) · right-aligned action hint (11px).
Base row height 56px, ~5 visible, radii 10px.

| State | Must communicate | Borrowed from | Designed against |
| --- | --- | --- | --- |
| Empty query | hop already knows you | Raycast / Flow's recents-by-default | a dead panel that looks like a fresh install every launch |
| Results | zero jank as providers resolve | GNOME Shell's `ListSearchResult` row anatomy | rofi/Flow's resize-with-results |
| No results | never a void | Albert's fallback-handler-as-result | the silent empty container rofi, PowerToys Run and Ulauncher all ship |
| Pending | this is not-yet-real data | Ulauncher's provider-attributed loading row | a row that looks like a finished, actionable result |
| Error | a provider failed, and which one | GNOME Shell's `ProviderInfo` attribution, applied to failure | a failed provider vanishing silently |
| Offline | this data may be stale | nothing surveyed does this well | cached data reading as live |

Two placement rules the states settled:

- **The empty state populates real rows immediately** from the learning store.
  Never a placeholder illustration.
- **An error row pins to the bottom of the frame**, below all real results,
  never interleaved in rank order. Per-provider isolation means an error
  coexists with real results, and interleaving would interrupt a top-down scan
  of them. Pinning keeps the scan path intact while guaranteeing the error is
  never dropped from the frame.

## Honesty-critical states, and the rule they force

Three of the six carry `.hop-honesty` (`docs/theme-token-contract.md`): pending
skeleton rows, error rows, the offline indicator, and the cached-data "as of"
labels. hop locks their presence, opacity, dimensions and contrast above
`GTK_STYLE_PROVIDER_PRIORITY_USER`.

The contract permits a user theme to restyle font family and accent on those
members. So the design rule is:

> **A member's meaning must live in its words and its shape, never in a
> colour.** Colour is the one thing a theme may still change.

Concretely, and each verified against a hostile-theme mock that set
`opacity: 0` and washed the accent to grey:

- The error row says "unavailable" and "Retry" **in words**. It survives
  greyscale; a red dot would not.
- The offline state stamps **`as of 09:14` per row** in mono. The timestamp
  itself is load-bearing, not a badge colour.
- Skeleton bars carry a **locked minimum width and height** — a zero-width bar
  collapses to invisible, which is the same lie as hiding it — alongside the
  provider's own icon, so "something is loading, from this source" reads by
  shape alone.

## Motion

| Motion | Duration | Easing | What moves | Reduced-motion |
| --- | --- | --- | --- | --- |
| Window open | 140ms | `cubic-bezier(.16,1,.3,1)` | opacity 0→1, scale .97→1, Y −6px→0 | opacity only, ~85ms |
| Window close | 110ms | `cubic-bezier(.4,0,1,1)` | opacity 1→0, scale 1→.98, Y 0→−3px | opacity only, ~85ms |
| Selection move | 90ms | `cubic-bezier(.22,1,.36,1)` | one indicator translates | instant reposition |
| Skeleton → resolved | 100ms | ease-out | crossfade, gated on a resolve event | instant swap |
| State change | 120ms | `cubic-bezier(.4,0,.2,1)` | opacity only, never a slide | instant swap |
| Action hint | 80ms, 40ms delay | ease-out | opacity | show immediately |

The open/close durations are inherited from the predecessor GNOME extension's
tuned values. **The easing is deliberately asymmetric**: opening must orient the
user, and deceleration reads as arriving; closing follows a completed action and
should read as getting out of the way, which acceleration communicates better
than a slow-motion reverse.

Two implementation traps, both load-bearing:

- **Never animate `setup()` or `bind()`.** `GtkListView` recycles row widgets,
  so an entrance animation attached to bind replays on every scroll. Track a
  per-item resolved flag and fade only when resolution is observed while that
  widget is bound and visible.
- **Selection is a single translating indicator, not a per-row class.** The
  indicator is not a recycled widget, so it sidesteps the trap entirely, and CSS
  transition retargeting handles rapid arrow bursts with no manual
  cancellation. List scroll-into-view must be **instant** — native smooth-scroll
  queues under key-repeat and fights the indicator's own tween.

Reduced motion strips transforms and keeps shorter opacity fades, rather than
disabling motion wholesale: translation and scale are the vestibular triggers,
not fades.

## Accessibility floor

Numbers, not adjectives — each is checkable against a build.

| Element | Minimum | Criterion |
| --- | --- | --- |
| Title, query, body text | 4.5:1 | WCAG 1.4.3 AA |
| Subtitle and secondary text | 4.5:1 | WCAG 1.4.3 AA |
| Path, timestamp, muted text | 4.5:1 | WCAG 1.4.3 AA |
| Accent as small text or glyph | 4.5:1 | WCAG 1.4.3 AA |
| Selection indicator vs adjacent surface | 3:1 | WCAG 1.4.11 AA |
| Dimmed hint text | 3:1 | hop target |
| Every `.hop-honesty` text member | 4.5:1 | WCAG 1.4.3 AA |
| `.hop-honesty` non-text | 3:1 | WCAG 1.4.11 AA |

**Ratios are computed against the surface the token actually renders on** — for
a selected row that is the *composited* background (the accent at 14% over the
window, `#2f2719` dark / `#efeadf` light), not a flat neutral. Checking against
the wrong surface is what let two failures through the first draft:

- `--hop-neutral-500` was `#55555e` — **2.54:1**, failing the 3:1 floor. Raised
  to `#6a6a74` (3.50:1).
- `--hop-neutral-600-light` was `#706b5d` — **4.43:1** on its selected row,
  failing 4.5:1. Raised to `#6a6559` (4.84:1).

A third claimed failure did not survive checking, and is recorded so it is not
"fixed" again: a subtitle at 60% opacity over `#121214` composites to `#a0a0a1`,
which is **7.16:1** and passes. The durable rule is *measure the composited
value*, not *avoid alpha*.

Two structural requirements the palette cannot satisfy on its own:

- **Screen readers and a streaming list.** The list is `listbox`/`option`
  (GTK: `GTK_ACCESSIBLE_ROLE_LIST_BOX` / `LIST_ITEM`) and updates **silently** —
  replacing rows never speaks. A separate polite `status` node announces a
  coalesced result count once the stream quiesces (~500ms) or on a final-frame
  flag; a separate assertive `alert` node carries errors and offline, once per
  occurrence. Per-row arrival and skeleton resolution stay silent.
- **Focus never leaves the query entry.** This is the `aria-activedescendant`
  combobox pattern: the entry is `GTK_ACCESSIBLE_ROLE_SEARCH_BOX` with
  `ACTIVE_DESCENDANT` pointing at the selected row; rows are never
  `grab_focus()`ed. Each row exposes one concatenated accessible name — title,
  subtitle and default action together — plus `SET_SIZE`/`POS_IN_SET` so the
  reader can say "2 of 47". Every row's full action set is exposed via
  `DESCRIBED_BY`: sighted users read the hint glyphs, and a screen-reader user
  gets nothing unless it is explicit text.

## Follow-ups this pass creates

- `docs/theme-token-contract.md` states that "token identifiers and values will
  be added when `tokens.css` is implemented". `assets/tokens.css` now exists, so
  that document needs a pointer to it. Filed separately rather than folded in
  here — it is a normative contract and #126's record.
- The locked block in `assets/tokens.css` is inert until the renderer installs
  it above `GTK_STYLE_PROVIDER_PRIORITY_USER`. That is the frontend's
  obligation, and the hostile-theme test is what proves it.
