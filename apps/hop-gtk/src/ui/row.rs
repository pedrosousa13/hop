//! Builds and populates the `Row` node's widget: one reusable horizontal
//! container per visible slot's `Row` page — a leading icon and a title —
//! populated and cleared as the list scrolls, never destroyed and rebuilt.
//! Acceptance criterion 4.
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
//! Issue #190 is what turned the widget from a bare `gtk::Label` into the
//! `gtk::Box` [`build`] returns below, so it could carry a `gtk::Image` for
//! the item's icon alongside the title label that was already there. See
//! "the icon slot" section below for how that widget reserves its space,
//! and [`bind`]'s doc comment for how it resolves an [`hop_protocol::IconSpec`].
//!
//! # Fixed-height reserved rows
//!
//! [`build`] gives its container [`tokens::ROW_HEIGHT_PX`] as a height
//! request immediately, before any item's title or icon is known — because
//! `gtk::Stack` is homogeneous by default (sizes to fit every page it
//! holds, not only the visible one), that height request already governs
//! the dispatch container's own natural height the moment this widget is
//! placed into it, in `setup`, before `bind` ever runs for that slot. The
//! slot's on-screen size is therefore decided before any content — a
//! title, an icon, in this slice — is placed into it, which is what stops
//! an async result frame from shifting layout when it lands: nothing here
//! waits on content to know how tall a row is, so a title or icon arriving
//! later never changes a size that was already settled.
//!
//! The height request now lives on the outer `gtk::Box` — [`build`]'s
//! return value, and the widget `ui::view::build_dispatch_container` adds
//! as the `Row` page — rather than on the title label the way it did
//! before this issue. That is a deliberate choice, not an incidental move:
//! the `gtk::Box` is the widget the `gtk::Stack` actually measures to size
//! the page, so putting the request directly on it says outright "this
//! page reserves this much height" instead of relying on `gtk::Box`'s own
//! natural-height computation (the max of its children's requests) to
//! carry a label's request up to the box by accident. A future third or
//! fourth child of this row could then ask for its own height without that
//! computation silently changing what the box as a whole reserves.
//!
//! # The icon slot
//!
//! The leading `gtk::Image` gets a fixed size request of
//! [`tokens::ICON_SIZE_PX`] on both axes, also in [`build`], for the exact
//! same reason the row height is fixed up front: whether an item's icon
//! resolves to a theme lookup, a decoded file, `image-missing`, or nothing
//! at all, the pixels that slot occupies are decided before [`bind`] ever
//! runs, so no bind can make the row narrower or wider than the one before
//! it. [`gtk::Image::set_pixel_size`] is set alongside the size request so
//! a resolved icon-name lookup renders at that size rather than whatever
//! size the icon theme's default happens to be.
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
//! into a widget.

use std::io::Read;

use gtk::prelude::*;
use gtk::{gdk, glib};

use hop_protocol::{IconPath, IconSpec, Item};

use crate::tokens;

/// GTK's own stand-in icon for "something was supposed to be here and
/// isn't" — used both for a theme lookup GTK cannot satisfy (its own
/// built-in fallback, left alone per [`bind`]'s doc comment) and, set
/// explicitly here, for a `Path` arm that failed to open, read, or decode.
/// Naming it once means both call sites agree on the exact string.
const IMAGE_MISSING_ICON: &str = "image-missing";

/// The widget name [`build`] gives its leading `gtk::Image`, so [`icon_widget`]
/// can find it back out of the `gtk::Box` [`build`] returns by name rather
/// than by position — see that function's doc comment for why that is the
/// safer of the two.
const ICON_CHILD_NAME: &str = "hop-row-icon";

/// The widget name [`build`] gives its title `gtk::Label`, the same
/// pairing [`ICON_CHILD_NAME`] describes for the icon.
const TITLE_CHILD_NAME: &str = "hop-row-title";

/// The `Row` page's leading icon, reached out of `container` (the `gtk::Box`
/// [`build`] returns) by the name [`build`] gave it — see [`find_named_child`]
/// for why a name search is used instead of trusting append order.
///
/// `pub` rather than private: this is the one typed accessor [`bind`] and
/// [`unbind`] both go through to reach the icon, and it doubles as the
/// accessor `tests/view_tree_renderer.rs` uses to make assertions on the
/// same widget instance those two functions mutate — a test that reached
/// into the `gtk::Box` with its own `first_child`/`downcast` chain could
/// silently start asserting on the wrong child if this module's internal
/// child order ever changed; going through the same accessor production
/// code uses means a test failure here is a real behavior change, not a
/// second, drifting copy of this lookup.
pub fn icon_widget(container: &gtk::Box) -> Option<gtk::Image> {
    find_named_child(container, ICON_CHILD_NAME)
}

/// The `Row` page's title label — [`icon_widget`]'s pairing for the other
/// child [`build`] adds.
pub fn title_widget(container: &gtk::Box) -> Option<gtk::Label> {
    find_named_child(container, TITLE_CHILD_NAME)
}

/// Builds one `Row` node's widget: a horizontal `gtk::Box` holding a
/// leading icon and a title, sized to [`tokens::ROW_HEIGHT_PX`] before any
/// item is known — see this module's "fixed-height reserved rows" and "the
/// icon slot" doc sections. Called once per slot, from
/// `ui::view::build_dispatch_container`, itself called once per slot from
/// `ui::view::build`'s `connect_setup` handler.
pub fn build() -> gtk::Box {
    let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    container.set_height_request(*tokens::ROW_HEIGHT_PX);

    let icon = gtk::Image::new();
    icon.set_widget_name(ICON_CHILD_NAME);
    icon.set_size_request(*tokens::ICON_SIZE_PX, *tokens::ICON_SIZE_PX);
    icon.set_pixel_size(*tokens::ICON_SIZE_PX);
    container.append(&icon);

    let label = gtk::Label::new(None);
    label.set_widget_name(TITLE_CHILD_NAME);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    container.append(&label);

    container
}

/// Walks `container`'s direct children looking for one whose
/// [`gtk::Widget::widget_name`] is `name`, downcast to `W`. The engine
/// behind [`icon_widget`] and [`title_widget`], not called any other way.
///
/// This is a positional traversal at the GTK API level
/// (`first_child`/`next_sibling`, the only way to enumerate a `gtk::Box`'s
/// children at all) but not a *positional accessor* in the sense the icon
/// and the title could be told apart by which one comes first: it filters
/// by the name [`build`] stamped onto each child, so an accidental reorder
/// of the two `append` calls in [`build`], or a third child inserted
/// between them later, cannot make this function return the wrong widget —
/// it would either still find the right one by name or find `None`, never
/// silently return the icon where the title was expected. `build` gives
/// each child a name once and never again, so there is exactly one
/// candidate this can find for either name.
fn find_named_child<W: IsA<gtk::Widget>>(container: &gtk::Box, name: &str) -> Option<W> {
    let mut next = container.first_child();
    while let Some(child) = next {
        if child.widget_name() == name {
            return child.downcast::<W>().ok();
        }
        next = child.next_sibling();
    }
    None
}

/// Reads `path` and decodes it into a texture, or `None` on any failure —
/// the open refused, the read failed, or the bytes did not decode as an
/// image [`bind`]'s `Path` arm treats identically: all three collapse to
/// "set `image-missing`", per this issue's brief.
///
/// [`IconPath::open_regular_file`] is the only opener this crate uses (see
/// this crate's global constraint on that point): it is the one path in
/// `hop-protocol` that makes the syscall confirming what was opened is
/// actually a regular file rather than a FIFO, a device, or a directory.
/// Nothing here — and nothing reachable from here — opens `path` any other
/// way.
fn load_path_texture(path: &IconPath) -> Option<gdk::Texture> {
    let mut file = path.open_regular_file().ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    gdk::Texture::from_bytes(&glib::Bytes::from(&bytes)).ok()
}

/// Resolves `spec` onto `icon`, the three-way distinction this issue's
/// brief and the plan's global constraints both name explicitly:
///
/// - `None` (the item promised no icon at all) clears `icon`, so nothing is
///   drawn — a blank slot, not [`IMAGE_MISSING_ICON`].
/// - `Some(IconSpec::Name(name))` hands the theme lookup to GTK itself via
///   [`gtk::Image::set_icon_name`]. GTK already falls back to
///   `image-missing` on its own when the theme has no such name — that is
///   the desired behavior per this issue's brief, so this arm does nothing
///   more than set the property and let GTK's own fallback do the rest.
/// - `Some(IconSpec::Path(path))` tries [`load_path_texture`]; a resolved
///   texture is set as `icon`'s paintable, and any failure sets
///   [`IMAGE_MISSING_ICON`] explicitly, so this arm ends up looking exactly
///   like the `Name` arm's own fallback from a caller's point of view — a
///   promise that was broken renders the same as a lookup that came up
///   empty, distinct only from a promise that was never made (`None`,
///   above).
fn resolve_icon(icon: &gtk::Image, spec: Option<&IconSpec>) {
    match spec {
        None => icon.clear(),
        Some(IconSpec::Name(name)) => icon.set_icon_name(Some(name.as_str())),
        Some(IconSpec::Path(path)) => match load_path_texture(path) {
            Some(texture) => icon.set_paintable(Some(&texture)),
            None => icon.set_icon_name(Some(IMAGE_MISSING_ICON)),
        },
    }
}

/// Populates `widget` (built by [`build`]) with `item`'s title and icon.
/// `widget` is typed as a bare `gtk::Widget` rather than `gtk::Box` because
/// its caller, `ui::view::bind`, reaches it back out of a `gtk::Stack` page
/// by name — `gtk::Stack::child_by_name` hands back the general widget type
/// regardless of what was added, so the downcast belongs here, next to the
/// one place that knows a `Row` page's widget is actually the `gtk::Box`
/// [`build`] returns.
pub fn bind(widget: &gtk::Widget, item: &Item) {
    let Some(container) = widget.downcast_ref::<gtk::Box>() else {
        return;
    };
    if let Some(label) = title_widget(container) {
        label.set_text(item.title.as_str());
    }
    if let Some(icon) = icon_widget(container) {
        resolve_icon(&icon, item.icon.as_ref());
    }
}

/// Clears whatever [`bind`] last put in `widget`.
///
/// Clearing the title and the icon on unbind (rather than leaving whatever
/// the last-bound item left behind) means a recycled row never has a flash
/// of the *previous* occupant's title or icon visible between unbind and
/// the next bind — defensive, not load-bearing, since GTK does not render
/// a row between the two, but it keeps this widget from ever holding stale
/// application data it should not have. Symmetrical with [`bind`]: every
/// property `bind` can set here, `unbind` resets.
pub fn unbind(widget: &gtk::Widget) {
    let Some(container) = widget.downcast_ref::<gtk::Box>() else {
        return;
    };
    if let Some(label) = title_widget(container) {
        label.set_text("");
    }
    if let Some(icon) = icon_widget(container) {
        icon.clear();
    }
}
