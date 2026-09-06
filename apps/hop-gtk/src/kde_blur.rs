//! Issue #259: KDE Wayland blur, end to end. [`probe`] answers whether this
//! Wayland session's compositor advertises KDE's
//! `org_kde_kwin_blur_manager` global — the KDE-specific Wayland extension
//! `material`'s module doc names as the thing GNOME's Mutter has no
//! equivalent of at all — and [`apply_blur`] is what acts on a positive
//! answer: it binds the manager, creates a surface-bound `org_kde_kwin_blur`
//! object for the mapped window's own `wl_surface`, and commits it.
//! `material::decide` only ever resolves Wayland to [`crate::material::Mode::Blur`]
//! once [`probe`] has confirmed the manager exists (`material.rs`'s own
//! module doc, "Wayland: KDE's `org_kde_kwin_blur_manager`"); [`apply_blur`]
//! is what then makes that decision actually true of the pixels on screen,
//! rather than a translucent CSS class with nothing behind it compositing —
//! see that function's own doc comment for exactly what it does and does
//! not guarantee.
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
//! connection back out: both [`probe`] and [`apply_blur`] reconstruct a
//! `wayland-client` `Connection` from the same `Backend` GDK's own
//! `WaylandDisplay` already opened, via the public, safe `wl_display()`
//! accessor and `Proxy::backend()` — no new socket, no new `unsafe` (the
//! only `unsafe` in that chain lives inside `gdk4-wayland` itself,
//! reconstructing the `Backend` from GDK's C `wl_display*`; nothing this
//! crate writes touches a raw pointer). [`apply_blur`] reaches the surface's
//! own `wl_surface` the same safe way, through `WaylandSurfaceExtManual`,
//! rather than accepting a `wl_surface` handed in from outside — the same
//! "ask GDK for its own object" posture, applied to the surface instead of
//! the display.
//!
//! # A dedicated queue, never GTK's own
//!
//! Sharing the *connection* is not sharing the *event queue*. GTK's main
//! loop already dispatches whatever queue backs its own Wayland objects
//! (input, frame callbacks, surface configure events, …), and neither
//! [`probe`] nor [`apply_blur`] may ever compete with that dispatch or
//! consume an event GTK expects to see. `Connection::new_event_queue`
//! creates a queue with no bearing on any other queue on the same
//! connection — every object bound through it (the registry, and anything
//! bound from it) is tagged to dispatch only on this queue
//! (`wayland-backend`'s `wl_proxy_set_queue`, underneath the safe API this
//! module calls) — so neither function's own roundtrip touches anything
//! GTK owns.
//!
//! # Binding and applying: what `apply_blur` does once the manager exists
//!
//! A confirmed manager is not yet a blurred window: three more steps stand
//! between "the global exists" and "this surface blurs" — binding the
//! manager (capturing its `name` *and* `version`, unlike [`probe`]'s bare
//! bool), creating the surface-bound `org_kde_kwin_blur` object, and
//! committing it. [`apply_blur`]'s own doc comment covers each in detail:
//! why the bind is capped at `version.min(1)`, why `set_region(None)`
//! rather than a real region, and why the object commits itself but the
//! surface never does. The two probes intentionally do **not** share one
//! `Dispatch` state type — see [`BlurManagerLookup`]'s own doc comment for
//! why widening [`RegistryState`] instead was rejected.
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

use gdkwayland::prelude::WaylandSurfaceExtManual;
use gtk::prelude::*;

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
    /// object would actually apply to any one surface — [`probe`] itself
    /// binds nothing and creates no blur object, the identical epistemic
    /// status `CompositorProbe::ManagerPresent` has for X11's
    /// compositing-manager selection. [`crate::material::resolve`] is what
    /// turns a positive probe into an actual, surface-bound blur, by
    /// calling [`apply_blur`] once [`crate::material::decide`] has resolved
    /// to [`crate::material::Mode::Blur`] on its strength — and even then,
    /// [`apply_blur`]'s own `create` call could still fail for reasons this
    /// probe cannot see (its own doc comment says so explicitly).
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

/// Per-roundtrip [`Dispatch`] state for [`probe`]: nothing but whether the
/// blur manager global showed up. [`probe`] never binds anything — it only
/// ever needs to know the global exists, not its `name` or `version` — so
/// there is nothing to hold beyond the one bool the registry callback below
/// sets. [`apply_blur`] needs more (a bind needs both, see
/// [`BlurManagerLookup`]) and deliberately uses its own state type rather
/// than widening this one: this struct, and therefore [`probe`]'s own
/// behavior and tests, are untouched by anything [`apply_blur`] adds.
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
/// registry roundtrip, no bind, no blur object, no commit — binding and
/// applying happen only in [`apply_blur`], and only once
/// [`crate::material::decide`] has turned a positive answer here into
/// [`crate::material::Mode::Blur`].
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

/// Per-roundtrip [`Dispatch`] state for [`apply_blur`]'s own registry
/// lookup. Distinct from [`RegistryState`] because binding a global needs
/// its `name` *and* `version` — [`probe`] only ever needed a bool, so
/// widening that struct for this would have handed `probe` a field it
/// never reads and this module's doc comment already explains why the two
/// stay separate. `None` until the roundtrip's callback sees the global (or
/// forever, on a compositor that never advertises it).
struct BlurManagerLookup {
    global: Option<(u32, u32)>,
}

impl Dispatch<WlRegistry, ()> for BlurManagerLookup {
    fn event(
        state: &mut Self,
        _registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == OrgKdeKwinBlurManager::interface().name
        {
            state.global = Some((name, version));
        }
    }
}

// Neither the manager nor the blur object carries a single event in
// `blur.xml` (this module's own doc comment quotes the protocol in full) —
// both interfaces are requests only. `delegate_noop!` is `wayland-client`'s
// own answer to "this type has nothing to dispatch", the same macro
// `wayland-client`'s own `simple_window` example uses for its buffer/shm/
// surface/compositor objects, and strictly less code (and less to keep in
// sync with the protocol) than a hand-written `Dispatch` impl whose `event`
// body could only ever be empty.
delegate_noop!(BlurManagerLookup: ignore OrgKdeKwinBlurManager);
delegate_noop!(BlurManagerLookup: ignore OrgKdeKwinBlur);

/// What [`apply_blur`] keeps alive for as long as the surface it was built
/// for — see that function's own doc comment, "Object lifetime", for why
/// all three fields matter and none of them may be dropped early.
struct BlurSession {
    /// Kept alive only because `blur` (and, transitively, every proxy
    /// reachable through it) was minted from it — `wayland-rs` gives no
    /// guarantee that sending a request through a proxy still works once
    /// the `Connection` it came from is gone, and this module does not rely
    /// on an unwritten guarantee to save one small struct field.
    _connection: Connection,
    /// Same reasoning as `_connection`, one level down: `blur` is tagged to
    /// this specific queue (`wl_proxy_set_queue`, underneath the safe API
    /// this module calls — see the module doc's "A dedicated queue"
    /// section), and dropping the queue while a proxy tagged to it still
    /// lives is exactly the situation this struct exists to avoid.
    _event_queue: EventQueue<BlurManagerLookup>,
    /// The live protocol object. Held so a later remap can call
    /// [`OrgKdeKwinBlur::release`] on *this* one before creating its
    /// replacement — see `apply_blur`'s own doc comment for why a plain
    /// `drop` would leak it instead.
    blur: OrgKdeKwinBlur,
}

/// Demotes `window` to [`crate::material::Mode::Opaque`] — the honesty
/// invariant's one enforcement point for [`apply_blur`]'s closure, called
/// from every failure path that runs *after* the closure's `WaylandSurface`
/// downcast has already succeeded (see that function's own doc comment,
/// "The honesty invariant enforced on every bind-failure path", for exactly
/// which paths those are and why the boundary sits there and nowhere
/// earlier). `material::apply` is idempotent, so calling this on a window
/// already wearing `Mode::Opaque` — the X11/`Other` case, which never calls
/// this at all — would have been harmless too; the boundary is enforced by
/// discipline at each call site regardless, because "harmless if misused
/// here" is not the same claim as "correct to call here", and this
/// function's only job is to be the one place that claim is made.
fn demote_to_opaque(window: &adw::ApplicationWindow) {
    crate::material::apply(window, crate::material::Mode::Opaque);
}

/// Wires `window` up so that every time it maps on a Wayland display whose
/// compositor advertises `org_kde_kwin_blur_manager`, a surface-bound
/// `org_kde_kwin_blur` object is created, region-blurred to the whole
/// surface, and committed — the step that turns
/// [`crate::material::decide`]'s honest [`crate::material::Mode::Blur`]
/// answer on Wayland into a compositor that is actually blurring something,
/// closing the gap issue #259's own module docs (`material.rs`,
/// `ui::window`) describe. Shaped on `x11::apply_self_positioning`, this
/// crate's established pattern for "do something to the native surface
/// once it exists": clone `window`, `connect_map`, and inside, every step
/// is a `let ... else { return; }` early exit. No panics, no
/// `unwrap`/`expect` (this crate's `clippy::unwrap_used` lint, `-D
/// warnings`) — failure here is silent-and-opaque, never fatal, the same
/// posture [`probe`]'s own `ProbeFailed` arm takes and for the same
/// reason: the window was already painted opaque or blur by
/// `material::apply` before this ever runs, and a failure partway through
/// this function must never turn that into a translucent ground with
/// nothing behind it compositing.
///
/// # This function is its own X11/`Other` guard
///
/// The first step downcasts the mapped surface to
/// [`gdkwayland::WaylandSurface`]; on X11 (or broadway, or any future
/// backend) that downcast fails and the function returns immediately. This
/// is why the call site in `ui::window` needs no session check of its own
/// before calling this unconditionally whenever `material::resolve`
/// answered [`crate::material::Mode::Blur`]: X11's own blur was already
/// fully implemented by issue #253 through `assets/stylesheet.css`'s CSS
/// class alone, and this function simply has nothing to do there.
///
/// # What this function cannot guarantee
///
/// [`crate::material::decide`] resolves [`crate::material::Mode::Blur`] on
/// Wayland from manager *presence* alone (`material.rs`'s own module doc
/// explains why presence, not a live bind, is what `decide` can honestly
/// check without a surface to bind against). Presence is not a guarantee
/// that `create` below will succeed on *this* surface at *this* moment —
/// a compositor could in principle advertise the global and still refuse a
/// `create` request for a reason this module has no way to observe from
/// here.
///
/// # The honesty invariant enforced on every bind-failure path
///
/// Before the first downcast to [`gdkwayland::WaylandSurface`] succeeds,
/// nothing here is a KDE-Wayland failure at all — see "This function is its
/// own X11/`Other` guard" above — so every exit before that point leaves the
/// window exactly as `material::apply` already painted it, which is correct
/// on X11 and every other backend: their blur (or lack of it) was never this
/// function's to touch. Once that downcast has succeeded, though, this
/// surface is one `material::decide` already committed to
/// [`crate::material::Mode::Blur`], and every failure this function *can*
/// observe from there on — the remaining downcasts, the roundtrip, a missing
/// global, a `flush` error — demotes the window to
/// [`crate::material::Mode::Opaque`] itself (see [`demote_to_opaque`])
/// before returning; "leaving it exactly as painted" would be the dishonest,
/// translucent-with-nothing-behind-it state `material.rs`'s own module doc
/// forbids. `connect_map` fires again on every subsequent show, so a
/// demotion is never permanent: once a later map's bind actually succeeds,
/// this function re-applies [`crate::material::Mode::Blur`] before
/// returning. The one gap nothing on the client side can close is a
/// `create`/`commit` that the wire protocol accepts but the compositor
/// silently ignores — that failure produces no observable signal at all, so
/// it is left to the real-KWin-session follow-up this crate's other
/// Wayland-only assertions already defer to (see `tests/kde_blur_probe.rs`'s
/// own module doc for the identical shape of deferral).
///
/// # Object lifetime: why a bare `OrgKdeKwinBlur` is not enough
///
/// `window.connect_map` fires again every time the window is hidden and
/// re-shown (`hide_on_close(true)`, `ui::window`'s own builder call), and
/// GTK4 destroys and recreates the `GdkSurface` across that cycle — so the
/// `wl_surface` [`apply_blur`] sees on a second map is a genuinely
/// *different* object, not the one this function already blurred. Two
/// things follow, both handled below:
///
/// - The [`Connection`], [`EventQueue`] and [`OrgKdeKwinBlur`] this map
///   creates must stay alive for as long as this surface does — see
///   [`BlurSession`]'s own doc comment for why all three, not just the
///   blur object, and why relying on drop order across them would be
///   relying on something `wayland-rs` does not document.
/// - `wayland-rs` sends nothing to the compositor when a proxy is merely
///   dropped — a plain `drop` of the previous map's `OrgKdeKwinBlur` would
///   leak one `org_kde_kwin_blur` protocol object on the compositor's side
///   per show/hide cycle. [`OrgKdeKwinBlur::release`] is the protocol's own
///   destructor request (`blur.xml`'s `release` request, `type="destructor"`)
///   and is what actually frees it — so this function calls `release()` on
///   the *stale* session's blur object before building the replacement,
///   every time `connect_map` fires a second or later time.
///
/// A `RefCell<Option<BlurSession>>` captured by the closure holds "the
/// session for whichever surface is currently mapped, if any" — `None`
/// before the first successful bind, `Some` afterward, replaced (with the
/// old one released first) on every subsequent map.
///
/// # Flush ordering: bookkeeping first, the flush last
///
/// `create` has already allocated an object id the moment it is called —
/// before anything is flushed to the compositor at all. An earlier version
/// of this function released the previous session's blur, stored the new
/// one, and flushed in that order but returned *before* storing on a
/// flush error, which meant the very object this paragraph is about could
/// end up allocated on the wire (a `create` partially written before
/// `flush` failed) with nothing in `session` ever pointing back to it — a
/// permanent leak this function's own "Object lifetime" section above
/// exists to prevent, not one it can be allowed to reintroduce on its own
/// error path. The fix is ordering, not a new case: release the previous
/// session's blur and store this map's `BlurSession` *before* the flush
/// runs, over a cloned `Connection` handle (cheap — a `Connection` is a
/// thin, `Clone`-able reference to the same backend, not a second
/// connection), so every object this function ever creates is tracked in
/// `session` by the time flushing can fail at all. A flush failure past
/// that point is then just another observable failure under the honesty
/// invariant above — [`demote_to_opaque`] and return — and the object is
/// never orphaned: a later remap still finds it in `session` and releases
/// it before creating its replacement, exactly as it would have on any
/// other subsequent map.
///
/// # Why `set_region(None)`, not a real region
///
/// `blur.xml`'s `set_region` takes a `wl_region`, and `wl_region` can only
/// ever describe axis-aligned rectangles. Hop's window is a rounded CSD
/// (client-side-decorated) surface — no finite union of rectangles traces
/// its actual visible outline exactly, so any region this function could
/// construct here would either clip real corners off the blur or leak
/// square corners past the rounded ones. `set_region(None)` — the
/// `allow-null="true"` arm the protocol reserves for exactly this — asks
/// the compositor to blur the surface's own bounds instead, which is
/// honestly imprecise at the four corners rather than precisely wrong
/// everywhere; narrowing it to a real region is real-KWin-session work
/// this module explicitly leaves to a follow-up (see "What this function
/// cannot guarantee" above), since evaluating it needs to be looked at,
/// not computed blind on a machine that cannot render it.
///
/// # Why this function never calls `wl_surface.commit()`
///
/// GTK owns this surface's frame cycle end to end — every attach, damage
/// and commit on it already goes through GDK's own rendering loop. A
/// second, uncoordinated `wl_surface.commit()` from here would race that
/// loop rather than cooperate with it: whichever commit reaches the
/// compositor first could commit a half-updated buffer state the other
/// side did not intend yet. `org_kde_kwin_blur`'s own `commit()` request
/// only commits the *blur* object's pending state (the region just set);
/// KWin only starts blurring behind the surface once GTK's *own* next
/// `wl_surface.commit()` lands, whenever GDK's frame cycle gets there next
/// on its own schedule. `win.queue_draw()` is this function's one push
/// toward "promptly" rather than "eventually": it schedules a redraw,
/// which schedules GTK's own next commit, without this function reaching
/// into a frame cycle it does not own.
///
/// # A GObject reference cycle, inert today but worth naming
///
/// `let win = window.clone(); window.connect_map(move |_| { .. })` closes
/// over `win`, and the closure itself is owned by the signal handler GTK
/// attaches to `window` — so `window` transitively keeps its own closure
/// alive, a reference cycle neither side ever breaks. This is the identical
/// shape `x11::apply_self_positioning` already has, and it is harmless
/// there for the same reason it is harmless here: `hide_on_close(true)`
/// plus `app.rs`'s single-instance `active_window()` check mean exactly one
/// window exists for the life of the process and it is never destroyed, so
/// nothing ever needs this cycle to break. The difference worth recording is
/// what the cycle keeps alive: `x11.rs`'s closure captures only a copied
/// XID, so its cycle would leak a handful of bytes if the window-lifetime
/// model ever changed; this closure's `session` cell can hold a live
/// `Connection`, `EventQueue` and `OrgKdeKwinBlur` (see [`BlurSession`]), so
/// the same future change — multiple windows, or a window that is actually
/// destroyed and rebuilt — would leak live protocol objects here where
/// `x11.rs`'s would just no-op. Not a bug against this crate's actual
/// window-lifetime model today, and not restructured for a model this
/// crate does not have.
pub fn apply_blur(window: &adw::ApplicationWindow) {
    let win = window.clone();
    let session: std::cell::RefCell<Option<BlurSession>> = std::cell::RefCell::new(None);
    window.connect_map(move |_| {
        let Some(surface) = win.surface() else {
            return;
        };
        // The function's own X11/`Other` guard — see this function's doc
        // comment, "This function is its own X11/`Other` guard".
        let Some(wayland_surface) = surface.downcast_ref::<gdkwayland::WaylandSurface>() else {
            return;
        };
        // Past this point the downcast above has already succeeded, so
        // every remaining exit is an observable KDE-Wayland failure and
        // must demote rather than merely return — see this function's doc
        // comment, "The honesty invariant enforced on every bind-failure
        // path".
        let Some(wl_surface) = wayland_surface.wl_surface() else {
            demote_to_opaque(&win);
            return;
        };
        let Some(wayland_display) = surface
            .display()
            .downcast::<gdkwayland::WaylandDisplay>()
            .ok()
        else {
            demote_to_opaque(&win);
            return;
        };
        let Some(wl_display) = wayland_display.wl_display() else {
            demote_to_opaque(&win);
            return;
        };
        let Some(backend) = wl_display.backend().upgrade() else {
            demote_to_opaque(&win);
            return;
        };
        let connection = Connection::from_backend(backend);

        // A queue private to this bind — see the module doc, "A dedicated
        // queue, never GTK's own".
        let mut event_queue = connection.new_event_queue::<BlurManagerLookup>();
        let qh = event_queue.handle();
        let registry = wl_display.get_registry(&qh, ());

        let mut lookup = BlurManagerLookup { global: None };
        if event_queue.roundtrip(&mut lookup).is_err() {
            demote_to_opaque(&win);
            return;
        }
        let Some((name, version)) = lookup.global else {
            demote_to_opaque(&win);
            return;
        };

        // The protocol is version 1 (`blur.xml`, quoted in this module's
        // doc comment) — asking to bind higher than what the compositor
        // just advertised is a protocol error, not a negotiation, so the
        // request is capped at the lower of the two rather than at our own
        // ceiling alone.
        let manager: OrgKdeKwinBlurManager = registry.bind(name, version.min(1), &qh, ());
        let blur = manager.create(&wl_surface, &qh, ());
        // See this function's doc comment, "Why `set_region(None)`, not a
        // real region".
        blur.set_region(None);
        blur.commit();

        // Release the previous surface's blur object and store this map's
        // session *before* flushing — see this function's doc comment,
        // "Flush ordering: bookkeeping first, the flush last" — so the
        // object just created above is never left untracked if the flush
        // below fails partway through.
        if let Some(previous) = session.borrow_mut().take() {
            previous.blur.release();
        }
        let flush_connection = connection.clone();
        *session.borrow_mut() = Some(BlurSession {
            _connection: connection,
            _event_queue: event_queue,
            blur,
        });

        if flush_connection.flush().is_err() {
            demote_to_opaque(&win);
            return;
        }

        // The bind succeeded end to end. If an earlier map on this same
        // window demoted it to Mode::Opaque, restore Mode::Blur now — see
        // this function's doc comment, "... this function re-applies
        // Mode::Blur before returning". `material::apply` is idempotent, so
        // this is a no-op on the common case where nothing ever demoted the
        // window in the first place.
        crate::material::apply(&win, crate::material::Mode::Blur);

        // See this function's doc comment, "Why this function never calls
        // `wl_surface.commit()`" — this schedules GTK's own next commit
        // rather than racing it with one of our own.
        win.queue_draw();
    });
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

    /// Pins [`BlurManagerLookup`]'s `Dispatch` impl directly — the
    /// interface-name match that decides whether a `Global` event is the
    /// blur manager at all, and the `(name, version)` capture a bind
    /// depends on to ask for the right object with the right version. This
    /// is the one piece of `apply_blur`'s logic that both
    /// `tests/kde_blur_apply_live.rs` (a real but non-KDE compositor, so
    /// the blur manager interface never appears there) and
    /// `tests/kde_blur_apply.rs` (broadway, where the downcast guard
    /// returns before any registry lookup runs at all) leave completely
    /// unexercised — so it is pinned here instead, needing no real
    /// compositor: a `Dispatch::event` call needs a `&WlRegistry`, a
    /// `&Connection` and a `&QueueHandle`, and all three exist the moment a
    /// `Connection` is constructed and asks for a registry — no bytes ever
    /// need to cross the socket for that, so the other end of this
    /// `UnixStream::pair()` is never read from or written to.
    #[test]
    fn blur_manager_lookup_captures_name_and_version_only_for_the_matching_interface() {
        use std::os::unix::net::UnixStream;

        assert_eq!(
            OrgKdeKwinBlurManager::interface().name,
            "org_kde_kwin_blur_manager",
            "this test's whole premise is pinning the interface name apply_blur's registry \
             lookup matches against — if the generated binding's name ever drifted from the \
             protocol's, this is the assertion meant to catch it"
        );

        let (stream, _unused_peer) = UnixStream::pair()
            .expect("a freshly created, unconnected-to-anything socketpair cannot fail to pair");
        let connection = Connection::from_socket(stream)
            .expect("wrapping a fresh socketpair as a wayland-client Backend cannot fail");
        let event_queue = connection.new_event_queue::<BlurManagerLookup>();
        let qh = event_queue.handle();
        // `get_registry` only allocates a client-side object id and buffers
        // the request; nothing is written to the socket until an explicit
        // `flush`, which this test never calls.
        let registry = connection.display().get_registry(&qh, ());

        let mut state = BlurManagerLookup { global: None };

        // An unrelated interface must never be captured.
        <BlurManagerLookup as Dispatch<WlRegistry, ()>>::event(
            &mut state,
            &registry,
            wl_registry::Event::Global {
                name: 1,
                interface: "wl_compositor".to_string(),
                version: 5,
            },
            &(),
            &connection,
            &qh,
        );
        assert_eq!(
            state.global, None,
            "an unrelated global must never be captured as the blur manager"
        );

        // The blur manager's own interface, with its exact (name, version).
        <BlurManagerLookup as Dispatch<WlRegistry, ()>>::event(
            &mut state,
            &registry,
            wl_registry::Event::Global {
                name: 42,
                interface: OrgKdeKwinBlurManager::interface().name.to_string(),
                version: 7,
            },
            &(),
            &connection,
            &qh,
        );
        assert_eq!(
            state.global,
            Some((42, 7)),
            "the matching interface must capture both its name and its version"
        );
    }
}
