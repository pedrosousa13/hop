# Hop theme token contract

Version: v1
Status: normative for theme authors; renderer enforcement is future work

This is the durable, author-facing contract for Hop's v1 theme surface. It
defines the boundary that a theme must respect before the concrete token names
exist. Token identifiers and values will be added when `tokens.css` is
implemented; this contract deliberately does not invent them in advance.

## Ordinary user-theme surface

User themes remain authoritative everywhere outside the reserved
`.hop-honesty` class. A theme may choose the ordinary surface's colors,
typography, spacing, shape, and other presentation details according to the
token API available when `tokens.css` is implemented.

The boundary is narrow. On honesty-critical elements, a user theme may still
restyle the font family and accent, provided the element remains present and
legible. Hop's locked values win whenever a requested style would violate the
truthfulness guarantee below.

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

The contract locks these property categories, not concrete token identifiers.
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
hostile-theme checks, hot-reload enforcement, or the GTK renderer already
exist.
