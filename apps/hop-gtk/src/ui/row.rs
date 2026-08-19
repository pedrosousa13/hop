//! Builds and populates the `Row` node's widget: one reusable `gtk::Label`
//! per visible slot's `Row` page, populated and cleared as the list scrolls
//! — never destroyed and rebuilt. Acceptance criterion 4.
//!
//! Before issue #181's view-tree seam, this module *was* the `GtkListView`
//! factory outright — its `build` constructed the factory, and its
//! `connect_setup`/`connect_bind`/`connect_unbind` closures ran directly off
//! GTK's signals ("row content can be a plain label for now" per the #179
//! walking-skeleton brief). `ui::view::build` owns the factory now, and
//! dispatches to the three plain functions below — [`build`], [`bind`],
//! [`unbind`] — from inside its own `connect_setup`/`connect_bind`/
//! `connect_unbind` closures, once it has resolved which node type a slot
//! is showing. Nothing about what these three functions *do* changed in
//! that move — only who calls them, and how directly.
//!
//! # Fixed-height reserved rows
//!
//! [`build`] gives its label [`tokens::ROW_HEIGHT_PX`] as a height request
//! immediately, before any item's title is known — because `gtk::Stack` is
//! homogeneous by default (sizes to fit every page it holds, not only the
//! visible one), that height request already governs the dispatch
//! container's own natural height the moment `ui::view::build_dispatch_container`
//! places this label into it, in `setup`, before `bind` ever runs for that
//! slot. The slot's on-screen size is therefore decided before any content
//! — a title, in this slice — is placed into it, which is what stops an
//! async result frame from shifting layout when it lands: nothing here
//! waits on content to know how tall a row is, so a title arriving later
//! never changes a size that was already settled.
//!
//! # `build` and `bind` never animate
//!
//! A `GtkListView` factory reuses the *same* row widget across many
//! different items as the list is scrolled — that reuse is the whole point
//! (recycling, not destroy-and-rebuild), and it holds just as much now that
//! [`bind`] is called from `ui::view::bind`'s dispatch rather than straight
//! from a `connect_bind` signal handler. An entrance animation wired into
//! [`bind`] would therefore replay every time a recycled row is bound to a
//! new item, i.e. on every scroll step, not just when a row is first shown
//! to the user. Neither function below starts one, or anything that could
//! grow into one by accident — both are a straight-line read of a field
//! into a label.

use gtk::prelude::*;

use hop_protocol::Item;

use crate::tokens;

/// Builds one `Row` node's widget: a label sized to
/// [`tokens::ROW_HEIGHT_PX`] before any item is known — see this module's
/// "fixed-height reserved rows" doc section. Called once per slot, from
/// `ui::view::build_dispatch_container`, itself called once per slot from
/// `ui::view::build`'s `connect_setup` handler.
pub fn build() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_height_request(*tokens::ROW_HEIGHT_PX);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

/// Populates `widget` (built by [`build`]) with `item`'s title. `widget` is
/// typed as a bare `gtk::Widget` rather than `gtk::Label` because its caller,
/// `ui::view::bind`, reaches it back out of a `gtk::Stack` page by name —
/// `gtk::Stack::child_by_name` hands back the general widget type
/// regardless of what was added, so the downcast belongs here, next to the
/// one place that knows a `Row` page's widget is actually a `gtk::Label`.
pub fn bind(widget: &gtk::Widget, item: &Item) {
    let Some(label) = widget.downcast_ref::<gtk::Label>() else {
        return;
    };
    label.set_text(item.title.as_str());
}

/// Clears whatever [`bind`] last put in `widget`.
///
/// Clearing text on unbind (rather than leaving whatever the last-bound
/// item left behind) means a recycled row never has a flash of the
/// *previous* occupant's title visible between unbind and the next
/// bind — defensive, not load-bearing, since GTK does not render a row
/// between the two, but it keeps this widget from ever holding stale
/// application data it should not have.
pub fn unbind(widget: &gtk::Widget) {
    let Some(label) = widget.downcast_ref::<gtk::Label>() else {
        return;
    };
    label.set_text("");
}
