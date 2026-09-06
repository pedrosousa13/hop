//! `hop-gtk` — the GTK4 + libadwaita launcher window.
//!
//! Issue #179's walking skeleton: a pre-built hidden window presented on
//! demand, a results list over `GtkListView` with row recycling, all socket
//! IO kept off the GTK main thread (see [`ipc`]'s module doc for how that is
//! enforced rather than merely written down), a layer-shell probe with a
//! working fallback, and the headless harness (`--screenshot <path>`) the
//! design spec's §11 makes non-optional.
//!
//! Split into a library and a thin [`main`](../../src/main.rs) so that this
//! crate's integration tests (`tests/`) — which can only see a lib crate's
//! `pub` surface, never a bin crate's — can drive [`ipc::spawn`] and the
//! headless screenshot path directly, the same reason `hop-cli` is split the
//! same way (`crates/hop-cli/src/lib.rs`'s own doc comment).

pub mod app;
pub mod cli;
pub mod fonts;
mod icon_roots;
pub mod ipc;
pub mod kde_blur;
pub mod keymap;
pub mod layer_shell;
pub mod material;
pub mod screenshot;
pub mod session;
pub mod style;
pub mod stylesheet;
pub mod tokens;
pub mod ui;
pub mod x11;
