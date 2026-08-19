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
use std::sync::LazyLock;

/// The full contents of the repo's `assets/tokens.css`, bundled into the
/// binary at compile time rather than read from disk at startup — a
/// launcher's structural geometry should not depend on the working directory
/// or an install layout finding the source tree.
const TOKENS_CSS: &str = include_str!("../../../assets/tokens.css");

/// Finds a `--custom-property: <N>px;` declaration in [`TOKENS_CSS`] and
/// returns `N`. Panics with the property name and the file this is sourced
/// from on any failure — a missing or reshaped token is a build-time
/// programming error to catch immediately, not a degraded runtime state to
/// carry forward silently (the exact failure mode this module's doc comment
/// says a raw `CssProvider` load would produce).
fn px_token(name: &str) -> i32 {
    let needle = format!("--{name}:");
    let after = TOKENS_CSS
        .split_once(&needle)
        .unwrap_or_else(|| panic!("assets/tokens.css has no `--{name}` declaration"))
        .1;
    let digits: String = after
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

/// `--hop-window-w`, `--hop-window-h`, in pixels: the pre-built window's
/// starting size, before §8a's design pass owns sizing outright.
pub static WINDOW_SIZE_PX: LazyLock<(i32, i32)> =
    LazyLock::new(|| (px_token("hop-window-w"), px_token("hop-window-h")));

/// Finds a `--custom-property: <value>;` declaration in [`TOKENS_CSS`] and
/// returns `<value>`, trimmed of surrounding whitespace — the same lookup
/// [`px_token`] performs before it goes on to require the value be a bare
/// `<N>px`. Factored out because issue #184's tokens (the mode label's
/// typography, its letter-spacing, and the marker highlight's colour) come in
/// a few more shapes than a bare pixel integer: a hex colour, a `font:`
/// shorthand, and an `em` tracking value.
fn raw_token(name: &str) -> &'static str {
    let needle = format!("--{name}:");
    let after = TOKENS_CSS
        .split_once(&needle)
        .unwrap_or_else(|| panic!("assets/tokens.css has no `--{name}` declaration"))
        .1;
    after
        .split_once(';')
        .unwrap_or_else(|| panic!("assets/tokens.css's `--{name}` has no terminating `;`"))
        .0
        .trim()
}

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
/// the rest of tokens.css's "SEMANTIC LAYER" section), so this never needs to
/// follow an indirection — unlike [`font_token`] below, which does have one
/// `var(--hop-font-*)` hop to resolve. See each `LazyLock`'s own doc comment
/// for why its particular literal was the one chosen.
fn hex_token(name: &str) -> (u8, u8, u8) {
    let raw = raw_token(name);
    let expected = "a bare `#rrggbb` value";
    let hex = raw
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

fn font_token(name: &str) -> FontToken {
    let raw = raw_token(name);
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

    FontToken {
        weight,
        size_px,
        line_height_px,
        family: raw_token(family_name),
    }
}

/// `--hop-tracking-*`: an `em` letter-spacing token, e.g.
/// `--hop-tracking-section`'s `0.08em`. `em` here is relative to the type
/// token it is paired with (`--hop-text-section`'s own `size_px`) — the same
/// pairing D5/criterion 4 name explicitly: "`--hop-text-section` with
/// `--hop-tracking-section`".
fn em_token(name: &str) -> f64 {
    raw_token(name)
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
/// one deliberate use of the accent this issue's brief and the M3 visual
/// spec both name explicitly ("`--hop-accent` is available here ... use it
/// deliberately rather than decoratively"); every other reservation
/// (selection indicator, focus ring, action hints) stays off limits. 8.85:1
/// against the dark window ground, clearing the accessibility floor's
/// "Accent as small text or glyph" row at 4.5:1 — chosen over the softer
/// `--hop-accent-subdued` wash the selected-row fill uses, because D7 makes
/// legibility, not subtlety, the point: `w ` vs `wx ` has to read as
/// different at a glance, before the query is committed, not on close
/// inspection.
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
}
