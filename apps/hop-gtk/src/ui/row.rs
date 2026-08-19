//! The `GtkListView` factory: one reusable row widget per visible slot,
//! bound and unbound as the list scrolls — never destroyed and rebuilt.
//! Acceptance criterion 4.
//!
//! # Fixed-height reserved rows
//!
//! Every row's widget is given [`tokens::ROW_HEIGHT_PX`] as its height
//! request in `setup`, before `bind` ever runs — the slot exists, and is
//! already the right size, before any content (a title, in this slice) is
//! placed into it. That is what stops an async result frame from shifting
//! layout when it lands: nothing here waits on content to know how tall a
//! row is, so a title arriving later never changes the row's already-decided
//! height.
//!
//! # `setup` and `bind` never animate
//!
//! A factory reuses the *same* row widget across many different items as
//! the list is scrolled — that reuse is the whole point (recycling, not
//! destroy-and-rebuild). An entrance animation wired into `bind` would
//! therefore replay every time a recycled row is bound to a new item, i.e.
//! on every scroll step, not just when a row is first shown to the user.
//! Neither callback below starts one, or anything that could grow into one
//! by accident — both are a straight-line read of a field into a label.

use gtk::prelude::*;

use crate::tokens;
use crate::ui::model;

/// Builds a `GtkSignalListItemFactory` whose rows show a plain label — "row
/// content can be a plain label for now" per the brief; the view-tree
/// renderer that replaces this with real row content is issue #181's seam,
/// not this issue's.
pub fn build() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    // GTK 4.8 widened these signals' second parameter from the concrete
    // `GtkListItem` to a bare `GObject` (gtk4-rs's generated signature
    // follows suit: `Fn(&Self, &glib::Object)`), so every callback below
    // downcasts back to `gtk::ListItem` itself before using any of that
    // type's methods.
    factory.connect_setup(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_height_request(*tokens::ROW_HEIGHT_PX);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        list_item.set_child(Some(&label));
    });

    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(item_object) = list_item.item() else {
            return;
        };
        let item = model::item_of(&item_object);
        if let Some(label) = list_item
            .child()
            .and_then(|w| w.downcast::<gtk::Label>().ok())
        {
            label.set_text(item.title.as_str());
        }
    });

    // Clearing text on unbind (rather than leaving whatever the last-bound
    // item left behind) means a recycled row never has a flash of the
    // *previous* occupant's title visible between unbind and the next
    // bind — defensive, not load-bearing, since GTK does not render a row
    // between the two, but it keeps this factory's widget from ever holding
    // stale application data it should not have.
    factory.connect_unbind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(label) = list_item
            .child()
            .and_then(|w| w.downcast::<gtk::Label>().ok())
        {
            label.set_text("");
        }
    });

    factory
}
