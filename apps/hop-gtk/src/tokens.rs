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
}
