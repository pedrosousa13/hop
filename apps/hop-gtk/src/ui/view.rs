//! The view tree the results list renders, and the dispatch point that
//! turns one of its nodes into the visible content of one `GtkListView`
//! slot — issue #181's seam.
//!
//! # Why a node is data, not a widget
//!
//! Decision D2 of the M3 design spec (`docs/superpowers/specs/2026-08-10-hop-m3-frontend-design.md`)
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
//!   [`build_dispatch_container`], one more match arm each in [`bind`] and
//!   [`unbind`], and one more arm in [`Node::for_item`] — the one place
//!   that decides which variant a bound `Item` becomes — with the
//!   factory's own structure ([`build`], below) untouched.
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
//! a misreading of it. [`Node::for_item`] is not an exception to this: it
//! is the one place the seam decides *which* variant a bound `Item`
//! becomes, and with one variant it has nothing to decide — its body is
//! `Node::Row(item)`, unconditionally, not a match with one arm dressed up
//! to look like a classifier. What proves the dispatch point in this
//! module *can* carry a second node type — acceptance criterion 3 — is the
//! shape of [`build_dispatch_container`], [`bind`], [`unbind`], and
//! [`Node::for_item`] above, together with
//! `tests/view_tree_renderer.rs`'s recycling test, never a second variant
//! added to prove it.

use gtk::prelude::*;

use hop_protocol::Item;

use crate::keymap::Keymap;
use crate::ui::{model, row};

/// One node in the view tree a `GtkListView` slot can be asked to render.
///
/// Exactly one variant exists — see this module's doc comment for why that
/// is a deliberate, guarded property of this issue rather than an
/// incomplete catalog. `Row` carries the [`Item`] it renders, not a widget:
/// see this module's doc comment's "why a node is data, not a widget"
/// section for why that distinction is the whole basis of decision D2.
///
/// # Issue #197: why `Row` also carries a [`Keymap`]
///
/// The row's action hint needs both the item's own default-action label
/// *and* the key that runs [`crate::keymap::Action::Activate`] — the
/// second of which lives on a [`Keymap`], not on [`Item`] at all. Two
/// shapes were considered for getting that keymap to `ui::row::bind`,
/// which is where `assets/tokens.css`'s hint tokens say the label and the
/// key glyph both have to be resolved (two typographically distinct
/// widgets, one resolving function — see `ui::row`'s own "Issue #197" doc
/// section):
///
/// - **Pre-resolve a display string in `ui::view::bind` and carry that
///   instead of a `Keymap`.** Rejected: it would split the hint's
///   resolution across two modules — the *lookup* (`keymap.binding_for`)
///   here, the *pairing with the item's own label* over in `ui::row` —
///   for no reason `ui::view` otherwise has to know about `Action` at all
///   (this module never once imports `crate::keymap::Action`, and should
///   not have to gain that import just to plumb a hint through). Every
///   other per-item resolution this crate does (the icon, the subtitle)
///   already lives entirely inside `ui::row`, next to the item fields it
///   reads; carrying the whole `Keymap` keeps the hint the same shape as
///   those two instead of a one-off exception.
/// - **Carry the `Keymap` itself**, `Clone`d once per bind/unbind call
///   from the copy [`build`] captures. Chosen: `Keymap` is already
///   `Clone` (phase A of this issue), its two `HashMap`s hold at most one
///   entry per [`crate::keymap::Action`] variant — ten today — so cloning
///   it on every bind is not a cost worth avoiding, and `ui::row::bind`
///   ends up with exactly the same shape it already has for `Item`: read
///   whatever fields the row needs, no pre-digestion by a caller that
///   does not render anything itself.
///
/// This widens what `Row` carries, not how many variants exist — the
/// "one node type, no branching" guarantee this module's own doc comment
/// describes is about [`Node`]'s variant count and about [`bind`]/[`unbind`]
/// never gaining a second match arm to reach `ui::row`, neither of which
/// this change touches.
pub enum Node {
    /// A single result row, rendered by [`row::build`]/[`row::bind`]
    /// against the carried [`Item`] and [`Keymap`].
    Row(Item, Keymap),
}

impl Node {
    /// Builds the node a bound `item` should render as — the one place in
    /// this seam that decides *which* [`Node`] variant an [`Item`] becomes.
    ///
    /// Before this constructor existed, [`build`]'s `connect_bind` and
    /// `connect_unbind` closures each wrote `Node::Row(item)` directly,
    /// which quietly put that decision *inside* the factory itself —
    /// harmless with one variant, but it meant a second node type would
    /// have needed classification logic added identically to both closures
    /// in `build()`, which is exactly the change to the factory's own
    /// structure acceptance criterion 3 rules out. Routing both call sites
    /// through this function instead means the decision lives in exactly
    /// one place, and `build()` never has to change to accommodate it.
    ///
    /// This always returns `Node::Row(item, keymap)`, unconditionally —
    /// deliberately not a `match`, an `if`, or any other branch, because
    /// there is no second variant to route toward yet (see this module's
    /// guard section). What a second, real node type changes is this
    /// function's body, and only this function's body: `build()`'s two
    /// closures keep calling `Node::for_item` exactly as they do today.
    ///
    /// `keymap` joined this constructor's signature in issue #197,
    /// alongside `item` — see [`Node`]'s own doc comment, "why `Row` also
    /// carries a `Keymap`", for why the row's action hint needs one.
    pub fn for_item(item: Item, keymap: Keymap) -> Node {
        Node::Row(item, keymap)
    }

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
            Node::Row(_, _) => Self::ROW_PAGE,
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
/// arm to the `match` below, one page to [`build_dispatch_container`], and
/// one arm to [`Node::for_item`] — never a change to [`build`]'s own
/// closures, which only ever call `bind` and `Node::for_item` by name and
/// never need to know how many variants either one now handles. That is
/// acceptance criterion 3.
pub fn bind(stack: &gtk::Stack, node: &Node) {
    let page_name = node.page_name();
    if let Some(widget) = stack.child_by_name(page_name) {
        match node {
            Node::Row(item, keymap) => row::bind(&widget, item, keymap),
        }
    }
    // Unconditional rather than nested inside the `if let` above: `setup`
    // (`build_dispatch_container`) adds a page for every name
    // `Node::page_name` can return — both are compiled from the same
    // `Node::ROW_PAGE` constant, see that constant's doc comment — so
    // `child_by_name` above cannot actually miss for any `node` this
    // function is called with. Once that invariant holds there is nothing
    // left for this line to guard.
    stack.set_visible_child_name(page_name);
}

/// The dispatch point's other half: clears whatever [`bind`] most recently
/// populated on `node`'s page of `stack`. Called from [`build`]'s
/// `connect_unbind` handler just before the slot's `item` property is
/// unset for good — see below for why that timing means `node` is
/// available here at all.
///
/// # Why this takes `&Node`, symmetrically with `bind`
///
/// An earlier version of this function took only `&gtk::Stack`, on the
/// premise that GTK's `unbind` signal hands the factory no item to build a
/// `Node` from. That premise was checked directly against GTK's own
/// documentation for `SignalListItemFactory::unbind`
/// (`/usr/share/gir-1.0/Gtk-4.0.gir` on this machine) while addressing
/// review, and it is wrong: unbind fires "when a listitem was removed from
/// use in a list widget and its `item` is about to be unset" — *about to
/// be*, not already gone. `list_item.item()` still returns the same item
/// `bind` saw, at the moment `unbind` runs, exactly symmetrically with
/// `bind`'s own `connect_bind` handler. [`build`]'s `connect_unbind`
/// handler therefore reads it the same way `connect_bind` does and builds
/// the same [`Node`] — via [`Node::for_item`], never by naming a variant
/// inline — that `bind` was given for that slot.
///
/// That symmetry is what keeps this a real seam rather than half of one.
/// Without it, a second node type would need new item-reading code added
/// *inside* `build`'s `connect_unbind` closure to work out which teardown
/// to run — a change to the factory's own structure, which acceptance
/// criterion 3 rules out. With it, a second variant is one more match arm
/// here, exactly like [`bind`], and the factory's own shape stays
/// untouched either way.
pub fn unbind(stack: &gtk::Stack, node: &Node) {
    if let Some(widget) = stack.child_by_name(node.page_name()) {
        match node {
            Node::Row(_, _) => row::unbind(&widget),
        }
    }
}

/// Builds the `GtkListView` factory: `setup` gives every slot the dispatch
/// container [`build_dispatch_container`] builds, and `bind`/`unbind` read
/// the slot's item (or lack of one), turn it into a [`Node`] with
/// [`Node::for_item`], and hand that to [`bind`]/[`unbind`] above. This is
/// acceptance criterion 2 — the factory renders through the dispatch point
/// rather than constructing a row directly — and it replaces what used to
/// be `ui::row::build`'s own responsibility before issue #181: see that
/// function's doc comment for what moved and what did not.
///
/// Neither closure below ever names a [`Node`] variant itself (an earlier
/// version of this function did, writing `Node::Row(item)` inline in both
/// — review caught that this quietly put the "which variant" decision
/// inside the factory, which a second node type would have had to
/// duplicate into both closures to extend, a change to the factory's own
/// structure that acceptance criterion 3 forbids). Both call
/// [`Node::for_item`] instead, so this function's own body has nothing left
/// to change when a second node type is added.
///
/// `keymap` is `Clone`d once into each of `connect_bind`'s and
/// `connect_unbind`'s closures below (issue #197), and once more out of
/// each closure's own capture on every call — see `Node`'s own doc
/// comment, "why `Row` also carries a `Keymap`", for why the row's action
/// hint needs a copy at all, and why cloning it this often is not a cost
/// worth engineering around. Nothing about `setup`'s closure changes: the
/// dispatch container it builds has no content to resolve yet, only pages
/// to register.
pub fn build(keymap: Keymap) -> gtk::SignalListItemFactory {
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

    let bind_keymap = keymap.clone();
    factory.connect_bind(move |_, object| {
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
        bind(&stack, &Node::for_item(item, bind_keymap.clone()));
    });

    factory.connect_unbind(move |_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        // Still set here — see `unbind`'s own doc comment for why GTK's
        // `unbind` signal fires before the slot's `item` property is
        // actually cleared, not after, which is what makes reading it
        // here (exactly as `connect_bind` above does) sound at all.
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
        unbind(&stack, &Node::for_item(item, keymap.clone()));
    });

    factory
}
