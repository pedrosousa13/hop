//! Issue #259: detects whether this Wayland session's compositor advertises
//! KDE's `org_kde_kwin_blur_manager` global — the KDE-specific Wayland
//! extension `material`'s module doc names as the thing GNOME's Mutter has
//! no equivalent of at all. **Detection only.** Nothing here creates a blur
//! object, sets a region, or commits a surface — see this module's doc
//! comment on [`probe`] for exactly where this slice's scope ends, and
//! `material.rs`'s own module doc for why a Wayland `Mode::Blur` decision
//! is not yet honestly *applied* until issue #259's second slice.
//!
//! # Why a probe at all, and why it looks like `probe_x11_compositor`
//!
//! `material::probe_x11_compositor` already establishes the shape a
//! capability probe in this crate takes: open (or borrow) a connection, ask
//! one narrow question, collapse every way that can fail — the connection
//! itself, a malformed reply, anything — into one "I don't know" outcome
//! rather than a distinguishable error type, because nothing upstream would
//! act differently on any of them (see that function's own doc comment).
//! [`probe`] below is the identical shape for the identical reason: whether
//! GDK's Wayland connection can be reached, whether the roundtrip completes,
//! or whether the manager is simply absent, [`decide`](crate::material::decide)
//! treats every non-present outcome the same way — opaque, per the honesty
//! invariant `material`'s module doc states outranks this feature entirely.
//!
//! # Sharing GDK's connection, not opening a second one
//!
//! X11's probe opens its own short-lived connection because X permits any
//! number of independent clients against one server with no ordering
//! constraint between them (`material.rs`'s doc comment on
//! [`crate::material::probe_x11_compositor`] explains why that is harmless
//! there). Wayland has no equivalent free lunch: a compositor's socket is
//! not designed around two unrelated `libwayland-client` connections
//! transacting over it with no coordination, and this crate has no reason
//! to open a second one when GDK already holds a live connection to the
//! exact compositor this question is about. `gdk4-wayland`'s `wayland_crate`
//! feature (`Cargo.toml`'s own comment on it) exists precisely to hand that
//! connection back out: [`probe`] reconstructs a `wayland-client`
//! `Connection` from the same `Backend` GDK's own `WaylandDisplay` already
//! opened, via the public, safe `wl_display()` accessor and
//! `Proxy::backend()` — no new socket, no new `unsafe` (the only `unsafe` in
//! that chain lives inside `gdk4-wayland` itself, reconstructing the
//! `Backend` from GDK's C `wl_display*`; nothing this crate writes touches
//! a raw pointer).
//!
//! # A dedicated queue, never GTK's own
//!
//! Sharing the *connection* is not sharing the *event queue*. GTK's main
//! loop already dispatches whatever queue backs its own Wayland objects
//! (input, frame callbacks, surface configure events, …), and this probe
//! must never compete with that dispatch or consume an event GTK expects to
//! see. `Connection::new_event_queue` creates a queue with no bearing on any
//! other queue on the same connection — every object bound through it (the
//! registry here) is tagged to dispatch only on this queue
//! (`wayland-backend`'s `wl_proxy_set_queue`, underneath the safe API this
//! module calls) — so [`probe`]'s one roundtrip touches nothing GTK owns.
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

/// What [`probe`] found. Consulted only on [`crate::session::SessionKind::Wayland`]
/// — the same "irrelevant on every other session kind" shape
/// [`crate::material::CompositorProbe`] already establishes for X11, and
/// tested the identical way: every session kind × every probe outcome, in
/// `material.rs`'s own test module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdeBlurProbe {
    /// The registry roundtrip observed a `org_kde_kwin_blur_manager` global
    /// — this compositor is KWin, or something else advertising the same
    /// protocol. Reaching this variant proves nothing about whether a blur
    /// object would actually apply (this slice creates none — see this
    /// module's doc comment); it proves only that the manager exists to
    /// ask, the identical epistemic status `CompositorProbe::ManagerPresent`
    /// has for X11's compositing-manager selection.
    ManagerPresent,
    /// The roundtrip completed and no `org_kde_kwin_blur_manager` global
    /// appeared — GNOME's Mutter, most wlroots compositors, or any other
    /// Wayland compositor that does not implement this KDE-specific
    /// extension.
    ManagerAbsent,
    /// The probe itself could not run: no `WaylandDisplay`'s `wl_display()`
    /// accessor returned one, no live `Backend` to reconstruct a
    /// `Connection` from, or the roundtrip itself failed (an I/O error, a
    /// protocol error). [`crate::material::decide`] treats this identically
    /// to [`KdeBlurProbe::ManagerAbsent`]: "I don't know" gets the same
    /// answer as "no", per the same honesty invariant
    /// [`crate::material::CompositorProbe::ProbeFailed`]'s doc comment
    /// states for X11.
    ProbeFailed,
}

/// Per-roundtrip [`Dispatch`] state: nothing but whether the blur manager
/// global showed up. This slice never binds it (see this module's doc
/// comment on scope), so there is nothing to hold beyond the one bool the
/// registry callback below sets.
struct RegistryState {
    blur_manager_seen: bool,
}

impl Dispatch<WlRegistry, ()> for RegistryState {
    fn event(
        state: &mut Self,
        _registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = event
            && interface == OrgKdeKwinBlurManager::interface().name
        {
            state.blur_manager_seen = true;
        }
    }
}

/// Asks GDK's own Wayland connection, over a dedicated event queue (this
/// module's doc comment, "A dedicated queue, never GTK's own"), whether the
/// compositor behind `display` advertises `org_kde_kwin_blur_manager`. One
/// registry roundtrip, no bind, no blur object, no commit — see this
/// module's doc comment for exactly what this slice does and does not do.
///
/// Any failure along the way — no live `wl_display()`, no upgradeable
/// `Backend`, the roundtrip itself failing — collapses to
/// [`KdeBlurProbe::ProbeFailed`] rather than propagating a distinguishable
/// error, mirroring [`crate::material::probe_x11_compositor`]'s identical
/// `let Some(..) = .. else { return ProbeFailed }` chain and the same
/// reasoning: nothing upstream would act differently on any one of these
/// failures over another, since [`crate::material::decide`] already treats
/// every one of them exactly like a confirmed absence, per the honesty
/// invariant this crate's material module exists to serve (fail toward
/// opaque, always).
pub fn probe(display: &gdkwayland::WaylandDisplay) -> KdeBlurProbe {
    let Some(wl_display) = display.wl_display() else {
        return KdeBlurProbe::ProbeFailed;
    };
    let Some(backend) = wl_display.backend().upgrade() else {
        return KdeBlurProbe::ProbeFailed;
    };
    let connection = Connection::from_backend(backend);

    // A queue private to this probe — see this module's doc comment, "A
    // dedicated queue, never GTK's own".
    let mut event_queue = connection.new_event_queue::<RegistryState>();
    let qh = event_queue.handle();
    let _registry = wl_display.get_registry(&qh, ());

    let mut state = RegistryState {
        blur_manager_seen: false,
    };
    if event_queue.roundtrip(&mut state).is_err() {
        return KdeBlurProbe::ProbeFailed;
    }

    if state.blur_manager_seen {
        KdeBlurProbe::ManagerPresent
    } else {
        KdeBlurProbe::ManagerAbsent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_variants_are_distinct() {
        // A bare sanity check that the three-variant shape mirrors
        // `CompositorProbe`'s — the real behavioral coverage of what each
        // variant means to `material::decide` lives in `material.rs`'s own
        // exhaustive matrix test, which is the one that actually matters
        // for the honesty invariant. Nothing here opens a display
        // connection: that only happens in `tests/kde_blur_probe.rs`'s
        // integration test, against this machine's live session.
        assert_ne!(KdeBlurProbe::ManagerPresent, KdeBlurProbe::ManagerAbsent);
        assert_ne!(KdeBlurProbe::ManagerAbsent, KdeBlurProbe::ProbeFailed);
        assert_ne!(KdeBlurProbe::ManagerPresent, KdeBlurProbe::ProbeFailed);
    }
}
