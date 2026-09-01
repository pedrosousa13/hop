//! Decides, once per presentation and before the window is ever shown,
//! whether the compositor behind this session's display actually composites
//! alpha honestly enough to justify `assets/stylesheet.css`'s translucent
//! `.hop-material-blur` window ground — and applies exactly one of that
//! class or `.hop-material-opaque` to the [`adw::ApplicationWindow`]
//! `ui::window::build` constructs (issue #253, design spec decision 8).
//!
//! # The contract this module fulfils
//!
//! `assets/stylesheet.css`'s own "MATERIAL MODES — issue #253" comment
//! states it verbatim: this module decides once, logs the outcome, and
//! `apply`s exactly one of the two classes. Wearing neither renders the
//! base `window.background` rule — also solid — so an unclassed window can
//! never be translucent either; that is deliberate, and this module never
//! needs to special-case it. Wearing *both* is never correct either, and
//! [`apply`] is written so a window cannot end up that way (it clears the
//! other class first) — the property `tests/material_mode.rs`'s live-window
//! test checks directly, since nothing at the unit level below ever
//! touches a real widget.
//!
//! The honesty invariant this whole module answers to, from the design
//! spec's decision record (decision 8): **never render a half-transparent
//! panel over hard pixels**. A false positive here (blur applied, nothing
//! actually composites) is a visual bug a user sees the moment they look at
//! the window; a false negative (opaque applied, blur would have rendered
//! fine) only looks a little flatter than it could have. Every arm below is
//! written to fail toward the second kind of mistake, never the first.
//!
//! # Detection reuses `session`, it does not re-derive it
//!
//! [`SessionKind::detect`] already answers "which display backend actually
//! opened this connection" by downcasting the live `GdkDisplay` — see
//! `session`'s own module doc for why that, and not an environment
//! variable, is the honest source of truth. This module calls it again
//! rather than inventing a second detector; the two questions
//! (`session`'s overlay *strategy*, this module's material *mode*) are
//! independent — a Wayland session's overlay strategy depends on
//! layer-shell support, but its material mode never does (see the Wayland
//! arm below) — so nothing is lost by asking `session` fresh instead of
//! threading its answer through as a parameter.
//!
//! # X11: what a compositing-manager probe can and cannot prove
//!
//! Freedesktop.org's compositing-manager convention has every compositor
//! (`picom`, `xcompmgr`, the long-retired `compton`, …) acquire ownership
//! of a selection named `_NET_WM_CM_S<screen>` for as long as it runs, and
//! release it on exit. There is no X extension query for "is anything
//! compositing" — watching who (if anyone) owns that selection *is* the
//! accepted way to ask, and [`probe_x11_compositor`] does exactly that,
//! over a second, short-lived connection opened the same way
//! `x11::center_on_screen` already opens one alongside GDK's (see that
//! module's doc comment for why a second client connection is harmless
//! here): no new dependency, `x11rb` is already this crate's.
//!
//! What that probe answers is narrower than "will this look blurred". Blur
//! specifically — picom's `blur-background` and its variants — is
//! compositor-side configuration with no X property or protocol exposing
//! it; there is no honest way to query it from a client. So this module
//! faces a real choice between two directions, and states which one it
//! took and why:
//!
//! - **Compositor-present-is-sufficient** (what this module does):
//!   whenever a manager owns the selection, alpha genuinely reaches a
//!   compositor and composites against real desktop content — the failure
//!   mode the honesty invariant exists to prevent (raw, uncomposited ARGB
//!   rendering as garbage) simply cannot happen once *any* compositor is
//!   present, blurring or not. A compositor present but not configured to
//!   blur still produces an honest, correctly-composited translucent
//!   window — plain see-through rather than frosted — which is a milder
//!   miss than the SPEC's own "best-effort" language already concedes
//!   (`assets/tokens.css`'s MATERIAL LAYERS comment: "X11 + picom-class
//!   compositors, best-effort"), never a dishonest one.
//! - **Require confirmed blur** (rejected): the only way to get closer
//!   would be inspecting the compositor's own identity or config (for
//!   instance reading `_NET_WM_NAME` off the selection-owner window and
//!   pattern-matching "picom" plus somehow reaching its config) — fragile,
//!   version-specific, and still no proof `blur-background` is actually
//!   turned on inside whatever config that compositor loaded. This would
//!   only convert honest "plain translucent" cases into "opaque", trading
//!   a cosmetic miss most users would not fault for a strictly duller
//!   result, at real implementation cost, and with no way to declare
//!   victory (a config file this module cannot see could always defeat the
//!   check).
//!
//! So: a compositing manager present is treated as sufficient for
//! [`Mode::Blur`] on X11. No manager present, or the probe itself failing
//! for any reason, both degrade to [`Mode::Opaque`] — see [`decide`].
//!
//! # Wayland: KDE's `org_kde_kwin_blur_manager`, detected but not yet applied
//!
//! GNOME's Mutter exposes no blur API of any kind to clients — the design
//! spec's own "Known constraints" section says so, the same fact
//! `layer_shell`'s module doc already leans on for its own GNOME arm — so
//! there is nothing to probe there at all, ever: this is a permanent fact
//! about Mutter, not a gap this module will one day close. KDE's KWin does
//! have a blur protocol, `org_kde_kwin_blur`, and issue #259 adds the probe
//! for it: [`crate::kde_blur::probe`] asks, over GDK's own Wayland
//! connection through a dedicated event queue (see that module's own doc
//! comment for exactly how and why), whether the compositor advertises
//! `org_kde_kwin_blur_manager` at all. As with X11's compositor probe,
//! presence is not proof of anything about *this specific surface* — this
//! slice binds nothing, creates no blur object, commits nothing — only that
//! the protocol exists to ask for it with.
//!
//! Deliberately incomplete, and said so out loud: issue #259 ships in two
//! slices. This first one wires detection all the way through
//! [`decide`] and [`report`], with unit coverage exhaustive over the widened
//! matrix below — but until the second slice adds the surface-bound
//! `org_kde_kwin_blur` object, a positive detection is not the same thing as
//! honest translucency. [`resolve`] accounts for this explicitly (see its
//! own doc comment): the call site downgrades an honestly-*detected*
//! `Mode::Blur` on Wayland to `Mode::Opaque` until the surface work lands,
//! rather than letting `decide` itself lie about what it found.
//!
//! [`decide`]'s `SessionKind::Wayland` arm stays its own match arm,
//! independent of X11's, exactly as before — see [`KdeBlurProbe`] for the
//! sibling-enum shape this module's own prior doc comment anticipated, now
//! filled in rather than merely reserved.
//!
//! # Everything else: fail toward opaque, always
//!
//! [`SessionKind::Other`] — broadway under this crate's own headless smoke
//! tests today, and any future backend this module has never heard of —
//! gets [`Mode::Opaque`] unconditionally, the same answer a detection
//! failure would get if `session` could produce one (it cannot: `detect`
//! is total). There is no scenario in which "I don't recognize this
//! session" should ever resolve to translucency.

use gtk::gdk;
use gtk::prelude::*;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

use crate::kde_blur::KdeBlurProbe;
use crate::session::SessionKind;

/// `assets/stylesheet.css`'s `window.background.hop-material-blur` selector
/// — the ONLY translucent window ground in the system, per that rule's own
/// comment.
pub const BLUR_CSS_CLASS: &str = "hop-material-blur";

/// `assets/stylesheet.css`'s `window.background.hop-material-opaque`
/// selector — the honest default.
pub const OPAQUE_CSS_CLASS: &str = "hop-material-opaque";

/// What [`probe_x11_compositor`] found. Consulted only on
/// [`SessionKind::X11`] — every other session kind ignores it, the same
/// shape `session::SessionKind::overlay_strategy`'s X11 arm gives
/// `layer_shell::Support` (irrelevant there since the protocol is
/// Wayland-only) and tested the identical way below: every session kind ×
/// every probe outcome, in [`decide`]'s own test module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositorProbe {
    /// A compositing manager owns `_NET_WM_CM_S<screen>` — see this
    /// module's doc comment, "X11: what a compositing-manager probe can
    /// and cannot prove", for exactly what this does and does not
    /// establish.
    ManagerPresent,
    /// Nobody owns the selection: no compositing manager is running, so
    /// ARGB alpha never reaches a compositor at all — a translucent window
    /// ground would render as raw, uncomposited garbage. This is the one
    /// fact [`decide`] must never let [`Mode::Blur`] through against.
    ManagerAbsent,
    /// The probe itself could not run — no X connection, a malformed
    /// atom/selection reply, or any other I/O failure along the way.
    /// [`decide`] treats this identically to [`CompositorProbe::ManagerAbsent`]:
    /// "I don't know" gets the same answer as "no", per the honesty
    /// invariant this module exists to serve.
    ProbeFailed,
}

/// Which of `assets/stylesheet.css`'s two `MATERIAL MODES` classes a window
/// should wear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// [`BLUR_CSS_CLASS`] — the translucent window ground. Reachable only
    /// when [`decide`] has proof, not a guess, that alpha composites
    /// honestly (see this module's doc comment).
    Blur,
    /// [`OPAQUE_CSS_CLASS`] — the solid window ground. The only mode a
    /// detection failure, an unrecognized session, or "I don't know" from
    /// the X11 probe may ever resolve to.
    Opaque,
}

impl Mode {
    /// The CSS class [`apply`] adds for this mode.
    pub fn css_class(self) -> &'static str {
        match self {
            Mode::Blur => BLUR_CSS_CLASS,
            Mode::Opaque => OPAQUE_CSS_CLASS,
        }
    }

    /// The name [`report`] logs — same reasoning as
    /// `session::SessionKind::name`/`OverlayStrategy::describe`: a method
    /// rather than a `Display` impl because there is exactly one consumer
    /// shape today and naming it keeps the string greppable next to its
    /// assertions in `tests/material_mode.rs`.
    pub fn describe(self) -> &'static str {
        match self {
            Mode::Blur => "blur",
            Mode::Opaque => "opaque",
        }
    }
}

/// Picks the mode for `kind`, given what [`probe_x11_compositor`] and
/// [`crate::kde_blur::probe`] found — each consulted only on its own
/// session kind, per this module's doc comment. Pure with respect to
/// everything but its three arguments: no display, no window, no I/O — the
/// same split `layer_shell::probe`/`session::SessionKind::overlay_strategy`
/// establish, and for the identical reason, so the whole degrade matrix is
/// unit-tested below with no display connection at all.
///
/// This function answers "did detection honestly confirm blur would
/// composite" — not "is it safe to apply that answer right now". On
/// Wayland specifically, [`resolve`]'s own doc comment records why those
/// are different questions during issue #259's first slice, and why the
/// gap between them is closed one call site up from here, not by weakening
/// this function's honesty.
pub fn decide(kind: SessionKind, x11_compositor: CompositorProbe, kde_blur: KdeBlurProbe) -> Mode {
    match kind {
        // X11: honest translucency needs proof alpha actually composites,
        // not merely that it was requested. A manager owning
        // `_NET_WM_CM_S<screen>` is that proof; anything less — confirmed
        // absent, or the probe itself failing — degrades to opaque. See
        // this module's doc comment, "X11: what a compositing-manager
        // probe can and cannot prove", for why presence alone is treated
        // as sufficient rather than also trying (and failing) to confirm
        // blur specifically. `kde_blur` is irrelevant here — never
        // consulted on this arm.
        SessionKind::X11 => match x11_compositor {
            CompositorProbe::ManagerPresent => Mode::Blur,
            CompositorProbe::ManagerAbsent | CompositorProbe::ProbeFailed => Mode::Opaque,
        },
        // Wayland: GNOME exposes no blur API at all (permanent — see this
        // module's doc comment, "Wayland: KDE's org_kde_kwin_blur_manager,
        // detected but not yet applied"), so only a confirmed
        // `org_kde_kwin_blur_manager` global earns `Mode::Blur`; absence or
        // a failed probe both degrade to opaque, the same "I don't know
        // gets the same answer as no" rule X11's arm follows.
        // `x11_compositor` is irrelevant here — never consulted on this arm.
        SessionKind::Wayland => match kde_blur {
            KdeBlurProbe::ManagerPresent => Mode::Blur,
            KdeBlurProbe::ManagerAbsent | KdeBlurProbe::ProbeFailed => Mode::Opaque,
        },
        // Broadway, or any future backend this module has never heard of:
        // the same answer a detection failure would get. Fail toward
        // opaque, always.
        SessionKind::Other => Mode::Opaque,
    }
}

/// Asks the X server, over a fresh connection, whether a compositing
/// manager owns `_NET_WM_CM_S<screen>` for the connection's default
/// screen — see this module's doc comment for the convention this reads
/// and why a second connection (rather than reusing GDK's own) is the
/// established, harmless mechanism `x11::center_on_screen` already uses.
///
/// Any failure along the way — the connection itself, the atom lookup, the
/// selection-owner query — collapses to [`CompositorProbe::ProbeFailed`]
/// rather than propagating a distinguishable error type: nothing upstream
/// would act differently on "the connection failed" versus "the atom
/// lookup failed" versus "the reply was malformed", since [`decide`]
/// already treats every one of them exactly like a confirmed absence, per
/// the honesty invariant this module exists to serve (fail toward opaque,
/// always).
fn probe_x11_compositor() -> CompositorProbe {
    let Ok((conn, screen_num)) = RustConnection::connect(None) else {
        return CompositorProbe::ProbeFailed;
    };
    let atom_name = format!("_NET_WM_CM_S{screen_num}");
    let Ok(atom_cookie) = conn.intern_atom(false, atom_name.as_bytes()) else {
        return CompositorProbe::ProbeFailed;
    };
    let Ok(atom_reply) = atom_cookie.reply() else {
        return CompositorProbe::ProbeFailed;
    };
    let Ok(owner_cookie) = conn.get_selection_owner(atom_reply.atom) else {
        return CompositorProbe::ProbeFailed;
    };
    let Ok(owner_reply) = owner_cookie.reply() else {
        return CompositorProbe::ProbeFailed;
    };
    if owner_reply.owner == x11rb::NONE {
        CompositorProbe::ManagerAbsent
    } else {
        CompositorProbe::ManagerPresent
    }
}

/// The one-line capability report [`resolve`] logs to stderr — names the
/// session kind, the decision, and the reason, matching the established
/// house style for a startup probe report (`session::startup_report`,
/// `layer_shell`'s doc comment on its own `probe`/`apply_or_fallback`
/// split).
///
/// `mode` is [`decide`]'s honest answer, not necessarily what gets applied
/// to a real window on Wayland this slice — see [`resolve`]'s doc comment.
/// This report always describes detection honestly; it is not the place a
/// caller's temporary downgrade gets hidden.
pub fn report(
    kind: SessionKind,
    x11_compositor: CompositorProbe,
    kde_blur: KdeBlurProbe,
    mode: Mode,
) -> String {
    let reason = match kind {
        SessionKind::X11 => match x11_compositor {
            CompositorProbe::ManagerPresent => {
                "a compositing manager owns _NET_WM_CM_S<screen>; alpha composites honestly \
                 (best-effort: presence is not proof of blur specifically — see module doc)"
            }
            CompositorProbe::ManagerAbsent => {
                "no compositing manager owns _NET_WM_CM_S<screen>; alpha would not composite"
            }
            CompositorProbe::ProbeFailed => {
                "the X11 compositor probe itself failed; degrading to opaque per the honesty \
                 invariant"
            }
        },
        SessionKind::Wayland => match kde_blur {
            KdeBlurProbe::ManagerPresent => {
                "a Wayland global advertises org_kde_kwin_blur_manager (issue #259 slice 1: \
                 detected only — no surface-bound blur object exists yet, see module doc)"
            }
            KdeBlurProbe::ManagerAbsent => {
                "no Wayland global advertises org_kde_kwin_blur_manager (GNOME/Mutter: never \
                 will; some other compositor: does not implement it — see module doc)"
            }
            KdeBlurProbe::ProbeFailed => {
                "the KDE org_kde_kwin_blur_manager probe itself failed; degrading to opaque \
                 per the honesty invariant"
            }
        },
        SessionKind::Other => "not a real display session; nothing to detect",
    };
    format!(
        "material mode: session={}; decision={}; reason: {reason}",
        kind.name(),
        mode.describe()
    )
}

/// Runs the whole decision for the default display: detects the session
/// ([`SessionKind::detect`], reused rather than re-derived — see this
/// module's doc comment), probes for a compositing manager when, and only
/// when, the session is X11, probes for KDE's blur manager when, and only
/// when, the session is Wayland, decides the mode, and logs the one-line
/// outcome [`report`] formats.
///
/// # This function stays honest; the two-phase Wayland gap is a call
/// # site's problem, not this function's
///
/// [`decide`] can now honestly answer [`Mode::Blur`] for a Wayland session
/// whose compositor advertises `org_kde_kwin_blur_manager` — this function
/// returns exactly that answer, and [`report`] logs exactly that reasoning.
/// It does **not** downgrade the answer to account for issue #259 being a
/// two-slice piece of work: this slice's [`crate::kde_blur::probe`] creates
/// no surface-bound blur object (see that module's doc comment), so a real
/// window applying `Mode::Blur` here today would render a translucent
/// ground the compositor is not actually blurring anything behind —
/// exactly the failure mode the honesty invariant exists to prevent.
///
/// That gap is closed at `ui::window::HopWindow::build`'s call site
/// instead of here, deliberately: this function's job is "what did
/// detection honestly find", not "what is currently safe to paint", and
/// conflating the two would make this function's own answer a function of
/// which slice of #259 happens to be merged — indistinguishable from
/// weakening the invariant. See that call site's own comment for the
/// explicit, temporary downgrade and why it is written to be easy to find
/// and delete once issue #259's second slice lands the surface-bound
/// application.
///
/// The `None`-display panic matches `app::install_stylesheet`'s and
/// `app::resolve_overlay_strategy`'s identical posture: by the time
/// `ui::window::HopWindow::build` runs, `GtkApplication`'s own default
/// startup handler has already resolved a display (both of those
/// functions' doc comments account for the ordering) — there is no window
/// to apply a material mode to, decided or not, without one.
pub fn resolve() -> Mode {
    let Some(display) = gdk::Display::default() else {
        panic!("hop-gtk: no gdk::Display available when deciding the material mode");
    };
    let kind = SessionKind::detect(&display);
    let x11_compositor = match kind {
        SessionKind::X11 => probe_x11_compositor(),
        // Never consulted for these arms (see `decide`'s match) — no X
        // connection is opened for an answer that cannot change the
        // outcome.
        SessionKind::Wayland | SessionKind::Other => CompositorProbe::ManagerAbsent,
    };
    let kde_blur = match kind {
        // `display` is already known to hold a `WaylandDisplay` — `detect`
        // just downcast it to reach `SessionKind::Wayland` — so this
        // second downcast cannot fail in practice; `ProbeFailed` is still
        // the fallback rather than a panic or an `unwrap`, matching this
        // crate's `clippy::unwrap_used` lint and the honesty invariant's
        // "any ambiguity degrades to opaque" (a value this function cannot
        // itself prove `Some` from `SessionKind` alone, the same posture
        // `ui::window`'s own `let Some(item) = .. else` comment states for
        // its structurally-unreachable case).
        SessionKind::Wayland => match display.downcast_ref::<gdkwayland::WaylandDisplay>() {
            Some(wayland_display) => crate::kde_blur::probe(wayland_display),
            None => KdeBlurProbe::ProbeFailed,
        },
        // Never consulted for these arms (see `decide`'s match) — no
        // Wayland connection is reached for an answer that cannot change
        // the outcome.
        SessionKind::X11 | SessionKind::Other => KdeBlurProbe::ManagerAbsent,
    };
    let mode = decide(kind, x11_compositor, kde_blur);
    eprintln!("hop-gtk: {}", report(kind, x11_compositor, kde_blur, mode));
    mode
}

/// Applies `mode`'s class to `window`, clearing the other mode's class
/// first so a window can never end up wearing both — the invariant
/// `tests/material_mode.rs`'s live-window test checks directly.
/// Idempotent: calling this any number of times with the same `mode`
/// leaves `window` in the same state every time.
pub fn apply(window: &impl IsA<gtk::Widget>, mode: Mode) {
    window.remove_css_class(BLUR_CSS_CLASS);
    window.remove_css_class(OPAQUE_CSS_CLASS);
    window.add_css_class(mode.css_class());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`CompositorProbe`] variant — the X11 axis of the cartesian
    /// product [`no_session_kind_ever_resolves_to_blur_without_a_confirmed_compositor`]
    /// walks.
    const ALL_X11_PROBES: [CompositorProbe; 3] = [
        CompositorProbe::ManagerPresent,
        CompositorProbe::ManagerAbsent,
        CompositorProbe::ProbeFailed,
    ];

    /// Every [`KdeBlurProbe`] variant — the Wayland axis of the same
    /// product.
    const ALL_KDE_PROBES: [KdeBlurProbe; 3] = [
        KdeBlurProbe::ManagerPresent,
        KdeBlurProbe::ManagerAbsent,
        KdeBlurProbe::ProbeFailed,
    ];

    /// Every [`SessionKind`] — the third axis.
    const ALL_SESSION_KINDS: [SessionKind; 3] =
        [SessionKind::X11, SessionKind::Wayland, SessionKind::Other];

    #[test]
    fn x11_blurs_only_when_a_compositing_manager_is_confirmed_present() {
        // `kde_blur` is irrelevant on this arm — exercised at every value
        // to prove it, matching `kde_blurs_only_when_the_manager_is_confirmed_present`'s
        // identical proof for the X11 probe on Wayland's arm below.
        for kde_blur in ALL_KDE_PROBES {
            assert_eq!(
                decide(SessionKind::X11, CompositorProbe::ManagerPresent, kde_blur),
                Mode::Blur
            );
            assert_eq!(
                decide(SessionKind::X11, CompositorProbe::ManagerAbsent, kde_blur),
                Mode::Opaque
            );
            assert_eq!(
                decide(SessionKind::X11, CompositorProbe::ProbeFailed, kde_blur),
                Mode::Opaque
            );
        }
    }

    #[test]
    fn a_failed_x11_probe_degrades_identically_to_a_confirmed_absent_compositor() {
        // "I don't know" must never be treated more favorably than a
        // confirmed "no" — the honesty invariant's own "when in doubt,
        // degrade to opaque".
        assert_eq!(
            decide(
                SessionKind::X11,
                CompositorProbe::ProbeFailed,
                KdeBlurProbe::ManagerAbsent
            ),
            decide(
                SessionKind::X11,
                CompositorProbe::ManagerAbsent,
                KdeBlurProbe::ManagerAbsent
            )
        );
    }

    #[test]
    fn kde_blurs_only_when_the_manager_is_confirmed_present() {
        // The Wayland mirror of `x11_blurs_only_when_a_compositing_manager_is_confirmed_present`.
        // `x11_compositor` is irrelevant on this arm — exercised at every
        // value to prove it.
        for x11_compositor in ALL_X11_PROBES {
            assert_eq!(
                decide(
                    SessionKind::Wayland,
                    x11_compositor,
                    KdeBlurProbe::ManagerPresent
                ),
                Mode::Blur
            );
            assert_eq!(
                decide(
                    SessionKind::Wayland,
                    x11_compositor,
                    KdeBlurProbe::ManagerAbsent
                ),
                Mode::Opaque
            );
            assert_eq!(
                decide(
                    SessionKind::Wayland,
                    x11_compositor,
                    KdeBlurProbe::ProbeFailed
                ),
                Mode::Opaque
            );
        }
    }

    #[test]
    fn a_failed_kde_probe_degrades_identically_to_a_confirmed_absent_manager() {
        // The Wayland mirror of `a_failed_x11_probe_degrades_identically_to_a_confirmed_absent_compositor`.
        assert_eq!(
            decide(
                SessionKind::Wayland,
                CompositorProbe::ManagerAbsent,
                KdeBlurProbe::ProbeFailed
            ),
            decide(
                SessionKind::Wayland,
                CompositorProbe::ManagerAbsent,
                KdeBlurProbe::ManagerAbsent
            )
        );
    }

    #[test]
    fn other_sessions_are_always_opaque_regardless_of_either_probe() {
        for x11_compositor in ALL_X11_PROBES {
            for kde_blur in ALL_KDE_PROBES {
                assert_eq!(
                    decide(SessionKind::Other, x11_compositor, kde_blur),
                    Mode::Opaque
                );
            }
        }
    }

    #[test]
    fn no_session_kind_ever_resolves_to_blur_without_a_confirmed_compositor() {
        // The exhaustive statement of the degrade matrix above, now over
        // the full cartesian product of all three enums (issue #259
        // widened this deliberately from the X11-only matrix a prior
        // revision of this test pinned — see this module's doc comment
        // and the issue's own triage comment for why changing this test
        // was the intended, not incidental, part of adding the KDE
        // branch): across every session kind × every X11 probe outcome ×
        // every KDE probe outcome, `Mode::Blur` comes back in exactly two
        // combinations — `(X11, ManagerPresent, *)` and
        // `(Wayland, *, ManagerPresent)` — and never in any other. Every
        // other combination must degrade to opaque, which is what keeps
        // this a real guard on the honesty invariant rather than a spot
        // check of a few hand-picked cases.
        let mut blur_combinations: Vec<(SessionKind, CompositorProbe, KdeBlurProbe)> = Vec::new();
        for kind in ALL_SESSION_KINDS {
            for x11_compositor in ALL_X11_PROBES {
                for kde_blur in ALL_KDE_PROBES {
                    if decide(kind, x11_compositor, kde_blur) == Mode::Blur {
                        blur_combinations.push((kind, x11_compositor, kde_blur));
                    }
                }
            }
        }
        for (kind, x11_compositor, kde_blur) in &blur_combinations {
            let is_x11_confirmed =
                *kind == SessionKind::X11 && *x11_compositor == CompositorProbe::ManagerPresent;
            let is_kde_confirmed =
                *kind == SessionKind::Wayland && *kde_blur == KdeBlurProbe::ManagerPresent;
            assert!(
                is_x11_confirmed || is_kde_confirmed,
                "unreachable Mode::Blur combination: {kind:?}, {x11_compositor:?}, {kde_blur:?}"
            );
        }
        // Exactly one X11 combination (the KDE probe is irrelevant there,
        // so it must appear once per KDE probe value) and exactly one
        // Wayland combination per X11 probe value — `ALL_KDE_PROBES.len()`
        // of each, `2 * ALL_KDE_PROBES.len()` total.
        assert_eq!(blur_combinations.len(), 2 * ALL_KDE_PROBES.len());
        assert_eq!(
            blur_combinations
                .iter()
                .filter(|(kind, ..)| *kind == SessionKind::X11)
                .count(),
            ALL_KDE_PROBES.len(),
            "X11 confirmed-present must blur regardless of the irrelevant KDE probe value"
        );
        assert_eq!(
            blur_combinations
                .iter()
                .filter(|(kind, ..)| *kind == SessionKind::Wayland)
                .count(),
            ALL_X11_PROBES.len(),
            "Wayland confirmed-present must blur regardless of the irrelevant X11 probe value"
        );
    }

    #[test]
    fn mode_css_classes_match_the_stylesheet_contract() {
        assert_eq!(Mode::Blur.css_class(), "hop-material-blur");
        assert_eq!(Mode::Opaque.css_class(), "hop-material-opaque");
    }

    #[test]
    fn report_names_the_session_the_decision_and_the_reason() {
        let text = report(
            SessionKind::X11,
            CompositorProbe::ManagerAbsent,
            KdeBlurProbe::ManagerAbsent,
            Mode::Opaque,
        );
        assert!(text.contains("X11"), "{text}");
        assert!(text.contains("opaque"), "{text}");
        assert!(text.contains("_NET_WM_CM_S"), "{text}");

        let text = report(
            SessionKind::Wayland,
            CompositorProbe::ManagerAbsent,
            KdeBlurProbe::ManagerAbsent,
            Mode::Opaque,
        );
        assert!(text.contains("Wayland"), "{text}");
        assert!(text.contains("opaque"), "{text}");
        assert!(text.contains("org_kde_kwin_blur_manager"), "{text}");

        // The KDE-present arm's own reason: says the manager was found,
        // and that this slice does not yet apply blur from it (issue
        // #259's own two-slice honesty — see `resolve`'s doc comment).
        let text = report(
            SessionKind::Wayland,
            CompositorProbe::ManagerAbsent,
            KdeBlurProbe::ManagerPresent,
            Mode::Blur,
        );
        assert!(text.contains("Wayland"), "{text}");
        assert!(text.contains("blur"), "{text}");
        assert!(text.contains("org_kde_kwin_blur_manager"), "{text}");
        assert!(text.contains("slice 1"), "{text}");
    }
}
