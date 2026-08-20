# hop authors its own GTK stylesheet; `tokens.css` is a palette, not one

`assets/tokens.css` is the v1 design system and the source of truth for every
design value, but it cannot be handed to GTK. hop therefore authors a separate
GTK stylesheet and resolves its values out of `tokens.css` at startup, so the
design exists in one place and is expressed in two languages.

## Why `tokens.css` cannot simply be loaded

A reader's first instinct — and the first instinct of the issue that prompted
this — is that hop should install a `gtk::CssProvider` loaded from
`assets/tokens.css` and be done. That fails twice over, and both halves were
verified against the pinned GTK (4.14.5) rather than assumed.

**GTK's CSS is not web CSS.** It borrows the syntax and a subset of properties,
but it implements no custom properties, no `var()`, no `:root`, no `display`,
no `visibility` and no `@media`. `tokens.css` is built almost entirely out of
exactly those: a hundred-odd custom-property declarations and the `var()`
chains that layer a semantic palette over a neutral ramp. Loading the real file
into a real provider and capturing the parsing-error signal produces twenty
errors and almost no usable rules.

The failure is **silent**. GTK drops what it cannot parse and carries on — no
exception, no warning on the console, nothing visibly wrong beyond a window that
looks a little more stock than it should. That silence is the reason this went
unnoticed through several frontend slices, and it is why hop now treats a parse
error in its own sheet as a programming error rather than a cosmetic one.

**`tokens.css` contains no component rules.** Every selector block in it is
either a token table or one of the honesty-critical locks. Nothing in it — or
anywhere else, in any language — describes what a row, the mode label, the
selection indicator or the status line should look like. Even a GTK that parsed
the file perfectly would render nothing different, because there is nothing in
it to apply.

## Considered options

**Hand-copy literal values into a GTK stylesheet.** Rejected: it puts a second
copy of every design value in the repo, which is precisely what the token
module exists to prevent — its own doc comment warns that a hardcoded copy
"would silently drift from the value every mock and every other component
actually renders against."

**Generate the sheet from `tokens.css` in a build script.** Rejected, narrowly.
It is a sound design and catches placeholder typos at build time, but it would
be the workspace's first `build.rs`, and the benefit over resolving at startup
is small for a file this size.

**Author the GTK rules as Rust format strings** over the existing token
constants. Rejected: it gets compile-time checking of token names, but it puts
a stylesheet inside string literals, and this is design work that will be
iterated on visually.

**Chosen: a GTK stylesheet in the repo's assets, written with token
placeholders, resolved at startup.** CSS stays CSS — diffable, greppable,
editable — and no design value is ever written down twice.

## Consequences

Resolving placeholders means resolving `var()` chains, which the token module
did not previously do: it read only direct literals, so the entire semantic
layer was unreachable from Rust. Resolution is also palette-aware, because
following the system colour scheme means resolving the same token names through
the light overlay rather than the base table.

The design sheet installs at GTK's *application* priority — above the system
theme, deliberately below user themes — because `docs/theme-token-contract.md`
keeps user themes authoritative outside the honesty-critical class. The locked
block that must outrank a user theme is a separate provider, arriving with the
widgets it protects.

One guarantee gets stronger in the move. The contract locks presence, and
expresses it in `tokens.css` as `visibility` and `display`. GTK has neither —
but GTK widget visibility is a widget property, so no stylesheet can hide a
widget at all. Presence is enforced in code, and there is no property left for
a hostile theme to fight over.
