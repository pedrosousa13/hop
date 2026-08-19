# Hop theme token contract

Version: v1
Status: normative for theme authors; renderer enforcement is future work

This is the durable, author-facing contract for Hop's v1 theme surface. It
defines the boundary that a theme must respect over the concrete token API in
`assets/tokens.css`, the v1 design system this contract deliberately declined
to invent in advance.

## Ordinary user-theme surface

User themes remain authoritative everywhere outside the reserved
`.hop-honesty` class. A theme may choose the ordinary surface's colors,
typography, spacing, shape, and other presentation details according to the
token API in `assets/tokens.css`.

The boundary is narrow. On honesty-critical elements, a user theme may still
restyle the font family and accent, provided the element remains present and
legible. Hop's locked values win whenever a requested style would violate the
truthfulness guarantee below.

Because colour is the one thing a theme may still change on these elements,
the design rule this carve-out forces is: **a member's meaning must live in
its words and its shape, never in a colour.** A red dot conveys nothing once a
theme desaturates it; the word "unavailable" and a locked-width skeleton bar
still do.

## Reserved honesty-critical class

`.hop-honesty` is reserved for exactly these members:

- cached-data “as of” labels
- pending skeleton rows
- the offline indicator
- error rows

The class is not a general-purpose styling hook. The future renderer must
apply it to each member listed above.

## Locked guarantees

Hop owns the properties that carry the truthfulness guarantee for every
`.hop-honesty` member:

- rendered visibility/presence
- opacity
- dimensions sufficient to remain perceivable
- foreground/background contrast sufficient to remain legible

The contract locks these property categories, not concrete token identifiers:
a future revision of `assets/tokens.css` could rename or restructure what
backs them without changing this contract. As implemented today, in that
file's honesty-critical members block:

- presence — `.hop-honesty { visibility: visible }`, and `display: flex` on
  `.hop-honesty--row`, `.hop-honesty--error`, `.hop-honesty--offline`
- opacity — `.hop-honesty { opacity: 1 }`
- dimensions — the locked `min-width`/`min-height` on
  `.hop-honesty .hop-skeleton`
- contrast — `--hop-fg` and `--hop-text-subtitle` on
  `.hop-honesty .hop-honesty-text`; `--hop-fg-2` and `--hop-text-timestamp`
  on `.hop-honesty .hop-honesty-stamp`

Presence, opacity, and dimensions are fixed declarations rather than
overridable custom properties — that absence of a token is deliberate, since
none of the three may ever become theme-swappable. Contrast is the one
guarantee carried by named tokens, because legibility rides on the same color
and type-scale tokens the rest of the surface uses.

When a requested theme style would hide a member, make it imperceptible,
collapse it below perceivable dimensions, or make it illegible through
insufficient contrast, Hop's locked values take precedence.

## Why this boundary exists

User CSS is untrusted input even though it does not execute code. It can still
hide or visually erase freshness, pending, offline, and error signals. If a
theme hides those signals, the UI lies by omission: cached data can look
current, work can look complete, connectivity can look available, or a failed
operation can look successful.

## Future enforcement status

This document records the obligation; it does not claim that GTK enforcement
exists today. The future renderer must apply `.hop-honesty` and install the
locked styling above `GTK_STYLE_PROVIDER_PRIORITY_USER`, so user CSS cannot
override the locked property categories.

Hostile-theme behavior and hot-reload behavior remain deferred under issue
#126's narrowed decision. This v1 contract therefore makes no claim that
hostile-theme checks, hot-reload enforcement, the GTK renderer, or a running
`gtk::CssProvider` loading `assets/tokens.css` into the window already
exist.
