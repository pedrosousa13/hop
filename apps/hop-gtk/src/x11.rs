//! X11's overlay positioning: centers the window on the screen at map time,
//! implementing design spec §2's X11 row — a "normal override-positioned
//! window" — for every WM/DE and, critically, with **no** window manager at
//! all (the under-Xvfb shape `tests/x11_smoke.rs` verifies).
//!
//! # Why this goes through the X server directly — there is no GTK4 API left
//!
//! GTK3 had `gtk_window_move()`; **GTK4 removed it**, along with every other
//! client-facing way to position a toplevel: GDK4's `gdk::Surface` exposes no
//! position setter, `gdk::ToplevelLayout` carries no position field, and the
//! Wayland protocol has no such concept at all (which is why only the X11
//! arm of §2 promises override positioning). What GDK *does* still expose,
//! through its X11-specific surface subclass, is the toplevel's XID
//! ([`gdkx11::X11Surface::xid`]) — and an XID is all it takes to send the X
//! protocol's own `ConfigureWindow` request from a second connection
//! (`x11rb`, pure Rust, no unsafe — see `Cargo.toml`'s dependency comment).
//! That is the boring mechanism chosen, and the reasons it survives both of
//! the environments that matter:
//!
//! - **No WM (Xvfb, a bare `startx`, an Xephyr nest):** nothing else ever
//!   positions the window, so whatever the app configures is final. A plain
//!   default toplevel would sit at (0, 0); this module is what makes hop
//!   appear centered instead.
//! - **With a reparenting WM:** the WM places the frame during its map
//!   handling and the client's `ConfigureWindow` moves the window inside
//!   (or against) that placement afterwards. A WM that aggressively re-places
//!   windows can fight a post-map move — that is inherent to GTK4 having no
//!   pre-map position hook (WM_SIZE_HINTS' USPosition flag would need to be
//!   written before mapping, and GDK computes those hints itself) — but
//!   "client moves itself after map" is an ordinary, honored X11 idiom, and
//!   the spec's row asks for exactly this shape.
//!
//! The two connections' requests carry no ordering guarantee against each
//! other, which sounds like a race but is not one: if our `ConfigureWindow`
//! lands before the server processes GDK's map, the window simply maps at
//! the configured position; if it lands after, the window visibly snaps
//! there. Either way the final geometry is the same.
//!
//! # Why the move runs off the main thread
//!
//! Opening the second X connection is real I/O (a Unix-socket connect plus
//! the handshake round-trip). It has no business stalling the GTK main
//! thread — the same reasoning that keeps `ipc`'s socket traffic on its own
//! thread — so [`apply_self_positioning`] spawns a short-lived thread per
//! map. The thread needs nothing from GTK afterwards: the XID is a plain
//! integer copied in, and the result is reported to stderr, not to any
//! widget.

use std::fmt;

use gtk::prelude::*;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt};
use x11rb::rust_connection::RustConnection;

/// Everything that can go wrong centering the window. Reported to stderr,
/// never fatal: a window that fails to center is a degraded overlay, not a
/// reason to take the whole launcher down (matching how `layer_shell`'s
/// fallback treats an absent capability).
#[derive(Debug)]
pub enum PositionError {
    /// The second X connection could not be established — no `$DISPLAY`,
    /// or the server refused it.
    Connect(x11rb::errors::ConnectError),
    /// A request could not be sent — most plausibly a stale XID raced a
    /// window that was already gone.
    Request(x11rb::errors::ConnectionError),
    /// A request's reply failed for the same class of reason.
    Reply(x11rb::errors::ReplyError),
    /// The connection carried no screens at all; cannot happen against any
    /// conforming server, but the setup structure allows it.
    NoScreen,
}

impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PositionError::Connect(err) => write!(f, "X11 connection failed: {err}"),
            PositionError::Request(err) => write!(f, "X11 request failed: {err}"),
            PositionError::Reply(err) => write!(f, "X11 reply failed: {err}"),
            PositionError::NoScreen => write!(f, "X11 server reported no screens"),
        }
    }
}

impl std::error::Error for PositionError {}

impl From<x11rb::errors::ConnectError> for PositionError {
    fn from(err: x11rb::errors::ConnectError) -> Self {
        PositionError::Connect(err)
    }
}

impl From<x11rb::errors::ConnectionError> for PositionError {
    fn from(err: x11rb::errors::ConnectionError) -> Self {
        PositionError::Request(err)
    }
}

impl From<x11rb::errors::ReplyError> for PositionError {
    fn from(err: x11rb::errors::ReplyError) -> Self {
        PositionError::Reply(err)
    }
}

/// Wires `window` up so that every time it maps on an X11 display, it is
/// re-centered on the screen. A documented no-op when the window's surface
/// turns out not to be an X11 surface — the strategy decision upstream
/// (`session`) should only call this on X11, but the guard here keeps the
/// mechanism honest about its own precondition rather than trusting the
/// caller.
///
/// Every map re-centers, not just the first: the pre-built window hides on
/// close (`hide_on_close`) and re-presents on the next toggle, and each
/// presentation should put the launcher back where users expect it —
/// centered — wherever they last moved it.
pub fn apply_self_positioning(window: &adw::ApplicationWindow) {
    let win = window.clone();
    window.connect_map(move |_| {
        let Some(surface) = win.surface() else {
            return;
        };
        let Some(x11_surface) = surface.downcast_ref::<gdkx11::X11Surface>() else {
            return;
        };
        let xid = x11_surface.xid();
        // XIDs are CARD32 on the wire; gdk4-x11's `XWindow` alias is the C
        // `XID`'s `unsigned long` and so is wider than the protocol needs.
        // An XID that outgrew u32 is not a window this server ever handed
        // out — skip rather than truncate.
        let Ok(xid) = u32::try_from(xid) else {
            return;
        };
        std::thread::spawn(move || {
            if let Err(err) = center_on_screen(xid) {
                eprintln!("hop-gtk: could not center window on X11: {err}");
            }
        });
    });
}

/// Moves the window `xid` to the center of its screen, sized as it already
/// is. Runs on its own thread — see this module's doc comment.
fn center_on_screen(xid: u32) -> Result<(), PositionError> {
    // `None` means "read `$DISPLAY`", the same server GDK's X11 backend is
    // already connected to.
    let (conn, screen_num) = RustConnection::connect(None)?;
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or(PositionError::NoScreen)?;

    // The window's current size comes from the server, not from GTK's
    // request: what ConfigureWindow positions is the actual X window, whose
    // pixel size already reflects any scale factor GDK applied, so centering
    // against the server's own root dimensions is correct without either
    // side of the arithmetic needing to know about scaling.
    let geo = conn.get_geometry(xid)?.reply()?;
    let (x, y) = centered_origin(
        i32::from(root.width_in_pixels),
        i32::from(root.height_in_pixels),
        i32::from(geo.width),
        i32::from(geo.height),
    );

    conn.configure_window(xid, &ConfigureWindowAux::new().x(x).y(y))?;
    conn.flush()?;
    Ok(())
}

/// Where a `win_w` × `win_h` window's top-left corner goes to sit centered
/// on a `screen_w` × `screen_h` screen — clamped at zero for a window larger
/// than the screen, because a negative origin would hang the window off the
/// top-left edge rather than centering the part that fits.
///
/// Pure and unit-tested: this is the whole of §2's "override-positioned"
/// arithmetic, kept out of the I/O around it so a regression here fails in
/// `cargo test --workspace` on every machine, not just where Xvfb exists.
fn centered_origin(screen_w: i32, screen_h: i32, win_w: i32, win_h: i32) -> (i32, i32) {
    ((screen_w - win_w).max(0) / 2, (screen_h - win_h).max(0) / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centers_a_window_smaller_than_the_screen() {
        assert_eq!(centered_origin(1280, 1024, 400, 500), (440, 262));
        assert_eq!(centered_origin(1920, 1080, 400, 500), (760, 290));
    }

    #[test]
    fn clamps_a_window_larger_than_the_screen_to_the_origin() {
        // Not a shape hop ships (tokens pin the overlay well below any
        // screen), but the clamp is the difference between "pinned to the
        // top-left, fully reachable" and "half off-screen at negative
        // coordinates" if the token size ever grows past a tiny display.
        assert_eq!(centered_origin(320, 240, 400, 500), (0, 0));
    }

    #[test]
    fn odd_remainders_round_toward_the_top_left() {
        // Integer division truncates; 641/2 = 320. Which side absorbs the
        // odd pixel is arbitrary — pinning it here so a change is visible
        // rather than accidental.
        assert_eq!(centered_origin(641, 481, 1, 1), (320, 240));
    }
}
