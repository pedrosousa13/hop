//! Installs hop's own [`gtk::CssProvider`] — the wiring that makes Task 1's
//! palette-aware token table and Task 2's resolved `assets/stylesheet.css`
//! actually govern what a running window looks like, rather than sitting in
//! the binary unused. Nothing before this module ever called
//! [`gtk::style_context_add_provider_for_display`] anywhere in this crate —
//! `tokens.rs` and `stylesheet.rs` both say so in their own doc comments,
//! naming this module's job (issue #193's own plan, Task 3) as the one still
//! missing.
//!
//! # Exactly one provider, reloaded, never replaced
//!
//! [`install`] builds a single [`gtk::CssProvider`], adds it once at
//! [`gtk::STYLE_PROVIDER_PRIORITY_APPLICATION`] — above GTK's own built-in
//! theme, deliberately *below* [`gtk::STYLE_PROVIDER_PRIORITY_USER`], the
//! priority a user's own `~/.config/gtk-4.0/gtk.css` loads at. That ordering
//! is not this module's call to make; it is `docs/theme-token-contract.md`'s
//! "Ordinary user-theme surface" section, a normative document this issue
//! does not get to contradict just because a *stronger* provider would be
//! easier to reach for. A second, above-user-priority provider that *would*
//! outrank a user theme is exactly what `.hop-honesty`'s locks eventually
//! need — and exactly what this module deliberately does not add: nothing
//! in this crate carries the `.hop-honesty` class yet (issue #200), so a
//! second provider today would have no widget to protect and nothing to
//! prove it works.
//!
//! A colour-scheme change (see "Following the system colour scheme" below)
//! or a motion-setting change (see "Following the reduced-motion setting")
//! does not add a second provider either — [`gtk::CssProvider::load_from_string`]
//! *replaces* whatever a provider was previously loaded with (this is
//! `gtk_css_provider_load_from_data`'s own documented behavior, not an
//! assumption), so reloading the same instance already installed is
//! sufficient. This is also why [`install`] must be called exactly once per
//! process: a second call would install a second, redundant provider at the
//! same priority, doubling every rule's specificity contest for no benefit.
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
//! [`gtk::CssProvider::connect_parsing_error`] is connected on hop's own
//! provider, and only on it — a user's own theme loads through a
//! *different* `gtk::CssProvider` GTK constructs internally for
//! `~/.config/gtk-4.0/gtk.css` and the system theme, which this module never
//! touches and never connects to. That is what makes "a parse error in
//! hop's own sheet is a programming error; a parse error in a user's theme
//! must never be fatal" true by construction rather than by a runtime
//! branch: the two error sources are physically different objects, and this
//! module's handler only ever hears from one of them.
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

use crate::tokens::{Motion, Palette};

/// Installs hop's stylesheet onto `display`, following every design
/// decision this module's own doc comment explains. Called once per
/// process, from a `connect_startup` handler — see `app.rs` for the two call
/// sites and why both exist.
///
/// Returns the installed [`gtk::CssProvider`] itself, so a caller can hold
/// the exact live instance `follow_colour_scheme`/`follow_motion_setting`
/// are reloading. `app.rs`'s own call sites drop it (a colour-scheme or
/// motion-setting change reloads the provider through the display it is
/// already attached to; nothing in production code needs to touch it again
/// after installing it), but a test that wants to prove either runtime hook
/// actually fires needs a handle to the *same* provider to read back with
/// [`gtk::CssProvider::to_str`] after driving a change — see
/// `tests/style_colour_scheme.rs` for the palette axis and
/// `tests/motion_setting.rs` for the motion one.
pub fn install(display: &gtk::gdk::Display) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    guard_parse_errors(&provider);
    reload(&provider, current_palette(), current_motion());

    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    follow_colour_scheme(provider.clone());
    follow_motion_setting(provider.clone());
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

/// Re-resolves [`crate::stylesheet::resolve`] for `palette` and `motion`
/// and loads it into `provider`, replacing whatever it held before — see
/// this module's doc comment, "Exactly one provider, reloaded, never
/// replaced", for why that replace-in-place behavior is what lets a
/// colour-scheme *or* motion-setting change reuse this same `provider`
/// instance rather than installing a second one.
fn reload(provider: &gtk::CssProvider, palette: Palette, motion: Motion) {
    provider.load_from_string(&crate::stylesheet::resolve(palette, motion));
}

/// Subscribes `provider` to libadwaita's style manager, so a live
/// colour-scheme change re-resolves and reloads it for the other palette —
/// re-reading [`current_motion`] fresh on every fire, rather than assuming
/// the motion axis is unchanged, so a colour-scheme flip can never silently
/// revert a motion-setting change that happened first. See this module's
/// doc comment, "Subscription lifetime", for why the `StyleManager` handle
/// and the returned `SignalHandlerId` are both deliberately left unstored
/// here rather than threaded back out to a caller that would otherwise
/// have nothing correct to do with either.
fn follow_colour_scheme(provider: gtk::CssProvider) {
    let style_manager = adw::StyleManager::default();
    style_manager.connect_dark_notify(move |manager| {
        reload(&provider, palette_for(manager.is_dark()), current_motion());
    });
}

/// Subscribes `provider` to `gtk::Settings::default()`, so a live change to
/// `gtk-enable-animations` — GNOME's reduced-motion toggle, per this
/// module's doc comment, "Following the reduced-motion setting" — re-
/// resolves and reloads it for the other motion state. Symmetrical with
/// [`follow_colour_scheme`] in every way that matters: re-reads
/// [`current_palette`] fresh on every fire (so a motion-setting change can
/// never silently revert a colour-scheme change that happened first), and
/// deliberately leaves both the `Settings` handle and the returned
/// `SignalHandlerId` unstored, for the identical "process-wide singleton,
/// not a value this module owns" reasoning this module's doc comment gives
/// for `AdwStyleManager` — see that section for the full argument, and
/// [`gtk_enable_animations`]'s own doc comment for why the `Option` this
/// call site's `gtk::Settings::default()` can return is handled by
/// panicking rather than by a silent fallback.
fn follow_motion_setting(provider: gtk::CssProvider) {
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
