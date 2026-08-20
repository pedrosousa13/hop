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

use crate::tokens::Palette;

/// Installs hop's stylesheet onto `display`, following every design
/// decision this module's own doc comment explains. Called once per
/// process, from a `connect_startup` handler — see `app.rs` for the two call
/// sites and why both exist.
pub fn install(display: &gtk::gdk::Display) {
    let provider = gtk::CssProvider::new();
    guard_parse_errors(&provider);
    reload(&provider, initial_palette());

    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    follow_colour_scheme(provider);
}

/// The palette to load at startup, before any `notify::dark` has ever
/// fired — see this module's doc comment, "Following the system colour
/// scheme", for why `is_dark` rather than `color_scheme` is the property
/// read.
fn initial_palette() -> Palette {
    palette_for(adw::StyleManager::default().is_dark())
}

/// `AdwStyleManager`'s boolean `is-dark` reading, translated to
/// [`crate::tokens::Palette`] — the one conversion both [`initial_palette`]
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

/// Re-resolves [`crate::stylesheet::resolve`] for `palette` and loads it
/// into `provider`, replacing whatever it held before — see this module's
/// doc comment, "Exactly one provider, reloaded, never replaced", for why
/// that replace-in-place behavior is what lets a colour-scheme change reuse
/// this same `provider` instance rather than installing a second one.
fn reload(provider: &gtk::CssProvider, palette: Palette) {
    provider.load_from_string(&crate::stylesheet::resolve(palette));
}

/// Subscribes `provider` to libadwaita's style manager, so a live
/// colour-scheme change re-resolves and reloads it for the other palette.
/// See this module's doc comment, "Subscription lifetime", for why the
/// `StyleManager` handle and the returned `SignalHandlerId` are both
/// deliberately left unstored here rather than threaded back out to a
/// caller that would otherwise have nothing correct to do with either.
fn follow_colour_scheme(provider: gtk::CssProvider) {
    let style_manager = adw::StyleManager::default();
    style_manager.connect_dark_notify(move |manager| {
        reload(&provider, palette_for(manager.is_dark()));
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
