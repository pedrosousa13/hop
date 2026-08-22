# hop — design refresh (M7)

Status: **approved by the maintainer 2026-08-22** via the grill session recorded
below. This document supersedes the *material, motion, and identity* portions of
`2026-08-19-hop-m3-visual-design.md`; the six-state table and its honesty rules
carry forward unchanged in spirit and are re-gated by the approved frames in
`sixstates.html`. The §8a process was re-run in full: interactive mock frames,
iterated with Pedro until approved.

## The decision record (grill session, 2026-08-22)

Nine decisions, each made explicitly by the maintainer:

1. **Direction — macOS-modern on Linux.** Frosted material depth (blur-behind,
   hairline borders, layered soft shadows), grounded: it must render honestly on
   GTK4/Linux, not imitate macOS chrome.
2. **Motion — minimal set only.** Open/close fade, hover/selection transitions
   (≤140ms), copy-feedback toast. No row cascades, no panel springs, nothing
   decorative. `Motion::Reduced` collapses what remains to opacity-only.
3. **Customization — staged.** This pass ships accent presets + dark/light
   follow. Density control and the theme-file surface are later, separately
   grilled slices.
4. **Accent switching — in-launcher.** Typing `accent` yields swatch rows;
   selecting applies live. A `config.toml` key mirrors it for the AFK path.
5. **Action panel — ctrl K, full, this pass.** All actions of the selected item
   (providers already ship `actions` on the wire — no protocol change),
   type-to-filter; per-row action icons appear on hover/selection for mouse;
   right-click opens the panel at the cursor. This is the structural home of
   #247 (copy affordance) and #249 (keyboard contract).
6. **Mouse contract — full.** Hover = row lift + action icons fade in · click
   row = default action · click an action icon = that exact action · right-click
   row = action panel at cursor · wheel scrolls with overlay scrollbar on hover ·
   no text selection inside rows (the copy action owns that) · double-click =
   single click.
7. **Identity — full rebrand.** Typeface: **Geist** (UI) + **Geist Mono**
   (machine text: query, computed values, keycaps, timestamps) — bundled like
   Iosevka/Inter were. Accent: **ice blue `#5AA9E6` default**, five presets ship
   (amber `#E3A83B`, ice, mint `#2EC27E`, coral `#E6685C`, iris `#8F7FF0`);
   every accent must clear WCAG contrast against both grounds before shipping —
   the tokens.css contrast discipline carries over unchanged.
8. **Material honesty — progressive.** Real translucency where the compositor
   supports blur (KDE Wayland, X11 + picom); opaque-but-layered elsewhere
   (GNOME Wayland). Runtime detection; never a half-transparent panel over hard
   pixels.
9. **Process — new M7 milestone.** Identity round → six-state approval → this
   spec → sliced ready-for-agent issues.

## Approved frames

All committed beside this document; they are the acceptance gates:

| File | Content |
| --- | --- |
| `sixstates.html` | The six states (empty / results / no-results / pending / error / offline) in final identity |
| `mocks3.html` | Living main state + ctrl K panel with the animation choreography as explored |
| `identity.html` | Five accents applied to the locked Geist frame |
| `mocks2.html`, `mocks.html` | The exploration trail (terminal-luxe, paper-glass, slab, aura; first-round directions) |

The six-state semantics (empty populates real recents; error pins below results;
offline stamps age per row in mono; pending reads as not-yet-real and names its
provider) carry over from the approved M3 pass verbatim.

## What supersedes what

- Superseded: "editorial dark" material direction, amber-as-fixed-accent,
  Iosevka Term + Inter bundling, zero-motion posture.
- Unchanged: D1–D8 frontend decisions (custom chrome, window model, GTK4 stack),
  the token architecture (`assets/tokens.css` remains the single source; Geist/
  ice become its values), the theme-token contract's override rules, the six
  state semantics, WCAG contrast discipline, p95 latency budgets.

## Known constraints carried into implementation

- GNOME Wayland exposes no blur API: the frosted material's translucent mode is
  KDE/X11-only; detection and degrade belong in the material slice (#TBD in
  M7's issue list).
- GTK4 draws CSD shadows inside the X11 surface (5px/side inset measured in
  #246): capture assertions encode declared-size-minus-inset, not naively
  WINDOW_SIZE_PX.
- The runner geometry flake class (socket-exists vs listening, dropped queries
  during IPC reconnect windows) is fixed on main as of #250; new UI slices must
  keep x11_smoke green under CI's no-WM Xvfb.
