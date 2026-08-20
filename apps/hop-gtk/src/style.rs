//! Installs hop's own [`gtk::CssProvider`] — the wiring that makes Task 1's
//! palette-aware token table and Task 2's resolved `assets/stylesheet.css`
//! actually govern what a running window looks like, rather than sitting in
//! the binary unused. Nothing before this module ever called
//! [`gtk::style_context_add_provider_for_display`] anywhere in this crate —
//! `tokens.rs` and `stylesheet.rs` both say so in their own doc comments,
//! naming this module's job (issue #193's own plan, Task 3) as the one still
//! missing.
//!
//! # Exactly one *ordinary* provider, reloaded, never replaced
//!
//! [`install`] builds a single [`gtk::CssProvider`], adds it once at
//! [`gtk::STYLE_PROVIDER_PRIORITY_APPLICATION`] — above GTK's own built-in
//! theme, deliberately *below* [`gtk::STYLE_PROVIDER_PRIORITY_USER`], the
//! priority a user's own `~/.config/gtk-4.0/gtk.css` loads at. That ordering
//! is not this module's call to make; it is `docs/theme-token-contract.md`'s
//! "Ordinary user-theme surface" section, a normative document this issue
//! does not get to contradict just because a *stronger* provider would be
//! easier to reach for.
//!
//! This section's title says "ordinary" now, not just "one": since issue
//! #200, [`install_locked`] below adds a *second*, independent provider —
//! see "A second provider, above user priority" further down for that one's
//! own reasoning, kept deliberately separate from this section's rather
//! than folded in, because the two providers answer different questions
//! ("what does hop look like" vs "what may a user theme never take away")
//! and conflating their doc comments would blur that they are not two
//! options for the same job. Every claim in *this* section is still about
//! [`install`] and [`install`] alone: one `gtk::CssProvider`, one priority,
//! reloaded in place, never replaced, never duplicated.
//!
//! A colour-scheme change (see "Following the system colour scheme" below)
//! or a motion-setting change (see "Following the reduced-motion setting")
//! does not add a second *ordinary* provider either — [`gtk::CssProvider::load_from_string`]
//! *replaces* whatever a provider was previously loaded with (this is
//! `gtk_css_provider_load_from_data`'s own documented behavior, not an
//! assumption), so reloading the same instance already installed is
//! sufficient. This is also why [`install`] must be called exactly once per
//! process: a second call would install a second, redundant provider at the
//! same priority, doubling every rule's specificity contest for no benefit.
//! ([`install_locked`] must likewise be called exactly once — the identical
//! argument, one priority level up, made in full in its own section below
//! rather than assumed to transfer without being said.)
//!
//! # A second provider, above user priority — the honesty-critical lock
//!
//! Issue #200. [`install_locked`] builds a *second*, independent
//! [`gtk::CssProvider`] — not the same object [`install`] built, reused at a
//! different priority — and adds it at [`STYLE_PROVIDER_PRIORITY_LOCKED`],
//! one above [`gtk::STYLE_PROVIDER_PRIORITY_USER`]. `app.rs`'s
//! `install_stylesheet` calls both, from the same `connect_startup` handler
//! that used to call only [`install`].
//!
//! **Why a second object, not the first one re-added at a second priority.**
//! A single [`gtk::CssProvider`] can only ever hold one loaded stylesheet
//! text at a time — [`gtk::CssProvider::load_from_string`] *replaces* it, as
//! the section above already establishes — so there is no way for one
//! provider instance to carry the *whole* sheet at
//! [`gtk::STYLE_PROVIDER_PRIORITY_APPLICATION`] and *simultaneously* carry
//! only the honesty-critical rules at a second, higher priority; whichever
//! text it last loaded is the only text it has. Two independent provider
//! objects, each attached to the display at its own priority, is the only
//! way GTK's style-provider API expresses "these two rule sets rank
//! differently" at all — [`gtk::style_context_add_provider_for_display`]
//! takes one provider and one priority per call, with no notion of a single
//! provider carrying two.
//!
//! **Why the locked provider's content is a narrow slice, not the whole
//! sheet re-added a second time.** This is the sharper reason "two
//! providers, not one reconfigured" matters, and the brief for this issue
//! is emphatic about it: if [`install_locked`] loaded the *same*, full
//! `stylesheet::resolve` text this second provider's higher priority would
//! win *every* rule against a user theme, not only the honesty-critical
//! ones — silently revoking `docs/theme-token-contract.md`'s "Ordinary
//! user-theme surface" guarantee that a user theme is authoritative
//! *everywhere outside* `.hop-honesty`. [`stylesheet::resolve_locked_block`]
//! is what keeps that from happening: it resolves only the four rules
//! bracketed by `assets/stylesheet.css`'s own
//! `HOP-HONESTY-LOCKED-BLOCK-START`/`-END` sentinel comments — see that
//! function's own doc comment for why that slice, rather than a
//! hand-duplicated second copy of the same four rules, is what this second
//! provider loads. [`install`]'s own provider still carries the *entire*
//! sheet, honesty-critical rules included, at
//! [`gtk::STYLE_PROVIDER_PRIORITY_APPLICATION`] — so a widget wearing
//! `.hop-honesty` still renders correctly even if [`install_locked`] were
//! somehow never called (a defense-in-depth side effect of not deleting
//! those rules from the ordinary sheet, not this module's actual guarantee,
//! which is [`install_locked`] itself).
//!
//! **What it protects, concretely, and what it does not.**
//! `ui::offline_indicator::build` is the first (and, per this issue's own scope,
//! only) widget carrying `.hop-honesty` — its offline text and its per-row
//! "as of HH:MM" stamp are what this provider's opacity and contrast rules
//! now have a real subject to lock. `.hop-honesty .hop-skeleton`'s
//! dimension lock has no widget wearing it yet (the pending-skeleton-rows
//! member is later, separately-scoped work — see
//! `assets/stylesheet.css`'s own "HONESTY-CRITICAL SELECTORS" comment) —
//! its rule is loaded into this provider regardless, inert until then, for
//! the same "already correct, only needs a class applied" reasoning
//! `assets/stylesheet.css`'s own comment already gives for why it was
//! authored before anything used it. This provider never attempts a
//! presence lock of any kind — see `assets/stylesheet.css`'s own "PRESENCE
//! IS NEVER EXPRESSED HERE" section for why that is not this provider's
//! job at all, CSS having no `display`/`visibility` to lock in the first
//! place.
//!
//! **Reload and subscription, generalized rather than duplicated.** Both
//! providers need the identical live behavior the sections below describe
//! (re-resolve and reload on a colour-scheme change, and again on a
//! motion-setting change) — the honesty-critical lock is worthless if it
//! silently reverted to a stale palette's colours the moment the desktop's
//! dark/light toggle flipped, since the contrast guarantee is specifically
//! about *this* palette's tokens remaining legible. Rather than hand-write
//! that subscription logic a second time — a second [`reload`], a second
//! [`follow_colour_scheme`], a second [`follow_motion_setting`], each a
//! near-duplicate of the first and free to quietly drift out of sync with
//! it — [`install`] and [`install_locked`] both funnel through
//! [`install_provider`], parameterized by which of
//! [`crate::stylesheet::resolve`]/[`crate::stylesheet::resolve_locked_block`]
//! to re-resolve with. Both are plain `fn(Palette, Motion) -> String`
//! function pointers (`Copy`, no captured state), not closures, so this
//! adds no runtime indirection beyond an ordinary function call, and no
//! generic type parameter a caller has to reason about — [`install`] and
//! [`install_locked`] each still construct their own, fully independent
//! [`gtk::CssProvider`], at their own priority, with their own signal
//! subscriptions; only the *shape* of "build, guard, reload, attach,
//! subscribe" is shared, never the runtime objects themselves. A future
//! divergence between the two providers' *reload* behavior — say, one
//! needing to consult something the other should not — remains exactly as
//! easy to express as it would be with two hand-duplicated copies: nothing
//! about sharing [`install_provider`] forces the two resolver functions to
//! behave identically, only the surrounding "install it, guard it, keep it
//! live" plumbing.
//!
//! # Why `connect_startup`, not `connect_activate`
//!
//! `app.rs`'s existing `run_interactive` builds its window only on the
//! *first* `activate` (`if let Some(existing) = app.active_window() {
//! existing.present(); return; }` guards every later one — see that
//! function's own body). Piggybacking provider installation on that same
//! guard would work, but it would couple two lifecycles that do not share a
//! reason to be coupled: "build the window once" and "install the
//! stylesheet once" happen to both want "once", not "at the same time for
//! the same reason". `GApplication`'s own "startup" signal already *is* the
//! "exactly once, before the first activate" hook GTK ships for exactly this
//! kind of one-time setup — its default class handler (which is what
//! actually opens a display and calls the underlying `gtk_init`) runs before
//! any user-connected `connect_startup` closure, because "startup" is a
//! `G_SIGNAL_RUN_FIRST` signal: GTK's own class handler fires first, then
//! ordinary (non-`_after`) connections. So by the time [`install`] runs
//! inside a `connect_startup` closure, [`gtk::gdk::Display::default`] is
//! already resolved — confirmed against GTK4's own CSS documentation and the
//! `gtk4-rs` book's own CSS chapter, which uses this identical
//! `connect_startup` shape to load an application stylesheet. `app.rs`
//! calls [`install`] from both `run_interactive`'s and `run_screenshot`'s
//! `connect_startup` handlers — see that module's own comment at each call
//! site for why both, not just one.
//!
//! # Following the system colour scheme
//!
//! [`install`] reads `adw::StyleManager::default()`'s `is-dark` property
//! once, for the initial load, then subscribes to its `notify::dark` signal
//! (`connect_dark_notify`) so a live change — the user flips GNOME's
//! system-wide dark/light toggle while hop is running — re-resolves and
//! reloads the same provider for the other palette. `is-dark`, not
//! `color-scheme`, is what is read: `StyleManager::color_scheme` reports
//! *what the application requested* (`Default`, `ForceLight`,
//! `ForceDark`, ...), which this crate never sets and has no reason to;
//! `is_dark` reports what libadwaita actually *resolved*, after folding in
//! the desktop's own preference — the one bit [`crate::tokens::Palette`]
//! needs.
//!
//! ## Subscription lifetime — a process-wide singleton, not a value this
//! module owns
//!
//! `adw::StyleManager::default()` is libadwaita's own singleton, cached and
//! kept alive internally for the life of the default display (confirmed
//! against libadwaita's own `AdwStyleManager` documentation: "there's a
//! single instance of `AdwStyleManager` associated to each display"). This
//! module never stores the `StyleManager` handle or the returned
//! `SignalHandlerId` anywhere: the [`glib::SignalHandlerId`] a `connect_*`
//! call returns is only ever needed to later *disconnect* that specific
//! handler by hand (`SignalHandlerId::disconnect`), which nothing here ever
//! wants to do for the life of the process — dropping the ID does not
//! disconnect it, it is a plain integer wrapper, not an RAII guard. The
//! closure itself keeps the [`gtk::CssProvider`] it captured alive for as
//! long as the signal connection exists, and the signal connection lives as
//! long as the singleton `StyleManager` object does — the process's own
//! lifetime, since libadwaita never tears that singleton down early. So this
//! neither leaks (nothing is ever allocated and forgotten with no owner —
//! the connection *is* the owner, exactly as intended) nor drops early
//! (there is no local binding whose scope end could sever it).
//!
//! # Following the reduced-motion setting
//!
//! Issue #207's Task 2. [`install`] reads `gtk::Settings::default()`'s
//! `gtk-enable-animations` property once, for the initial load, then
//! subscribes to its `notify::gtk-enable-animations` signal
//! (`connect_gtk_enable_animations_notify`) so a live change — the user
//! flips GNOME's reduced-motion toggle while hop is running — re-resolves
//! and reloads the same provider for the other [`crate::tokens::Motion`]
//! state. This is GTK's own setting, not a `libadwaita` one, mirroring the
//! previous section's shape one level down the stack: `gtk-enable-animations`
//! is what `assets/tokens.css`'s own `@media (prefers-reduced-motion:
//! reduce)` comment already commits to as the source of truth ("GTK drives
//! this from Gtk.Settings:gtk-enable-animations"), and it is the *only*
//! signal this module reads for motion — no portal call, no direct read of
//! `org.gnome.desktop.interface`'s `enable-animations` GSettings key,
//! matching this issue's own brief.
//!
//! `Gtk.Settings`'s own documentation says plainly what makes this a real
//! GObject property with a real change notification, not a static
//! environment read: "On Wayland, the settings are obtained either via a
//! settings portal, or by reading desktop settings from `Gio.Settings`" —
//! GTK itself is what bridges the desktop's reduced-motion preference into
//! this property and its `notify` signal, which is exactly the live
//! subscription this issue's brief is emphatic must exist rather than a
//! startup-only read. No obstacle was found that would justify falling
//! back to one: `connect_gtk_enable_animations_notify` is a real,
//! generated binding on the pinned `gtk4` crate (confirmed directly against
//! `gtk4-0.11.4`'s own `auto/settings.rs`), exercised end to end by
//! `tests/motion_setting.rs`, which drives the property through its own
//! public setter and confirms the installed provider reloads — the same
//! shape `tests/style_colour_scheme.rs` already proves for the palette
//! axis.
//!
//! ## Subscription lifetime — the identical reasoning as the colour scheme
//! above, one level down
//!
//! `gtk::Settings::default()` is GTK's own singleton, "one `GtkSettings`
//! instance per display" per its own documentation (`/usr/share/gir-1.0/
//! Gtk-4.0.gir`'s `Settings` class doc, confirmed directly against this
//! machine's installed GTK), the identical shape `AdwStyleManager`'s own
//! documentation gives one section up. Every argument the previous
//! section's "Subscription lifetime" makes therefore transfers unchanged:
//! this module never stores the `Settings` handle or the returned
//! `SignalHandlerId`, the closure keeps the [`gtk::CssProvider`] it
//! captured alive for as long as the signal connection exists, and the
//! connection lives as long as the singleton `Settings` object does — the
//! process's own lifetime, since GTK never tears that singleton down
//! early. Neither leaks nor drops early, for exactly the reasons already
//! given once above; they are not repeated a second time here.
//!
//! # Guarding parse errors — loud in debug/test, quiet in release
//!
//! [`gtk::CssProvider::connect_parsing_error`] is connected on both of hop's
//! own providers (issue #200 added the second — see "A second provider,
//! above user priority" above), and only on them — a user's own theme loads
//! through a *different* `gtk::CssProvider` GTK constructs internally for
//! `~/.config/gtk-4.0/gtk.css` and the system theme, which this module never
//! touches and never connects to. That is what makes "a parse error in
//! hop's own sheet is a programming error; a parse error in a user's theme
//! must never be fatal" true by construction rather than by a runtime
//! branch: the error sources are physically different objects, and this
//! module's handler only ever hears from the two it connected itself.
//!
//! `cfg!(debug_assertions)` (not a `#[cfg]` attribute — see [`guard_parse_errors`]'s
//! own doc comment for why a runtime `if` on a compile-time constant was
//! chosen over two separately-compiled function bodies) decides which way a
//! caught error goes: `panic!`, naming the section and the underlying
//! `glib::Error`, in every build this crate's own `cargo test -p hop-gtk`
//! and any ordinary (non-`--release`) `cargo build` produce — the exact
//! failure mode that let issue #193 exist unnoticed for as long as it did,
//! now made loud instead of silent. A `--release` build instead prints the
//! same information to stderr and leaves the provider running with whatever
//! it *did* manage to parse — degrading a cosmetic fault rather than
//! crashing a user's launcher over it, per this crate's own brief.
//!
//! Panicking from inside a GTK signal callback is itself worth naming
//! plainly: the callback runs across an `extern "C"` FFI boundary
//! ([`gtk::CssProvider::connect_parsing_error`]'s own trampoline), and Rust
//! has defined the behavior of an unwind reaching a non-`C-unwind` `extern
//! "C"` boundary as an abort since Rust 1.71 — not undefined behavior. The
//! default panic hook still prints the panic message to stderr *before* the
//! unwind begins, so this still satisfies "fail loudly": a clear message,
//! then a deterministic process abort, which is the same shape every other
//! fail-loudly panic in this crate already uses (`tokens.rs`,
//! `stylesheet.rs`), just crossing one more FFI frame to get there.

use crate::stylesheet;
use crate::tokens::{Motion, Palette};

/// One above [`gtk::STYLE_PROVIDER_PRIORITY_USER`] (800) — GTK names no
/// "outranks a user theme" priority of its own; [`STYLE_PROVIDER_PRIORITY_USER`]
/// is the highest tier it names, since GTK's own priority scheme was never
/// designed to let anything out-rank the user. This crate names the one
/// value above it that [`install_locked`] needs, per
/// `docs/theme-token-contract.md`'s "Future enforcement status" section:
/// "install the locked styling above `GTK_STYLE_PROVIDER_PRIORITY_USER`".
/// Any value greater than 800 would satisfy that — GTK compares provider
/// priorities as plain integers, with no reserved gaps or required
/// spacing — and `+ 1` is the smallest one, which is also the clearest to
/// read: "the next tier up from user", not an arbitrary large constant that
/// would invite a reader to wonder what headroom it was leaving for.
///
/// `pub` — not merely an [`install_locked`] implementation detail — so
/// `tests/honesty_locked_provider.rs` can attach its own probe providers at
/// exactly this priority rather than re-deriving `+ 1` a second time
/// somewhere a future change to this constant would not reach.
///
/// [`gtk::STYLE_PROVIDER_PRIORITY_USER`]: gtk::STYLE_PROVIDER_PRIORITY_USER
pub const STYLE_PROVIDER_PRIORITY_LOCKED: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER + 1;

/// Installs hop's *ordinary* stylesheet onto `display` — the full,
/// resolved `assets/stylesheet.css`, at
/// [`gtk::STYLE_PROVIDER_PRIORITY_APPLICATION`] — following every design
/// decision this module's own doc comment explains. Called once per
/// process, from a `connect_startup` handler — see `app.rs` for the two call
/// sites and why both exist.
///
/// Returns the installed [`gtk::CssProvider`] itself, so a caller can hold
/// the exact live instance the colour-scheme/motion-setting subscriptions
/// are reloading. `app.rs`'s own call sites drop it (a colour-scheme or
/// motion-setting change reloads the provider through the display it is
/// already attached to; nothing in production code needs to touch it again
/// after installing it), but a test that wants to prove either runtime hook
/// actually fires needs a handle to the *same* provider to read back with
/// [`gtk::CssProvider::to_str`] after driving a change — see
/// `tests/style_colour_scheme.rs` for the palette axis and
/// `tests/motion_setting.rs` for the motion one.
///
/// See this module's doc comment, "A second provider, above user
/// priority", for [`install_locked`] — the sibling this function does
/// *not* call itself; `app.rs`'s `install_stylesheet` calls both.
pub fn install(display: &gtk::gdk::Display) -> gtk::CssProvider {
    install_provider(
        display,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        stylesheet::resolve,
    )
}

/// Installs the honesty-critical *locked block* onto `display` — issue
/// #200, and this module's doc comment, "A second provider, above user
/// priority", for the full account of what this is and why it is a
/// genuinely separate [`gtk::CssProvider`] rather than [`install`]'s own
/// provider re-added at a second priority. Called once per process,
/// alongside [`install`], from the same `connect_startup` handler —
/// `app.rs`'s `install_stylesheet`.
///
/// Returns the installed provider for the identical reason [`install`]
/// does: nothing in production code needs it back (`app.rs` drops it), but
/// a test proving this provider's own colour-scheme/motion-setting
/// subscriptions actually fire needs a handle to read back with
/// [`gtk::CssProvider::to_str`] — see `tests/honesty_locked_provider.rs`.
pub fn install_locked(display: &gtk::gdk::Display) -> gtk::CssProvider {
    install_provider(
        display,
        STYLE_PROVIDER_PRIORITY_LOCKED,
        stylesheet::resolve_locked_block,
    )
}

/// The shared "build, guard, reload, attach, subscribe" shape both
/// [`install`] and [`install_locked`] follow — see this module's doc
/// comment, "Reload and subscription, generalized rather than duplicated",
/// for why a shared function parameterized by which stylesheet resolver to
/// use was chosen over hand-duplicating [`reload`]/[`follow_colour_scheme`]/
/// [`follow_motion_setting`] a second time.
///
/// `resolve_sheet` is a plain function pointer
/// (`fn(Palette, Motion) -> String`), not a `Fn` closure trait object or an
/// `impl Fn` generic — [`crate::stylesheet::resolve`] and
/// [`crate::stylesheet::resolve_locked_block`] are both already exactly
/// that signature, with no state to capture, so a function pointer is the
/// simplest type that fits; nothing here needs the extra generality (or the
/// extra monomorphized code size) a generic `impl Fn` parameter would add
/// for a caller that only ever passes one of two free functions.
fn install_provider(
    display: &gtk::gdk::Display,
    priority: u32,
    resolve_sheet: fn(Palette, Motion) -> String,
) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    guard_parse_errors(&provider);
    reload(
        &provider,
        resolve_sheet,
        current_palette(),
        current_motion(),
    );

    gtk::style_context_add_provider_for_display(display, &provider, priority);

    follow_colour_scheme(provider.clone(), resolve_sheet);
    follow_motion_setting(provider.clone(), resolve_sheet);
    provider
}

/// The palette to (re)resolve against, read fresh from libadwaita's
/// singleton style manager every time this is called — at startup (before
/// any `notify::dark` has ever fired) and again inside
/// [`follow_motion_setting`]'s own handler, so a motion-setting change
/// reloads the *current* palette rather than whichever one happened to be
/// active when [`install`] first ran. See this module's doc comment,
/// "Following the system colour scheme", for why `is_dark` rather than
/// `color_scheme` is the property read.
fn current_palette() -> Palette {
    palette_for(adw::StyleManager::default().is_dark())
}

/// `AdwStyleManager`'s boolean `is-dark` reading, translated to
/// [`crate::tokens::Palette`] — the one conversion both [`current_palette`]
/// and the `notify::dark` handler installed by [`follow_colour_scheme`]
/// need, kept in one place so the two can never disagree about which way
/// the bit maps.
fn palette_for(is_dark: bool) -> Palette {
    if is_dark {
        Palette::Dark
    } else {
        Palette::Light
    }
}

/// The motion state to (re)resolve against, read fresh from GTK's own
/// `gtk-enable-animations` setting every time this is called — at startup
/// (before any `notify::gtk-enable-animations` has ever fired) and again
/// inside [`follow_colour_scheme`]'s own handler, so a colour-scheme change
/// reloads the *current* motion state rather than whichever one happened
/// to be active when [`install`] first ran. See this module's doc comment,
/// "Following the reduced-motion setting", for the full account of why
/// this is the one and only signal this crate reads for motion, per issue
/// #207's brief and `assets/tokens.css`'s own `@media` comment.
fn current_motion() -> Motion {
    motion_for(gtk_enable_animations())
}

/// `Gtk.Settings:gtk-enable-animations`'s boolean reading, translated to
/// [`crate::tokens::Motion`] — [`current_motion`]'s pairing for
/// [`palette_for`], and the identical shape: one conversion, kept in one
/// place, so [`current_motion`] and the `notify::gtk-enable-animations`
/// handler installed by [`follow_motion_setting`] can never disagree about
/// which way the bit maps.
fn motion_for(enable_animations: bool) -> Motion {
    if enable_animations {
        Motion::Full
    } else {
        Motion::Reduced
    }
}

/// Reads `gtk::Settings::default()`'s `gtk-enable-animations` property —
/// the one call site both [`current_motion`] and [`follow_motion_setting`]'s
/// `notify` handler go through.
///
/// `gtk::Settings::default()` returns `Option<Settings>`, `None` exactly
/// when GTK has no default [`gtk::gdk::Display`] to resolve settings for
/// (confirmed against `gtk4-rs`'s own binding: it wraps
/// `gtk_settings_get_default`, whose C doc says it returns `NULL` under
/// that one condition and no other). [`install`] only ever runs from a
/// `connect_startup` handler — `app.rs`'s `install_stylesheet`, which
/// itself panics if [`gtk::gdk::Display::default`] is `None` at that point
/// — so by the time this function is ever called, a default display (and
/// therefore a default `Settings`) is already guaranteed to exist; a `None`
/// here would mean that invariant broke, which is a programming error worth
/// failing loudly over, the same posture `install_stylesheet` already takes
/// one call site earlier for the identical reason. This is a `panic!`, not
/// an `unwrap`/`expect`, matching this crate's own fail-loudly precedent
/// (`tokens.rs`, `stylesheet.rs`, and this module's own
/// [`guard_parse_errors`]) rather than the bare `Option::expect` this
/// issue's own brief rules out for a fallible runtime value.
fn gtk_enable_animations() -> bool {
    let Some(settings) = gtk::Settings::default() else {
        panic!(
            "hop-gtk: no gtk::Settings available to read gtk-enable-animations from — this can \
             only happen with no default gdk::Display, which app.rs's install_stylesheet \
             already guarantees exists before style::install ever runs"
        );
    };
    settings.is_gtk_enable_animations()
}

/// Re-resolves `resolve_sheet` for `palette` and `motion` and loads it into
/// `provider`, replacing whatever it held before — see this module's doc
/// comment, "Exactly one *ordinary* provider, reloaded, never replaced",
/// for why that replace-in-place behavior is what lets a colour-scheme *or*
/// motion-setting change reuse this same `provider` instance rather than
/// installing a second one. `resolve_sheet` is
/// [`install_provider`]'s own parameter, threaded one call further in —
/// this function does not care, and does not need to, whether it was
/// handed [`crate::stylesheet::resolve`] or
/// [`crate::stylesheet::resolve_locked_block`]; either is "the current
/// stylesheet text for this provider", which is all a reload ever needs.
fn reload(
    provider: &gtk::CssProvider,
    resolve_sheet: fn(Palette, Motion) -> String,
    palette: Palette,
    motion: Motion,
) {
    provider.load_from_string(&resolve_sheet(palette, motion));
}

/// Subscribes `provider` to libadwaita's style manager, so a live
/// colour-scheme change re-resolves (via `resolve_sheet`) and reloads it
/// for the other palette — re-reading [`current_motion`] fresh on every
/// fire, rather than assuming the motion axis is unchanged, so a
/// colour-scheme flip can never silently revert a motion-setting change
/// that happened first. See this module's doc comment, "Subscription
/// lifetime", for why the `StyleManager` handle and the returned
/// `SignalHandlerId` are both deliberately left unstored here rather than
/// threaded back out to a caller that would otherwise have nothing correct
/// to do with either.
fn follow_colour_scheme(provider: gtk::CssProvider, resolve_sheet: fn(Palette, Motion) -> String) {
    let style_manager = adw::StyleManager::default();
    style_manager.connect_dark_notify(move |manager| {
        reload(
            &provider,
            resolve_sheet,
            palette_for(manager.is_dark()),
            current_motion(),
        );
    });
}

/// Subscribes `provider` to `gtk::Settings::default()`, so a live change to
/// `gtk-enable-animations` — GNOME's reduced-motion toggle, per this
/// module's doc comment, "Following the reduced-motion setting" — re-
/// resolves (via `resolve_sheet`) and reloads it for the other motion
/// state. Symmetrical with [`follow_colour_scheme`] in every way that
/// matters: re-reads [`current_palette`] fresh on every fire (so a
/// motion-setting change can never silently revert a colour-scheme change
/// that happened first), and deliberately leaves both the `Settings`
/// handle and the returned `SignalHandlerId` unstored, for the identical
/// "process-wide singleton, not a value this module owns" reasoning this
/// module's doc comment gives for `AdwStyleManager` — see that section for
/// the full argument, and [`gtk_enable_animations`]'s own doc comment for
/// why the `Option` this call site's `gtk::Settings::default()` can return
/// is handled by panicking rather than by a silent fallback.
fn follow_motion_setting(provider: gtk::CssProvider, resolve_sheet: fn(Palette, Motion) -> String) {
    let Some(settings) = gtk::Settings::default() else {
        panic!(
            "hop-gtk: no gtk::Settings available to subscribe to — see \
             gtk_enable_animations's own doc comment for why this is a programming-error panic, \
             not a silent fallback"
        );
    };
    settings.connect_gtk_enable_animations_notify(move |settings| {
        reload(
            &provider,
            resolve_sheet,
            current_palette(),
            motion_for(settings.is_gtk_enable_animations()),
        );
    });
}

/// Connects `provider`'s `parsing-error` signal — see this module's doc
/// comment, "Guarding parse errors", for the full account of why a plain
/// runtime `if cfg!(debug_assertions)` was chosen here over two
/// `#[cfg(...)]`-gated closures: both branches are one line, sharing them
/// under one closure keeps the "which build, which behavior" decision
/// readable as a single sentence instead of two near-duplicate function
/// bodies that could drift apart under future edits, and `cfg!` still
/// costs nothing at runtime — it is a compile-time boolean literal, not a
/// real branch, so the unreached arm is dead code the compiler drops, not
/// a check paid for on every parse error.
fn guard_parse_errors(provider: &gtk::CssProvider) {
    provider.connect_parsing_error(|_provider, section, error| {
        if cfg!(debug_assertions) {
            panic!("hop-gtk: assets/stylesheet.css failed to parse at {section:?}: {error}");
        }
        eprintln!(
            "hop-gtk: assets/stylesheet.css failed to parse at {section:?}: {error} \
             (release build: degrading rather than crashing)"
        );
    });
}
