//! Pulls the handful of *structural* values this slice needs out of
//! `assets/tokens.css`, rather than hardcoding a second copy of them.
//!
//! §8a of the design spec reserves every visual decision — colour, type,
//! spacing, motion — for the design pass this issue explicitly does not do
//! (see this crate's top-level doc comment). But a few of `tokens.css`'s
//! `GEOMETRY` values are load-bearing for structure this issue *does* build:
//! the brief says outright to "take `--hop-row-h` from `assets/tokens.css`"
//! for the fixed-height reserved row, and the pre-built window needs *some*
//! starting size before the design pass owns its final one. Both are read
//! out of the real file below rather than retyped as a bare `56` or `400` —
//! the second a maintainer changes one in `tokens.css`, a hardcoded copy
//! here would silently drift from the value every mock and every other
//! component actually renders against.
//!
//! # Why parsing, not a GTK `CssProvider` load
//!
//! `tokens.css` is authored as ordinary web CSS — `:root { --x: 1px; }` and
//! `var(--x)` — because it is the source of truth for the *design* tool the
//! §8a mocks come from, not for GTK's stylesheet engine. GTK4's CSS parser
//! has no notion of custom properties or `var()` at all; loading this file
//! into a [`gtk::CssProvider`] as-is would not fail loudly, it would just
//! silently drop every rule GTK's parser does not recognise, wasting the
//! artifact this file is deliberately used as. Once GTK-flavoured stylesheet
//! rules exist (§8a's own future work), *they* will hardcode literal values —
//! GTK CSS has nothing else to hardcode them *as* — but until then, the two
//! values this crate structurally depends on are extracted from the same
//! authored file everything else will eventually agree with.
//!
//! # A parsed token *table*, not a single-shot text scan
//!
//! The original version of this module answered every lookup with a
//! first-match text scan: find `--<name>:`, read up to the next `;`, done.
//! That is correct for a *literal* — `--hop-row-h: 56px;` — but
//! `assets/tokens.css` also has a "SEMANTIC LAYER" section (`--hop-bg`,
//! `--hop-fg`, `--hop-sel-fill`, …) whose declarations are `var()` chains
//! onto the neutral ramp (`--hop-bg: var(--hop-neutral-950);`), and a
//! `.hop-theme-light` block that *redefines the same semantic names* for the
//! light palette, later in the file. A first-match text scan gets both
//! wrong: it returns the literal text `var(--hop-neutral-950)` instead of
//! following it, and it can never reach `.hop-theme-light`'s
//! redeclarations, because the dark `:root`'s come first.
//!
//! [`TokenTable`] fixes both by actually understanding the file's block
//! structure well enough to tell blocks apart, and [`resolve`] follows
//! `var()` references to a concrete value instead of returning them as
//! literal text. `tokens.css` contains five kinds of block; here is what
//! each becomes in the table, and why:
//!
//! - **The two unconditional `:root` blocks** — the neutral/type/spacing
//!   ramp, and the "SEMANTIC LAYER" aliases onto it — merge into one `base`
//!   table. They declare disjoint names (the ramp's `--hop-neutral-*` never
//!   collides with the semantic layer's `--hop-bg`/`--hop-fg`/etc.), so
//!   merging them loses nothing and correctly models "true regardless of
//!   palette, or anything else."
//! - **`.hop-theme-light`** redeclares exactly the semantic layer's names,
//!   for the light palette. It is kept as a *separate* overlay table rather
//!   than merged into `base`, because — unlike the two `:root` blocks — it
//!   is conditional: folding it into `base` would make the light values win
//!   unconditionally, destroying the dark palette [`resolve`] must still be
//!   able to produce. [`Palette::Light`] resolution checks this overlay
//!   first and falls back to `base` for names it does not redeclare (the
//!   ramp literals it points at, e.g. `--hop-neutral-0-light`, which live in
//!   `base` because they are unconditional too); [`Palette::Dark`] never
//!   consults the overlay at all.
//! - **The `@media (prefers-reduced-motion: reduce)` block** — including the
//!   third `:root` nested inside it — is skipped entirely, on purpose. It
//!   redefines the motion-duration tokens, but nothing in this crate reads
//!   `Gtk.Settings:gtk-enable-animations` or otherwise consumes a
//!   reduced-motion signal yet (reduced motion is explicitly out of scope
//!   for the issue this change belongs to), so folding it into `base`
//!   unconditionally would silently apply the *reduced*-motion durations
//!   regardless of the user's actual system preference — worse than not
//!   modelling it at all. When reduced-motion support is built, it should
//!   get its own overlay table shaped like `light_overlay`, selected the
//!   same way — not be retrofitted into `base`.
//! - **The five `.hop-honesty*` rule blocks** are skipped entirely: they are
//!   component rules (`opacity: 1;`, `color: var(--hop-fg);`, `min-width:
//!   var(--hop-skeleton-min-w);`), not `--custom-property` *declarations*,
//!   so they have no business in a token table regardless of how they are
//!   reached.
//!   [`classify_selector`] only recognises `:root` and `.hop-theme-light` as
//!   token-bearing selectors; every other selector — honesty rules included
//!   — is `Skip` by construction, not by an incidental syntax mismatch (a
//!   `.hop-honesty*` body never happens to contain a `--name: value;` shape
//!   in this file, but the classification does not rely on that holding).
use std::collections::HashMap;
use std::sync::LazyLock;

/// The full contents of the repo's `assets/tokens.css`, bundled into the
/// binary at compile time rather than read from disk at startup — a
/// launcher's structural geometry should not depend on the working directory
/// or an install layout finding the source tree.
const TOKENS_CSS: &str = include_str!("../../../assets/tokens.css");

/// Which of `tokens.css`'s two palettes to resolve a token against. Dark is
/// hop's default (`assets/tokens.css`'s own header: "hop is dark-first");
/// Light is the `.hop-theme-light` overlay. See this module's top-level doc
/// comment for how each variant treats the parsed [`TokenTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Dark,
    Light,
}

/// A parsed, unresolved view of `assets/tokens.css`: every `--name: value;`
/// declaration this module cares about, sorted into the two tables the
/// module doc comment describes. Values are stored exactly as written —
/// still possibly a `var(--other-name)` chain — [`resolve`] is what follows
/// those.
struct TokenTable {
    /// Both unconditional `:root` blocks, merged.
    base: HashMap<String, String>,
    /// `.hop-theme-light`'s redeclarations, consulted only for
    /// [`Palette::Light`], checked before falling back to `base`.
    light_overlay: HashMap<String, String>,
}

/// Parsed once, on first use, and kept for the process's lifetime — parsing
/// `tokens.css` is pure text work with no I/O beyond the `include_str!`
/// already baked into the binary, so there is nothing to invalidate or
/// re-run.
static TABLE: LazyLock<TokenTable> = LazyLock::new(|| TokenTable::parse(TOKENS_CSS));

impl TokenTable {
    fn parse(css: &str) -> Self {
        let stripped = strip_comments(css);
        let mut base = HashMap::new();
        let mut light_overlay = HashMap::new();

        let mut rest: &str = &stripped;
        while let Some(open_rel) = rest.find('{') {
            let selector = rest[..open_rel].trim();
            let close_rel = find_matching_brace(rest, open_rel);
            let body = &rest[open_rel + 1..close_rel];

            match classify_selector(selector) {
                BlockKind::Base => parse_declarations(body, &mut base),
                BlockKind::LightOverlay => parse_declarations(body, &mut light_overlay),
                BlockKind::Skip => {}
            }

            rest = &rest[close_rel + 1..];
        }

        Self {
            base,
            light_overlay,
        }
    }
}

/// What a top-level (or `@media`-nested) block's selector means for the
/// token table. See this module's top-level doc comment for the reasoning
/// behind each arm.
enum BlockKind {
    Base,
    LightOverlay,
    Skip,
}

fn classify_selector(selector: &str) -> BlockKind {
    match selector {
        ":root" => BlockKind::Base,
        ".hop-theme-light" => BlockKind::LightOverlay,
        _ => BlockKind::Skip,
    }
}

/// Removes every `/* ... */` comment from `css`, replacing each with
/// nothing (every comment in `tokens.css` is already surrounded by
/// whitespace on both sides, so this never fuses two tokens together that a
/// comment used to separate).
///
/// This has to run *before* any block or declaration scanning, not after:
/// several comments in `tokens.css` mention a real token name in prose
/// (`/* Ratios are against --hop-neutral-950 unless noted. */`, inside the
/// `:root` block that also *declares* `--hop-neutral-950`). A scanner
/// looking for the next `--name:` would find that prose mention first, and
/// go hunting for its `:` — which it would find, wrongly, several lines
/// later at the real declaration's own colon, producing one corrupt
/// multi-line "name" and silently losing the real, short one.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("*/")
            .unwrap_or_else(|| panic!("assets/tokens.css has an unterminated `/* ... */` comment"));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Given the index of a `{` in `s`, returns the index of the `}` that closes
/// it, counting nested braces so a block containing another block — the
/// `@media` block's nested `:root { ... }` is the only case `tokens.css`
/// actually has — is skipped as one unit rather than stopping at the first
/// `}`, which would be the *inner* block's own close.
fn find_matching_brace(s: &str, open_at: usize) -> usize {
    let bytes = s.as_bytes();
    let mut depth = 1u32;
    let mut i = open_at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("assets/tokens.css has a `{{` with no matching `}}`")
}

/// Parses every `--name: value;` declaration out of a block's body (comments
/// already stripped) into `out`. Handles more than one declaration per
/// line — `tokens.css`'s `SPACING` block packs four onto one — by looping
/// until no further `--` is found.
fn parse_declarations(body: &str, out: &mut HashMap<String, String>) {
    let mut rest = body;
    while let Some(dash_pos) = rest.find("--") {
        let after = &rest[dash_pos + 2..];
        let colon_pos = after.find(':').unwrap_or_else(|| {
            panic!("assets/tokens.css has a `--` custom property with no terminating `:`")
        });
        let name = after[..colon_pos].trim().to_string();

        let value_rest = &after[colon_pos + 1..];
        let semi_pos = value_rest.find(';').unwrap_or_else(|| {
            panic!("assets/tokens.css's `--{name}` declaration has no terminating `;`")
        });
        let value = value_rest[..semi_pos].trim().to_string();

        out.insert(name, value);
        rest = &value_rest[semi_pos + 1..];
    }
}

/// Looks up `name`'s declaration in `table` for `palette`, **unresolved** —
/// a value that is itself `var(--x)` is returned as that literal text, not
/// followed. [`resolve`] is the chain-following counterpart; [`font_token`]
/// is the one caller that needs this unresolved form, because it must split
/// a `<weight> <size>/<line> var(--family)` shorthand into pieces *before*
/// the trailing piece is resolved — resolving the whole shorthand first
/// would inline the family stack's own internal whitespace
/// (`"Inter", -apple-system, ...`) into the string being split, breaking the
/// assumption that the shorthand has exactly one whitespace-delimited
/// trailing token.
///
/// Panics naming `name` and the file if neither table has it — a missing
/// token is a build-time programming error to catch immediately, in the
/// same spirit as every panic in this module.
fn raw_from<'a>(table: &'a TokenTable, name: &str, palette: Palette) -> &'a str {
    let value = match palette {
        Palette::Light => table
            .light_overlay
            .get(name)
            .or_else(|| table.base.get(name)),
        Palette::Dark => table.base.get(name),
    };
    value
        .map(String::as_str)
        .unwrap_or_else(|| panic!("assets/tokens.css has no `--{name}` declaration"))
}

/// [`raw_from`] against the module's real, parsed [`TABLE`]. `TABLE` is a
/// `'static` item, so a borrow into it — including the transitive borrow
/// this returns — is itself `'static`, which is what lets
/// [`font_token`] hand its `family` field a `&'static str` without owning a
/// copy.
fn raw(name: &str, palette: Palette) -> &'static str {
    raw_from(&TABLE, name, palette)
}

/// Resolves `name` to a concrete value under `palette`, following every
/// `var(--other-name)` reference the declaration contains — to arbitrary
/// depth, not just the one hop `assets/tokens.css`'s semantic layer happens
/// to use today. Panics naming the missing token if a reference points at a
/// name with no declaration, and panics naming the whole cycle if a chain
/// ever revisits a name it has already started resolving, rather than
/// recursing forever — see `resolve_with_path`'s own doc comment for how.
pub fn resolve(name: &str, palette: Palette) -> String {
    resolve_from(&TABLE, name, palette)
}

fn resolve_from(table: &TokenTable, name: &str, palette: Palette) -> String {
    let mut path = Vec::new();
    resolve_with_path(table, name, palette, &mut path)
}

/// `path` holds the names currently being resolved, outermost first — the
/// call stack's own contents, made inspectable. Before resolving `name`,
/// this checks whether `name` is already in `path`: if it is, `name` refers
/// back to itself through some chain of `var()`s, and resolving it further
/// would recurse the same cycle forever. Catching that *here*, by checking a
/// bounded `Vec` before each recursive step, is what turns a `var()` cycle
/// into a panic that names every link in the loop instead of an unbounded
/// recursion that would eventually blow the stack — the difference the
/// brief for this module requires: "detected, not hit as a stack overflow."
fn resolve_with_path(
    table: &TokenTable,
    name: &str,
    palette: Palette,
    path: &mut Vec<String>,
) -> String {
    if let Some(pos) = path.iter().position(|seen| seen.as_str() == name) {
        let mut cycle: Vec<String> = path[pos..].to_vec();
        cycle.push(name.to_string());
        panic!(
            "assets/tokens.css has a `var()` reference cycle: {}",
            cycle.join(" -> ")
        );
    }

    path.push(name.to_string());
    let raw_value = raw_from(table, name, palette);
    let resolved = substitute_vars(table, raw_value, palette, path);
    path.pop();
    resolved
}

/// Replaces every `var(--other-name)` occurrence in `value` with that name's
/// own resolved value, left to right. Most tokens this module reads contain
/// at most one `var()` (the semantic layer's whole-value aliases, and the
/// type scale's trailing `var(--hop-font-*)`), but this loops rather than
/// assuming that, so a future declaration combining more than one — a
/// shorthand mixing a literal with a referenced colour, say — resolves
/// correctly too.
fn substitute_vars(
    table: &TokenTable,
    value: &str,
    palette: Palette,
    path: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("var(--") {
        out.push_str(&rest[..start]);
        let after_marker = &rest[start + "var(--".len()..];
        let close = after_marker
            .find(')')
            .unwrap_or_else(|| panic!("assets/tokens.css has an unterminated `var()` reference"));
        let inner_name = after_marker[..close].trim();
        out.push_str(&resolve_with_path(table, inner_name, palette, path));
        rest = &after_marker[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Finds a `--custom-property: <N>px;` declaration and returns `N`. Panics
/// with the property name and the file this is sourced from on any failure
/// — a missing or reshaped token is a build-time programming error to catch
/// immediately, not a degraded runtime state to carry forward silently (the
/// exact failure mode this module's doc comment says a raw `CssProvider`
/// load would produce).
///
/// Every caller of this function reads a `GEOMETRY` token, none of which
/// `.hop-theme-light` redeclares, so resolving against [`Palette::Dark`]
/// always agrees with [`Palette::Light`] here — there is no palette
/// parameter to thread through for values that structurally cannot differ
/// by palette.
fn px_token(name: &str) -> i32 {
    let value = resolve(name, Palette::Dark);
    let digits: String = value
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("assets/tokens.css's `--{name}` is not a bare `<N>px` value"))
}

/// `--hop-row-h`, in pixels: the fixed height every result row (and the
/// selection indicator that tracks one) reserves regardless of its content
/// — see `ui::row`'s doc comment for why that matters for the walking
/// skeleton's no-layout-shift requirement.
pub static ROW_HEIGHT_PX: LazyLock<i32> = LazyLock::new(|| px_token("hop-row-h"));

/// `--hop-icon-size`, in pixels: the fixed side length of the icon slot every
/// row reserves at its leading edge, whether or not an item's icon resolves
/// — the same reserved-space discipline `ROW_HEIGHT_PX` documents, applied to
/// the row's other load-bearing dimension. 26px is not this crate's choice to
/// make; it is the icon size the M3 visual spec's row anatomy fixes as one
/// term of a 56px row (`docs/superpowers/specs/2026-08-19-hop-m3-visual-design.md`,
/// "Row anatomy: 26px icon · title ... Base row height 56px"), so it is read
/// out of `tokens.css` rather than retyped as a bare `26` here, for the exact
/// drift reason this module's top-level doc comment gives for `ROW_HEIGHT_PX`
/// itself.
pub static ICON_SIZE_PX: LazyLock<i32> = LazyLock::new(|| px_token("hop-icon-size"));

/// `--hop-window-w`, `--hop-window-h`, in pixels: the pre-built window's
/// starting size, before §8a's design pass owns sizing outright.
pub static WINDOW_SIZE_PX: LazyLock<(i32, i32)> =
    LazyLock::new(|| (px_token("hop-window-w"), px_token("hop-window-h")));

/// Panics with a message naming `name` and what its declaration was expected
/// to look like.
///
/// A genuine function — never `-> !` type here would be inferred as a fixed
/// closure `Output` — rather than a `let fail = || panic!(...)` closure bound
/// once and reused: [`hex_token`] and [`font_token`] below each call this
/// (via a fresh `|| bad_token(...)` closure literal) from several
/// `unwrap_or_else` sites that each need a *different* return type. A single
/// closure *value* reused across sites like that fails to compile — a
/// closure's own `Output` type is fixed once, by whichever use constrains it
/// first, so the second, differently-typed use is a mismatch. This function's
/// real, honest return type is `!` (it only ever panics), and `!` coerces to
/// whatever a given call site needs fresh, every time, with no such
/// restriction.
fn bad_token(name: &str, expected: &str) -> ! {
    panic!("assets/tokens.css's `--{name}` is not {expected}")
}

/// Parses a bare `#rrggbb` token into its three 8-bit channels.
///
/// Every colour this module reads — [`ACCENT_RGB`], [`MODE_LABEL_RGB`] — is a
/// ramp-level literal (`--hop-accent`, `--hop-neutral-400`) rather than a
/// *semantic* alias one `var()` hop away (`--hop-sel-bar`, `--hop-fg-3` and
/// the rest of tokens.css's "SEMANTIC LAYER" section), so [`resolve`] never
/// actually has a chain to follow for either of these names — unlike
/// [`font_token`] below, which does have one `var(--hop-font-*)` hop to
/// resolve. Routed through [`resolve`] anyway, rather than a bespoke direct
/// lookup, so this module has exactly one lookup path rather than two. See
/// each `LazyLock`'s own doc comment for why its particular literal was the
/// one chosen. Also palette-invariant, for the same reason [`px_token`]'s
/// doc comment gives.
fn hex_token(name: &str) -> (u8, u8, u8) {
    let value = resolve(name, Palette::Dark);
    let expected = "a bare `#rrggbb` value";
    let hex = value
        .strip_prefix('#')
        .unwrap_or_else(|| bad_token(name, expected));
    if hex.len() != 6 || !hex.is_ascii() {
        bad_token(name, expected);
    }
    let byte =
        |slice: &str| u8::from_str_radix(slice, 16).unwrap_or_else(|_| bad_token(name, expected));
    (byte(&hex[0..2]), byte(&hex[2..4]), byte(&hex[4..6]))
}

/// One `--hop-text-*` type-scale token, parsed: `<weight> <size>px/<line-height>px
/// var(--hop-font-<family>)` — e.g. `--hop-text-section`'s
/// `600 11px/14px var(--hop-font-sans)`.
pub struct FontToken {
    pub weight: u16,
    pub size_px: f64,
    pub line_height_px: f64,
    /// The resolved `--hop-font-*` value, already the literal comma-separated
    /// family list Pango's `family` property expects — not the
    /// `var(--hop-font-sans)` indirection `--hop-text-section` itself spells
    /// it as. This is the one place in this module that *does* follow a
    /// `var()` hop, because the type-scale tokens are authored to share their
    /// two typeface stacks by reference rather than repeating either one
    /// eleven times.
    pub family: &'static str,
}

/// Parses a `--hop-text-*` type-scale token into a [`FontToken`]. Resolved
/// against [`Palette::Dark`] unconditionally, with no palette parameter of
/// its own — palette-invariant for the same reason [`px_token`]'s doc
/// comment gives: `.hop-theme-light` redeclares exactly 11 names (the
/// SEMANTIC LAYER's `--hop-bg`/`--hop-fg`/etc.), and every `--hop-text-*`
/// token lives only in the first, unconditional `:root` block alongside the
/// `--hop-tracking-*` and `--hop-font-*` tokens it references — none of the
/// three families is among those 11, so there is no light-palette
/// declaration for `resolve` to ever prefer over the dark one here.
fn font_token(name: &str) -> FontToken {
    let raw = raw(name, Palette::Dark);
    let expected = "`<weight> <N>px/<N>px var(--hop-font-*)`";
    let mut parts = raw.split_whitespace();

    let weight: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| bad_token(name, expected));

    let size_and_line = parts.next().unwrap_or_else(|| bad_token(name, expected));
    let (size_str, line_str) = size_and_line
        .split_once('/')
        .unwrap_or_else(|| bad_token(name, expected));
    let parse_px = |s: &str| -> f64 {
        s.strip_suffix("px")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| bad_token(name, expected))
    };
    let size_px = parse_px(size_str);
    let line_height_px = parse_px(line_str);

    let family_var = parts.next().unwrap_or_else(|| bad_token(name, expected));
    let family_name = family_var
        .strip_prefix("var(--")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or_else(|| bad_token(name, expected));

    // `resolve` (not the unresolved `raw`) because `--hop-font-*` is itself a
    // bare literal with no further `var()` of its own — resolving it is a
    // no-op substitution, and doing it through the same chain-following path
    // as everything else needs no special case for "this particular
    // reference happens to be one hop and no more."
    //
    // Leaked, not cloned into an owned `String` field: `TABLE` — and
    // therefore this value, since it is built once inside a `LazyLock` that
    // itself never drops — already lives for the process's entire lifetime.
    // Leaking here spends that unavoidable lifetime up front, in exchange
    // for keeping `FontToken::family`'s type (`&'static str`) exactly what
    // it was before this module gained the ability to follow `var()` chains,
    // so nothing downstream of this struct (`ui::mode_label`) has to change.
    let family: &'static str = Box::leak(resolve(family_name, Palette::Dark).into_boxed_str());

    FontToken {
        weight,
        size_px,
        line_height_px,
        family,
    }
}

/// `--hop-tracking-*`: an `em` letter-spacing token, e.g.
/// `--hop-tracking-section`'s `0.08em`. `em` here is relative to the type
/// token it is paired with (`--hop-text-section`'s own `size_px`) — the same
/// pairing D5/criterion 4 name explicitly: "`--hop-text-section` with
/// `--hop-tracking-section`". Resolved against [`Palette::Dark`]
/// unconditionally, palette-invariant for the same reason
/// [`font_token`]'s doc comment gives: every `--hop-tracking-*` name lives
/// only in the first, unconditional `:root` block, not among
/// `.hop-theme-light`'s 11 redeclared names.
fn em_token(name: &str) -> f64 {
    resolve(name, Palette::Dark)
        .strip_suffix("em")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| bad_token(name, "a bare `<N>em` value"))
}

/// `--hop-text-section`, parsed — the mode label's typeface, weight, size and
/// line height (`ui::mode_label`'s brief: "`--hop-text-section` with
/// `--hop-tracking-section`").
pub static MODE_LABEL_FONT: LazyLock<FontToken> = LazyLock::new(|| font_token("hop-text-section"));

/// `--hop-tracking-section`, in em — the mode label's letter-spacing.
pub static MODE_LABEL_TRACKING_EM: LazyLock<f64> =
    LazyLock::new(|| em_token("hop-tracking-section"));

/// `--hop-neutral-400`, the mode label's text colour: the same muted,
/// path/timestamp-tier ramp step tokens.css already uses for small
/// informational text, rather than the primary `--hop-fg`/`--hop-fg-2` that
/// query text and titles get. 5.77:1 against the dark window ground — the M3
/// visual spec's accessibility floor holds "Path, timestamp, muted text" to
/// 4.5:1, and the mode label is real content a screen reader announces
/// (criterion 6), not decoration, so it is held to that bar rather than the
/// lower 3:1 "dimmed hint text" one a merely decorative label could use.
pub static MODE_LABEL_RGB: LazyLock<(u8, u8, u8)> = LazyLock::new(|| hex_token("hop-neutral-400"));

/// `--hop-space-3`, in pixels: the mode label's margin from the query field's
/// trailing edge, so the label reads as sitting *inside* the query bar
/// (§8a's placement for the empty-state prefix cheatsheet, "inline in the
/// query bar, right-aligned") rather than flush against the window edge.
pub static MODE_LABEL_MARGIN_END_PX: LazyLock<i32> = LazyLock::new(|| px_token("hop-space-3"));

/// `--hop-accent`, the consumed-marker highlight's foreground colour — the
/// one deliberate exception to `assets/tokens.css`'s own header rule that the
/// accent is "used ONLY for the selection indicator, the focus ring, and
/// action hints ... Never for body text". Issue #184's own body is what
/// authorizes this exception, verbatim: "The accent (`--hop-accent`) is
/// available here, but note it is otherwise reserved for the selection
/// indicator, focus ring and action hints, so use it deliberately rather
/// than decoratively." (The M3 visual spec,
/// `docs/superpowers/specs/2026-08-19-hop-m3-visual-design.md`, never
/// mentions the marker or this highlight at all — the issue is the only
/// source for this exception, not a second one alongside it.) Every other
/// reservation (selection indicator, focus ring, action hints) stays off
/// limits. 8.85:1 against the dark window ground, clearing the
/// accessibility floor's "Accent as small text or glyph" row at 4.5:1 —
/// chosen over the softer `--hop-accent-subdued` wash the selected-row fill
/// uses, because D7 makes legibility, not subtlety, the point: `w ` vs
/// `wx ` has to read as different at a glance, before the query is
/// committed, not on close inspection.
pub static ACCENT_RGB: LazyLock<(u8, u8, u8)> = LazyLock::new(|| hex_token("hop-accent"));

/// Widens one 8-bit colour channel (this module's [`hex_token`] result) to
/// the 16-bit channel `pango::AttrColor`/GDK colour APIs expect, by byte
/// replication (`v * 257`) rather than a left-shift alone — a left-shift
/// leaves the low byte zero, which would slightly darken every channel that
/// is not already saturated; replication is what makes `0xff` map to
/// `0xffff` exactly, matching the `#rrggbb` → 16-bit convention `gdk::RGBA`
/// and Pango's own colour parsing both already use.
pub fn widen_channel(channel: u8) -> u16 {
    u16::from(channel) * 257
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_height_matches_tokens_css() {
        // Pinned to the literal in `assets/tokens.css` at the time this was
        // written, so a future edit to that file is a visible test failure
        // here rather than a silent behavior change nobody asked this test
        // to catch.
        assert_eq!(*ROW_HEIGHT_PX, 56);
    }

    #[test]
    fn icon_size_matches_tokens_css() {
        // Pinned to the literal in `assets/tokens.css` at the time this was
        // written, for the same reason `row_height_matches_tokens_css` pins
        // 56: a future edit to that file should be a visible test failure
        // here, not a silent behavior change nobody asked this test to
        // catch.
        assert_eq!(*ICON_SIZE_PX, 26);
    }

    #[test]
    fn window_size_matches_tokens_css() {
        assert_eq!(*WINDOW_SIZE_PX, (400, 500));
    }

    #[test]
    fn mode_label_font_matches_tokens_css() {
        let font = &*MODE_LABEL_FONT;
        assert_eq!(font.weight, 600);
        assert_eq!(font.size_px, 11.0);
        assert_eq!(font.line_height_px, 14.0);
        assert!(
            font.family.contains("Inter"),
            "expected the sans stack, got: {}",
            font.family
        );
    }

    #[test]
    fn mode_label_tracking_matches_tokens_css() {
        assert_eq!(*MODE_LABEL_TRACKING_EM, 0.08);
    }

    #[test]
    fn mode_label_rgb_matches_tokens_css() {
        assert_eq!(*MODE_LABEL_RGB, (0x8f, 0x8e, 0x95));
    }

    #[test]
    fn mode_label_margin_matches_tokens_css() {
        assert_eq!(*MODE_LABEL_MARGIN_END_PX, 12);
    }

    #[test]
    fn accent_rgb_matches_tokens_css() {
        assert_eq!(*ACCENT_RGB, (0xe3, 0xa8, 0x3b));
    }

    #[test]
    fn widen_channel_replicates_the_byte_rather_than_shifting() {
        assert_eq!(widen_channel(0x00), 0x0000);
        assert_eq!(widen_channel(0xff), 0xffff);
        assert_eq!(widen_channel(0xe3), 0xe3e3);
    }

    #[test]
    fn resolves_a_var_chain_to_the_real_files_concrete_value() {
        // `--hop-bg` is a `var(--hop-neutral-950)` chain in the real,
        // shipped `tokens.css` — the exact case the first-match text scanner
        // this module used to have could not follow (it would have returned
        // the literal text `var(--hop-neutral-950)`).
        assert_eq!(resolve("hop-bg", Palette::Dark), "#121214");
    }

    #[test]
    fn resolves_a_multi_hop_synthetic_chain() {
        // The real file only ever chains one `var()` deep. A synthetic table
        // proves `resolve` actually walks a chain rather than special-casing
        // "exactly one hop", the way `raw`/`font_token` used to have to.
        let table = TokenTable::parse(":root { --a: var(--b); --b: var(--c); --c: 3px; }");
        assert_eq!(resolve_from(&table, "a", Palette::Dark), "3px");
    }

    #[test]
    #[should_panic(expected = "hop-this-token-does-not-exist")]
    fn missing_token_panics_naming_it() {
        resolve("hop-this-token-does-not-exist", Palette::Dark);
    }

    #[test]
    #[should_panic(expected = "cycle")]
    fn var_cycle_panics_rather_than_overflowing() {
        // A naive recursive resolver with no cycle guard would recurse `a`
        // -> `b` -> `a` -> `b` -> ... forever and abort the whole test
        // process with a stack overflow — which cargo would report as the
        // entire test binary crashing, not as a clean, isolated
        // `#[should_panic]` pass/fail for this one test. Confirmed by
        // running this test: `cargo test -p hop-gtk --lib` completes
        // normally and reports this test as passed, rather than the process
        // aborting — the empirical proof that `resolve_with_path`'s
        // path-tracking guard actually catches the cycle instead of hitting
        // the stack limit.
        let table = TokenTable::parse(":root { --a: var(--b); --b: var(--a); }");
        resolve_from(&table, "a", Palette::Dark);
    }

    #[test]
    fn light_palette_resolves_a_semantic_token_to_the_light_ramp() {
        // `--hop-fg` is `.hop-theme-light`'s redeclaration of a name the
        // dark `:root` already defines — the exact case the first-match
        // scanner this module used to have could never reach, because the
        // dark declaration always comes first in the file.
        let dark = resolve("hop-fg", Palette::Dark);
        let light = resolve("hop-fg", Palette::Light);
        assert_eq!(dark, "#f4f3f1", "dark hop-fg is --hop-neutral-100");
        assert_eq!(
            light, "#211f1a",
            "light hop-fg is --hop-neutral-900-light, via .hop-theme-light"
        );
        assert_ne!(
            dark, light,
            "the light palette must not resolve to the dark value"
        );
    }

    #[test]
    fn light_palette_falls_back_to_base_for_names_it_does_not_redeclare() {
        // `.hop-theme-light` only redeclares the semantic layer's names, not
        // the ramp literals they point at (`--hop-neutral-0-light` etc.),
        // which live unconditionally in `base`. Palette::Light must still
        // resolve a bare ramp name like `hop-icon-size` — it is not present
        // in the light overlay at all.
        assert_eq!(resolve("hop-icon-size", Palette::Light), "26px");
    }
}
