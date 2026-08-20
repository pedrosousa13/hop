//! The action vocabulary a key press or a mouse click can mean, the
//! `[keymap]` section of `config.toml` that binds each one, and the lookup
//! a handler consults to turn a raw GDK key press into one of those
//! actions.
//!
//! # Why this exists at all — issue #182
//!
//! §8 of the design spec's 2026-07-31 amendment: "The whole keymap is
//! configurable, not just the menu key." `ui::window` used to have no key
//! handling whatsoever beyond `GtkEntry`'s own built-in `activate` signal
//! (Enter, by GTK's definition, not by any comparison that module made), so
//! there was nothing to "convert" — the job here is building every handler
//! data-driven from the start, so a later handler never gets written the
//! hardcoded way in the first place. `ui::window` owns *dispatch* — turning
//! a resolved [`Action`] into an effect on the window — and asks a
//! [`Keymap`] for the resolution; nothing in `ui::window` ever compares a
//! `gdk::Key` against a literal.
//!
//! # The config schema chosen for a binding
//!
//! `config.toml` gets one new table, `[keymap]`, whose keys are this
//! module's own action names ([`Action::config_key`]) and whose values are
//! strings spelling a binding: an optional run of modifier names joined by
//! `+` (`ctrl`, `shift`, `alt`, `super`; case-insensitive), then the key
//! itself, named the way GDK's own `gdk_keyval_from_name` table names it —
//! `"Up"`, `"Page_Down"`, `"Return"`, `"ctrl+k"`. This is the same
//! vocabulary [`Action::default_spelling`] writes every default in, so a
//! user rebinding one action can read this module's own defaults table (or
//! a documented example config built from it) and know every spelling in it
//! already parses. A flat string, rather than a nested `{ key = "...",
//! mods = [...] }` table, was chosen because §8's defaults carry at most one
//! key and (today) no modifiers at all — a structured value would spend a
//! table on generality nothing here exercises yet, the same "don't build
//! structure a real consumer hasn't asked for" call `ui::view`'s own module
//! doc makes about its dispatch container.
//!
//! Two things this schema deliberately does *not* do, both out of scope per
//! the issue: it has no notion of a *chord* (a binding needing two key
//! presses in sequence) — nothing in §8's default list needs one — and it
//! does not detect two actions bound to the same key. Conflict detection is
//! named explicitly as M6's own acceptance criterion (alongside the
//! settings-window capture widget), not this issue's; a `[keymap]` table
//! that rebinds two actions to the same key here simply means whichever
//! action's entry is applied last during parsing wins the lookup for that
//! key — silent in that one narrow sense, but not the silence criterion 4
//! guards against, which is about a binding nobody could resolve into
//! *any* action at all, not about two actions resolving to the one a user
//! happened to write last.
//!
//! # A rebound printable key can make that character unreachable
//!
//! Nothing in this schema stops a user from writing, say, `navigate_down =
//! "j"`. Once that binding exists, `ui::window`'s window-level
//! `EventControllerKey` (attached in `PropagationPhase::Capture` — see that
//! module's own doc comment for the full argument for why Capture) claims
//! every `j` key press before the query entry's own text-input handling
//! ever sees it, and returns `glib::Propagation::Stop`. The letter `j`
//! becomes unreachable to type into the query for as long as that binding
//! stands — there is no scoping by focus that exempts the entry while it
//! holds keyboard focus, because Capture runs *before* GTK resolves which
//! widget even has focus for the purposes of its own default key handling;
//! by design, this crate's controller sees the key first, indifferent to
//! what has focus.
//!
//! No §8 default triggers this: every default spelling
//! ([`Action::default_spelling`]) is a non-printable key (an arrow, Page
//! Up/Down, Home, End, Return, Tab, Escape, Menu) that a query never needs
//! to type, so the hazard is inert until a user's own `[keymap]` table
//! introduces a printable one. This module's own test suite demonstrates it
//! directly, rather than only describing it: `keymap::tests::a_rebound_action_answers_to_its_new_key_and_no_longer_its_old_one`
//! rebinds `navigate_down` to `"j"`, and the same lookup that proves `j` now
//! resolves to `NavigateDown` is exactly what would stop that letter
//! reaching a query's text.
//!
//! This is deliberately **not** guarded against here — no refusal, no
//! automatic focus-scoping. A binding that fights the query entry is a
//! *conflict*, between an action's key and the entry's own use of the
//! identical character, and conflict detection is named explicitly out of
//! scope for this issue: "Out of scope, per the issue: the settings-window
//! capture widget and conflict detection (M6)." Refusing printable-key
//! bindings, or scoping this controller by focus so the entry gets first
//! refusal on a character it might need, would both be this slice deciding
//! a policy M6 owns. A user who rebinds an action to a printable key gets
//! exactly what they asked for — that character intercepted, everywhere,
//! unconditionally — with no warning about it anywhere in this crate until
//! M6 gives it a place to warn from.
//!
//! # Refusal, and what it means for startup — criterion 4
//!
//! "An unparseable or unknown binding is refused with a message naming it,
//! rather than silently ignored or silently defaulted." Every
//! [`KeymapError`] variant's `Display` names the config path and, for the
//! two binding-shaped refusals, the offending action and the exact spelling
//! that failed — [`KeymapError::UnknownAction`] for a `[keymap]` key that
//! matches no [`Action`], [`KeymapError::UnparseableBinding`] for a value
//! that is not a string, or a string [`Binding::parse`] cannot turn into a
//! key plus modifiers. Neither ever falls back to that action's default;
//! [`Keymap::from_path`] returns the error instead of returning a
//! [`Keymap`] at all, exactly the "refuse the whole load" shape
//! `hopd::config::Config::from_path` already takes toward a bad
//! `max_results` — see that function's own doc comment for the identical
//! argument made about a sibling config.
//!
//! What refusal does to `hop-gtk` itself is `app::run`'s call, not this
//! module's — but the posture is decided here because the alternative
//! (start anyway, with an implicit complaint logged somewhere) is exactly
//! the silent-default failure mode criterion 4 rules out one level up: a
//! keymap nobody can see failed to load, running with defaults a user's own
//! `[keymap]` table disagrees with, is indistinguishable from a keymap that
//! loaded correctly until the moment a rebound key does not do what the
//! file says it should. `app::run` refuses to start `hop-gtk` on a
//! [`KeymapError`], printing it to stderr and exiting non-zero — the same
//! shape `hopd::run` already takes toward its own `ConfigError` ("a
//! malformed config must refuse to start the daemon before anything binds a
//! socket"), and the same shape `app::run` already takes toward a
//! `hop_protocol::socket::SocketPathError` two lines above where the keymap
//! load happens. A user who writes an unparseable `config.toml` finds out
//! immediately, from the terminal that launched `hop-gtk`, rather than
//! discovering it later as "my rebound key does nothing" with no way to
//! tell that from "I made a typo".
//!
//! # Path resolution and the hazard-aware read
//!
//! [`Keymap::load`] resolves `$XDG_CONFIG_HOME/hop/config.toml` (falling
//! back to `$HOME/.config`), duplicating the small, non-hazardous part of
//! `hopd::config::Config::load`'s own path derivation rather than sharing
//! it — D1 of the plan this issue implements is explicit that each binary
//! parses the sections of this file it cares about, and the path
//! derivation is inseparable from that per-binary schema (this module's own
//! `CONFIG_DIR_NAME`/`CONFIG_FILE_NAME` constants, not `hopd`'s private
//! ones, which this crate cannot reach anyway). What *is* shared is the
//! part that is genuinely hazardous to get wrong twice:
//! [`hop_protocol::config_file::read`] opens with `O_NONBLOCK` so a FIFO at
//! this path cannot block `hop-gtk`'s startup, classifies the descriptor
//! rather than the path so a directory, device, or socket is refused rather
//! than misread, and bounds the read at [`MAX_KEYMAP_BYTES`] so an endless
//! device can never be read to completion. See that function's own module
//! doc for the full case; this module only supplies the path and its own
//! byte cap, exactly as `hopd::config::Config::from_path` supplies its own.
//!
//! # Why refusal messages here use plain `Path::display`, not `escape_path`
//!
//! `hopd::config::ConfigError` runs every path it displays through
//! `hop_core::sanitize::escape_path`, because that path is
//! `XDG_CONFIG_HOME`-derived (or a symlink target it follows) and therefore
//! attacker-influenceable. The identical hazard applies to the path this
//! module's own errors name — it is resolved by the identical derivation —
//! but `hop-gtk` does not depend on `hop-core` today (see
//! `apps/hop-gtk/Cargo.toml`: `hop-protocol` is its only in-repo dependency
//! besides GTK itself) and has no escaping discipline anywhere else in this
//! crate to stay consistent with. Adding a dependency on `hop-core` purely
//! to escape one family of error messages, in a crate whose every other
//! user-visible string (`ipc`'s `ConnectFailed`, `handle_outcome`'s
//! `couldn't open {url}`) already interpolates paths and text unescaped, would
//! fix this one call site while leaving the actual gap — this crate's
//! general lack of the discipline — exactly as open as it was. This mirrors
//! [`hop_protocol::config_file`]'s own module doc, which reasons through the
//! identical trade-off for the exact same crate boundary and reaches the
//! same conclusion: the discipline is not dropped, only left for whichever
//! issue gives `hop-gtk` a reason to adopt it everywhere at once.
//!
//! # The byte cap
//!
//! See [`MAX_KEYMAP_BYTES`] for the arithmetic. In short: this read bounds
//! the *file's* bytes, not just the `[keymap]` table's, because
//! [`hop_protocol::config_file::read`] hands back everything up to the cap
//! before this module ever looks for its own section — `hopd`'s two scalar
//! keys count against this budget exactly as much as this module's own
//! table does, the moment both live in the one file this crate and `hopd`
//! both open.

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use gtk::gdk;
use thiserror::Error;

/// The environment variable naming the config directory root — named once so
/// its spelling appears in exactly one place, including every error message
/// that names it. Not shared with `hopd::config`'s identical constant: that
/// one is private to `hopd`'s own crate, and duplicating an `&str` costs
/// nothing to keep independent per D1's "each binary parses its own
/// sections" rule.
const XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";

/// The fallback base for the config directory when `XDG_CONFIG_HOME` is
/// unset, per the XDG Base Directory spec: `$HOME/.config`.
const HOME: &str = "HOME";

/// The directory name under the config base every binary that reads this
/// file agrees on.
const CONFIG_DIR_NAME: &str = "hop";

/// The file name inside that directory.
const CONFIG_FILE_NAME: &str = "config.toml";

/// A single `[keymap]` entry's generous line budget, in bytes: the longest
/// config key this module defines (`secondary_action` or `complete_prefix`,
/// 16 bytes) plus `" = "`, a modifier-qualified spelling
/// (`"ctrl+shift+Page_Down"` comfortably fits in 32 bytes), room for a
/// trailing `# ...` comment about as long again, and the newline. Rounded
/// well past what that adds up to, matching the reasoning
/// `hopd::config::CONFIG_KEY_LINE_BYTES` (also `128`) uses for the
/// identical shape of budget on its own, shorter keys.
const KEYMAP_KEY_LINE_BYTES: u64 = 128;

/// Headroom, in key-lines, for the whole file [`hop_protocol::config_file::read`]
/// hands back — not just this module's own `[keymap]` table. The file this
/// reads is the one `hopd` also reads: its two scalar keys (`max_results`,
/// `max_term_chars`) sit in the same bytes this module's read has to pass
/// before ever finding `[keymap]`, exactly as this module's ~10-entry table
/// sits in the bytes `hopd`'s own read has to pass to find its two. A user
/// who writes out every documented key both binaries define — 2 + 10 = 12 —
/// is writing an ordinary, invited config, not an abusive one; pricing eight
/// times that count (the same multiplier `hopd::config::MAX_CONFIG_KEYS`'s
/// own doc comment uses, for the identical "never refuse an invited config"
/// reason) gives `8 * 12 = 96` key-lines of headroom before the cap is even
/// a question.
const MAX_KEYMAP_KEYS: u64 = 96;

/// Budget, in bytes, for the prose this repo's own commenting style puts
/// around a hand-written config — see `hopd::config::CONFIG_COMMENT_BUDGET_BYTES`,
/// which this mirrors exactly (same value, same reasoning: a config
/// documented the way this file documents itself runs to kilobytes before a
/// single extra key, and 8 KiB covers that comfortably without being
/// remotely "unbounded").
const KEYMAP_COMMENT_BUDGET_BYTES: u64 = 8 * 1024;

/// The byte ceiling this module passes to [`hop_protocol::config_file::read`].
/// See [`MAX_KEYMAP_KEYS`] and [`KEYMAP_COMMENT_BUDGET_BYTES`] for the two
/// halves of the arithmetic; the total, `128 * 96 + 8192` = 20 KiB, is
/// nowhere near enough to trouble memory even fully buffered, and nowhere
/// near small enough to reject a config that sets both of `hopd`'s knobs and
/// writes out this module's full default `[keymap]` table with a paragraph
/// of commentary above every key.
const MAX_KEYMAP_BYTES: u64 = KEYMAP_KEY_LINE_BYTES * MAX_KEYMAP_KEYS + KEYMAP_COMMENT_BUDGET_BYTES;

/// Every action a key press (or, for [`Action::Activate`], a mouse click on
/// a result row) can mean — every §8 default: six for list navigation, one
/// default action, a secondary-action menu key, prefix completion, and
/// dismiss. This is the *complete* vocabulary the design spec names for
/// this slice, not a subset — D4 of the plan this issue implements requires
/// [`Action::SecondaryAction`] and [`Action::CompletePrefix`] to exist and
/// be bound even though `hop-gtk` has no secondary-action menu or prefix
/// completer to run yet (`ui::window`'s own handlers for those two say so
/// in place). Leaving either out of this enum now would be exactly the
/// hardcoded-handler retrofit this issue exists to prevent: a later issue
/// building the menu or the completer would have to touch this module's
/// schema and parsing *and* write the feature, instead of only the feature.
///
/// # A different `Action` from `hop_protocol::item::Action`
///
/// `CONTEXT.md`'s glossary already defines **Action** as a domain term:
/// "something you can do to an item… An item's default action is the one
/// Enter runs." That is [`hop_protocol::item::Action`] — a wire type
/// naming what a *provider* offers to do with one item (open, focus, copy,
/// run, close a window, …), carried in [`hop_protocol::Item::actions`] and
/// picked by id.
///
/// This `Action` is a different, unrelated vocabulary: the frontend's own
/// *key* action vocabulary, naming what a key press or a click *means* at
/// the UI level (move the selection, dismiss the window, …), never
/// serialized, never seen by `hopd` or a provider. The two meet at exactly
/// one point — [`Action::Activate`] is the keymap action whose effect is to
/// *run* whichever `hop_protocol::item::Action` the selected item names as
/// its default — and that is the one place in this module's own doc
/// comments below where both types are named explicitly, rather than
/// leaning on the word "action" to carry both meanings at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Move the list selection one row toward the start.
    NavigateUp,
    /// Move the list selection one row toward the end.
    NavigateDown,
    /// Move the list selection one page toward the start.
    PageUp,
    /// Move the list selection one page toward the end.
    PageDown,
    /// Move the list selection to the first row.
    Home,
    /// Move the list selection to the last row.
    End,
    /// Runs the selected item's default action — [`hop_protocol::Item::default_action`]'s
    /// `ActionId`, naming one of the item's own [`hop_protocol::item::Action`]s,
    /// the wire-typed "what to do with this item" `CONTEXT.md`'s **Action**
    /// glossary entry describes, distinct from this enum (see this enum's
    /// own doc comment, "A different `Action` from `hop_protocol::item::Action`").
    /// Bound to Enter by default, and also the effect a mouse click on a
    /// row produces (D5 of the plan this issue implements): both routes
    /// resolve to this one keymap `Action` rather than to two independent
    /// code paths that happen to agree today.
    Activate,
    /// Open the secondary-action menu for the selected item. Bound and
    /// dispatched by this module and `ui::window`; the menu itself does not
    /// exist yet in `hop-gtk` — see
    /// [`ui::window::HopWindow::open_secondary_action_menu`] for the honest
    /// account of what is and is not built.
    SecondaryAction,
    /// Complete the query against the longest shared prefix among the
    /// current results. Bound and dispatched; the completer itself does not
    /// exist yet — see
    /// [`ui::window::HopWindow::complete_prefix`].
    CompletePrefix,
    /// Dismiss the window without running anything.
    Dismiss,
}

impl Action {
    /// Every variant, in the order [`Keymap::defaults`] builds them —
    /// walked wherever this module needs "all of them" rather than letting
    /// each call site re-enumerate the variants by hand and risk one
    /// silently missing a future addition.
    const ALL: [Action; 10] = [
        Action::NavigateUp,
        Action::NavigateDown,
        Action::PageUp,
        Action::PageDown,
        Action::Home,
        Action::End,
        Action::Activate,
        Action::SecondaryAction,
        Action::CompletePrefix,
        Action::Dismiss,
    ];

    /// This action's spelling as a `[keymap]` table key in `config.toml`.
    /// `snake_case`, matching every other key this repo's TOML schemas use
    /// (`max_results`, `max_term_chars` in `hopd::config`).
    fn config_key(self) -> &'static str {
        match self {
            Action::NavigateUp => "navigate_up",
            Action::NavigateDown => "navigate_down",
            Action::PageUp => "page_up",
            Action::PageDown => "page_down",
            Action::Home => "home",
            Action::End => "end",
            Action::Activate => "activate",
            Action::SecondaryAction => "secondary_action",
            Action::CompletePrefix => "complete_prefix",
            Action::Dismiss => "dismiss",
        }
    }

    /// This action's §8 default binding, spelled in the same grammar
    /// [`Binding::parse`] accepts from a user's own `config.toml` — see this
    /// module's doc comment, "The config schema chosen for a binding". Data
    /// [`Keymap::defaults`] parses at load time, not a value any handler
    /// reads directly — see this module's top doc comment.
    fn default_spelling(self) -> &'static str {
        match self {
            Action::NavigateUp => "Up",
            Action::NavigateDown => "Down",
            Action::PageUp => "Page_Up",
            Action::PageDown => "Page_Down",
            Action::Home => "Home",
            Action::End => "End",
            Action::Activate => "Return",
            // The X11/GDK keysym most keyboards with a dedicated
            // context-menu key report — the same key most desktop
            // environments already bind to "open the context menu for
            // whatever has focus", so this default asks nothing new of a
            // keyboard that has the key at all.
            Action::SecondaryAction => "Menu",
            Action::CompletePrefix => "Tab",
            Action::Dismiss => "Escape",
        }
    }

    /// The reverse of [`Action::config_key`]: which action (if any) a
    /// `[keymap]` table key names. `None` is what makes
    /// [`KeymapError::UnknownAction`] possible — a key nothing here
    /// recognizes.
    fn from_config_key(name: &str) -> Option<Action> {
        Action::ALL
            .into_iter()
            .find(|action| action.config_key() == name)
    }
}

/// One parsed binding: a GDK key plus the modifiers that must be held with
/// it. Never constructed with a spelling that failed to parse —
/// [`Binding::parse`] is the only constructor, and it returns `None` for
/// anything it cannot turn into both halves.
///
/// `pub`, as of issue #197, so [`Keymap::binding_for`]'s answer can be held
/// by a caller outside this module — `ui::row`'s action hint, in this
/// issue's phase B. Both fields stay private, and the only constructor
/// ([`Binding::parse`]) stays module-private too, so nothing outside this
/// module can build a `Binding` `Binding::parse` itself would have refused.
/// A caller outside this module gets an opaque, already-valid handle: a
/// value to hold and to format through [`Binding`]'s [`fmt::Display`] impl,
/// never something to pattern-match or reconstruct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    key: gdk::Key,
    modifiers: gdk::ModifierType,
}

impl Binding {
    /// Parses one `config.toml` binding spelling — see this module's doc
    /// comment, "The config schema chosen for a binding", for the grammar.
    /// `None` covers every way a spelling can fail: an empty key part (a
    /// trailing or lone `+`), a modifier word this module does not
    /// recognize, or a key name [`gdk::Key::from_name`]'s own table does not
    /// know. [`Keymap::parse_table`] turns a `None` here into a
    /// [`KeymapError::UnparseableBinding`] naming both the action and the
    /// exact spelling that failed — never a silent fallback to that
    /// action's default.
    fn parse(spelling: &str) -> Option<Binding> {
        let mut parts: Vec<&str> = spelling.split('+').map(str::trim).collect();
        let key_name = parts.pop()?;
        if key_name.is_empty() {
            return None;
        }

        let mut modifiers = gdk::ModifierType::empty();
        for part in parts {
            let bit = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => gdk::ModifierType::CONTROL_MASK,
                "shift" => gdk::ModifierType::SHIFT_MASK,
                "alt" => gdk::ModifierType::ALT_MASK,
                "super" => gdk::ModifierType::SUPER_MASK,
                _ => return None,
            };
            modifiers |= bit;
        }

        let key = gdk::Key::from_name(key_name)?;
        Some(Binding { key, modifiers })
    }
}

/// How a [`Binding`] is spelled back to a user — the row action hint's key
/// glyph (issue #197's phase A; the widget that shows it is phase B's).
/// This is deliberately a separate function from [`Keymap::binding_for`]
/// itself: `assets/tokens.css` already pairs `--hop-text-hint-label` with
/// `--hop-text-hint-key` because the hint's item-supplied label and its key
/// glyph are two typographic elements with two separate rules, and a
/// reverse lookup that answered with one glued-together string (say,
/// `"Enter · Open"`) would make that pairing impossible for whichever
/// widget renders the hint to apply. [`Keymap::binding_for`] hands back the
/// [`Binding`] itself; this `Display` impl is the one place that turns it
/// into text, so every call site renders it identically instead of each
/// inventing its own spelling.
///
/// # The convention
///
/// **Modifiers first, key last, `+`-joined** — `Ctrl+K`, not `K+Ctrl` and
/// not a bare `⌃K`. Three sub-rules, each picked over an alternative for a
/// stated reason:
///
/// - **Words, not symbols, for modifiers.** `Ctrl`, `Shift`, `Alt`, `Super`
///   — not `⌃ ⇧ ⌥ ⌘`. Those four glyphs are Apple's own iconography for keys
///   this crate does not target — §8's spec is written for a GNOME/Linux
///   desktop, and `Cargo.toml` pins this crate to GTK 4.14 / libadwaita 1.5
///   for that platform specifically. GNOME's own apps spell modifiers as
///   words in their own accelerator UI, and a word needs no special font to
///   render, unlike `⌘`, which several common UI fonts simply have no glyph
///   for.
/// - **`Ctrl`, never `Control`.** [`Binding::parse`] accepts both spellings
///   in `config.toml` (see this module's doc comment, "The config schema
///   chosen for a binding") because a user typing a config value should not
///   have to remember which one this module picked; display has no such
///   audience-of-one-typing-it-once constraint, so it picks the shorter of
///   the two, matching the word length of `Shift`/`Alt`/`Super` rather than
///   standing out as the one long word among four short ones.
/// - **Fixed order — Ctrl, Shift, Alt, Super — regardless of the order a
///   user wrote them in `config.toml`.** [`Binding::parse`] folds every
///   modifier into one `gdk::ModifierType` bitmask, so the order a user
///   typed them in is already gone by the time this function ever sees a
///   `Binding` — there is no "as-written" order left to preserve, only a
///   choice of which fixed order to impose. This picks the same order
///   [`Binding::parse`]'s own `match` already lists the four modifier words
///   in, rather than inventing a second, unrelated order for display to
///   disagree with for no reason.
///
/// **Non-printable keys get a short, familiar rendering, not GDK's own
/// keysym spelling.** [`gdk::Key::name`] is queried first — the same table
/// [`Binding::parse`] itself reads from, so anything that parsed also
/// names — and then translated by [`key_display`]:
///
/// - `Return` → `Enter`, `Escape` → `Esc`: GDK's own keysym names for these
///   two are accurate but not how either key is normally labeled in
///   run-of-the-mill desktop UI copy.
/// - The four arrows → `↑ ↓ ← →`: GDK spells these `Up`/`Down`/`Left`/`Right`,
///   indistinguishable in plain text from `Home`/`End`/`Tab`; the glyphs are
///   the one place this convention departs from "words, not symbols" above,
///   because an arrow *is* a symbol first and a word only second — no
///   reader parses "Up" as fast as ↑ in a dense two-word hint chip.
/// - `Page_Up` / `Page_Down` → `Page Up` / `Page Down`: GDK's underscore is
///   config-grammar punctuation ([`Binding::parse`]'s own multi-word key
///   names use it because TOML values are plain strings with no natural word
///   break), not something a reader should see.
/// - `Home`, `End`, `Tab`, `Menu` (the remaining §8 defaults) are already
///   the exact word a UI would show, so they pass through unchanged.
/// - Anything else this table does not name — every other key a user's own
///   `config.toml` could rebind an action to — falls back to GDK's own
///   keysym name with underscores turned into spaces: not a polished label
///   for every one of GDK's hundreds of keysyms, but always something
///   readable, and never a panic. A single-character keysym name (every
///   letter and digit — none of §8's own ten defaults, but a rebinding
///   could use one) is upper-cased, `k` → `K`, matching how a shortcut is
///   conventionally written even though the physical key is unshifted.
///
/// # Why not `gtk::accelerator_get_label`
///
/// GTK already ships a function that does something like this
/// (`gtk_accelerator_get_label`, bound as `gtk::accelerator_get_label`). It
/// was deliberately not used here: its output is locale-translated (GTK
/// looks the modifier names up through its own gettext catalog) and
/// platform-conditional (it renders Apple's symbol glyphs when GTK detects a
/// macOS-style keyboard), so the same binding can render different text on
/// two machines running the identical `hop-gtk` binary — precisely the
/// "different answer on two runs" defect issue #197's brief calls out for
/// the *lookup* half, extended here to the *display* half. It would also be
/// the first call in this module requiring GTK to be initialized: every
/// test in this file today runs under a plain `cargo test`, with no
/// `gtk::init()` and no display connection (see the doc comment on
/// `defaults_cover_every_action_with_its_documented_key` for why that
/// matters), and this convention keeps that property.
impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MODIFIER_WORDS: [(gdk::ModifierType, &str); 4] = [
            (gdk::ModifierType::CONTROL_MASK, "Ctrl"),
            (gdk::ModifierType::SHIFT_MASK, "Shift"),
            (gdk::ModifierType::ALT_MASK, "Alt"),
            (gdk::ModifierType::SUPER_MASK, "Super"),
        ];

        for (bit, word) in MODIFIER_WORDS {
            if self.modifiers.contains(bit) {
                write!(f, "{word}+")?;
            }
        }

        write!(f, "{}", key_display(self.key))
    }
}

/// The non-modifier half of [`Binding`]'s [`fmt::Display`] convention — see
/// that impl's own doc comment for the full rationale. A free function,
/// not a `Binding` method, because it operates on a bare [`gdk::Key`] with
/// no modifiers in scope, matching the shape [`gdk::Key::name`] itself has.
fn key_display(key: gdk::Key) -> String {
    /// GDK's own keysym spelling → this convention's display spelling, for
    /// every §8 default whose GDK name reads worse than a short, familiar
    /// alternative. Checked before the generic fallback below.
    const NAMED: [(&str, &str); 6] = [
        ("Return", "Enter"),
        ("Escape", "Esc"),
        ("Up", "↑"),
        ("Down", "↓"),
        ("Left", "←"),
        ("Right", "→"),
    ];

    let Some(name) = key.name() else {
        // Unreachable for any `Binding` this module actually constructs —
        // `Binding::parse` only ever succeeds via `gdk::Key::from_name`,
        // whose result names itself by definition — but `gdk::Key::name`'s
        // own signature returns `Option`, and this function does not get to
        // assume its caller can only ever hold a `Binding` this module
        // built (see `Binding`'s own doc comment: it is `pub` precisely so
        // outside code can hold one). A readable, non-panicking fallback
        // costs nothing.
        return format!("Key({key:?})");
    };
    let name = name.as_str();

    if let Some((_, glyph)) = NAMED.iter().find(|(spelling, _)| *spelling == name) {
        return (*glyph).to_string();
    }

    let mut chars = name.chars();
    if let (Some(only), None) = (chars.next(), chars.next()) {
        return only.to_ascii_uppercase().to_string();
    }

    name.replace('_', " ")
}

/// Modifier bits [`Keymap::lookup`] treats as meaningful, out of everything
/// GDK can set in a key event's state. Deliberately excludes `LOCK_MASK`
/// (Caps Lock's *toggle* state, not a key being held, and irrelevant to
/// every §8 binding — none of them is letter-cased) and the five
/// `BUTTON{1..5}_MASK` bits (mouse buttons already down during the key
/// press, meaningless to a keyboard binding).
///
/// Without this mask, a binding parsed with no modifiers at all — the
/// ordinary case, since none of §8's defaults uses one — would stop
/// matching the instant Caps Lock was toggled on, because GDK ORs
/// `LOCK_MASK` into every key event's state for as long as the lock is
/// latched, regardless of which key was pressed. That would be an easy,
/// surprising way for this issue's own rebinding behavior to work in
/// development and fail for a user (or a CI runner) with Caps Lock on.
fn relevant_modifiers() -> gdk::ModifierType {
    gdk::ModifierType::SHIFT_MASK
        | gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK
        | gdk::ModifierType::HYPER_MASK
}

/// Every way loading a keymap can be refused. Nothing in here is a silent
/// fallback to defaults — see this module's doc comment, "Refusal, and what
/// it means for startup", for the full argument and for what `app::run`
/// does with one of these.
///
/// Every variant's `path` is formatted with plain [`Path::display`] rather
/// than an escaping routine — see this module's doc comment, "Why refusal
/// messages here use plain `Path::display`, not `escape_path`", for why.
#[derive(Debug, Error)]
pub enum KeymapError {
    /// Neither `XDG_CONFIG_HOME` nor `HOME` is set, so no config path can be
    /// derived — the same refusal `hopd::config::ConfigError::MissingHome`
    /// makes for the identical situation on the identical file.
    #[error("neither {XDG_CONFIG_HOME} nor {HOME} is set; cannot locate a config directory")]
    MissingHome,

    /// The config file exists but could not be turned into text — a
    /// permission error, a descriptor `fstat` could not classify, non-UTF-8
    /// bytes, or (per
    /// [`hop_protocol::config_file::ConfigFileError::Read`]'s own doc
    /// comment) a Unix domain socket at the path, which fails the open
    /// itself before there is ever a descriptor to classify.
    #[error("could not read config file {}: {source}", path.display())]
    Read {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying IO error.
        #[source]
        source: io::Error,
    },

    /// The config path resolves to something other than a regular file — a
    /// directory, a FIFO, or a device. See
    /// [`hop_protocol::config_file::read`]'s own doc comment for why this is
    /// reported of the opened descriptor, not the path, and why that
    /// distinction matters.
    #[error("config file {} is not a regular file", path.display())]
    NotARegularFile {
        /// The path that did not resolve to a regular file.
        path: PathBuf,
    },

    /// The config file is larger than [`MAX_KEYMAP_BYTES`].
    #[error("config file {} is larger than the {max_bytes}-byte limit", path.display())]
    TooLarge {
        /// The path whose contents exceeded the cap.
        path: PathBuf,
        /// The cap that was exceeded.
        max_bytes: u64,
    },

    /// The config file is not valid TOML at all.
    #[error("config file {} is not valid TOML: {source}", path.display())]
    Parse {
        /// The path that did not parse.
        path: PathBuf,
        /// The underlying TOML parse error.
        #[source]
        source: toml::de::Error,
    },

    /// `[keymap]` is present in the file but is not a table of `action =
    /// "spelling"` entries — for example, `keymap = "oops"`. Distinct from
    /// [`KeymapError::UnknownAction`] and [`KeymapError::UnparseableBinding`]:
    /// neither of those can even be evaluated until there is a table to walk.
    #[error("config `[keymap]` in {} must be a table of action = \"key\" entries", path.display())]
    SectionNotATable {
        /// The config path that carried the malformed section.
        path: PathBuf,
    },

    /// A `[keymap]` key does not name any [`Action`] this module knows —
    /// criterion 4's second refusal shape. Distinct from an unknown
    /// top-level *section* elsewhere in `config.toml` (D1: `hopd`'s own
    /// keys, or any other program's section, are simply not this module's
    /// concern) — this is about a key *inside* `[keymap]` specifically,
    /// which this module does own.
    #[error("config `[keymap]` in {} binds unknown action `{name}`", path.display())]
    UnknownAction {
        /// The config path that carried the unknown action.
        path: PathBuf,
        /// The unrecognized key, exactly as written in the file.
        name: String,
    },

    /// A `[keymap]` value is not a usable binding — criterion 4's first
    /// refusal shape. Covers both a value that is not a string at all (a
    /// number, a boolean, an array) and a string [`Binding::parse`] could
    /// not turn into a key plus modifiers.
    #[error(
        "config `[keymap]` in {} binds `{action}` to `{spelling}`, which does not parse as a \
         key (optionally `mod+mod+key`, e.g. `ctrl+k`)",
        path.display()
    )]
    UnparseableBinding {
        /// The config path that carried the unparseable binding.
        path: PathBuf,
        /// The action's `config.toml` key — [`Action::config_key`].
        action: &'static str,
        /// The offending value, as written (or, for a non-string value, its
        /// TOML debug representation — there is no "spelling" for a value
        /// that was never a string to begin with, but the message still
        /// names exactly what was there).
        spelling: String,
    },
}

/// The action a key press resolves to, per `config.toml`'s `[keymap]`
/// section or (for anything the section does not mention) §8's defaults.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<(gdk::Key, gdk::ModifierType), Action>,

    /// The same bindings `bindings` inverts from, kept the other way round
    /// as well — see [`Keymap::binding_for`]'s doc comment for why this
    /// field, rather than a scan over `bindings`, is what answers it.
    by_action: HashMap<Action, Binding>,
}

impl Keymap {
    /// The §8 default keymap, with no `config.toml` consulted at all — used
    /// both as [`Keymap::load`]'s answer when no config file exists (D2 of
    /// the plan: absence is the documented default, not an error) and as
    /// the starting point [`parse_table`] overrides one action at a time.
    pub fn defaults() -> Keymap {
        let by_action = default_bindings();
        Keymap {
            bindings: build_lookup(&by_action),
            by_action,
        }
    }

    /// Loads the keymap from the real environment — `$XDG_CONFIG_HOME/hop/config.toml`,
    /// falling back to `$HOME/.config`.
    ///
    /// # Errors
    ///
    /// [`KeymapError::MissingHome`] if neither environment variable is set.
    /// Every other [`KeymapError`] variant if a config file exists but is
    /// not safe or usable — see this module's doc comment, "Refusal, and
    /// what it means for startup". An absent file is not an error: it
    /// yields [`Keymap::defaults`].
    pub fn load() -> Result<Keymap, KeymapError> {
        let xdg = env::var(XDG_CONFIG_HOME).ok().filter(|v| !v.is_empty());
        let home = env::var(HOME).ok().filter(|v| !v.is_empty());
        Self::load_from_env(xdg, home)
    }

    /// The pure core of [`Keymap::load`]: given the *values* of
    /// `XDG_CONFIG_HOME` and `HOME` rather than reading them, resolves the
    /// path and loads it. This is what the tests below drive — this
    /// workspace denies `unsafe_code` and Rust 2024 makes `env::set_var`
    /// `unsafe`, so a test cannot safely mutate process environment, and
    /// must instead pass the values it wants directly, exactly the seam
    /// `hopd::config::Config::load_from_env` uses for the identical reason.
    fn load_from_env(
        xdg_config_home: Option<String>,
        home: Option<String>,
    ) -> Result<Keymap, KeymapError> {
        let xdg_config_home = xdg_config_home.filter(|v| !v.is_empty());
        let home = home.filter(|v| !v.is_empty());
        let base_dir = match xdg_config_home {
            Some(dir) => PathBuf::from(dir),
            None => match home {
                Some(home_dir) => PathBuf::from(home_dir).join(".config"),
                None => return Err(KeymapError::MissingHome),
            },
        };
        Self::from_path(&base_dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
    }

    /// Loads from a concrete config file path — see this module's doc
    /// comment, "Path resolution and the hazard-aware read", for what
    /// [`hop_protocol::config_file::read`] protects against here.
    fn from_path(path: &Path) -> Result<Keymap, KeymapError> {
        let data = match hop_protocol::config_file::read(path, MAX_KEYMAP_BYTES) {
            Ok(Some(data)) => data,
            Ok(None) => return Ok(Keymap::defaults()),
            Err(hop_protocol::config_file::ConfigFileError::Read { source, .. }) => {
                return Err(KeymapError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
            Err(hop_protocol::config_file::ConfigFileError::NotARegularFile { .. }) => {
                return Err(KeymapError::NotARegularFile {
                    path: path.to_owned(),
                });
            }
            Err(hop_protocol::config_file::ConfigFileError::TooLarge { max_bytes, .. }) => {
                return Err(KeymapError::TooLarge {
                    path: path.to_owned(),
                    max_bytes,
                });
            }
        };

        let text = String::from_utf8(data).map_err(|err| KeymapError::Read {
            path: path.to_owned(),
            source: io::Error::new(io::ErrorKind::InvalidData, err.utf8_error()),
        })?;

        parse_table(path, &text)
    }

    /// Looks up which [`Action`] (if any) `key` pressed with `modifiers`
    /// held means — the one function every handler in `ui::window` calls
    /// instead of comparing a `gdk::Key` against a literal. `modifiers` is
    /// masked down to [`relevant_modifiers`] before the lookup, so a caller
    /// can pass the raw state GTK's `key-pressed` signal reports without
    /// pre-filtering it itself.
    pub fn lookup(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> Option<Action> {
        let modifiers = modifiers & relevant_modifiers();
        self.bindings.get(&(key, modifiers)).copied()
    }

    /// The reverse of [`Keymap::lookup`]: given an [`Action`], the one
    /// [`Binding`] that runs it — `None` if nothing does. New for issue
    /// #197's row action hint, which needs to answer "what key runs
    /// [`Action::Activate`]?", the opposite direction [`Keymap::lookup`]
    /// answers.
    ///
    /// # Why this can never depend on `HashMap` iteration order
    ///
    /// #197's brief is explicit that this must never be answered by
    /// scanning `bindings` (the key-press-keyed map [`Keymap::lookup`]
    /// queries) for the first entry whose value equals `action` — a
    /// `HashMap`'s iteration order is seeded per-instance from
    /// `RandomState`, so a scan answering from a map that (hypothetically)
    /// held more than one binding for the same action could name a
    /// different one on every process launch, or even between two
    /// `Keymap`s built from the identical `config.toml` within the same
    /// run.
    ///
    /// This answers from `by_action` instead, which makes the guarantee
    /// structural rather than defended by a tie-break rule: [`default_bindings`]
    /// and [`parse_table`] both build it by *inserting one [`Binding`] per
    /// [`Action`] key* — `HashMap::insert` on a key already present
    /// overwrites, it never keeps both — so `by_action` cannot hold two
    /// bindings for one action at all, regardless of insertion order,
    /// regardless of the map's own iteration order, regardless of what
    /// `config.toml` says. There is exactly one value at
    /// `by_action[action]` or there is none; a `HashMap::get` on an
    /// action-shaped key is the entire implementation. This is the
    /// stronger of the two claims this module could make —
    /// "deterministic because the data structure only admits one answer"
    /// rather than "deterministic because a scan is sorted before it picks
    /// a winner" — and it costs nothing extra: `by_action` already had to
    /// exist as an intermediate value in [`Keymap::defaults`] and
    /// [`parse_table`] before [`build_lookup`] inverted it into `bindings`;
    /// this only keeps it instead of discarding it.
    ///
    /// Two different actions bound to the identical key spelling is a
    /// related but separate hazard: `bindings` can only hold one action per
    /// key, so [`Keymap::lookup`] — the *forward* direction — would
    /// silently prefer whichever action [`build_lookup`]'s
    /// `HashMap::collect` happened to insert last for that key, itself
    /// dependent on `HashMap` iteration order for the same underlying
    /// reason this function's own answer must not be. That hazard lives
    /// entirely in `lookup`'s forward map, is pre-existing, and is out of
    /// scope here: conflict detection between two actions sharing a key is
    /// M6's, per this module's own top doc comment ("The config schema
    /// chosen for a binding").
    pub fn binding_for(&self, action: Action) -> Option<Binding> {
        self.by_action.get(&action).copied()
    }
}

/// Builds every action's default binding, keyed by action — the map
/// [`Keymap::defaults`] converts into its lookup table directly, and the
/// starting point [`parse_table`] overrides entries of one at a time so a
/// `config.toml` that mentions only one action still gets every other
/// action's default for free (criterion 2).
fn default_bindings() -> HashMap<Action, Binding> {
    Action::ALL
        .into_iter()
        .map(|action| {
            let binding = Binding::parse(action.default_spelling()).unwrap_or_else(|| {
                panic!(
                    "this module's own compiled-in default spelling {:?} for {action:?} failed \
                     to parse as a binding — a bug in this module's own defaults table, never \
                     in a user's config",
                    action.default_spelling()
                )
            });
            (action, binding)
        })
        .collect()
}

/// Inverts an action-keyed binding map into the key-press-keyed lookup table
/// [`Keymap::lookup`] actually queries. Takes `bindings` by reference,
/// rather than consuming it, so [`Keymap::defaults`] and [`parse_table`]
/// can keep the action-keyed map afterward too — see [`Keymap::binding_for`]'s
/// doc comment for why that retained map, not a scan over this function's
/// output, is what answers the reverse lookup.
fn build_lookup(
    bindings: &HashMap<Action, Binding>,
) -> HashMap<(gdk::Key, gdk::ModifierType), Action> {
    bindings
        .iter()
        .map(|(&action, &binding)| ((binding.key, binding.modifiers), action))
        .collect()
}

/// Parses `text` (already read from `path`) into a [`Keymap`], starting from
/// [`default_bindings`] and overriding one action at a time for each entry
/// `[keymap]` carries — never starting from empty, which is what lets a
/// `config.toml` that rebinds a single action still leave every other §8
/// default intact (criterion 2).
///
/// A `[keymap]` section absent from the file entirely is not an error and
/// not a partial table — every action keeps its default, exactly as if the
/// file had said nothing about keybindings at all. This mirrors D1: an
/// unknown *section* (or, here, a section this module simply does not find)
/// is not an unknown *binding* — only a key found *inside* `[keymap]` that
/// names no action is.
fn parse_table(path: &Path, text: &str) -> Result<Keymap, KeymapError> {
    let value: toml::Value = toml::from_str(text).map_err(|err| KeymapError::Parse {
        path: path.to_owned(),
        source: err,
    })?;

    let mut resolved = default_bindings();

    if let Some(section) = value.get("keymap") {
        let table = section
            .as_table()
            .ok_or_else(|| KeymapError::SectionNotATable {
                path: path.to_owned(),
            })?;

        for (name, raw) in table {
            let action =
                Action::from_config_key(name).ok_or_else(|| KeymapError::UnknownAction {
                    path: path.to_owned(),
                    name: name.clone(),
                })?;

            let spelling = raw
                .as_str()
                .ok_or_else(|| KeymapError::UnparseableBinding {
                    path: path.to_owned(),
                    action: action.config_key(),
                    spelling: format!("{raw:?}"),
                })?;

            let binding =
                Binding::parse(spelling).ok_or_else(|| KeymapError::UnparseableBinding {
                    path: path.to_owned(),
                    action: action.config_key(),
                    spelling: spelling.to_string(),
                })?;

            resolved.insert(action, binding);
        }
    }

    Ok(Keymap {
        bindings: build_lookup(&resolved),
        by_action: resolved,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::fs;

    use super::*;

    /// Writes `text` into a fresh temp dir's `hop/config.toml` and loads it
    /// through [`Keymap::load_from_env`] — the same fixture shape
    /// `hopd::config`'s own tests use, for the same reason (a pure function
    /// of explicit env values, never the real process environment).
    fn keymap_from_text(text: &str) -> (Keymap, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        let keymap =
            Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        (keymap, dir)
    }

    /// No display needed: [`gdk::Key::from_name`] and every [`gdk::ModifierType`]
    /// operation this module uses are pure table lookups and bit operations
    /// with no GDK runtime or display connection behind them — confirmed by
    /// this whole file running under a plain `cargo test`, with no
    /// `gtk::init()` and no `GDK_BACKEND` anywhere in it.
    #[test]
    fn defaults_cover_every_action_with_its_documented_key() {
        let keymap = Keymap::defaults();
        for action in Action::ALL {
            let key = gdk::Key::from_name(action.default_spelling()).unwrap();
            assert_eq!(
                keymap.lookup(key, gdk::ModifierType::empty()),
                Some(action),
                "default spelling {:?} for {action:?} did not resolve back to it",
                action.default_spelling()
            );
        }
    }

    #[test]
    fn absent_file_uses_every_default() {
        let dir = tempfile::tempdir().unwrap();
        let keymap =
            Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("Down").unwrap(),
                gdk::ModifierType::empty()
            ),
            Some(Action::NavigateDown),
            "an absent config file must fall back to the §8 defaults (criterion 2)"
        );
    }

    #[test]
    fn a_keymap_section_absent_from_a_present_file_still_uses_defaults() {
        // D1: a file that exists but says nothing about `[keymap]` at all —
        // not even the section header — must still yield every default,
        // exactly like a missing file. Distinguishes "no keymap section"
        // from "no config file", which absent_file_uses_every_default above
        // already covers.
        let (keymap, _dir) = keymap_from_text("some_future_key = 1\n");
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("Escape").unwrap(),
                gdk::ModifierType::empty()
            ),
            Some(Action::Dismiss)
        );
    }

    #[test]
    fn a_hopd_owned_key_alongside_keymap_does_not_confuse_parsing() {
        // D1: this is the exact file both `hopd` and `hop-gtk` open. Proves
        // this module parses only `[keymap]`, indifferent to `hopd`'s own
        // top-level scalar keys sitting in the same bytes.
        let (keymap, _dir) = keymap_from_text("max_results = 10\n\n[keymap]\ndismiss = \"q\"\n");
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("q").unwrap(),
                gdk::ModifierType::empty()
            ),
            Some(Action::Dismiss)
        );
    }

    /// The load-bearing test: criterion 3. A `config.toml` that rebinds one
    /// action to a different key must change which key triggers it, with no
    /// code change — proven at the keymap's own pure lookup, which needs no
    /// display, per this issue's brief.
    #[test]
    fn a_rebound_action_answers_to_its_new_key_and_no_longer_its_old_one() {
        let (keymap, _dir) = keymap_from_text("[keymap]\nnavigate_down = \"j\"\n");

        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("j").unwrap(),
                gdk::ModifierType::empty()
            ),
            Some(Action::NavigateDown),
            "the newly bound key must resolve to the rebound action"
        );
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("Down").unwrap(),
                gdk::ModifierType::empty()
            ),
            None,
            "the old default key must stop resolving to anything once its action is rebound — \
             a config that merely added a second way to trigger NavigateDown would not prove \
             the config was consulted at all, since Down still working could just as easily \
             mean the file was ignored"
        );

        // Every other default must be untouched by a config that rebinds
        // only one action (criterion 2's "the §8 list as defaults when the
        // file says nothing" applies key by key, not file by file).
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("Up").unwrap(),
                gdk::ModifierType::empty()
            ),
            Some(Action::NavigateUp)
        );
    }

    #[test]
    fn a_modifier_qualified_rebinding_parses_and_resolves() {
        let (keymap, _dir) = keymap_from_text("[keymap]\nsecondary_action = \"ctrl+k\"\n");
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("k").unwrap(),
                gdk::ModifierType::CONTROL_MASK
            ),
            Some(Action::SecondaryAction)
        );
        // Plain "k", with no modifier held, must not also trigger it.
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("k").unwrap(),
                gdk::ModifierType::empty()
            ),
            None
        );
    }

    #[test]
    fn caps_lock_does_not_break_an_unmodified_binding() {
        // relevant_modifiers's whole reason to exist: LOCK_MASK riding along
        // in the event state must not stop a plain, unmodified binding from
        // matching.
        let keymap = Keymap::defaults();
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("Down").unwrap(),
                gdk::ModifierType::LOCK_MASK
            ),
            Some(Action::NavigateDown)
        );
    }

    /// Criterion 4, first refusal shape: an unparseable spelling.
    #[test]
    fn an_unparseable_binding_spelling_is_refused_naming_the_action_and_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[keymap]\nactivate = \"NotARealGdkKeyName\"\n").unwrap();

        let err = Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match &err {
            KeymapError::UnparseableBinding {
                action, spelling, ..
            } => {
                assert_eq!(*action, "activate");
                assert_eq!(spelling, "NotARealGdkKeyName");
            }
            other => panic!("expected UnparseableBinding, got {other:?}"),
        }
        let message = err.to_string();
        assert!(message.contains("activate"), "{message:?}");
        assert!(message.contains("NotARealGdkKeyName"), "{message:?}");
    }

    #[test]
    fn a_non_string_binding_value_is_refused_as_unparseable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[keymap]\nactivate = 5\n").unwrap();

        let err = Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, KeymapError::UnparseableBinding { .. }),
            "expected UnparseableBinding, got {err:?}"
        );
    }

    /// Criterion 4, second refusal shape: an unknown action name.
    #[test]
    fn an_unknown_action_name_is_refused_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[keymap]\nsuper_secret_action = \"F1\"\n").unwrap();

        let err = Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match &err {
            KeymapError::UnknownAction { name, .. } => {
                assert_eq!(name, "super_secret_action");
            }
            other => panic!("expected UnknownAction, got {other:?}"),
        }
        assert!(err.to_string().contains("super_secret_action"), "{err}");
    }

    #[test]
    fn a_keymap_value_that_is_not_a_table_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "keymap = \"oops\"\n").unwrap();

        let err = Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, KeymapError::SectionNotATable { .. }),
            "expected SectionNotATable, got {err:?}"
        );
    }

    #[test]
    fn malformed_toml_is_refused_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[keymap\nactivate = \"Return\"").unwrap();

        let err = Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match &err {
            KeymapError::Parse { path: p, .. } => assert_eq!(*p, path),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_at_the_keymap_path_is_not_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(&path).unwrap();

        let err = Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, KeymapError::NotARegularFile { .. }),
            "expected NotARegularFile, got {err:?}"
        );
    }

    #[test]
    fn a_config_file_over_the_byte_cap_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x".repeat(MAX_KEYMAP_BYTES as usize + 1)).unwrap();

        let err = Keymap::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, KeymapError::TooLarge { .. }),
            "expected TooLarge, got {err:?}"
        );
    }

    #[test]
    fn missing_both_envs_is_an_explicit_error() {
        let err = Keymap::load_from_env(None, None).unwrap_err();
        assert!(matches!(err, KeymapError::MissingHome), "got {err:?}");
    }

    #[test]
    fn home_fallback_is_honored() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home
            .path()
            .join(".config")
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME);
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, "[keymap]\ndismiss = \"q\"\n").unwrap();

        let home_str = home.path().to_string_lossy().into_owned();
        let keymap = Keymap::load_from_env(None, Some(home_str)).unwrap();
        assert_eq!(
            keymap.lookup(
                gdk::Key::from_name("q").unwrap(),
                gdk::ModifierType::empty()
            ),
            Some(Action::Dismiss)
        );
    }

    // --- Issue #197: the reverse lookup (`Action` -> `Binding`) ---

    /// Criterion 1: the reverse lookup answers every default action with the
    /// exact binding [`Action::default_spelling`] parses to — proven
    /// against [`Binding::parse`] directly rather than a hand-written
    /// [`Binding`] literal, since its fields stay private even now that the
    /// type itself is `pub`.
    #[test]
    fn binding_for_answers_the_binding_that_runs_each_default_action() {
        let keymap = Keymap::defaults();
        for action in Action::ALL {
            let expected = Binding::parse(action.default_spelling());
            assert_eq!(
                keymap.binding_for(action),
                expected,
                "binding_for({action:?}) did not answer its documented default binding"
            );
        }
    }

    /// A `config.toml` rebinding must show up on the reverse lookup exactly
    /// as it shows up on the forward one — `binding_for` is not a second,
    /// independently-wrong source of truth about what an action is bound to.
    #[test]
    fn binding_for_reflects_a_config_rebinding() {
        let (keymap, _dir) = keymap_from_text("[keymap]\nsecondary_action = \"ctrl+k\"\n");
        assert_eq!(
            keymap.binding_for(Action::SecondaryAction),
            Binding::parse("ctrl+k")
        );
    }

    /// Criterion 1's explicit `None` case. Unreachable through this
    /// module's own public constructors — [`Keymap::defaults`] and
    /// `parse_table` both build `by_action` total over [`Action::ALL`], so
    /// neither can ever produce a `Keymap` missing an action — but the brief
    /// asks for coverage of the `None` arm regardless. This constructs a
    /// `Keymap` directly (private-field access, same module tree) with an
    /// empty `by_action` to prove that arm is implemented and correct, not
    /// merely unreachable-and-untested.
    #[test]
    fn binding_for_answers_none_when_the_action_keyed_map_has_no_entry() {
        let keymap = Keymap {
            bindings: HashMap::new(),
            by_action: HashMap::new(),
        };
        assert_eq!(keymap.binding_for(Action::Activate), None);
    }

    /// `binding_for` must not depend on `HashMap` iteration order — see its
    /// own doc comment for the full argument. `HashMap`'s default
    /// `RandomState` reseeds on every `HashMap::new()`/`collect()`, so
    /// rebuilding `Keymap::defaults()` many times exercises many distinct
    /// iteration orders over `bindings`; if `binding_for` were (wrongly) a
    /// scan over that map instead of a `by_action` lookup, this is the test
    /// likely to catch it flipping between runs.
    #[test]
    fn binding_for_is_stable_across_many_independently_built_keymaps() {
        let first = Keymap::defaults().binding_for(Action::Activate);
        for _ in 0..64 {
            assert_eq!(Keymap::defaults().binding_for(Action::Activate), first);
        }
    }

    // --- Issue #197: rendering a `Binding` as text ---

    /// See `impl Display for Binding`'s own doc comment for the convention;
    /// each test below pins one clause of it.
    #[test]
    fn display_spells_return_as_enter() {
        assert_eq!(Binding::parse("Return").unwrap().to_string(), "Enter");
    }

    #[test]
    fn display_spells_escape_as_esc() {
        assert_eq!(Binding::parse("Escape").unwrap().to_string(), "Esc");
    }

    #[test]
    fn display_spells_arrow_keys_as_glyphs() {
        assert_eq!(Binding::parse("Up").unwrap().to_string(), "↑");
        assert_eq!(Binding::parse("Down").unwrap().to_string(), "↓");
        assert_eq!(Binding::parse("Left").unwrap().to_string(), "←");
        assert_eq!(Binding::parse("Right").unwrap().to_string(), "→");
    }

    #[test]
    fn display_spells_page_up_and_page_down_with_a_space_not_an_underscore() {
        assert_eq!(Binding::parse("Page_Up").unwrap().to_string(), "Page Up");
        assert_eq!(
            Binding::parse("Page_Down").unwrap().to_string(),
            "Page Down"
        );
    }

    #[test]
    fn display_leaves_home_end_tab_and_menu_as_gdks_own_spelling() {
        assert_eq!(Binding::parse("Home").unwrap().to_string(), "Home");
        assert_eq!(Binding::parse("End").unwrap().to_string(), "End");
        assert_eq!(Binding::parse("Tab").unwrap().to_string(), "Tab");
        assert_eq!(Binding::parse("Menu").unwrap().to_string(), "Menu");
    }

    #[test]
    fn display_uppercases_a_bare_printable_key() {
        assert_eq!(Binding::parse("k").unwrap().to_string(), "K");
    }

    #[test]
    fn display_orders_modifiers_ctrl_shift_alt_super_regardless_of_config_order() {
        assert_eq!(
            Binding::parse("shift+ctrl+super+alt+k")
                .unwrap()
                .to_string(),
            "Ctrl+Shift+Alt+Super+K"
        );
    }

    #[test]
    fn display_spells_the_long_config_form_control_the_same_as_ctrl() {
        assert_eq!(
            Binding::parse("control+k").unwrap().to_string(),
            Binding::parse("ctrl+k").unwrap().to_string()
        );
        assert_eq!(Binding::parse("control+k").unwrap().to_string(), "Ctrl+K");
    }
}
