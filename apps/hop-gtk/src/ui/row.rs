//! Builds and populates the `Row` node's widget: one reusable horizontal
//! container per visible slot's `Row` page — a leading icon and a stacked
//! title/subtitle text column — populated and cleared as the list scrolls,
//! never destroyed and rebuilt. Acceptance criterion 4.
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
//! # Issue #196: the title-over-subtitle text column
//!
//! [`build`]'s container was, until this issue, a single horizontal
//! `gtk::Box` with exactly two direct children — the icon and the title —
//! matching the M3 row anatomy only up through "26px icon · title", not the
//! full "26px icon · title · subtitle · right-aligned action hint" the v1
//! visual spec actually calls for. A subtitle appended as a flat third
//! child of that same horizontal box would land to the *right* of the
//! title, a sibling at the same vertical position, not underneath it — the
//! anatomy calls for a stack, not a third column.
//!
//! [`build`] below therefore nests a second, *vertical* `gtk::Box` — the
//! text column — as the outer container's second child, in the position
//! the title alone used to occupy; the title and the new subtitle label are
//! its two children, in that order, so the subtitle always lays out beneath
//! the title rather than beside it. The icon stays a direct child of the
//! outer, horizontal box, exactly as before — nesting only the two labels
//! that need to stack is the smallest change that gets the real anatomy: a
//! third horizontal box wrapping icon+column, or promoting the whole
//! container to vertical and re-deriving the icon's placement, would both
//! change more of this module's shape than the one thing #196 asks for.
//!
//! The text column carries the `hexpand`/ellipsize responsibilities the
//! title label held by itself before this issue — see [`build`]'s own
//! comments on the column and its two children for exactly which property
//! moved where and why.
//!
//! ## The absent case: hide, don't reserve
//!
//! Issue #190 set the precedent that a *size* — the row height, the icon
//! slot — is reserved before any item is known, so nothing already on
//! screen ever reflows once a bind lands. The subtitle is not the same
//! shape of problem, because `Option<ItemSubtitle>` is not a size question:
//! an item that never carries a subtitle is not a smaller version of one
//! that does, it is a *title-only* row, and an always-visible, always-empty
//! subtitle label would leave that row's title sitting above a permanent
//! blank line — a gap nothing ever fills, for the lifetime of the process,
//! on every row whose item happens to have no subtitle.
//!
//! [`bind`] instead hides the subtitle label outright
//! (`gtk::Widget::set_visible(false)`) whenever `item.subtitle` is `None`,
//! and shows it whenever it is `Some`. The text column's own [`gtk::Align`]
//! is `Center` on the *cross* axis (vertical, since the column sits inside
//! the outer *horizontal* box) — set once, in [`build`], never touched
//! again — so the column's natural height (title alone, or title+subtitle
//! together) is always centred within the row's full reserved height rather
//! than pinned to its top. A hidden subtitle removes itself from that
//! natural-height computation entirely (GTK does not allocate space to an
//! invisible widget), so a title-only row's title recovers the full
//! vertical centring a title+subtitle row's *pair* already gets — never a
//! title stuck above a gap the way an always-present empty label would
//! produce. `tests/view_tree_renderer.rs`'s "issue #196" section pins both
//! halves of this directly: `subtitle.is_visible()` toggling with
//! `item.subtitle`'s presence, and the row's reserved layout
//! (`row_layout`) holding identical across every bind in that section
//! regardless of which state the subtitle is in.
//!
//! # `find_named_child` now searches descendants, not only direct children
//!
//! Before this issue, [`find_named_child`] only ever needed to look at
//! `container`'s direct children — the icon and the title were both direct
//! children of the one `gtk::Box` [`build`] returned. Nesting the text
//! column above means the title and the new subtitle label are no longer
//! direct children of that outer box; they are direct children of the text
//! column, which is itself a direct child of the outer box. Rather than
//! give the title and subtitle accessors a second, column-shaped lookup
//! path (asking a caller to know the internal nesting, or hand-walking one
//! extra level only for those two widgets), [`find_named_child`] was
//! widened into a depth-first search over the whole descendant subtree —
//! see its own doc comment for why that keeps, rather than loosens, the
//! "name, not position" discipline it already argued for, and why every
//! accessor ([`icon_widget`], [`title_widget`], [`subtitle_widget`]) can
//! still go through the exact same one function regardless of how deep its
//! target widget now sits.
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

use hop_protocol::{IconPath, IconSpec, Item, ItemSubtitle};

use crate::icon_roots;
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

/// The widget name — and, per this issue's brief, the CSS style class —
/// [`build`] gives its subtitle `gtk::Label`. Reusing the same string for
/// both `gtk::Widget::set_widget_name` (what [`find_named_child`] matches
/// on) and `gtk::Widget::add_css_class` (what `assets/stylesheet.css`'s
/// `.hop-row-subtitle` rule selects on) is deliberate, not a coincidence of
/// two constants that happen to agree: it is what the brief means by "the
/// selector and the accessor key off the same thing" — one name, spent
/// once, rather than a widget-name constant and a separately-chosen class
/// string that could quietly drift apart from each other later. The icon
/// and title widgets have no stylesheet rule of their own yet, so they have
/// no equivalent CSS class to keep in sync — only this one needs both.
const SUBTITLE_CHILD_NAME: &str = "hop-row-subtitle";

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

/// The `Row` page's subtitle label, added by issue #196 — reached the same
/// way [`title_widget`] reaches the title, even though the subtitle is no
/// longer a direct child of `container` but of the nested text column
/// [`build`] now creates. See [`find_named_child`]'s own doc comment for
/// why that nesting needed no second lookup path.
pub fn subtitle_widget(container: &gtk::Box) -> Option<gtk::Label> {
    find_named_child(container, SUBTITLE_CHILD_NAME)
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

    // The text column — see this module's top doc comment, "Issue #196:
    // the title-over-subtitle text column", for why a nested vertical
    // `gtk::Box` is what makes the subtitle stack under the title instead
    // of beside it. `hexpand` moves here from the title label (it used to
    // carry this directly, as the outer box's second and last child) since
    // the column, not either label alone, is now the outer box's second
    // child claiming the remaining horizontal width. `valign(Center)` is
    // what recovers the title's vertical centring when the subtitle is
    // hidden — see this module's "The absent case" doc section for why
    // that is set here, once, rather than toggled alongside the subtitle's
    // own visibility in `bind`.
    let text_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text_column.set_hexpand(true);
    text_column.set_valign(gtk::Align::Center);

    let title = gtk::Label::new(None);
    title.set_widget_name(TITLE_CHILD_NAME);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text_column.append(&title);

    // The subtitle label — created once here, never in `bind`, per this
    // issue's "created once" acceptance criterion. Starts invisible: the
    // very first `bind` a freshly built row ever gets might carry
    // `subtitle: None`, and [`bind`]'s own "The absent case" behaviour
    // (hide, not merely clear) has to hold from that very first bind, not
    // only from the second one onward.
    let subtitle = gtk::Label::new(None);
    subtitle.set_widget_name(SUBTITLE_CHILD_NAME);
    subtitle.add_css_class(SUBTITLE_CHILD_NAME);
    subtitle.set_xalign(0.0);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle.set_visible(false);
    text_column.append(&subtitle);

    container.append(&text_column);

    container
}

/// Searches `container`'s descendants — not only its direct children —
/// looking for one whose [`gtk::Widget::widget_name`] is `name`, downcast
/// to `W`. The engine behind [`icon_widget`], [`title_widget`], and
/// [`subtitle_widget`], not called any other way.
///
/// This is a positional traversal at the GTK API level
/// (`first_child`/`next_sibling`, the only way to enumerate a widget's
/// children at all) but not a *positional accessor* in the sense any two
/// of the icon, title, and subtitle could be told apart by which one comes
/// first, or by how deep it sits: it filters by the name [`build`] stamped
/// onto each named widget, so an accidental reorder of `build`'s `append`
/// calls, or a widget nested one level deeper than another, cannot make
/// this function return the wrong widget — it would either still find the
/// right one by name or find `None`, never silently return one named
/// widget where a different one was expected. `build` gives each named
/// widget a name once and never again, so there is exactly one candidate
/// this can find for any name.
///
/// # Why this walks the full subtree, not only `container`'s direct
/// children, since issue #196
///
/// Before that issue, the icon and the title were both direct children of
/// `container`, so a direct-children scan was already a complete search.
/// Issue #196 nested the title (and the new subtitle) one level deeper,
/// inside a text-column `gtk::Box` that is itself `container`'s direct
/// child — see this module's top doc comment for why. Rather than give
/// [`title_widget`]/[`subtitle_widget`] a second lookup path that assumes
/// the column's existence (which would mean two different ways to find a
/// named widget depending on which one it is — exactly the "keep exactly
/// one lookup path per widget" discipline this issue's brief asks this
/// module to preserve), this function was widened into a depth-first
/// search over every descendant. The "name, not position" guarantee above
/// is unaffected by that: nesting depth is just one more kind of position
/// this function already refused to rely on.
fn find_named_child<W: IsA<gtk::Widget>>(container: &gtk::Box, name: &str) -> Option<W> {
    find_named_descendant(container.upcast_ref::<gtk::Widget>(), name)
}

/// [`find_named_child`]'s recursive engine: searches `root`'s children,
/// and each child's own children in turn, for the first widget whose name
/// is `name`. A plain, pre-order depth-first walk — `root` is never itself
/// checked (its own name is not a "child" of itself), only its descendants
/// are, matching what every current caller needs: `container` is never
/// itself a candidate for `ICON_CHILD_NAME`/`TITLE_CHILD_NAME`/
/// `SUBTITLE_CHILD_NAME`.
fn find_named_descendant<W: IsA<gtk::Widget>>(root: &gtk::Widget, name: &str) -> Option<W> {
    let mut next = root.first_child();
    while let Some(child) = next {
        if child.widget_name() == name
            && let Ok(found) = child.clone().downcast::<W>()
        {
            return Some(found);
        }
        if let Some(found) = find_named_descendant(&child, name) {
            return Some(found);
        }
        next = child.next_sibling();
    }
    None
}

/// Reads `path` and decodes it into a texture, or `None` on any failure —
/// the open refused, the resolved target is outside every allowed icon
/// root, the read failed, or the bytes did not decode as an image —
/// [`bind`]'s `Path` arm treats all four identically: "set `image-missing`",
/// per this issue's brief and issue #93's.
///
/// [`IconPath::open_regular_file`] is the only opener this crate uses —
/// issue #190's agent brief named it the one call that issue's work may use
/// to open an icon file ("no second opener is introduced anywhere in the
/// crate"), and the reason behind that rule is concrete, not just a house
/// preference someone wrote down: it is the one path in `hop-protocol` that
/// makes the syscall confirming what was opened is actually a regular file
/// rather than a FIFO, a device, or a directory — every GTK/GDK
/// path-taking helper (`gtk::Image::from_file`, `set_from_file`,
/// `gdk::Texture::from_file`/`from_filename`) bypasses that check entirely.
/// Nothing here — and nothing reachable from here — opens `path` any other
/// way.
///
/// # Issue #93: the allow-list check, right after the open
///
/// `open_regular_file` deliberately does not check that `path` sits under
/// any allowed root — `IconPath`'s own doc comment names that as an
/// obligation on whoever resolves the path, not something the wire
/// contract can enforce (the roots are environment-dependent; see that
/// type's "Where an icon is expected to live" section). This is the one
/// place in `hop-gtk` — the client, and the process whose environment is
/// authoritative — that resolves a path, so this is where that obligation
/// is discharged: `icon_roots::ALLOWED_ICON_ROOTS.permits(&file)` runs on
/// the descriptor `open_regular_file` already returned, before a single
/// byte is read from it. See [`icon_roots::AllowedIconRoots::permits`] for
/// the mechanism (resolving the open descriptor via `/proc/self/fd`, not
/// re-resolving the path) and why it, rather than `openat2` or
/// `O_NOFOLLOW`, was chosen.
fn load_path_texture(path: &IconPath) -> Option<gdk::Texture> {
    let mut file = path.open_regular_file().ok()?;
    if !icon_roots::ALLOWED_ICON_ROOTS.permits(&file) {
        return None;
    }
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
///   above). Issue #93's allow-list refusal — `path` opened but its
///   resolved target sits outside every allowed icon root — is one more
///   way [`load_path_texture`] can return `None`, not a fourth outcome this
///   function has to know about: it reaches this arm's existing failure
///   branch exactly like an open refusal or a decode failure would.
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
    if let Some(subtitle) = subtitle_widget(container) {
        resolve_subtitle(&subtitle, item.subtitle.as_ref());
    }
    if let Some(icon) = icon_widget(container) {
        resolve_icon(&icon, item.icon.as_ref());
    }
}

/// Resolves `item.subtitle` onto `subtitle` — this issue's own "absent
/// case" rule, in code: `Some` sets the text and shows the widget; `None`
/// clears the text and hides it, rather than leaving an empty label
/// visible. See this module's top doc comment, "The absent case: hide,
/// don't reserve", for why hiding (not merely clearing) is the behaviour
/// chosen, and for how [`build`]'s `Align::Center` on the text column is
/// what turns "hidden" into "the title recentres in the full row height".
///
/// Always the `--hop-text-subtitle` proportional treatment, via
/// `assets/stylesheet.css`'s `.hop-row-subtitle` rule — this function
/// itself sets nothing but text and visibility, no `set_attributes`/
/// `pango::AttrList`, per this issue's brief; see that stylesheet rule's
/// own comment, and `ui/mode_label.rs`'s "CSS supersedes the Pango
/// stand-in" section (issue #193), for why a Pango attribute list here
/// would make the CSS rule permanently and silently dead instead. Nothing
/// here reads `item.kind` or inspects the subtitle text for a path shape —
/// every subtitle gets this one treatment, by design; see this module's
/// top doc comment and issue #196's own brief for why a mono-path
/// treatment is explicitly out of this function's scope.
fn resolve_subtitle(subtitle: &gtk::Label, text: Option<&ItemSubtitle>) {
    match text {
        Some(text) => {
            subtitle.set_text(text.as_str());
            subtitle.set_visible(true);
        }
        None => {
            subtitle.set_text("");
            subtitle.set_visible(false);
        }
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
    if let Some(subtitle) = subtitle_widget(container) {
        resolve_subtitle(&subtitle, None);
    }
    if let Some(icon) = icon_widget(container) {
        icon.clear();
    }
}
