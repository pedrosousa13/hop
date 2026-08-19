//! The view tree the results list renders, and the dispatch point that
//! turns one of its nodes into the visible content of one `GtkListView`
//! slot — issue #181's seam.
//!
//! # Why a node is data, not a widget
//!
//! Decision D2 of the M3 design spec (`docs/superpowers/specs/…frontend-design.md`)
//! rules that the *view catalog* belongs to the wire protocol rather than to
//! whichever tier is doing the rendering: v3's sandboxed Tier 2 plugins get
//! the same catalog v2's trusted Tier 1 plugins do, because what a plugin
//! may *describe* is a property of the protocol, while the sandbox only ever
//! constrains what a plugin may *reach*. A declarative view tree is data,
//! and data crosses a sandbox boundary without difficulty — a live GTK
//! widget handle does not, since a widget lives on this process's GTK main
//! thread and cannot be handed across a process boundary at all. That is why
//! [`Node::Row`] below carries an [`Item`] — the data a row needs to render
//! — rather than a `gtk::Widget`: a variant that carried a widget would
//! quietly throw away the property this whole seam exists to keep.
//!
//! # Why the dispatch container is a `gtk::Stack`, built once in `setup`
//!
//! `GtkSignalListItemFactory::connect_setup` runs *before* the slot it is
//! given has an item bound to it — only `connect_bind` knows which node type
//! a slot will end up showing. That leaves two shapes for a factory that has
//! to answer to more than one node type:
//!
//! - Swap the slot's child in `bind` (`list_item.set_child(...)`, choosing
//!   the widget type on the spot). Rejected: it destroys and rebuilds the
//!   slot's widget every time a bind's node type differs from the last one
//!   the slot held, which is exactly the destroy-and-rebuild acceptance
//!   criterion 5 forbids, and it would silently undo the fixed-height
//!   reservation `ui::row::build`'s widget makes (see that function's own
//!   doc comment) — reintroducing the async layout shift that reservation
//!   exists to prevent, since a freshly built widget of a different node
//!   type would carry none of it.
//! - Build every node type's widget once in `setup`, as a page of a
//!   `gtk::Stack`, and have `bind` pick the right page by name
//!   (`set_visible_child_name`). The slot's widget tree — the stack itself —
//!   is created exactly once per slot and reused for the slot's entire
//!   lifetime; a second node type is one more page added in
//!   [`build_dispatch_container`] and one more match arm added in [`bind`],
//!   with the factory's own structure ([`build`], below) untouched.
//!
//! This module takes the second shape. With exactly one node type, the
//! stack [`build_dispatch_container`] builds holds exactly one page — that
//! is structure this issue knowingly pays for ahead of needing it (the issue
//! names this directly: "M3 carries structure it does not yet need"), not a
//! leftover from a second node type that used to exist. A reader who finds a
//! one-page `gtk::Stack` here should read this comment as the reason, not
//! wonder whether a page went missing.
//!
//! # The guard this module holds to
//!
//! [`Node`] has exactly one variant, `Row`. **No second variant belongs
//! here until a real consumer asks for one** — issue #181 names the actual
//! risk as over-abstracting against a view-tree catalog that does not exist
//! yet, and rules explicitly that adding a second, speculative node (a
//! `Detail`, an `ActionPanel`, anything test-only or commented out) would be
//! a misreading of it. What proves the dispatch point in this module *can*
//! carry a second node type — acceptance criterion 3 — is the shape of
//! [`build_dispatch_container`] and [`bind`] above, together with
//! `tests/view_tree_renderer.rs`'s recycling test, never a second variant
//! added to prove it.

use gtk::prelude::*;

use hop_protocol::Item;

use crate::ui::{model, row};

/// One node in the view tree a `GtkListView` slot can be asked to render.
///
/// Exactly one variant exists — see this module's doc comment for why that
/// is a deliberate, guarded property of this issue rather than an
/// incomplete catalog. `Row` carries the [`Item`] it renders, not a widget:
/// see this module's doc comment's "why a node is data, not a widget"
/// section for why that distinction is the whole basis of decision D2.
pub enum Node {
    /// A single result row, rendered by [`row::build`]/[`row::bind`].
    Row(Item),
}

impl Node {
    /// The `Row` variant's page name on the dispatch container's
    /// `gtk::Stack`.
    ///
    /// This constant is the one place a page's name is spelled out.
    /// [`build_dispatch_container`] (setup, no `Node` value in hand yet)
    /// reads it directly to register the page; [`Node::page_name`] (bind,
    /// a `Node` value in hand) reads it back out through the match below.
    /// Both sides end up compiled from the same identifier, which is what
    /// keeps them from drifting onto two different strings as a second
    /// variant is added — the failure mode this constant exists to close
    /// off is a `setup` that registers `"detail"` while `bind` asks for
    /// `"details"`, silently landing on the empty page GTK's `gtk::Stack`
    /// falls back to when `set_visible_child_name` is given a name with no
    /// matching page.
    const ROW_PAGE: &'static str = "row";

    /// This node's page name on the dispatch container — [`bind`]'s half of
    /// the pairing [`Node::ROW_PAGE`] describes.
    fn page_name(&self) -> &'static str {
        match self {
            Node::Row(_) => Self::ROW_PAGE,
        }
    }
}

/// Builds the dispatch container: a `gtk::Stack` holding one page per node
/// type, added by name so [`bind`] can select one later by
/// `set_visible_child_name` rather than by rebuilding anything. Called once
/// per slot, from [`build`]'s `connect_setup` handler — see this module's
/// doc comment for why that timing (before any item is bound) is exactly
/// what rules out a shape that builds a node's widget in `bind` instead.
///
/// With one node type this stack holds exactly one page (`Node::ROW_PAGE`,
/// [`row::build`]'s widget) — the deliberate, paid-for-ahead-of-need
/// structure this module's doc comment names, not an oversight.
fn build_dispatch_container() -> gtk::Stack {
    let stack = gtk::Stack::new();
    stack.add_named(&row::build(), Some(Node::ROW_PAGE));
    stack
}

/// The dispatch point: given the `gtk::Stack` [`build_dispatch_container`]
/// built for a slot and the [`Node`] that slot is now bound to, selects that
/// node's page and populates it.
///
/// This function's signature is itself part of what rules out the rejected
/// "swap the child in `bind`" shape this module's doc comment describes: it
/// takes `&gtk::Stack`, not `&gtk::ListItem`, so there is no slot-level
/// `set_child` in scope here to reach for even by mistake — the only thing
/// this function can do to `stack` is select one of the pages `setup`
/// already built into it. Adding a second [`Node`] variant means adding one
/// arm to the `match` below (and one page to
/// [`build_dispatch_container`]) — the factory `build` returns keeps its own
/// shape unchanged either way, which is acceptance criterion 3.
pub fn bind(stack: &gtk::Stack, node: &Node) {
    let page_name = node.page_name();
    if let Some(widget) = stack.child_by_name(page_name) {
        match node {
            Node::Row(item) => row::bind(&widget, item),
        }
    }
    stack.set_visible_child_name(page_name);
}

/// Clears whatever [`bind`] most recently populated on `stack` — the direct
/// analogue of `ui::row`'s pre-#181 `connect_unbind`, moved here because the
/// widget being cleared is now the dispatch container's `Row` page rather
/// than the slot's only possible child.
///
/// This reaches for `Node::ROW_PAGE` directly rather than dispatching
/// through a `Node` value the way [`bind`] does, because GTK's `unbind`
/// signal hands the factory no item at all — there is nothing to match on.
/// With one node type that is not a loss: the row page is the only page
/// there is to clear. A second node type would need `unbind` to first read
/// `stack.visible_child_name()` (the same name [`bind`] just set) to know
/// *which* page's teardown to run before it could route to more than one —
/// a real decision this seam does not have to make yet, and is not making
/// here ahead of a second variant that would require it.
pub fn unbind(stack: &gtk::Stack) {
    if let Some(widget) = stack.child_by_name(Node::ROW_PAGE) {
        row::unbind(&widget);
    }
}

/// Builds the `GtkListView` factory: `setup` gives every slot the dispatch
/// container [`build_dispatch_container`] builds, and `bind`/`unbind` read
/// the slot's item (or lack of one) and hand it to [`bind`]/[`unbind`]
/// above. This is acceptance criterion 2 — the factory renders through the
/// dispatch point rather than constructing a row directly — and it replaces
/// what used to be `ui::row::build`'s own responsibility before issue #181:
/// see that function's doc comment for what moved and what did not.
pub fn build() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    // GTK 4.8 widened these signals' second parameter from the concrete
    // `GtkListItem` to a bare `GObject` (gtk4-rs's generated signature
    // follows suit: `Fn(&Self, &glib::Object)`), so every callback below
    // downcasts back to `gtk::ListItem` itself before using any of that
    // type's methods — carried over unchanged from `ui::row::build`'s
    // pre-#181 version of this same factory.
    factory.connect_setup(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        list_item.set_child(Some(&build_dispatch_container()));
    });

    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(item_object) = list_item.item() else {
            return;
        };
        let Some(stack) = list_item
            .child()
            .and_then(|widget| widget.downcast::<gtk::Stack>().ok())
        else {
            return;
        };
        let item = model::item_of(&item_object);
        bind(&stack, &Node::Row(item));
    });

    factory.connect_unbind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(stack) = list_item
            .child()
            .and_then(|widget| widget.downcast::<gtk::Stack>().ok())
        else {
            return;
        };
        unbind(&stack);
    });

    factory
}
