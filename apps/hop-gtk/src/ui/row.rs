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
//! `hop_protocol::item::Action` is the only `Action` vocabulary this module
//! ever names — see [`default_action_label`]'s own doc comment for the
//! distinction from `crate::keymap::Action`, `CONTEXT.md`'s **Action**
//! glossary entry, and this issue's own brief, all of which flag conflating
//! the two as the obvious mistake for exactly this issue's own element.
//! Issue #197 review, finding 3, is what keeps `crate::keymap::Action` from
//! ever needing to be named here at all, rather than merely under a
//! disambiguating alias: [`resolve_hint`] used to call
//! `keymap.binding_for(`[`crate::keymap::Action::Activate`]`)` itself,
//! which meant this module imported `crate::keymap::Action` (aliased
//! `KeymapAction`) purely to spell that one call. That lookup now lives on
//! [`crate::keymap::Keymap::activate_binding_display`] instead, called once
//! by `ui::view::build` rather than once per bind here — see that method's
//! own doc comment, and `ui::view::Node`'s, for the full account — so
//! [`resolve_hint`] receives the already-formatted display string as a
//! plain `Option<&str>` and this module has had no reason to import
//! `crate::keymap` at all since.
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
//! # `build` and `bind` do not blindly animate
//!
//! A `GtkListView` factory reuses the *same* row widget across many
//! different items as the list is scrolled — that reuse is the whole point
//! (recycling, not destroy-and-rebuild), and it holds just as much now that
//! [`bind`] is called from `ui::view::bind`'s dispatch rather than straight
//! from a `connect_bind` signal handler. Before issue #207, neither
//! function below started an animation, or anything that could grow into
//! one by accident — both were a straight-line read of a field into a
//! widget, and this section's title said so outright: "`build` and `bind`
//! *never* animate."
//!
//! # Issue #207: the one narrow exception, and why it does not violate the
//! rule above
//!
//! [`bind`] now can start the hint's entrance fade — but only on a
//! *genuine* not-shown-to-shown transition for the widget currently bound,
//! never merely because `bind` ran. The hazard this section used to warn
//! against — an entrance animation wired into `bind` replaying on every
//! scroll step — is exactly the failure mode [`sync_hint_shown_class`]
//! exists to rule out: `ui::view::build`'s own factory calls `unbind` before
//! every `bind` that reassigns a recycled slot's item (`ui::view::unbind`'s
//! doc comment confirms this against `SignalListItemFactory`'s own
//! semantics), and [`unbind`]/[`clear_hint`] reset the hint chips'
//! *visibility* on every one of those — but deliberately **never** touch
//! [`HINT_SHOWN_CLASS`]. That class is this widget's own persistent memory
//! of "was the hint genuinely showing the last time `bind` decided," and it
//! survives the unbind/bind pair untouched specifically so [`bind`] can
//! tell "stayed shown across a recycle" (no class churn, no fade) apart
//! from "genuinely just appeared" (class absent, then added, fade plays) —
//! without reaching for `unsafe` GObject qdata (this crate denies
//! `unsafe_code`) to store that memory anywhere else. See [`bind`]'s own
//! doc comment, "the recycling constraint", for the mechanism in full, and
//! [`hint_entered_shown`]'s doc comment for the pure decision at its core.
//!
//! [`build`] itself is unchanged by this: it still never animates anything,
//! and the CSS class it *does* add ([`HINT_CHILD_NAME`], now doubled as a
//! style class too — see that constant's own doc comment) only ever
//! establishes the hint's base, un-shown `opacity: 0` state
//! (`assets/stylesheet.css`'s `.hop-row-hint` rule), never the `-shown`
//! modifier that actually triggers a transition.
//!
//! # Issue #254: clickable action icons — mouse parity of affordance
//!
//! SPEC decision 5/6 (`docs/design/2026-08-22-design-refresh/SPEC.md`) asks
//! for "per-row action icons [that] appear on hover/selection for mouse"
//! and, decision 6's own wording, "click an action icon = that exact
//! action" — a *different* requirement from the hint above: the hint names
//! one action (the default) as text, for keyboard users, unconditionally
//! once bound; this is a small number of genuinely clickable buttons, for
//! mouse users, that only appear while the pointer is over the row (or the
//! row is the keyboard selection — see `assets/stylesheet.css`'s own
//! `.hop-row-actions` comment for why hover *and* selection both matter).
//!
//! ## How many icons: [`ROW_ACTION_ICON_CAP`], not `item.actions.len()`
//!
//! See that constant's own doc comment for the bound and why it is fixed
//! at 2, independent of how many actions a provider declares (up to
//! [`hop_protocol::limits::MAX_ACTIONS_PER_ITEM`], 32) — the short version:
//! one icon would only duplicate what clicking the row's own body already
//! does (decision 6: "click row = default action"), two covers the default
//! plus the next most useful action without needing a second, measurement-
//! driven collapse the way [`should_show_label_chip`] needs for the hint,
//! and the ctrl-K/right-click action panel (`ui::action_panel`, issue #254
//! phase 1) is the deliberate, already-built overflow route for the rest —
//! nothing an item can do becomes unreachable by mouse, only the long tail
//! costs one more click or keystroke to reach.
//!
//! ## Which two: the first [`ROW_ACTION_ICON_CAP`] of `item.actions`, in
//! wire order
//!
//! Not "the default action plus the next distinct one" — that would need
//! [`default_action_label`]'s own id-search repeated here, and a provider
//! is already free to put its most mouse-relevant actions first in
//! `item.actions` if it wants them to be the ones a row surfaces (the wire
//! protocol makes no promise that `default_action` is `actions[0]`, and
//! this module does not invent one). [`resolve_action_icons`] is the one
//! place this rule is applied; see its own doc comment.
//!
//! ## How a click runs the *right* action: a GAction target, not a
//! `gtk::ListItem`-capturing closure
//!
//! A row's action-icon buttons are built exactly once, in [`build`], and
//! rebound to a different item's different actions on every recycle — the
//! identical constraint [`HINT_SHOWN_CLASS`]'s own doc comment describes
//! for the hint, except here the data that must survive a recycle without
//! going stale is not a boolean fade flag but *which item and which action*
//! a click should send.
//!
//! An earlier draft of this doc comment claimed here that two shapes were
//! considered and both dead-ended without `unsafe` GObject qdata, making
//! the GAction shape below the only one that worked at all. That claim was
//! false, and this crate's own `ui::view` module is what disproves it:
//! issue #181's `ui::view::build` keeps the `&gtk::ListItem` GTK hands its
//! `connect_setup` closure alive for that slot's entire recycled lifetime
//! (GTK reuses the *same* `ListItem` object across every rebind of one
//! visual row), and its `connect_bind`/`connect_unbind` closures call
//! `list_item.item()` to read back whichever object is *currently* bound,
//! decoded through [`crate::ui::model::item_of`]. A `connect_clicked`
//! closure built once in [`build`] could equally have cloned that same
//! `gtk::ListItem` handle — an ordinary, reference-counted GObject clone,
//! not qdata or any interior-mutability trick of this crate's own — and,
//! at click time, called `.item()` on it fresh to find out which item, and
//! which `item.actions[slot]`, that click should send. No `Rc<RefCell<_>>`
//! bridging one `build` call to a later `bind` call would even be needed:
//! the `ListItem` itself is already the thing GTK keeps alive across the
//! recycle. That shape is genuinely `unsafe`-free, exactly like the one
//! actually used below — the choice between them was a design judgment,
//! not a safety one, and the two shapes below are named for the judgment
//! that separates them, not to claim only one of them could have worked.
//!
//! - **A `connect_clicked` closure capturing a cloned `gtk::ListItem`,
//!   read back via `.item()` at click time.** Rejected on layering, not
//!   safety: nothing under `ui::row` today — not [`build`], not [`bind`],
//!   not [`unbind`] — takes or names a `gtk::ListItem` anywhere, and that
//!   is this module's own boundary, drawn deliberately by issue #181 (see
//!   this file's top doc comment, above): `ui::view` owns the
//!   `GtkListView` factory and everything `gtk::ListItem`-shaped, and
//!   dispatches down to this module's plain [`build`]/[`bind`]/[`unbind`]
//!   functions with already-resolved, `gtk::ListItem`-free values (an
//!   [`Item`], a `&gtk::Widget`) once it has done that resolving. Giving
//!   [`build`] a `&gtk::ListItem` parameter so its buttons' closures could
//!   hold onto it would thread a view-layer, list-recycling type into the
//!   one module in this crate whose whole job is to not need to know that
//!   machinery exists — an ongoing coupling cost paid on every later
//!   change to how a row is built, not a one-time expedient.
//! - **A [`gio::SimpleAction`] target, set via
//!   [`gtk::prelude::ActionableExt::set_action_target_value`].** Chosen.
//!   GTK's own `Actionable` interface already gives every widget an
//!   `action-target` *property* — ordinary GObject state GTK stores and
//!   retrieves for the widget itself, reachable from exactly the
//!   `&gtk::Widget`/`&gtk::Box`-shaped functions this module already has
//!   ([`bind`], via [`resolve_action_icons`]) — so this mechanism needs no
//!   new parameter threaded through the `build`/`bind`/`unbind` boundary
//!   the rejected shape above would require: [`resolve_action_icons`] can
//!   simply call `set_action_target_value` again on every `bind`, exactly
//!   like it already calls `set_icon_name`/`set_tooltip_text` on the same
//!   button. [`build`] sets each button's `action-name` once
//!   (`{{ROW_ACTION_GROUP_PREFIX}}.{{ROW_ACTION_NAME}}`, see those
//!   constants' own doc comment) and never touches it again; only the
//!   *target* — an `(item_id, action_id)` pair, packed as a `(String,
//!   String)` [`glib::Variant`] via [`glib::variant::ToVariant`] — changes
//!   per bind. `ui::window::HopWindow::build` is where the actual
//!   [`gio::SimpleAction`] this name resolves to is registered, and where
//!   the target is unpacked back into an [`hop_protocol::ItemId`]/
//!   [`hop_protocol::ActionId`] pair and turned into an `IpcCommand::
//!   Execute` — this module never imports `crate::ipc` to do any of that,
//!   matching `ui::action_panel`'s own "this is the widget, not the
//!   wiring" scope discipline.
//!
//! ## Icon glyph: derived from [`hop_protocol::ActionKind`], not carried
//! on the wire
//!
//! [`hop_protocol::item::Action`] has no icon field of its own — only
//! `id`, `kind`, and `label` (the same three fields
//! `ui::action_panel::kind_hint` already maps `kind` off of, for that
//! panel's own trailing kind hint). [`action_kind_icon_name`] is this
//! module's own exhaustive mapping from [`ActionKind`] to a themed,
//! symbolic icon name, deliberately mirroring [`resolve_icon`]'s own
//! `IconSpec::Name` arm: [`gtk::Button::set_icon_name`] hands the lookup to
//! GTK's icon theme the same way [`gtk::Image::set_icon_name`] already does
//! for an item's own leading icon, and GTK's own documented fallback
//! (`image-missing`-shaped) covers a theme that lacks the name — no second
//! fallback mechanism is invented here for a Button that already has one.
//!
//! ## Issue #254 review, finding 4 (maintainer decision, 2026-08-23): the
//! overflow chevron
//!
//! AC2 asks for "mouse parity of affordance, not just outcome" — every
//! action clickable through a row hover icon *or* the ctrl-K/right-click
//! panel. [`ROW_ACTION_ICON_CAP`]'s own doc comment already argues the
//! panel is where the long tail past two icons goes, but a row whose item
//! declares a third action gave a mouse user no on-row hint that a long
//! tail exists at all — the panel was reachable, never discoverable, from
//! the row itself. The maintainer's ruling: keep the 2-icon cap (unchanged
//! by this finding), and add a third, trailing affordance — the overflow
//! chevron below — that opens the panel, anchored at that point, whenever
//! a row genuinely has more to offer than its two dedicated icons show.
//!
//! [`build`] appends this button as the *third* child of `actions_wrapper`,
//! after the [`ROW_ACTION_ICON_CAP`] action-icon buttons and still inside
//! the same `.hop-row-actions` wrapper those buttons already share — not a
//! fourth, independent widget with a fade rule of its own. That placement
//! is what this finding's own "fades in ... through the same pure-CSS
//! state mechanism" requirement gets for free: `assets/stylesheet.css`'s
//! `.hop-row-actions` opacity rule already keys off `listview > row:hover`/
//! `:selected` with no Rust-driven class at all (see that rule's own
//! comment for why), and every child of that wrapper — this one included —
//! fades with it identically. Nothing new is needed on the Rust side to
//! make the chevron fade; only [`resolve_overflow_button`] deciding whether
//! it is visible *at all* for the currently bound item is new.
//!
//! ### When it shows: `item.actions.len() > `[`ROW_ACTION_ICON_CAP`]`, every
//! bind, unconditionally
//!
//! [`resolve_overflow_button`] recomputes this fresh on every [`bind`], the
//! same "no before/after comparison, just the current item's own length"
//! discipline [`resolve_action_icons`]'s own "the recycling constraint"
//! section already established for the two dedicated icons — not a
//! variant of [`HINT_SHOWN_CLASS`]'s shown/hidden memory, because nothing
//! here depends on what the *previous* bind on this recycled widget
//! decided. A row recycled from a three-action item (chevron shown) onto a
//! one-action item must show no chevron the instant it rebinds, and a
//! plain, unconditional recomputation already guarantees that with no
//! flag to forget to clear — see `tests/view_tree_renderer.rs`'s own
//! "issue #254 review, finding 4" assertions, driven through the same
//! 1→0→2→3→1-action recycling sequence that section's original coverage
//! already binds one widget through.
//!
//! Exactly `>`, not `>=`: an item with precisely [`ROW_ACTION_ICON_CAP`]
//! actions already has a dedicated icon for every one of them — showing a
//! chevron there would open a panel listing nothing the row does not
//! already offer directly, the exact "empty mystery box" shape
//! `ui::action_panel`'s own "Zero actions" section refuses for a different
//! reason. The chevron exists only for the genuine long tail.
//!
//! ### The glyph: `view-more-symbolic`, not anything from `mocks3.html`
//!
//! `docs/design/2026-08-22-design-refresh/mocks3.html`, the one approved
//! frame this design refresh works from, never renders a row with more
//! than two actions, so it names no glyph for this case at all. `view-more-
//! symbolic` is chosen here instead: it is the symbolic icon GNOME's own
//! icon themes ship specifically for an in-content "more options" overflow
//! affordance (the horizontal-dots glyph GTK/GNOME apps already use for
//! exactly this shape of "there is more here, click to see it" button),
//! resolved through [`gtk::Button::set_icon_name`] the identical way
//! [`action_kind_icon_name`]'s own icons are — GTK's own documented
//! fallback covers a theme that lacks it, so no second fallback mechanism
//! is invented here either. Set once, in [`build`]: unlike an action
//! icon's glyph (one of seven [`ActionKind`]-derived names, chosen fresh
//! per bind), this chevron always means the same thing regardless of which
//! item is bound, so its icon name never needs to change.
//!
//! ### No new size or spacing literal
//!
//! The chevron reuses [`ACTION_ICON_CLASS`] (`assets/stylesheet.css`'s
//! `.hop-row-action-icon`/`.hop-row-overflow-icon` rule pair — see that
//! rule's own comment) for its size and hover treatment, and
//! `actions_wrapper`'s own [`tokens::HINT_CHIP_GAP_PX`] gap (the
//! `gtk::Box::new` constructor argument, already applied uniformly between
//! every child that wrapper holds) for its spacing from the icon before
//! it — both already-declared tokens, spent again rather than a third
//! value invented for what is, visually, the same size of button in the
//! same row.
//!
//! ### A second GAction, not a third `(item_id, action_id)` target
//!
//! The chevron does not run an action — it opens [`ui::action_panel`] for
//! this row's *item*, so its own GAction target is a bare item id
//! ([`ROW_OPEN_ACTIONS_TARGET_TYPE`], `"s"`), not the `(ss)` pair
//! [`ROW_ACTION_TARGET_TYPE`] names for [`ROW_ACTION_NAME`]. Reusing
//! `ROW_ACTION_NAME` with a synthetic, meaningless action id in the second
//! slot of that same pair would be dishonest about what the click actually
//! does and would need `ui::window::HopWindow`'s own handler to
//! special-case that fake id — a second GAction,
//! [`ROW_OPEN_ACTIONS_NAME`] ("open-actions"), installed under the same
//! [`ROW_ACTION_GROUP_PREFIX`] group, says plainly that this button does a
//! *different* thing, with a parameter type that only ever holds what that
//! different thing needs. `ui::window::HopWindow::build` is, again, where
//! this name resolves to a real `gio::SimpleAction`, and where clicking it
//! is turned into the same select-then-present path a right-click already
//! uses ([`ui::window::HopWindow::present_action_panel_for_selected`]) —
//! this module never imports `ui::action_panel` or `crate::ipc` to do any
//! of that, the identical "this is the widget, not the wiring" boundary
//! its dedicated action icons already keep.
//!
//! ## No text selection inside these rows
//!
//! SPEC decision 6, verbatim: "no text selection inside rows (the copy
//! action owns that)." [`gtk::Label`]'s own documented default for
//! `selectable` is already `false`, so [`title_widget`]'s and
//! [`subtitle_widget`]'s labels were never selectable — but [`build`] now
//! sets `set_selectable(false)` on both explicitly rather than leaning on
//! that default silently, the identical judgment
//! `ui::action_panel::ActionPanel::reset_selection`'s own doc comment
//! already makes for a different GTK default ("state this crate's contract
//! explicitly rather than lean on a GTK default's incidental behavior").
//! The hint's two chips get the same explicit call for the same reason,
//! even though their text is short-lived and unlikely to be dragged over
//! by accident.

use std::io::Read;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use glib::variant::ToVariant;
use gtk::prelude::*;
use gtk::{gdk, glib};

use hop_protocol::{ActionKind, IconPath, IconSpec, Item, ItemSubtitle, Kind};

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
static OFFLINE_STATE: AtomicBool = AtomicBool::new(false);
static OFFLINE_SNAPSHOT: Mutex<Option<String>> = Mutex::new(None);
const STAMP_CHILD_NAME: &str = "hop-row-stamp";

/// The widget name — and, since issue #207, the CSS class too — [`build`]
/// gives the hint's own horizontal `gtk::Box`, the third direct child of
/// the outer row container (issue #197). Single identity until issue #207:
/// nothing styled the wrapper itself, only its two chip children below, so
/// this name existed solely for [`hint_widget`]'s lookup, the same
/// single-identity shape [`ICON_CHILD_NAME`]/[`TITLE_CHILD_NAME`] still use.
/// Issue #207 doubled it — the same reasoning [`SUBTITLE_CHILD_NAME`]'s own
/// doc comment gives, applied here for the first time to this widget:
/// `assets/stylesheet.css`'s `.hop-row-hint` rule (base, un-shown
/// `opacity: 0`) needs a selector to style the wrapper by, and
/// [`find_named_child`] still needs the same string as a name, so one
/// constant serves both rather than risking the two drifting apart.
const HINT_CHILD_NAME: &str = "hop-row-hint";

/// The CSS class [`bind`] adds to the hint's wrapper on a genuine
/// not-shown-to-shown transition, and leaves alone (never removes, and
/// never redundantly re-adds) for as long as the hint stays shown across
/// however many later binds recycle this same widget — see [`bind`]'s own
/// doc comment, "the recycling constraint," and this module's "Issue #207"
/// top-level doc section for the full mechanism this class is the load-
/// bearing piece of.
///
/// Doing double duty, deliberately, the same way [`HINT_CHILD_NAME`] now
/// does: it is both the trigger `assets/stylesheet.css`'s
/// `.hop-row-hint.hop-row-hint-shown` rule matches on (what actually plays
/// the entrance fade) *and* the one piece of state this recycled widget
/// carries across an `unbind`/`bind` pair that [`unbind`] never resets —
/// unlike the hint chips' own visibility, which it does — making it this
/// mechanism's substitute for the `unsafe` GObject qdata storage this
/// crate's `unsafe_code = "deny"` lint rules out.
///
/// `pub` for the same reason [`icon_widget`] is: `tests/view_tree_renderer.rs`
/// reads this class back via [`gtk::Widget::has_css_class`] to prove the
/// recycling distinction directly, at the level of observable widget state
/// rather than animation timing or pixels.
pub const HINT_SHOWN_CLASS: &str = "hop-row-hint-shown";

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

/// The maximum number of an item's actions that get a dedicated, always-
/// built row icon — this module's top doc comment, "Issue #254: clickable
/// action icons", "How many icons" section, has the full argument; this is
/// the short version pinned as a named constant rather than a bare literal
/// wherever a slot count is needed below.
///
/// [`hop_protocol::limits::MAX_ACTIONS_PER_ITEM`] permits up to 32 actions
/// per item; a row is a fixed, 56px-tall (`tokens::ROW_HEIGHT_PX`) band
/// that already carries a leading icon, a title, a subtitle, and the
/// hint's two chips, all sized and positioned before any item is ever
/// bound (this module's "fixed-height reserved rows" and "the icon slot"
/// sections) — 32 icon buttons would neither fit nor read as affordance,
/// and worse, would make the row's own reserved trailing width a function
/// of `item.actions.len()`, which every other element of this widget goes
/// out of its way *not* to be.
///
/// Fixed at exactly **2**: one icon alone would only ever mirror what
/// clicking the row's own body already does (SPEC decision 6: "click row =
/// default action"), buying no new affordance. A second slot gives mouse
/// users a one-click path to whichever action `item.actions` lists next
/// (see [`resolve_action_icons`]'s own doc comment for exactly which two),
/// without this module needing a [`should_show_label_chip`]-shaped
/// measured collapse to decide how many fit — a fixed count of built
/// widgets, hidden per [`resolve_action_icons`] rather than reflowed, is
/// enough because two 26px (`tokens::ICON_SIZE_PX`) buttons plus their gap
/// never come close to contending with the hint's own responsive-collapse
/// budget. Every action beyond this cap — on any item that has more than
/// two — is still reachable, in full, through `ui::action_panel`'s already-
/// built ctrl-K/right-click panel; this cap only ever decides how many get
/// a *dedicated row icon*, never how many are runnable at all.
pub const ROW_ACTION_ICON_CAP: usize = 2;

/// The GAction group prefix and bare action name every action-icon button
/// [`build`] constructs invokes on click (composed together, see
/// [`build`]'s own call site) — spelled once here rather than as a literal
/// at the two places that must agree on it: this module's [`build`], which
/// calls [`gtk::prelude::ActionableExt::set_action_name`] with the composed
/// string, and `ui::window::HopWindow::build`, which registers the
/// `gio::SimpleAction` this prefix and name resolve to and installs it
/// under this exact prefix via `gtk::prelude::WidgetExt::insert_action_group`.
/// The same "one name, spelled once" discipline [`SUBTITLE_CHILD_NAME`]'s
/// own doc comment argues for a widget-name/CSS-class pair, applied here to
/// a GAction prefix/name pair instead. See this module's top doc comment,
/// "How a click runs the right action", for why a GAction — rather than a
/// `connect_clicked` closure with its own separately-tracked mutable state —
/// is the mechanism at all.
pub const ROW_ACTION_GROUP_PREFIX: &str = "row";
/// The bare action name half of the pair [`ROW_ACTION_GROUP_PREFIX`]'s own
/// doc comment describes.
pub const ROW_ACTION_NAME: &str = "run-action";

/// The GVariant type string every action-icon button's own action target
/// carries — an `(item_id, action_id)` pair of strings, per this module's
/// top doc comment, "How a click runs the right action". Named once here so
/// [`build`]'s own `glib::VariantTy::new` call and
/// `ui::window::HopWindow::build`'s matching `gio::SimpleAction::new` call
/// (which must declare the identical parameter type for the action to ever
/// activate at all) read off the same string rather than two hand-typed
/// copies of `"(ss)"` that could silently drift apart.
pub const ROW_ACTION_TARGET_TYPE: &str = "(ss)";

/// The widget name **and** CSS class [`build`] gives the wrapper `gtk::Box`
/// holding the row's action-icon buttons — the doubled-identity precedent
/// [`SUBTITLE_CHILD_NAME`]'s own doc comment documents, applied here so
/// `assets/stylesheet.css`'s `.hop-row-actions` rule (the hover/selection
/// fade — see that rule's own comment for why this is plain `:hover`/
/// `:selected`, with no Rust-driven "-shown" class the way [`HINT_CHILD_NAME`]
/// needs one) has a selector, and [`find_named_child`] has a name, from one
/// string rather than two that could drift.
const ACTIONS_CHILD_NAME: &str = "hop-row-actions";

/// The CSS class every action-icon button carries, shared across all
/// [`ROW_ACTION_ICON_CAP`] of them — unlike [`HINT_LABEL_CHILD_NAME`]/
/// [`HINT_KEY_CHILD_NAME`], which need two *different* classes because the
/// label and key chips carry two different typographic treatments, every
/// action-icon button gets the identical visual treatment
/// (`assets/stylesheet.css`'s `.hop-row-action-icon` rule), so one shared
/// class is the right shape here, not one class per button. Each button
/// still gets its own, *distinct* widget name — see
/// [`action_icon_widget_name`] — since a shared name would make
/// [`find_named_child`] unable to tell the buttons apart at all.
const ACTION_ICON_CLASS: &str = "hop-row-action-icon";

/// The widget name **and** CSS class of the trailing overflow chevron —
/// issue #254 review, finding 4 (this module's top doc comment, "the
/// overflow chevron", has the full account). Doubled identity, the same
/// [`SUBTITLE_CHILD_NAME`] precedent every other named-and-styled child in
/// this module already follows: [`find_named_child`] needs the name,
/// `assets/stylesheet.css`'s `.hop-row-overflow-icon` rule needs the class.
/// This button also carries [`ACTION_ICON_CLASS`] (see [`build`]) for the
/// size/hover treatment it shares with the two dedicated action icons —
/// this second class is what lets a future rule style the chevron alone
/// without touching that shared one.
const OVERFLOW_CHILD_NAME: &str = "hop-row-overflow-icon";

/// The symbolic icon name [`build`] gives the overflow chevron — see this
/// module's top doc comment, "The glyph: `view-more-symbolic`", for why
/// this exact name and why it is set once, in [`build`], rather than
/// re-resolved on every [`bind`] the way [`action_kind_icon_name`]'s
/// per-action icons are.
const OVERFLOW_ICON_NAME: &str = "view-more-symbolic";

/// The tooltip [`build`] gives the overflow chevron — fixed, unlike an
/// action icon's own tooltip (that exact action's label): this button
/// always does the same thing regardless of which item is bound, so its
/// tooltip never needs to change on [`bind`] either.
const OVERFLOW_TOOLTIP: &str = "More actions";

/// The GAction name the overflow chevron invokes — see this module's top
/// doc comment, "A second GAction, not a third `(item_id, action_id)`
/// target", for why this is a distinct action from [`ROW_ACTION_NAME`]
/// rather than a second, synthetic entry sharing its target shape.
/// Composed with [`ROW_ACTION_GROUP_PREFIX`] the identical way
/// [`ROW_ACTION_NAME`] is, by [`build`] and by
/// `ui::window::HopWindow::build`, which must agree on the same string.
pub const ROW_OPEN_ACTIONS_NAME: &str = "open-actions";

/// The GVariant type string the overflow chevron's own action target
/// carries — a bare item id, unlike [`ROW_ACTION_TARGET_TYPE`]'s
/// `(item_id, action_id)` pair, because opening the panel for an item
/// needs no particular action singled out. Named once here for the same
/// "one string, not two hand-typed copies" reason [`ROW_ACTION_TARGET_TYPE`]
/// is.
pub const ROW_OPEN_ACTIONS_TARGET_TYPE: &str = "s";

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

/// The row's offline cache age stamp, shown only while the list is in the
/// disconnected state.
pub fn stamp_widget(container: &gtk::Box) -> Option<gtk::Label> {
    find_named_child(container, STAMP_CHILD_NAME)
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

/// The action-icons wrapper — the `gtk::Box` `assets/stylesheet.css`'s
/// `.hop-row-actions` rule fades in and out, added by issue #254.
pub fn action_icons_widget(container: &gtk::Box) -> Option<gtk::Box> {
    find_named_child(container, ACTIONS_CHILD_NAME)
}

/// The widget name [`build`] gives the `slot`-th action-icon button
/// (`slot` in `0..ROW_ACTION_ICON_CAP`) — computed from `slot` rather than
/// drawn from a fixed list of named constants, so [`ROW_ACTION_ICON_CAP`]
/// stays the *one* place this row's icon count is decided: bumping it
/// needs no matching bump to a hand-written list of name constants here.
/// [`find_named_child`] still resolves each button by this exact string —
/// the same "name, not position" discipline every other named child in
/// this module already follows, only computed rather than hand-spelled.
fn action_icon_widget_name(slot: usize) -> String {
    format!("hop-row-action-icon-{}", slot + 1)
}

/// The row's `slot`-th action-icon button (`slot` in
/// `0..ROW_ACTION_ICON_CAP`), or `None` if `slot` is out of range or the
/// widget cannot be found. `pub` for the same reason [`icon_widget`] is:
/// `tests/view_tree_renderer.rs` and this module's own `#[cfg(test)]`
/// module both need to reach the exact widget instance [`bind`]/[`unbind`]
/// mutate, rather than a second, independently-derived handle to it.
pub fn action_icon_widget(container: &gtk::Box, slot: usize) -> Option<gtk::Button> {
    find_named_child(container, &action_icon_widget_name(slot))
}

/// The row's trailing overflow chevron — issue #254 review, finding 4.
/// `pub` for the same reason [`action_icon_widget`] is:
/// `tests/view_tree_renderer.rs` reaches this exact widget instance
/// [`resolve_overflow_button`]/[`clear_overflow_button`] mutate.
pub fn overflow_button_widget(container: &gtk::Box) -> Option<gtk::Button> {
    find_named_child(container, OVERFLOW_CHILD_NAME)
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
    // SPEC decision 6: "no text selection inside rows (the copy action
    // owns that)." `gtk::Label`'s own documented default for `selectable`
    // is already `false`, so this is not changing this label's behaviour —
    // it is stating the contract explicitly rather than leaning on that
    // default silently, the same judgment this module's top doc comment's
    // "No text selection inside these rows" section names.
    title.set_selectable(false);
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
    // See `title.set_selectable(false)`'s own comment just above — the
    // identical explicit statement of SPEC decision 6's "no text selection
    subtitle.set_selectable(false);
    text_column.append(&subtitle);
    container.append(&text_column);

    // Cached rows keep their age metadata at the trailing edge, beside
    // actions and the keyboard hint, rather than in the title/subtitle
    // column. This preserves the approved frame's mono "as of HH:MM"
    // treatment without changing title centring for live rows.
    let stamp = gtk::Label::new(None);
    stamp.set_widget_name(STAMP_CHILD_NAME);
    stamp.add_css_class(STAMP_CHILD_NAME);
    stamp.add_css_class("hop-honesty-stamp");
    stamp.set_xalign(0.0);
    stamp.set_ellipsize(gtk::pango::EllipsizeMode::End);
    stamp.set_visible(false);
    stamp.set_selectable(false);
    let stamp_wrapper = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    stamp_wrapper.add_css_class("hop-honesty");
    stamp_wrapper.set_valign(gtk::Align::Center);
    stamp_wrapper.set_margin_start(*tokens::HINT_MARGIN_START_PX);
    stamp_wrapper.append(&stamp);
    container.append(&stamp_wrapper);

    // The action-icon buttons — issue #254. A third direct child of the
    // outer horizontal `container`, appended *before* `hint` below (not
    // nested inside `text_column`, for the identical reason `hint` itself
    // is not — see this module's "Issue #197" doc section above): both are
    // trailing, vertically-centred affordances at the row's own trailing
    // edge, not more lines of the title/subtitle stack. `text_column`'s own
    // `hexpand(true)` (set above) still supplies every pixel neither the
    // icon slot, this wrapper, nor `hint` claims.
    //
    // See this module's top doc comment, "Issue #254: clickable action
    // icons", for why exactly `ROW_ACTION_ICON_CAP` buttons are built here,
    // why each one's action-*name* is fixed at build time while its
    // action-*target* is the only thing `bind` ever changes, and why a
    // GAction target — not a `connect_clicked` closure capturing a cloned
    // `gtk::ListItem` — is the mechanism chosen here: both are `unsafe`-free,
    // and the doc comment linked above names the real (layering, not
    // safety) reason this one won.
    let actions_wrapper = gtk::Box::new(gtk::Orientation::Horizontal, *tokens::HINT_CHIP_GAP_PX);
    actions_wrapper.set_widget_name(ACTIONS_CHILD_NAME);
    actions_wrapper.add_css_class(ACTIONS_CHILD_NAME);
    actions_wrapper.set_valign(gtk::Align::Center);
    // Reusing the hint's own start-margin token rather than inventing a
    // second one for an identical concept ("the gap between the text
    // column and the next trailing element") — see `tokens::
    // HINT_MARGIN_START_PX`'s own doc comment; nothing about that value is
    // hint-specific in `tokens.css` itself (it resolves `--hop-space-3`,
    // the generic spacing scale), only its Rust name is.
    actions_wrapper.set_margin_start(*tokens::HINT_MARGIN_START_PX);

    let row_action_full_name = format!("{ROW_ACTION_GROUP_PREFIX}.{ROW_ACTION_NAME}");
    for slot in 0..ROW_ACTION_ICON_CAP {
        let button = gtk::Button::new();
        button.set_widget_name(&action_icon_widget_name(slot));
        button.add_css_class(ACTION_ICON_CLASS);
        // A plain icon-only affordance, not GTK's own raised button chrome
        // — `.flat` is the standard GNOME idiom for exactly this shape of
        // small, in-content clickable icon (a toolbar button, a list row's
        // own inline action), and `assets/stylesheet.css` carries no rule
        // of its own for a bare `button`/`.flat` that this could collide
        // with (checked: this file declares neither selector anywhere
        // before this issue).
        button.add_css_class("flat");
        // Hidden until `bind`/`resolve_action_icons` decides this slot has
        // a real action for the bound item — "hide, don't reserve", this
        // module's own precedent (see "The absent case" section above) for
        // every other optional row element: a freshly built row's very
        // first bind might resolve to zero or one action, and this button
        // must occupy no space, and be un-clickable, before that decision
        // has ever been made even once.
        button.set_visible(false);
        // The action *name* is fixed here, once, and [`bind`] never
        // touches it again — only the action *target*
        // ([`resolve_action_icons`], every bind) changes per rebind. See
        // [`ROW_ACTION_GROUP_PREFIX`]'s own doc comment for why a GAction,
        // whose `action-target` property GTK itself stores and hands back
        // per widget instance, is the mechanism this module uses to make
        // that split possible with no new parameter threaded through the
        // `build`/`bind`/`unbind` boundary — not the only `unsafe`-free
        // shape available, per that doc comment's own account.
        button.set_action_name(Some(&row_action_full_name));
        actions_wrapper.append(&button);
    }

    // The overflow chevron — issue #254 review, finding 4. A third child
    // of `actions_wrapper`, after the `ROW_ACTION_ICON_CAP` action-icon
    // buttons above and inside the same wrapper (not a fourth, standalone
    // widget) so it fades in and out with them through the identical,
    // Rust-bookkeeping-free `.hop-row-actions` opacity rule — see this
    // module's top doc comment, "the overflow chevron", for the full
    // account of why this button lives here rather than beside `hint`.
    let overflow_button = gtk::Button::new();
    overflow_button.set_widget_name(OVERFLOW_CHILD_NAME);
    overflow_button.add_css_class(OVERFLOW_CHILD_NAME);
    // The same shared size/hover treatment the two action icons above
    // carry — see [`OVERFLOW_CHILD_NAME`]'s own doc comment for why this
    // button carries both classes rather than only its own.
    overflow_button.add_css_class(ACTION_ICON_CLASS);
    overflow_button.add_css_class("flat");
    // Hidden until `bind`/`resolve_overflow_button` decides the bound item
    // genuinely has more actions than `ROW_ACTION_ICON_CAP` — "hide, don't
    // reserve", the identical precedent every other optional row element
    // in this module already follows.
    overflow_button.set_visible(false);
    // The icon and tooltip are fixed here, once, and never change on any
    // later `bind` — see [`OVERFLOW_ICON_NAME`]'s and [`OVERFLOW_TOOLTIP`]'s
    // own doc comments for why, unlike an action icon's own per-action
    // glyph and label.
    overflow_button.set_icon_name(OVERFLOW_ICON_NAME);
    overflow_button.set_tooltip_text(Some(OVERFLOW_TOOLTIP));
    // Only the action *target* — this row's own item id, alone, see
    // [`ROW_OPEN_ACTIONS_TARGET_TYPE`]'s own doc comment — changes per
    // bind, via [`resolve_overflow_button`]; the action *name* is fixed
    // here, exactly like the two dedicated action-icon buttons above.
    overflow_button.set_action_name(Some(&format!(
        "{ROW_ACTION_GROUP_PREFIX}.{ROW_OPEN_ACTIONS_NAME}"
    )));
    actions_wrapper.append(&overflow_button);

    container.append(&actions_wrapper);

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
    // Issue #207: the base, un-shown `opacity: 0` state
    // `assets/stylesheet.css`'s `.hop-row-hint` rule declares — see
    // `HINT_CHILD_NAME`'s own doc comment for why this reuses that name
    // rather than a third, separate class. `HINT_SHOWN_CLASS` is
    // deliberately *not* added here: a freshly built row's hint starts
    // un-shown, exactly like its two chips below, and only [`bind`] ever
    // adds the `-shown` modifier, on a genuine transition.
    hint.add_css_class(HINT_CHILD_NAME);
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
    // See `title.set_selectable(false)`'s own comment above — SPEC
    // decision 6's "no text selection inside rows", stated explicitly for
    // this chip too rather than left to `gtk::Label`'s own default.
    hint_label.set_selectable(false);
    hint.append(&hint_label);

    let hint_key = gtk::Label::new(None);
    hint_key.set_widget_name(HINT_KEY_CHILD_NAME);
    hint_key.add_css_class(HINT_KEY_CHILD_NAME);
    hint_key.set_visible(false);
    hint_key.set_selectable(false);
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
/// icon, and — issue #197 — its action hint, paired with
/// `activate_key_display`. `widget` is typed as a bare `gtk::Widget` rather
/// than `gtk::Box` because its caller, `ui::view::bind`, reaches it back out
/// of a `gtk::Stack` page by name — `gtk::Stack::child_by_name` hands back
/// the general widget type regardless of what was added, so the downcast
/// belongs here, next to the one place that knows a `Row` page's widget is
/// actually the `gtk::Box` [`build`] returns.
///
/// `activate_key_display` is `Option<&str>`, not `&Keymap`, as of issue
/// #197 review, finding 3: this function needs only the one value the
/// key-glyph chip renders — [`crate::keymap::Keymap::activate_binding_display`]'s
/// answer, already resolved to text — never `Keymap` itself, so it never
/// has to import [`crate::keymap::Action`] to reach into one. See
/// `ui::view::Node`'s own doc comment for the full account of what used to
/// be threaded through here instead, and why.
///
/// # Issue #207: the recycling constraint
///
/// [`resolve_hint`] alone decides *whether* the hint ends up shown for this
/// bind; this function is where the *separate* decision of whether that
/// counts as a fade-worthy entrance gets made, by comparing the hint's
/// shown state immediately before and immediately after [`resolve_hint`]
/// runs:
///
/// - `was_shown`, read from [`HINT_SHOWN_CLASS`]'s presence on `hint`
///   *before* `resolve_hint` touches anything. This is the widget's own
///   memory of "was the hint genuinely showing the last time this function
///   decided" — see [`HINT_SHOWN_CLASS`]'s own doc comment for why a CSS
///   class, of all things, is what carries that memory across an
///   `unbind`/`bind` pair, when [`unbind`] resets the chips' own visibility
///   on every one of those.
/// - `now_shown`, read from `hint_key`'s own visibility immediately *after*
///   `resolve_hint` runs. [`resolve_hint`]'s "both halves or neither" rule
///   (its own doc comment) means `hint_key` is visible if and only if the
///   hint just resolved to non-empty — a signal independent of
///   [`should_show_label_chip`]'s responsive collapse, which only ever
///   toggles `hint_label`, never `hint_key`. So this reads "does the hint
///   slot have content", not "is the full two-chip hint currently wide
///   enough to show both chips" — the two are deliberately different
///   questions, and only the first is what an entrance fade should key on.
///
/// [`hint_entered_shown`] turns that pair into the one decision that
/// matters: `was_shown = false, now_shown = true` is the *only* case that
/// starts the fade (by adding [`HINT_SHOWN_CLASS`], which is what makes
/// `assets/stylesheet.css`'s `.hop-row-hint.hop-row-hint-shown` rule match
/// and its `transition:` play). Every other combination — stayed shown
/// across a recycle (`true, true`), stayed hidden (`false, false`), or
/// genuinely lost its hint (`true, false`) — either leaves the class alone
/// or removes it via [`hint_left_shown`], never re-triggering a fade. This
/// is what keeps a recycled row's rebind from replaying the entrance fade
/// regardless of whether the new item's hint text differs from the old
/// one's — the exact hazard this module's "Issue #207" top-level doc
/// section names.
pub fn bind(widget: &gtk::Widget, item: &Item, activate_key_display: Option<&str>) {
    let Some(container) = widget.downcast_ref::<gtk::Box>() else {
        return;
    };
    sync_state_classes(container, item);
    if let Some(label) = title_widget(container) {
        label.set_text(item.title.as_str());
    }
    if let Some(subtitle) = subtitle_widget(container) {
        resolve_subtitle(&subtitle, item.subtitle.as_ref());
    }
    if let Some(stamp) = stamp_widget(container) {
        resolve_stamp(&stamp, OFFLINE_STATE.load(Ordering::Relaxed));
    }
    if let Some(icon) = icon_widget(container) {
        resolve_icon(&icon, item.icon.as_ref());
    }
    if let (Some(hint), Some(hint_label), Some(hint_key)) = (
        hint_widget(container),
        hint_label_widget(container),
        hint_key_widget(container),
    ) {
        let was_shown = hint.has_css_class(HINT_SHOWN_CLASS);
        resolve_hint(&hint, &hint_label, &hint_key, item, activate_key_display);
        let now_shown = hint_key.is_visible();
        sync_hint_shown_class(&hint, was_shown, now_shown);
    }
    resolve_action_icons(container, item);
    resolve_overflow_button(container, item);
}

fn sync_state_classes(container: &gtk::Box, item: &Item) {
    container.remove_css_class("hop-row-fallback");
    container.remove_css_class("hop-row-prefixes");
    if item.append_to_end || matches!(item.kind, Kind::WebSearch) {
        container.add_css_class("hop-row-fallback");
    }
    if item.id.as_str() == "hop:prefixes" {
        container.add_css_class("hop-row-prefixes");
    }
}
/// `as_of_hh_mm` is captured once when the connection is lost and reused by
/// every cached row, so recycling cannot make the cache appear to move
/// forward in time.
pub fn set_offline_state(offline: bool, as_of_hh_mm: Option<&str>) {
    OFFLINE_STATE.store(offline, Ordering::Relaxed);
    if let Ok(mut snapshot) = OFFLINE_SNAPSHOT.lock() {
        *snapshot = if offline {
            as_of_hh_mm.map(str::to_owned)
        } else {
            None
        };
    }
}

fn resolve_stamp(stamp: &gtk::Label, offline: bool) {
    if offline {
        let snapshot = OFFLINE_SNAPSHOT
            .lock()
            .ok()
            .and_then(|snapshot| snapshot.clone());
        let text = snapshot
            .map(|as_of| format!("as of {as_of}"))
            .unwrap_or_else(|| "as of --:--".to_string());
        stamp.set_text(&text);
        stamp.set_visible(true);
    } else {
        stamp.set_text("");
        stamp.set_visible(false);
    }
}

/// Maps an [`ActionKind`] to a themed, symbolic icon name — this module's
/// top doc comment, "Icon glyph", has the full argument for why this is
/// derived rather than carried on the wire, and why no fallback beyond
/// GTK's own is needed here. Deliberately exhaustive, no `_` arm: a future
/// `ActionKind` variant fails this match at compile time rather than
/// silently rendering a blank icon — the identical discipline
/// `ui::action_panel::kind_hint` already applies to the same enum, for the
/// same enum-completeness reason.
fn action_kind_icon_name(kind: &ActionKind) -> &'static str {
    match kind {
        ActionKind::Open => "document-open-symbolic",
        ActionKind::Focus => "view-restore-symbolic",
        ActionKind::Copy => "edit-copy-symbolic",
        ActionKind::Run => "system-run-symbolic",
        ActionKind::CloseWindow => "window-close-symbolic",
        ActionKind::MoveToWorkspace => "view-grid-symbolic",
        ActionKind::OpenUrl => "external-link-symbolic",
    }
}

/// Populates every one of this row's `ROW_ACTION_ICON_CAP` action-icon
/// buttons from `item.actions`, in wire order — this module's top doc
/// comment, "Which two", is the argument for why the first
/// [`ROW_ACTION_ICON_CAP`] actions in that order, not a default-action
/// search: slot `n` gets `item.actions[n]` when it exists (icon, tooltip,
/// and the `(item_id, action_id)` GAction target a click sends — see
/// [`ROW_ACTION_GROUP_PREFIX`]'s own doc comment), and is hidden and
/// cleared via [`clear_action_icon`] when it does not, exactly the "hide,
/// don't reserve" rule this module already applies to the subtitle and the
/// hint's own chips.
///
/// # The recycling constraint
///
/// A row bound to a three-action item and later recycled onto a one-action
/// item must not go on offering the second action after that rebind — the
/// same "a recycled row does not carry stale [content]" hazard this
/// module's own "build and bind do not blindly animate" section warns
/// about for a different mechanism. Unlike [`HINT_SHOWN_CLASS`], no
/// before/after comparison is needed here to get that right: every slot's
/// button is either given `item.actions[slot]`'s real data or explicitly
/// cleared, unconditionally, on every single bind — there is no shown/
/// hidden *history* this function needs to consult, only `item.actions`'s
/// current length against `slot`.
fn resolve_action_icons(container: &gtk::Box, item: &Item) {
    for slot in 0..ROW_ACTION_ICON_CAP {
        let Some(button) = action_icon_widget(container, slot) else {
            continue;
        };
        match item.actions.get(slot) {
            Some(action) => {
                button.set_icon_name(action_kind_icon_name(&action.kind));
                button.set_tooltip_text(Some(action.label.as_str()));
                button.set_action_target_value(Some(
                    &(item.id.as_str(), action.id.as_str()).to_variant(),
                ));
                button.set_visible(true);
            }
            None => clear_action_icon(&button),
        }
    }
}

/// Hides one action-icon button and clears everything [`resolve_action_icons`]
/// can set on it — [`unbind`]'s own symmetry rule ("every property `bind`
/// can set here, `unbind` resets") applied to this widget, and
/// [`resolve_action_icons`]'s own "does not exist for this item" branch.
/// Clearing the action target specifically (`set_action_target_value(None)`)
/// is the load-bearing half: an invisible, un-clickable button cannot be
/// clicked by a real pointer, but leaving a stale target on it would still
/// be exactly the kind of "holds stale application data it should not
/// have" this module's own `unbind` doc comment calls out, defensive
/// rather than reachable in practice.
fn clear_action_icon(button: &gtk::Button) {
    button.set_visible(false);
    button.set_tooltip_text(None);
    button.set_action_target_value(None);
    button.set_icon_name("");
}

/// Shows or hides the row's overflow chevron for the currently bound
/// `item` — issue #254 review, finding 4. See this module's top doc
/// comment, "When it shows", for why `item.actions.len() >
/// ROW_ACTION_ICON_CAP` (strictly greater, not `>=`) is the one condition
/// checked, recomputed fresh on every single [`bind`] with no shown/hidden
/// memory carried from the previous one — the identical "no before/after
/// comparison, just the current item's own shape" discipline
/// [`resolve_action_icons`]'s own "the recycling constraint" section
/// already establishes for the two dedicated action icons, applied here to
/// a third, boolean decision instead of a per-slot one.
fn resolve_overflow_button(container: &gtk::Box, item: &Item) {
    let Some(button) = overflow_button_widget(container) else {
        return;
    };
    if item.actions.len() > ROW_ACTION_ICON_CAP {
        button.set_action_target_value(Some(&item.id.as_str().to_variant()));
        button.set_visible(true);
    } else {
        clear_overflow_button(&button);
    }
}

/// Hides the overflow chevron and clears its action target —
/// [`resolve_overflow_button`]'s own "does not apply to this item" branch,
/// and [`unbind`]'s symmetry rule applied to this button, the identical
/// shape [`clear_action_icon`] already gives the two dedicated action
/// icons.
fn clear_overflow_button(button: &gtk::Button) {
    button.set_visible(false);
    button.set_action_target_value(None);
}

/// Whether a hint that was in `was_shown`'s state before this bind and is
/// now in `now_shown`'s state has just genuinely appeared — the one, pure
/// decision at the core of [`bind`]'s "the recycling constraint" doc
/// section, isolated here specifically so it can be unit-tested without
/// `gtk::init()`, the same way [`default_action_label`]'s own tests below
/// need no GTK — a plain truth table, not a GTK behavior.
fn hint_entered_shown(was_shown: bool, now_shown: bool) -> bool {
    now_shown && !was_shown
}

/// [`hint_entered_shown`]'s mirror: whether a hint that was shown has just
/// genuinely gone away — [`sync_hint_shown_class`]'s other branch, and the
/// one case besides "genuinely appeared" that changes
/// [`HINT_SHOWN_CLASS`]'s presence at all.
fn hint_left_shown(was_shown: bool, now_shown: bool) -> bool {
    was_shown && !now_shown
}

/// Reconciles `hint`'s [`HINT_SHOWN_CLASS`] against [`bind`]'s own
/// before/after read of the hint's shown state — see that function's doc
/// comment, "the recycling constraint", for what `was_shown`/`now_shown`
/// mean and why this is the one place either gets compared.
///
/// Only [`hint_entered_shown`] and [`hint_left_shown`] ever change the
/// class; the two remaining combinations (stayed shown, stayed hidden) fall
/// through to neither branch and leave it exactly as it was — no redundant
/// `add_css_class` on a class already present, which matters here not for
/// correctness (GTK's own `add_css_class` is itself idempotent) but for
/// intent: this function's shape is the actual, auditable gate this
/// mechanism relies on, not GTK's incidental de-duplication.
fn sync_hint_shown_class(hint: &gtk::Box, was_shown: bool, now_shown: bool) {
    if hint_entered_shown(was_shown, now_shown) {
        hint.add_css_class(HINT_SHOWN_CLASS);
    } else if hint_left_shown(was_shown, now_shown) {
        hint.remove_css_class(HINT_SHOWN_CLASS);
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
/// never `crate::keymap::Action` — this module's brief is explicit that
/// `CONTEXT.md` flags conflating the two `Action` types as the obvious
/// mistake for exactly this issue, and (since issue #197 review, finding 3
/// — see this module's top doc comment) this module does not even import
/// `crate::keymap::Action` any more to have the chance: the only other
/// `Action`-shaped value [`resolve_hint`] touches is a plain `Option<&str>`
/// display string, already resolved by
/// [`crate::keymap::Keymap::activate_binding_display`] before it ever
/// reaches this module.
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
/// *current* text, not a number typed into this function by hand — plus
/// [`tokens::HINT_CHIP_GAP_PX`] (the horizontal spacing [`build`] gives
/// `hint`, its `gtk::Box::new` constructor argument, between the label chip
/// and the key chip) and [`tokens::HINT_MARGIN_START_PX`] (`hint`'s own
/// `set_margin_start`, in [`build`]) — both fixed pixel amounts `hint`
/// carries unconditionally, present whether or not the label chip is
/// showing, so both count against the same budget the two chips'
/// own widths do. Review on this issue's original PR caught that this sum
/// used to stop at the two chips' widths alone, undercounting the hint's
/// real on-screen footprint by roughly `HINT_CHIP_GAP_PX +
/// HINT_MARGIN_START_PX` (about 20px) — enough that the chip stayed visible
/// past the width at which it was actually being pushed off-window, the
/// wrong direction to err in for a collapse this note requires to happen
/// *before* that, not after. The title and subtitle are deliberately left
/// out of this sum: both ellipsize (`ui::row::build`'s `EllipsizeMode::End`),
/// so neither has a minimum width this function would need to protect —
/// the one thing that cannot shrink and does not ellipsize is the hint
/// itself (its own gap and margin included), which is exactly the element
/// `assets/tokens.css`'s note says must not be "pushed off-window."
///
/// # Statelessness: why `hint_label` must already be visible when measured
///
/// [`gtk::Widget::measure`] returns `0` for a widget whose own
/// [`gtk::Widget::is_visible`] is `false`, confirmed directly against this
/// crate's real, installed GTK 4.14 while fixing the review finding above
/// (a throwaway probe measured a realized, invisible `gtk::Label` and read
/// back `0`, then `set_visible(true)` and the label's own real natural
/// width). `hint_label`'s visibility going into a bind is whatever the
/// *previous* bind on this recycled row decided — GTK's own list-view
/// recycling means that is not "nothing," the way a freshly built widget's
/// would be, on every bind after the first — so measuring it as this
/// function is entered, before this bind's own decision has been written
/// back to it, would silently substitute `0` for a collapsed row's real
/// label width and answer a different `needed` than a row that arrives at
/// the identical width having never collapsed. That is precisely the
/// "different answer depending on what the hint currently shows" defect
/// this function's contract rules out — the same shape of hazard the
/// rejected "measure the `hint` container itself" alternative has, just
/// reached through the label widget's own visibility rather than through
/// its container's child-exclusion, and just as able to make a real
/// recycled row's hint render wrong. [`resolve_hint`] is what forces
/// `hint_label` visible immediately before calling this function, for
/// exactly this reason — see that function's own comment on the call site.
/// `hint_key` needs no equivalent treatment: [`resolve_hint`] already sets
/// it visible unconditionally, every bind, before this function ever runs.
///
/// `tests/view_tree_renderer.rs`'s "issue #197 code review, finding 1"
/// section proves both halves of this doc comment together: a width chosen
/// to satisfy the old (chip-widths-only) sum but not this one must
/// collapse, and it must collapse identically whether the row arrives
/// there having never collapsed or having just collapsed and widened back.
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
    let needed = *tokens::ICON_SIZE_PX
        + label_natural
        + key_natural
        + *tokens::HINT_CHIP_GAP_PX
        + *tokens::HINT_MARGIN_START_PX;
    surface_width >= needed
}

/// Resolves the row's right-aligned action hint onto its two chip widgets —
/// issue #197. Pairs [`default_action_label`]'s answer (the label chip)
/// with `activate_key_display` (the key glyph — already resolved, by the
/// time this function ever sees it, through [`Binding`]'s own
/// [`fmt::Display`] convention; see `crate::keymap`'s doc comment on that
/// `impl` for the spelling rules, and
/// [`crate::keymap::Keymap::activate_binding_display`]'s own doc comment
/// for where and how often that resolution actually runs — not on every
/// bind, as of issue #197 review, finding 3), and applies
/// [`should_show_label_chip`]'s responsive collapse once both are known to
/// exist.
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
/// widget is touched. `activate_key_display` being `None` here means
/// exactly what `keymap.binding_for(Activate)` being `None` used to mean
/// before finding 3's change moved that lookup out of this function: the
/// caller already asked the keymap once, up front, and got no answer.
fn resolve_hint(
    hint: &gtk::Box,
    hint_label: &gtk::Label,
    hint_key: &gtk::Label,
    item: &Item,
    activate_key_display: Option<&str>,
) {
    let label = default_action_label(item);

    let (Some(label), Some(key)) = (label, activate_key_display) else {
        clear_hint(hint_label, hint_key);
        return;
    };

    hint_label.set_text(label);
    hint_key.set_text(key);
    hint_key.set_visible(true);

    // `hint_label` is forced visible *before* `should_show_label_chip`
    // measures it, immediately overwritten below by that function's real
    // answer — never left at whatever visibility a previous bind on this
    // recycled row happened to leave it in. See
    // `should_show_label_chip`'s own doc comment, "Statelessness: why
    // `hint_label` must already be visible when measured", for why an
    // invisible label chip would otherwise measure as `0` width and corrupt
    // the very decision this call is about to make. `hint_key` needs no
    // matching line: it was already set visible, unconditionally, two
    // lines above.
    hint_label.set_visible(true);
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
///
/// # Issue #207: `HINT_SHOWN_CLASS` is the one deliberate exception to that
/// symmetry
///
/// [`clear_hint`] resets the chips' own text and visibility here, same as
/// always, but this function never touches [`HINT_SHOWN_CLASS`] on the
/// hint's wrapper — not an oversight, the whole mechanism [`bind`]'s "the
/// recycling constraint" doc section describes depends on it surviving
/// this call untouched. If `unbind` cleared it too, every recycled row's
/// next `bind` would see `was_shown = false` regardless of whether the
/// hint had genuinely just been showing, which is exactly "did `bind` run"
/// rather than "did the hint's own shown state genuinely transition" —
/// the distinction this issue's brief is explicit must never be conflated.
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
    for slot in 0..ROW_ACTION_ICON_CAP {
        if let Some(button) = action_icon_widget(container, slot) {
            clear_action_icon(&button);
        }
    }
    if let Some(button) = overflow_button_widget(container) {
        clear_overflow_button(&button);
    }
    if let Some(stamp) = stamp_widget(container) {
        resolve_stamp(&stamp, false);
    }
    container.remove_css_class("hop-row-fallback");
    container.remove_css_class("hop-row-prefixes");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use hop_protocol::{Action, ActionId, ActionKind, ItemId, ItemTitle, Kind};

    use super::*;

    /// A minimal, GTK-free item — `default_action_label` touches only
    /// `hop_protocol::Item` fields, so this needs no `gtk::init()`, unlike
    /// almost everything else in this module (see this module's top doc
    /// comment, "build and bind do not blindly animate", and
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

    /// [`hint_entered_shown`]/[`hint_left_shown`]'s full truth table —
    /// issue #207's "the recycling constraint," exercised as pure logic
    /// with no `gtk::init()` at all, the same GTK-free shape
    /// `default_action_label`'s tests above use. This is the mechanism
    /// itself, isolated from GTK's own CSS-class machinery entirely: a
    /// widget-level proof that `bind` actually wires this decision to the
    /// real [`HINT_SHOWN_CLASS`] lives in
    /// `tests/view_tree_renderer.rs` instead, since that half needs a real
    /// `gtk::Box` to call `has_css_class` on.
    #[test]
    fn hint_entered_shown_is_true_only_for_the_not_shown_to_shown_transition() {
        assert!(
            hint_entered_shown(false, true),
            "not-shown to shown is exactly the genuine entrance this issue's fade exists for"
        );
        assert!(
            !hint_entered_shown(true, true),
            "shown to shown — a recycled row rebinding while the hint stays shown — must \
             never read as an entrance, regardless of whether the bound item changed"
        );
        assert!(
            !hint_entered_shown(false, false),
            "not-shown to not-shown is no transition at all"
        );
        assert!(
            !hint_entered_shown(true, false),
            "shown to not-shown is the hint genuinely leaving, not entering"
        );
    }

    #[test]
    fn hint_left_shown_is_true_only_for_the_shown_to_not_shown_transition() {
        assert!(
            hint_left_shown(true, false),
            "shown to not-shown is exactly when HINT_SHOWN_CLASS should be removed"
        );
        assert!(
            !hint_left_shown(false, true),
            "not-shown to shown is an entrance, not a departure"
        );
        assert!(
            !hint_left_shown(true, true),
            "shown to shown — stayed shown across a recycle — must not read as leaving"
        );
        assert!(
            !hint_left_shown(false, false),
            "not-shown to not-shown is no transition at all"
        );
    }

    /// [`sync_hint_shown_class`] needs a real `gtk::Widget` to call
    /// `add_css_class`/`remove_css_class`/`has_css_class` on, so its own
    /// behavior — as opposed to the pure decision the two tests above
    /// already pin — is proven in `tests/view_tree_renderer.rs` instead,
    /// alongside `bind`'s real recycling behavior. Nothing here duplicates
    /// that; this module's own `#[cfg(test)]` stays GTK-free by design
    /// (see this file's own doc comment on why almost everything else in
    /// this module needs a broadway display to test at all).
    #[test]
    fn hint_shown_class_name_matches_the_stylesheets_selector() {
        // Pinned so a future rename of either side (this constant, or
        // `assets/stylesheet.css`'s `.hop-row-hint-shown` selector) is a
        // visible test failure rather than a silent drift between the two.
        assert_eq!(HINT_SHOWN_CLASS, "hop-row-hint-shown");
    }
}
