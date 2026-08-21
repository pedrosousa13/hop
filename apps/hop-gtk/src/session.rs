//! Detects which kind of display session `hop-gtk` is running in, and
//! decides — once, at startup — which of the design spec §2 platform
//! table's overlay strategies this run gets (issue #232).
//!
//! # Why a decision made here rather than ad hoc where it is needed
//!
//! The spec's §2 table gives every platform its own overlay row: layer-shell
//! on KDE/wlroots Wayland, an ordinary compositor-placed window on GNOME
//! Wayland, a normal override-positioned window on X11. Three different
//! behaviors (positioning, focus handling) all follow from that one fact,
//! and they are needed in two places each (`ui::window`'s build wires them
//! into the window; M6's `hop doctor` reports them), so the decision lives
//! in one type here instead of being re-derived — possibly differently — at
//! each call site. This is also the capability-reporting groundwork the
//! spec's graceful-degradation rule names ("every capability probe has a
//! defined fallback, and `hop doctor` reports what was detected and why"):
//! [`startup_report`] is the human-readable half of that report, printed to
//! stderr at startup so a user (or a doctor run scraping logs) can see which
//! branch reality took.
//!
//! # How detection works, and what it must not be
//!
//! [`SessionKind::detect`] downcasts the live [`gdk::Display`] to GDK's
//! backend-specific subclasses (`GdkX11Display`, `GdkWaylandDisplay`) — the
//! same objects GDK itself constructed when it opened the display, so the
//! answer cannot disagree with the backend actually in use. It deliberately
//! does **not** read `$GDK_BACKEND`, `$DISPLAY`, or `$WAYLAND_DISPLAY`: those
//! say what the *environment requested*, not what the process *got* (GDK's
//! auto-probe can silently fall back when a request fails, which is exactly
//! the situation a capability report exists to surface). Anything neither
//! subclass — broadway under the headless smoke tests, today — is
//! [`SessionKind::Other`]: not a real user session, and handled below.

use gtk::gdk;
use gtk::prelude::*;

use crate::layer_shell;

/// Which kind of display session this process is running in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// A Wayland compositor (GNOME, KDE, wlroots, …).
    Wayland,
    /// An X11 server, with or without a window manager.
    X11,
    /// Neither — broadway under the headless smoke tests today. Not a real
    /// user session; nothing overlay-specific should be attempted.
    Other,
}

/// How the window presents itself, per design spec §2's platform table.
///
/// The two bool-carrying variants exist because the *focus* half of the
/// strategy differs by session even where the positioning half matches:
/// close-on-focus-loss is meaningful on real sessions (the launcher should
/// vanish when the user clicks away — the GNOME Wayland row's documented
/// behavior, and X11 parity with it) but meaningless under a headless test
/// backend, where "focus" is whatever the harness last did and dismissing
/// mid-capture would only add flakiness to the smoke tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayStrategy {
    /// `gtk4-layer-shell` engaged — KDE/wlroots Wayland. The compositor
    /// positions the surface and owns its keyboard focus entirely; nothing
    /// here adds behavior on top.
    LayerShell,
    /// An ordinary top-level window placed by the compositor or window
    /// manager — GNOME Wayland's row (which cannot self-position: Wayland
    /// clients have no position API at all), and every non-session display
    /// (`SessionKind::Other`) besides.
    CompositorPlaced {
        /// Whether losing keyboard input focus dismisses the window.
        dismiss_on_focus_loss: bool,
    },
    /// An ordinary top-level window the app positions itself — X11's row.
    /// See `x11`'s module doc for why GTK4 leaves no other mechanism.
    SelfPositioned,
}

impl SessionKind {
    /// Asks the live display which backend actually opened it. Pure with
    /// respect to everything but the display passed in — it opens nothing,
    /// changes nothing — so startup can log the result before any window
    /// exists, and the strategy table below is unit-testable without one.
    pub fn detect(display: &gdk::Display) -> Self {
        if display.downcast_ref::<gdkx11::X11Display>().is_some() {
            return SessionKind::X11;
        }
        if display
            .downcast_ref::<gdkwayland::WaylandDisplay>()
            .is_some()
        {
            return SessionKind::Wayland;
        }
        SessionKind::Other
    }

    /// The name startup logs and `hop doctor` will report. A method rather
    /// than `Display` impl because there is exactly one consumer shape today
    /// and naming it keeps the strings greppable next to their assertions in
    /// `tests/x11_smoke.rs`.
    pub fn name(self) -> &'static str {
        match self {
            SessionKind::Wayland => "Wayland",
            SessionKind::X11 => "X11",
            SessionKind::Other => "other",
        }
    }

    /// Picks the overlay strategy for this session given what
    /// [`crate::layer_shell::probe`] reported. Pure — see [`Self::detect`].
    pub fn overlay_strategy(self, layer_shell_support: layer_shell::Support) -> OverlayStrategy {
        match self {
            // X11 never gets layer-shell (the protocol is Wayland-only;
            // `gtk4_layer_shell::is_supported()` answers false there), so
            // the probe result is irrelevant on this arm.
            SessionKind::X11 => OverlayStrategy::SelfPositioned,
            SessionKind::Wayland if layer_shell_support.is_active() => OverlayStrategy::LayerShell,
            // GNOME Wayland: the spec's own chosen shape, not a degraded
            // case — Mutter implements no zwlr_layer_shell_v1 (see
            // `layer_shell`'s module doc), and a Wayland client has no way
            // to position itself regardless. Close-on-focus-loss is the
            // same row's documented behavior.
            SessionKind::Wayland => OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: true,
            },
            // Headless/test backends: keep the default window, wire nothing
            // session-specific on top of it.
            SessionKind::Other => OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: false,
            },
        }
    }
}

impl OverlayStrategy {
    /// Whether the window must position itself (X11) rather than leave
    /// placement to the compositor/WM (or the layer-shell protocol).
    pub fn self_positions(self) -> bool {
        matches!(self, OverlayStrategy::SelfPositioned)
    }

    /// Whether losing keyboard focus should dismiss the window.
    pub fn dismisses_on_focus_loss(self) -> bool {
        match self {
            OverlayStrategy::LayerShell => false,
            OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss,
            } => dismiss_on_focus_loss,
            OverlayStrategy::SelfPositioned => true,
        }
    }

    /// The strategy's name for the startup report. Same reasoning as
    /// [`SessionKind::name`].
    pub fn describe(self) -> &'static str {
        match self {
            OverlayStrategy::LayerShell => "layer-shell overlay",
            OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: true,
            } => "compositor-placed window (close-on-focus-loss)",
            OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: false,
            } => "compositor-placed window (headless)",
            OverlayStrategy::SelfPositioned => {
                "override-positioned window (centered on map, close-on-focus-loss)"
            }
        }
    }

    /// Whether this strategy's window is presented as a layer surface —
    /// the gate `ui::window`'s build runs [`crate::layer_shell::
    /// apply_or_fallback`] behind (issue #233). Exactly one variant says
    /// yes, by construction of [`SessionKind::overlay_strategy`]: the
    /// protocol is Wayland-only, so X11's row never qualifies, and every
    /// fallback row *is* the ordinary window the layer-shell call would
    /// otherwise be a no-op on. Gating on the strategy rather than letting
    /// `apply_or_fallback` re-probe keeps one decision — recorded in the
    /// startup report — authoritative for both the wiring and the log.
    pub fn uses_layer_shell(self) -> bool {
        matches!(self, OverlayStrategy::LayerShell)
    }
}

/// The one-line capability report printed to stderr at startup — which
/// session was detected, what the layer-shell probe said, and which overlay
/// strategy follows from the two. `hop doctor` (M6) consumes this shape.
pub fn startup_report(
    kind: SessionKind,
    layer_shell_support: layer_shell::Support,
    strategy: OverlayStrategy,
) -> String {
    format!(
        "display session: {}; layer-shell support: {:?}; overlay strategy: {}",
        kind.name(),
        layer_shell_support,
        strategy.describe()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_self_positions_and_dismisses_on_focus_loss_regardless_of_layer_shell_probe() {
        // The probe result is irrelevant on X11 — the protocol cannot be
        // implemented there — so all three possible probe answers must
        // resolve to the same strategy. Pinning every arm means a future
        // edit that lets the probe answer leak into the X11 decision fails
        // here.

        for support in [
            layer_shell::Support::NotCompiledIn,
            layer_shell::Support::UnsupportedByCompositor,
            layer_shell::Support::Supported,
        ] {
            let strategy = SessionKind::X11.overlay_strategy(support);
            assert_eq!(strategy, OverlayStrategy::SelfPositioned);
            assert!(strategy.self_positions());
            assert!(strategy.dismisses_on_focus_loss());
        }
    }

    #[test]
    fn gnome_wayland_fallback_places_by_compositor_and_dismisses_on_focus_loss() {
        // The spec §2 GNOME row verbatim: "Normal window, centered,
        // close-on-focus-loss". Mutter never reports layer-shell support
        // (see `layer_shell`'s module doc), so the fallback arm is the one
        // GNOME users get — by design.
        let strategy = SessionKind::Wayland.overlay_strategy(layer_shell::Support::NotCompiledIn);
        assert_eq!(
            strategy,
            OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: true
            }
        );
        assert!(!strategy.self_positions());
        assert!(strategy.dismisses_on_focus_loss());

        // And when a build with the feature meets a supporting compositor,
        // layer-shell wins — the KDE/wlroots rows.
        let strategy = SessionKind::Wayland.overlay_strategy(layer_shell::Support::Supported);
        assert_eq!(strategy, OverlayStrategy::LayerShell);
        assert!(!strategy.dismisses_on_focus_loss());
    }

    #[test]
    fn headless_backends_wire_nothing_session_specific() {
        // Broadway (and any future non-session backend) gets the plain
        // window with no dismissal: the headless smoke tests present the
        // window to capture it, and a focus event arriving mid-capture must
        // not be able to hide it out from under the harness.
        let strategy = SessionKind::Other.overlay_strategy(layer_shell::Support::NotCompiledIn);
        assert_eq!(
            strategy,
            OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: false
            }
        );
        assert!(!strategy.dismisses_on_focus_loss());
        assert!(!strategy.self_positions());
    }

    #[test]
    fn only_the_layer_shell_strategy_applies_layer_shell() {
        // Issue #233: the strategy is what `ui::window`'s build gates the
        // `layer_shell::apply_or_fallback` call on, so the predicate must be
        // true for exactly one variant — the one `overlay_strategy` produces
        // for a Wayland session the compositor answered "supported" in — and
        // false for X11 and `Other`, which must never touch the layer-shell
        // API no matter what any probe said (the protocol is Wayland-only).
        assert!(OverlayStrategy::LayerShell.uses_layer_shell());
        assert!(!OverlayStrategy::SelfPositioned.uses_layer_shell());
        assert!(
            !OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: true
            }
            .uses_layer_shell()
        );
        assert!(
            !OverlayStrategy::CompositorPlaced {
                dismiss_on_focus_loss: false
            }
            .uses_layer_shell()
        );
    }

    #[test]
    fn startup_report_names_the_layer_shell_path_and_why() {
        // Criterion 5 of issue #233: the report must say not only which
        // overlay path a supporting-compositor run took but what the probe
        // answered, so `hop doctor` (M6) can tell "layer-shell because the
        // compositor implements it" from "ordinary window because the
        // feature was not compiled in". Both halves of every Wayland
        // outcome are pinned here, next to the exact substrings
        // tests/wlroots_smoke.rs asserts against.
        let report = startup_report(
            SessionKind::Wayland,
            layer_shell::Support::Supported,
            SessionKind::Wayland.overlay_strategy(layer_shell::Support::Supported),
        );
        assert!(
            report.contains("layer-shell support: Supported"),
            "a layer-shell run must record the probe's yes: {report}"
        );
        assert!(
            report.contains("overlay strategy: layer-shell overlay"),
            "a layer-shell run must name the layer-shell strategy: {report}"
        );

        let report = startup_report(
            SessionKind::Wayland,
            layer_shell::Support::UnsupportedByCompositor,
            SessionKind::Wayland.overlay_strategy(layer_shell::Support::UnsupportedByCompositor),
        );
        assert!(
            report.contains("layer-shell support: UnsupportedByCompositor"),
            "the compositor's no must be visible, not collapsed into the strategy: {report}"
        );
        assert!(
            report.contains("overlay strategy: compositor-placed"),
            "the fallback must be named as the ordinary window it is: {report}"
        );
    }

    #[test]
    fn startup_report_names_session_probe_and_strategy() {
        // The exact substrings tests/x11_smoke.rs asserts against — if these
        // words move, that assertion moves with them, visibly, in the same
        // diff.
        let report = startup_report(
            SessionKind::X11,
            layer_shell::Support::NotCompiledIn,
            OverlayStrategy::SelfPositioned,
        );
        assert!(
            report.contains("display session: X11"),
            "report must name the session: {report}"
        );
        assert!(
            report.contains("overlay strategy: override-positioned"),
            "report must name the strategy: {report}"
        );

        let report = startup_report(
            SessionKind::Wayland,
            layer_shell::Support::NotCompiledIn,
            SessionKind::Wayland.overlay_strategy(layer_shell::Support::NotCompiledIn),
        );
        assert!(report.contains("display session: Wayland"), "{report}");
        assert!(
            report.contains("close-on-focus-loss"),
            "the GNOME row's documented behavior must be visible in the report: {report}"
        );
    }
}
