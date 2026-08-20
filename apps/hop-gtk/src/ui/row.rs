//! Builds and populates the `Row` node's widget: one reusable horizontal
//! container per visible slot's `Row` page — a leading icon, a stacked
//! title/subtitle text column, and a trailing action hint — populated and
//! cleared as the list scrolls, never destroyed and rebuilt. Acceptance
//! criterion 4.
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
//! # Issue #197: the trailing action hint
//!
//! [`build`]'s outer, horizontal `container` gets a *third* direct child —
//! the hint, a small `gtk::Box` holding a label chip ("Open") and a
//! key-glyph chip ("Enter") — appended after the text column issue #196
//! introduced. Two placements were considered for where that third child
//! attaches:
//!
//! - **Nested inside the text column**, as a third stacked row under the
//!   subtitle. Rejected: the text column's entire reason to exist is
//!   stacking title *over* subtitle in a single visual line each — the
//!   hint is not a third line of text about the item, it is a
//!   right-aligned affordance that belongs at the row's trailing edge,
//!   vertically centred across the *whole* row, exactly like the icon at
//!   the leading edge. Nesting it in the column would put it under the
//!   subtitle instead of beside the pair, which is not the v1 row anatomy
//!   ("26px icon · title · subtitle · right-aligned action hint" — see
//!   `tokens.css`'s own GEOMETRY comment).
//! - **A third direct child of `container`**, alongside `icon` and
//!   `text_column`. Chosen: `text_column`'s `hexpand(true)` (already set,
//!   for issue #196) claims every pixel of width neither the fixed icon
//!   slot nor this hint uses, so appending the hint *after* the column is
//!   already sufficient to push it flush right — no alignment property of
//!   its own is needed on the hint, the same "hexpand carries the trailing
//!   child" trick the title relied on before #196 nested it.
//!
//! Two chips, not one glued-together label, because `assets/tokens.css`
//! pairs `--hop-text-hint-label` with `--hop-text-hint-key` — the label and
//! the key glyph are typographically distinct (proportional vs mono), which
//! only two separate widgets and two separate stylesheet rules can express.
//! See [`resolve_hint`]'s own doc comment for the "both halves or neither"
//! rule governing when either chip actually shows text, and
//! [`should_show_label_chip`]'s for the responsive collapse that can hide
//! the label chip specifically, independent of that rule, once both halves
//! already resolved.
//!
//! `keymap::Action` and `hop_protocol::item::Action` meet at exactly this
//! element and nowhere else in this module — see [`default_action_label`]'s
//! own doc comment for how [`resolve_hint`] keeps the two apart in both
//! code and naming, per this issue's own brief and `CONTEXT.md`'s
//! **Action** glossary entry.
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
use crate::keymap::{Action as KeymapAction, Keymap};
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
///
/// `ui/mode_label.rs`'s label carries only a CSS class (`"hop-mode-label"`)
/// and no widget name at all — a single identity, not this constant's
/// doubled one — and that precedent is worth naming here precisely because
/// this module doesn't follow it. The difference is not an oversight: the
/// mode label's own module never has to look that widget up out of a
/// parent container by name, since `ui::window::HopWindow` keeps the
/// `gtk::Label` handle `mode_label::build` returns for as long as the
/// window lives and never has to rediscover it later. The subtitle has no
/// such standing handle — [`bind`] and [`unbind`] are handed only the
/// `Row` page's outer `gtk::Widget` each time GTK's list-view recycling
/// calls them, and [`find_named_child`] is how they, and
/// `tests/view_tree_renderer.rs`'s assertions, get back to the specific
/// label a previous `build` call created — so a widget name is required
/// here regardless of what CSS does. And CSS cannot substitute for one: a
/// style class is not visible to `first_child`/`next_sibling`-based
/// traversal the way `widget_name()` is, so `.hop-row-subtitle` can select
/// this label for styling but could never answer [`find_named_child`]'s
/// "which child is this" question in its place. Single identity is right
/// for a widget nothing looks up by name; doubled identity is right for
/// one that both needs styling and must be found again — the mode label
/// and the subtitle simply are not the same shape of problem.
const SUBTITLE_CHILD_NAME: &str = "hop-row-subtitle";

/// The widget name [`build`] gives the hint's own horizontal `gtk::Box` —
/// the third direct child of the outer row container, issue #197. Not a
/// CSS class: nothing styles the wrapper itself, only its two chip children
/// below, so this name exists solely for [`hint_widget`]'s lookup, the same
/// single-identity shape [`ICON_CHILD_NAME`]/[`TITLE_CHILD_NAME`] already
/// use.
const HINT_CHILD_NAME: &str = "hop-row-hint";

/// The widget name and CSS class [`build`] gives the hint's label chip
/// (e.g. "Open") — doubled identity, matching [`SUBTITLE_CHILD_NAME`]'s own
/// reasoning: `assets/stylesheet.css`'s `.hop-row-hint-label` rule needs a
/// selector, and [`find_named_child`] needs a name, and one string serves
/// both rather than risking the two drifting apart.
const HINT_LABEL_CHILD_NAME: &str = "hop-row-hint-label";

/// The widget name and CSS class [`build`] gives the hint's key-glyph chip
/// (e.g. "Enter") — [`HINT_LABEL_CHILD_NAME`]'s pairing, styled by
/// `assets/stylesheet.css`'s `.hop-row-hint-key` rule with the mono
/// treatment `--hop-text-hint-key` names, distinct from the label chip's
/// proportional one.
const HINT_KEY_CHILD_NAME: &str = "hop-row-hint-key";

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

/// The `Row` page's action hint — the outer `gtk::Box` wrapping the label
/// and key-glyph chips, added by issue #197. See this module's "Issue
/// #197" doc section for why it is the outer row container's third direct
/// child, a sibling of the icon and the text column, rather than nested
/// inside the text column the way it would be if it stacked under the
/// title/subtitle instead of sitting beside them.
pub fn hint_widget(container: &gtk::Box) -> Option<gtk::Box> {
    find_named_child(container, HINT_CHILD_NAME)
}

/// The hint's label chip (e.g. "Open") — [`hint_widget`]'s wrapper's first
/// child.
pub fn hint_label_widget(container: &gtk::Box) -> Option<gtk::Label> {
    find_named_child(container, HINT_LABEL_CHILD_NAME)
}

/// The hint's key-glyph chip (e.g. "Enter") — [`hint_widget`]'s wrapper's
/// second child.
pub fn hint_key_widget(container: &gtk::Box) -> Option<gtk::Label> {
    find_named_child(container, HINT_KEY_CHILD_NAME)
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

    // The action hint — issue #197. A third, direct child of the outer
    // horizontal `container`, a sibling of `icon` and `text_column`, never
    // nested inside `text_column`: the text column's whole purpose (see
    // "Issue #196" above) is stacking title *over* subtitle, and the hint
    // is neither — it belongs beside that stack, at the row's trailing
    // edge, vertically centred in the row exactly as the icon is, not one
    // more line stacked underneath the subtitle. `text_column`'s own
    // `hexpand(true)` (set above) is what pushes this hint flush right:
    // the column claims every pixel of horizontal space neither the fixed
    // icon slot nor this hint uses, so appending the hint last, after the
    // column, is what puts it at the row's trailing edge with no explicit
    // alignment property of its own needed here.
    let hint = gtk::Box::new(gtk::Orientation::Horizontal, *tokens::HINT_CHIP_GAP_PX);
    hint.set_widget_name(HINT_CHILD_NAME);
    hint.set_valign(gtk::Align::Center);
    hint.set_margin_start(*tokens::HINT_MARGIN_START_PX);

    // Both chips start invisible, exactly like the subtitle above and for
    // the identical reason: the very first bind a freshly built row gets
    // might resolve to `resolve_hint`'s "neither" branch (see that
    // function's own doc comment, "both halves or neither"), and that
    // empty-slot behaviour has to hold from the first bind onward, not
    // only from the second one.
    let hint_label = gtk::Label::new(None);
    hint_label.set_widget_name(HINT_LABEL_CHILD_NAME);
    hint_label.add_css_class(HINT_LABEL_CHILD_NAME);
    hint_label.set_visible(false);
    hint.append(&hint_label);

    let hint_key = gtk::Label::new(None);
    hint_key.set_widget_name(HINT_KEY_CHILD_NAME);
    hint_key.add_css_class(HINT_KEY_CHILD_NAME);
    hint_key.set_visible(false);
    hint.append(&hint_key);

    container.append(&hint);

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
/// widget where a different one was expected. `build` is the only thing
/// that calls `set_widget_name` on any widget under `container`, and it
/// gives each of the three names above to exactly one widget, once — so
/// today, there really is exactly one candidate this can find for any
/// name. That is an invariant this function *relies on*, though, not one
/// it enforces: nothing here counts candidates or checks for a duplicate,
/// so if a future widget were ever added anywhere under `container` that
/// reused one of these names, this depth-first walk would not detect the
/// collision — it would simply return whichever candidate it reached
/// first and stay silent about the other. Keeping the names unique is an
/// obligation on whoever next adds a named widget to this tree, the same
/// way it already was when `build` named only direct children; widening
/// the search from direct children to the full subtree in issue #196
/// changed how far that obligation now has to reach, not who holds it.
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
///
/// # A name match with a failed downcast no longer stops the search
///
/// Before issue #196's widening, a child whose `widget_name()` matched
/// `name` but whose `downcast::<W>()` failed ended the search immediately,
/// returning `None` even if the intended widget sat among the remaining
/// children. [`find_named_descendant`]'s recursive shape does not preserve
/// that short-circuit: when a name matches but the downcast fails, the
/// walk falls through to search that same child's own descendants and
/// then its later siblings, exactly as it would for a child whose name
/// never matched at all. Given the one-name-one-widget invariant above,
/// this path is not expected to be exercised in practice — no name this
/// module hands out is ever attached to a widget of the wrong type — but
/// it is a genuine, previously-undocumented broadening of what counts as
/// "found," not only of *where* the search looks: a matched-but-wrong-
/// typed name used to end the search outright with `None`, and now it
/// does not. The behavior below treats "found" as "first name-and-type
/// match anywhere in the subtree," which is at least consistent with how
/// the function already treats a plain name mismatch, rather than
/// special-casing an early exit for the name-matched-wrong-type case
/// alone — but the honest cost is that it would also mask a violation of
/// the one-name-one-widget invariant instead of surfacing one: if that
/// invariant were ever broken by a later change, this function would
/// quietly return whichever correctly-typed candidate it reaches first
/// instead of failing loudly with `None`.
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

/// Populates `widget` (built by [`build`]) with `item`'s title, subtitle,
/// icon, and — issue #197 — its action hint, resolved against `keymap`.
/// `widget` is typed as a bare `gtk::Widget` rather than `gtk::Box` because
/// its caller, `ui::view::bind`, reaches it back out of a `gtk::Stack` page
/// by name — `gtk::Stack::child_by_name` hands back the general widget type
/// regardless of what was added, so the downcast belongs here, next to the
/// one place that knows a `Row` page's widget is actually the `gtk::Box`
/// [`build`] returns.
pub fn bind(widget: &gtk::Widget, item: &Item, keymap: &Keymap) {
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
    if let (Some(hint), Some(hint_label), Some(hint_key)) = (
        hint_widget(container),
        hint_label_widget(container),
        hint_key_widget(container),
    ) {
        resolve_hint(&hint, &hint_label, &hint_key, item, keymap);
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

/// Finds `item`'s own label for its *default* action — the
/// `hop_protocol::item::Action` in `item.actions` whose `id` matches
/// `item.default_action`, per `CONTEXT.md`'s **Action** glossary entry.
/// This is the item-wire vocabulary `Action` (`hop_protocol::item::Action`),
/// never [`crate::keymap::Action`] — [`resolve_hint`] imports that one under
/// the alias [`KeymapAction`] specifically so the two are never spelled the
/// same identifier anywhere in this module, per this issue's brief:
/// "`CONTEXT.md` flags [conflating the two `Action` types] as the obvious
/// mistake for exactly this issue."
///
/// [`hop_protocol::Item`] carries no such lookup itself —
/// `Item::default_action` is a bare `ActionId`, and matching it against
/// `Item::actions` is left to whoever needs the matching `Action`'s own
/// fields (here, its `label`). `None` covers the one malformed case a wire
/// peer could still produce despite every bound already enforced on each
/// field individually: `default_action` naming an id absent from `actions`
/// entirely. Nothing here trusts a provider to keep the two in sync, or
/// invents a placeholder label — [`resolve_hint`] treats this exactly like
/// a keymap with no binding for `Activate`: the row's hint slot renders
/// empty, never a panic and never a half-populated hint.
fn default_action_label(item: &Item) -> Option<&str> {
    item.actions
        .iter()
        .find(|action| action.id == item.default_action)
        .map(|action| action.label.as_str())
}

/// Whether [`resolve_hint`]'s label chip should render alongside the key
/// glyph, or the hint should collapse to the key glyph alone — issue #197's
/// responsive collapse, `assets/tokens.css`'s own GEOMETRY note: "the
/// action hint collapses to icon-only before it would be pushed
/// off-window."
///
/// # Why this reads the top-level surface, not the row's own allocation
///
/// GTK has no CSS media query — confirmed against a real, installed GTK
/// 4.14's CSS parser (`assets/stylesheet.css`'s own top doc comment makes
/// the identical finding for `:root`/`var()`/`@media`) — so nothing in
/// `assets/stylesheet.css` can express "collapse below N px" at all; this
/// has to come from real widget/window geometry read in Rust, per this
/// issue's brief.
///
/// `hint`'s own `gtk::Widget::native()` → `gtk::Native::surface()` →
/// `gdk::Surface::width()` chain reads the top-level window's *actual*,
/// currently-allocated pixel width directly from GDK — available the
/// moment the window is realized, independent of whether `hint`'s own
/// `gtk::Box` has been through a layout pass yet. A row's own
/// `gtk::Widget::width()` was considered and rejected: `GtkListView`
/// allocates a bound row's widget *after* `bind` runs (bind decides what
/// the row shows; allocation sizes it in response), so a freshly bound
/// row's own width reads back whatever a *previous* bind's content
/// allocated, or `0` for a row that has never been allocated at all — a
/// staler and strictly less useful number than the surface's, which is
/// current regardless of where this particular row sits in its own
/// bind/allocate cycle.
///
/// # Why this returns `true` (never collapse) with no surface to measure
///
/// A `hint` never yet added under a realized top-level window —
/// `tests/view_tree_renderer.rs`'s own "brief tests" section, which drives
/// `ui::view::bind` directly against a manufactured `gtk::ListItem` never
/// added to any window — has no honest answer to "is the window
/// constrained": collapsing by default would be a guess, and showing the
/// full hint by default is what every real, normally-sized window
/// (`--hop-window-w`'s own 400px) needs anyway. Same reasoning for a
/// surface reporting `0` or less, which `gdk::Surface::width()` can return
/// before its first real allocation.
///
/// # The threshold itself: measured, not guessed
///
/// This issue's brief is explicit that the collapse width must be
/// "deriv[ed] ... from measured geometry rather than pick[ed as] a
/// literal." The needed width is `ICON_SIZE_PX` (the row's other
/// fixed-size element, per `ui::row::build`'s own "the icon slot" doc
/// section) plus `hint_label`'s and `hint_key`'s own *natural* widths,
/// read fresh via [`gtk::Widget::measure`] — the same measurement
/// `tests/view_tree_renderer.rs`'s `row_layout` helper already trusts over
/// a `width_request` getter, for the identical reason: `measure` reflects
/// what the real, installed stylesheet's font/padding/tracking rules
/// (`--hop-text-hint-label`, `--hop-text-hint-key`, `--hop-tracking-hint`,
/// the chip padding) actually produce for `hint_label`'s and `hint_key`'s
/// *current* text, not a number typed into this function by hand. The
/// title and subtitle are deliberately left out of this sum: both
/// ellipsize (`ui::row::build`'s `EllipsizeMode::End`), so neither has a
/// minimum width this function would need to protect — the one thing that
/// cannot shrink and does not ellipsize is the hint itself, which is
/// exactly the element `assets/tokens.css`'s note says must not be "pushed
/// off-window."
fn should_show_label_chip(
    hint: &gtk::Widget,
    hint_label: &gtk::Label,
    hint_key: &gtk::Label,
) -> bool {
    let Some(surface_width) = hint
        .native()
        .and_then(|native| native.surface())
        .map(|surface| surface.width())
    else {
        return true;
    };
    if surface_width <= 0 {
        return true;
    }

    let (_, label_natural, _, _) = hint_label.measure(gtk::Orientation::Horizontal, -1);
    let (_, key_natural, _, _) = hint_key.measure(gtk::Orientation::Horizontal, -1);
    let needed = *tokens::ICON_SIZE_PX + label_natural + key_natural;
    surface_width >= needed
}

/// Resolves the row's right-aligned action hint onto its two chip widgets —
/// issue #197. Pairs [`default_action_label`]'s answer (the label chip)
/// with `keymap.binding_for(`[`KeymapAction::Activate`]`)`'s display string
/// (the key glyph, via [`Binding`]'s own [`fmt::Display`] convention — see
/// `crate::keymap`'s doc comment on that `impl` for the spelling rules), and
/// applies [`should_show_label_chip`]'s responsive collapse once both are
/// known to exist.
///
/// [`Binding`]: crate::keymap::Binding
///
/// # "Both halves or neither" — never a half-populated hint
///
/// This issue's brief is explicit: "An item with no default action, or a
/// keymap where `binding_for(Activate)` returns `None`, renders an *empty*
/// hint slot — not a half-populated one." A key glyph with no label reads
/// as an orphaned, meaningless badge; a label with no key glyph promises a
/// keyboard shortcut that does not exist. Both are worse than showing
/// nothing, so this function only ever writes `hint_label`/`hint_key` once
/// it holds *both* answers — the `let (Some(label), Some(key)) = (...)
/// else` below is the one place that rule is enforced, before either
/// widget is touched.
fn resolve_hint(
    hint: &gtk::Box,
    hint_label: &gtk::Label,
    hint_key: &gtk::Label,
    item: &Item,
    keymap: &Keymap,
) {
    let label = default_action_label(item);
    let key = keymap
        .binding_for(KeymapAction::Activate)
        .map(|binding| binding.to_string());

    let (Some(label), Some(key)) = (label, key) else {
        clear_hint(hint_label, hint_key);
        return;
    };

    hint_label.set_text(label);
    hint_key.set_text(&key);
    hint_key.set_visible(true);
    hint_label.set_visible(should_show_label_chip(
        hint.upcast_ref::<gtk::Widget>(),
        hint_label,
        hint_key,
    ));
}

/// Clears and hides both of the hint's chips — [`resolve_hint`]'s "neither"
/// branch, and [`unbind`]'s own symmetry with the title, subtitle, and icon.
fn clear_hint(hint_label: &gtk::Label, hint_key: &gtk::Label) {
    hint_label.set_text("");
    hint_label.set_visible(false);
    hint_key.set_text("");
    hint_key.set_visible(false);
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
    if let (Some(hint_label), Some(hint_key)) =
        (hint_label_widget(container), hint_key_widget(container))
    {
        clear_hint(&hint_label, &hint_key);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use hop_protocol::{Action, ActionId, ActionKind, ItemId, ItemTitle, Kind};

    use super::*;

    /// A minimal, GTK-free item — `default_action_label` touches only
    /// `hop_protocol::Item` fields, so this needs no `gtk::init()`, unlike
    /// almost everything else in this module (see this module's top doc
    /// comment, "build and bind never animate", and
    /// `tests/view_tree_renderer.rs`'s own module doc for why the rest of
    /// this file's behavior can only be proven under a real broadway
    /// display).
    fn item_with_actions(default_action: &str, actions: Vec<(&str, &str)>) -> Item {
        Item {
            id: ItemId::new("test:1").unwrap(),
            kind: Kind::Action,
            title: ItemTitle::new("test item").unwrap(),
            subtitle: None,
            icon: None,
            actions: actions
                .into_iter()
                .map(|(id, label)| Action {
                    id: ActionId::new(id).unwrap(),
                    kind: ActionKind::Open,
                    label: label.to_string(),
                })
                .collect(),
            default_action: ActionId::new(default_action).unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "test".to_string(),
        }
    }

    #[test]
    fn default_action_label_finds_the_matching_action() {
        let item = item_with_actions("open", vec![("open", "Open"), ("copy", "Copy")]);
        assert_eq!(default_action_label(&item), Some("Open"));
    }

    #[test]
    fn default_action_label_is_none_when_the_id_matches_no_action() {
        // The malformed case this function's own doc comment names: a
        // `default_action` naming an id `actions` does not carry — nothing
        // here should invent a label or panic.
        let item = item_with_actions("archive", vec![("open", "Open")]);
        assert_eq!(default_action_label(&item), None);
    }

    #[test]
    fn default_action_label_is_none_when_the_item_has_no_actions_at_all() {
        let item = item_with_actions("open", vec![]);
        assert_eq!(default_action_label(&item), None);
    }
}
