//! The GTK list model backing the results list: a `gio::ListStore` of
//! [`glib::BoxedAnyObject`]-wrapped [`hop_protocol::Item`]s.
//!
//! # Why `BoxedAnyObject` rather than a hand-written `glib::Object` subclass
//!
//! [`Item`] already carries every field the `Row` node's widget (`ui::row`)
//! needs and is `Clone` — a full `#[glib::object_subclass]` type exists to expose
//! GObject *properties* to something outside Rust (a `.ui` template, GTK
//! Inspector, a language binding). Nothing here needs that: `ui::row`'s
//! `bind` reads straight through a borrowed `Item`, in the same process, in
//! Rust. [`glib::BoxedAnyObject`] is exactly the escape hatch `glib-rs`
//! ships for this case — a `GObject` wrapper around an arbitrary `'static`
//! Rust value, usable anywhere a `gio::ListModel` requires `IsA<glib::Object>`
//! items, with none of a subclass's property boilerplate.

use gio::prelude::*;
use glib::BoxedAnyObject;
use hop_protocol::Item;

/// Builds an empty store, holding [`BoxedAnyObject`]-wrapped [`Item`]s.
pub fn new_store() -> gio::ListStore {
    gio::ListStore::new::<BoxedAnyObject>()
}

/// Replaces `store`'s entire contents with `items`, in one call — the same
/// "replace, never append" rule [`hop_protocol::DaemonMsg::Results`]
/// documents on the wire, carried through to the model backing the list
/// view. One [`gio::ListStore::splice`] (rather than `remove_all` then N
/// `append`s) is also what gives GTK a single `items-changed` signal to
/// diff against, which is what lets `GtkListView` recycle existing row
/// widgets for positions that still hold data instead of tearing every row
/// down and rebuilding it.
pub fn replace(store: &gio::ListStore, items: Vec<Item>) {
    let wrapped: Vec<BoxedAnyObject> = items.into_iter().map(BoxedAnyObject::new).collect();
    store.splice(0, store.n_items(), &wrapped);
}

/// Reads the [`Item`] a `BoxedAnyObject` wraps, cloned out from behind its
/// `RefCell` borrow. Used wherever code needs to hold the item past the
/// borrow's scope: `ui::row`'s `bind` only needs a borrow, but the
/// selection/execute path (`ui::window`) needs an owned value to send across
/// `ipc`'s channel.
///
/// # Panics
///
/// If `object` is not a `BoxedAnyObject<Item>` — which would mean this
/// crate's own [`new_store`]/[`replace`] pair put something else in the
/// store, a programming error in this module, not a condition a caller of
/// this function can otherwise produce.
pub fn item_of(object: &glib::Object) -> Item {
    object
        .downcast_ref::<BoxedAnyObject>()
        .expect("hop-gtk's list store holds only BoxedAnyObject<Item>")
        .borrow::<Item>()
        .clone()
}
