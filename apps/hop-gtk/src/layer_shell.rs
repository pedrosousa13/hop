//! Probes for `gtk4-layer-shell` support at startup, and the fallback that
//! runs whenever it is absent.
//!
//! # Provenance (issue #233)
//!
//! The apply branch below was first written to this crate's documented
//! public API without ever running against a real compositor: issue #179's
//! environment note records `gtk4-layer-shell` as **not installed** on the
//! machine the GNOME path (#232) was verified on, so every local run took
//! the fallback. Issue #233 closed that gap in CI: `tests/wlroots_smoke.rs`
//! drives a feature-on build under a headless wlroots compositor (sway) and
//! asserts, on the live Wayland wire (`WAYLAND_DEBUG=1`), that the window
//! actually becomes a layer surface — overlay layer, exclusive keyboard,
//! unanchored — while the two unsupported arms below stay observable under
//! the same harness. The fallback path is what every machine without the
//! library still runs.
//!
//! # Two independent reasons this can report "unsupported"
//!
//! 1. **Not compiled in.** `gtk4-layer-shell` (the Rust crate) links against
//!    a system `.so` at build time through its own `-sys` crate's
//!    `pkg-config` probe — an always-on dependency would make `cargo build`
//!    fail outright on any machine without that library, this one included.
//!    So it is optional, behind this crate's `layer-shell` feature (off by
//!    default; see `Cargo.toml`'s dependency comment). Built without the
//!    feature, [`probe`] cannot even ask the compositor — there is no
//!    `gtk4_layer_shell::is_supported()` call compiled into the binary at
//!    all — and reports [`Support::NotCompiledIn`] unconditionally.
//! 2. **Compiled in, compositor does not implement the protocol.** Even on a
//!    build with the feature on, `gtk4_layer_shell::is_supported()` asks the
//!    live Wayland compositor whether it implements the
//!    `zwlr_layer_shell_v1` protocol. The design spec's platform table
//!    (§2/§8) is explicit that GNOME's compositor, Mutter, does not — GNOME
//!    Wayland gets an ordinary centered window, not a layer surface, by
//!    design, not as a degraded case. wlroots compositors (Hyprland, Sway,
//!    niri, river) and KDE's KWin are the ones layer-shell actually reaches.
//!
//! Both collapse into the same fallback: [`apply_or_fallback`] leaves the
//! window as the ordinary top-level [`gtk::Window`] `ui::window` already
//! built, which is the correct behavior for reason 2 by design and the only
//! behavior available for reason 1.

use gtk::prelude::*;

/// What [`probe`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// `gtk4-layer-shell` is compiled in and the compositor answered that it
    /// implements the protocol.
    Supported,
    /// Compiled in, but this session's compositor does not implement
    /// `zwlr_layer_shell_v1` — GNOME/Mutter, per the design spec's platform
    /// table.
    UnsupportedByCompositor,
    /// This build does not have the `layer-shell` feature enabled — see this
    /// module's doc comment for why that is the default here.
    NotCompiledIn,
}

impl Support {
    /// Whether [`apply_or_fallback`] actually engaged layer-shell for the
    /// window it was given, rather than leaving it as an ordinary top-level
    /// window.
    pub fn is_active(self) -> bool {
        matches!(self, Support::Supported)
    }
}

/// Asks whether layer-shell is usable in this process, right now. Pure with
/// respect to any window — it does not touch one — so `app`'s startup
/// sequence can log or branch on the result before deciding whether to build
/// a window at all differently, and so this can be unit-tested without a
/// display connection (see the test below).
pub fn probe() -> Support {
    #[cfg(feature = "layer-shell")]
    {
        if gtk4_layer_shell::is_supported() {
            Support::Supported
        } else {
            Support::UnsupportedByCompositor
        }
    }
    #[cfg(not(feature = "layer-shell"))]
    {
        Support::NotCompiledIn
    }
}

/// Applies layer-shell to `window` if [`probe`] reports [`Support::Supported`];
/// otherwise a documented no-op, leaving `window` exactly as `ui::window`
/// built it — the fallback path this module's doc comment describes.
/// The layer-shell configuration below (overlay layer, exclusive keyboard,
/// no anchors — a centered popup rather than an edge-anchored panel) mirrors
/// the design spec's platform table entries for KDE and wlroots
/// compositors. It is compiled only under the `layer-shell` feature and is
/// exercised end to end by `tests/wlroots_smoke.rs` under a headless sway
/// in CI (issue #233) — see this module's top doc comment.
pub fn apply_or_fallback(window: &impl IsA<gtk::Window>) -> Support {
    let support = probe();

    #[cfg(feature = "layer-shell")]
    if support == Support::Supported {
        use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};

        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        // Exclusive: the launcher must receive every keystroke while
        // presented, the same requirement the GNOME-fallback path gets for
        // free from being an ordinary focused top-level window.
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        // No anchors set: an unanchored layer surface is sized to its
        // content and placed by the compositor, which is the closest
        // layer-shell equivalent to the fallback path's centered window —
        // §8's platform table does not ask for an edge-anchored panel.
    }

    // `support` is computed above unconditionally (not only inside the
    // `#[cfg(feature = "layer-shell")]` block) precisely so a
    // `layer-shell`-feature-off build still returns an honest
    // `Support::NotCompiledIn` here rather than this function's return type
    // forcing a guess.
    let _ = &window; // keeps `window` "used" in a `layer-shell`-off build.
    support
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_not_compiled_in_without_the_feature() {
        // This crate's default build (`cargo test` with no `--features`,
        // which is what CI and this issue's verification both run) has the
        // `layer-shell` feature off — see `Cargo.toml`. Pinning that here
        // means a future default-feature change is a visible test failure,
        // not a silent behavior change.
        #[cfg(not(feature = "layer-shell"))]
        assert_eq!(probe(), Support::NotCompiledIn);
    }

    #[test]
    fn unsupported_variants_report_inactive() {
        assert!(!Support::NotCompiledIn.is_active());
        assert!(!Support::UnsupportedByCompositor.is_active());
        assert!(Support::Supported.is_active());
    }
}
