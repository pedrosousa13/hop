//! `hop-hotkeyd` — the optional resident hotkey agent for X11, design spec
//! §2's X11 row and §3's third process: a real global grab (`XGrabKey` on
//! the user's configured binding) that runs the universal toggle when it
//! fires. This is the piece no DE custom shortcut can provide on X11, and
//! its shape is salvage-validated (§10: "X11 grab loop (configurable keys),
//! backoff logic" from the Rust branch) — an *optional* resident agent,
//! required on no platform.
//!
//! # What lives where
//!
//! - [`binding`] — the spelling a binding is written in (`ctrl+alt+space`,
//!   the same vocabulary `hop-gtk::keymap` established for `[keymap]`) and
//!   its translation into an X modifier mask plus keysym.
//! - [`config`] — the `[hotkey]` section of `config.toml`, read through
//!   [`hop_protocol::config_file::read`] like every other reader of that
//!   file, with the absent-or-malformed ⇒ logged-no-op posture issue #234's
//!   criterion 2 fixes.
//! - [`run`] — backend selection in issue #235's documented order
//!   (**GlobalShortcuts portal → X11 grab → DE-shortcut guidance**, chosen
//!   backend and reason logged at startup), the X11 grab loop itself (one
//!   X connection, one signalfd, one blocking `poll`; single-instance by
//!   `BadAccess` evidence; backoff on connection loss) and the guidance
//!   arm for sessions where neither automatic backend applies.
//! - [`portal`] — the GlobalShortcuts portal client: probe, `CreateSession`
//!   /`BindShortcuts`, and blocking on `Activated`, over zbus's blocking
//!   API so no async runtime enters this crate.
//!
//! # Why no tokio
//!
//! The same argument `hop-cli`'s module doc makes, one process over: this
//! daemon waits on exactly two descriptors (the X socket and the signal
//! descriptor) and dispatches at most one subprocess per keypress. A
//! runtime would schedule nothing that a two-entry `poll(2)` does not
//! already do — and unlike hopd, there is no fan-in of many connections to
//! justify one.
//!
//! # Why this crate is not part of hop-gtk or hopd
//!
//! §3's three-process shape keeps the hotkey path decoupled from both ends:
//! hop-gtk must not need an X connection of its own beyond GDK's (and must
//! keep working on Wayland, where none of this exists), and hopd serves
//! queries regardless of how the window was raised. When the grab fires,
//! this daemon runs `hop toggle` — the same command a DE-configured
//! shortcut would — and knows nothing else about either process.
pub mod binding;
pub mod config;
pub mod portal;
pub mod run;
