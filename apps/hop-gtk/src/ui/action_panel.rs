//! The ctrl-K action panel (issue #254, design spec decision 5): given one
//! [`hop_protocol::Item`], presents every one of its
//! [`hop_protocol::Action`]s, filterable by typed text, navigable by
//! [`Up`](gdk::Key::Up)/[`Down`](gdk::Key::Down), and reports whichever one
//! the user picks — by [`Return`](gdk::Key::Return), or a mouse click on a
//! row — through a plain callback the caller supplies at construction.
//!
//! # Scope: this is the widget, not the wiring
//!
//! [`ActionPanel`] does not know what ctrl-K is, does not know which item
//! is currently selected in the results list, and does not send anything
//! over `ipc` — `on_choose`, the one thing it *does* call, is a plain
//! `Fn(ActionId)` closure the caller supplies, the same shape
//! [`crate::ipc::CommandSender`] takes anywhere else in this crate (built
//! once, cloned into whichever signal closures need it, invoked with no
//! return value the caller waits on). A later, separate issue wires a
//! `ctrl+k` binding in `crate::keymap`, decides which item this panel
//! should be presented for, and turns a reported [`ActionId`] into an
//! `IpcCommand::Execute` the same way `ui::window::activate_at` already
//! does for the default action. This module's own `Cargo.toml`
//! dependencies are unchanged by this issue — in particular, nothing here
//! imports `crate::ipc` at all, which is the concrete, checkable form of
//! "do not send IPC yourself."
//!
//! # Why a `gtk::Popover`, not a plain `gtk::Box` with a visibility flag
//!
//! `ui::offline_indicator::OfflineIndicator` is this crate's other
//! built-once, presented-on-demand widget, and it is a bare `gtk::Box`
//! toggled with `set_visible` — no popover. That shape fits an indicator
//! that lives at a fixed spot in the window's own layout permanently; nothing
//! about it needs GTK to compute *where* on screen it goes. This panel is
//! the opposite: "present it anchored somewhere" is the brief's own
//! wording, and where "somewhere" is differs by how it was opened (design
//! spec decision 6: ctrl-K opens it as a general overlay, a right-click
//! opens it at the cursor). [`gtk::Popover`] is GTK's own widget for
//! exactly that — `set_parent`/`popup` position it relative to a widget the
//! caller names, `set_pointing_to` (not used by this issue, left for the
//! right-click slice) narrows that to a point within it, and it comes with
//! outside-click and focus-loss dismissal for free, none of which a bare
//! `gtk::Box` would give without this module reinventing it.
//!
//! [`ActionPanel::present`]'s own doc comment covers the one sharp edge
//! that shape brings: a `gtk::Popover::popup` is a no-op (a `g_critical`
//! logged by GTK, not a panic — confirmed directly against this crate's
//! real, installed GTK 4.14 while writing `tests/action_panel.rs`) unless
//! its parent already sits under a realized [`gtk::Native`], which is why
//! this module's own tests build a real `gtk::Window`, give it a real
//! child, and `present()` it before ever calling
//! [`ActionPanel::present`].
//!
//! # Zero actions: no mystery box
//!
//! The brief is explicit that an item with zero actions needs an explicit,
//! documented answer, not a panic and not "open it anyway." The decision
//! made here: **do not open.** [`ActionPanel::present`] populates the
//! (empty) list either way — so a panel already open when handed a
//! zero-action item does not keep stale rows from whatever it showed
//! before — but returns `false` rather than calling `popup()`, and its
//! caller is expected to treat that `false` as "there was nothing to show."
//! An empty popover with a filter entry and no rows under it would be
//! exactly the "empty mystery box" the brief calls out by name: it invites
//! a keystroke or an Enter that can only ever do nothing, with no row on
//! screen to explain why. Not opening is the honest alternative — the same
//! judgment `ui::row`'s "hide, don't reserve" section makes about an
//! absent subtitle, applied here to an absent *panel* rather than an
//! absent row element.
//!
//! # Filtering: case-insensitive substring, not fuzzy ranking
//!
//! See [`label_matches`]'s own doc comment for the reasoning in full — in
//! short, `hop-core` owns this workspace's one fuzzy matcher
//! (`hop-core::rank`, built on `nucleo-matcher`), `hop-gtk` does not depend
//! on `hop-core` today, and this issue's "no new dependencies" requirement
//! rules out changing that just to filter a list bounded at
//! [`hop_protocol::limits::MAX_ACTIONS_PER_ITEM`] rows. A plain,
//! case-insensitive substring check over `label` is not a compromise for a
//! list this short — every candidate is visible at a glance either way, so
//! typo tolerance buys nothing a fuzzy matcher would over a hundred-item
//! results list.
//!
//! # The state class the stylesheet needs for its fade
//!
//! Design spec decision 2 bounds this panel's *only* permitted motion to an
//! open/close fade, "≤220ms," collapsing to opacity-only under
//! `Motion::Reduced`. Per this issue's own "motion belongs in CSS, not
//! Rust" rule, nothing in this module starts a timer or an animation of its
//! own — [`PANEL_SHOWN_CLASS`] is the one hook it gives the stylesheet to
//! key that transition on. `assets/stylesheet.css` writes exactly the rule
//! this module's first draft anticipated: `.hop-action-panel { opacity: 0;
//! }` / `.hop-action-panel.hop-action-panel-shown { opacity: 1; transition:
//! opacity <=220ms; }`.
//!
//! ## Why the class add is deferred by one main-loop turn, not applied inside `present` itself
//!
//! An earlier version of [`ActionPanel::present`] added [`PANEL_SHOWN_CLASS`]
//! synchronously, in the same call as `popup()` — the same shape
//! `ui::row::HINT_SHOWN_CLASS` gets away with, but that precedent does not
//! transfer here, and a verified defect against this exact code is why:
//! `self.panel` (the popover's child, the node the class lands on) goes
//! from *unmapped* to *mapped* at the instant `popup()` runs, because that
//! is the first moment it has ever been part of a realized widget tree at
//! all. Adding `.hop-action-panel-shown` in that same call means the very
//! first computed style this node ever has already carries `opacity: 1` —
//! there is no earlier, different, *already-mapped* style for the CSS
//! transition to interpolate away from, so GTK has nothing to animate and
//! the panel still snaps to full opacity instantly, even though the
//! stylesheet rule is real and parses correctly. (`ui::row`'s hint chip has
//! no version of this problem: `.hop-row-hint` is already mapped, at
//! `opacity: 0`, for the row's entire lifetime before `HINT_SHOWN_CLASS` is
//! ever added on a later, genuine state change — it never goes from
//! unmapped to shown in the same turn.)
//!
//! [`ActionPanel::present`] instead calls `popup()` first — which maps
//! `self.panel` at its base, `opacity: 0` style, per the rule above — and
//! schedules the class add with [`glib::idle_add_local_once`], so it runs
//! on a *later* turn of the main loop, once the base style has already been
//! computed against a mapped widget. That second, later style change is
//! what `gtk_css_animated_style_create_css_transitions`
//! (`gtk/gtkcssanimatedstyle.c`) actually has something to transition
//! between. `glib::idle_add_local_once` was chosen over a frame-clock tick
//! callback (`gtk::Widget::add_tick_callback`) because this panel does not
//! otherwise touch `self.panel`'s frame clock anywhere, and an idle
//! callback needs no widget-realization bookkeeping to add or remove — it
//! simply runs once, the next time the default main context is idle, which
//! is already guaranteed to be after `popup()` returns and GTK has mapped
//! and styled the widget. **A future edit must not "simplify" this back to
//! a plain, synchronous `self.panel.add_css_class(PANEL_SHOWN_CLASS)` call
//! inside `present`** — that is the exact regression this comment (and
//! `tests/action_panel.rs`'s
//! `assert_present_defers_the_shown_class_until_the_widget_is_already_mapped`)
//! exist to catch: it would silently re-break the fade while leaving every
//! other observable behaviour (the popover opens, the class ends up
//! present, the stylesheet rule still parses) looking correct.
//!
//! [`ActionPanel::dismiss`] removes [`PANEL_SHOWN_CLASS`] on every close
//! route (Escape, a reported choice, or a future outside-click) — needed to
//! keep the panel's own state honest for its *next* `present` (so the
//! deferred add always starts from a genuine off state) — but this does
//! **not** produce a fade-out, and this module does not pretend it does.
//! `gtk::Popover::popdown()` unmaps the widget synchronously in the same
//! call (`gtk/gtkwidget.c`'s `gtk_widget_hide()`: `gtk_css_node_set_visible`
//! plus the unmap, both in that one call, no frame-clock deferral), so the
//! popover — and `self.panel` inside it — are gone from the screen before
//! any dismiss transition could paint even a single frame, regardless of
//! what order this function removes the class in relative to calling
//! `popdown()`. Achieving a real fade-out would mean this module
//! reinventing GTK's own popdown as a delayed, timer-driven hide, which is
//! out of this issue's scope; the honest choice made here is an
//! entrance-only fade, stated plainly rather than implied by a
//! symmetrical-looking removal of the same class dismissal cannot animate.
//!
//! # Self-referential closures, and why that is not a leak here
//!
//! [`ActionPanel::new`] wires its own `entry`'s `changed` signal and its own
//! `gtk::EventControllerKey` with closures that capture `self.clone()` —
//! [`ActionPanel`] is `Clone` (every field is a cheap, reference-counted
//! GObject or `Rc` handle, the same convention `ui::window::HopWindow` and
//! `ui::offline_indicator::OfflineIndicator` already use), so this creates
//! a genuine reference cycle: `entry` is a field of the panel, and the
//! panel clone lives inside a closure owned by `entry`'s own signal handler
//! list. `ui::window::HopWindow::wire_keyboard` takes the identical shape
//! (`let hop_window = self.clone();` moved into `self.window`'s own
//! `EventControllerKey`) for the identical reason: a widget meant to be
//! built once and live for as long as its owner does never actually needs
//! that cycle broken — nothing in this crate ever drops a `HopWindow`
//! before process exit, and nothing is expected to drop an `ActionPanel`
//! before its own owner (a future `HopWindow`) does either.
//!
//! [`Up`]: gdk::Key::Up

use std::rc::Rc;

use gio::prelude::*;
use glib::BoxedAnyObject;
use gtk::prelude::*;
use gtk::{gdk, glib};

use hop_protocol::{Action, ActionId, ActionKind, Item};

/// Whether `label` matches `needle` under this panel's own filter rule.
///
/// Case-insensitive substring, not fuzzy — see this module's top doc
/// comment, "Filtering: case-insensitive substring, not fuzzy ranking", for
/// why: `hop-core` carries a real fuzzy matcher (`hop-core::rank`), but
/// `hop-gtk` does not depend on `hop-core` today (`apps/hop-gtk/Cargo.toml`
/// names every dependency this crate has, and `hop-core` is not among
/// them), and this issue's own hard requirement is "no new dependencies."
/// Reaching for fuzzy ranking here would mean either adding that
/// dependency — ruled out — or hand-rolling a second, independent fuzzy
/// matcher next to the one `hop-core` already owns, which is worse: two
/// matchers with two tuning knobs, silently free to drift apart, for a list
/// that is at most [`hop_protocol::limits::MAX_ACTIONS_PER_ITEM`] rows long.
/// A plain substring check is the honest tool for a list that short: every
/// row is visible at a glance, so "narrows what's visible" is the whole
/// job, not "ranks a hundred candidates by typo tolerance."
///
/// An empty `needle` matches everything — the panel's freshly opened,
/// nothing-typed-yet state must show every action, not none.
fn label_matches(label: &str, needle: &str) -> bool {
    needle.is_empty() || label.to_lowercase().contains(&needle.to_lowercase())
}

/// The small trailing kind/type hint text for `.hop-action-row-kind` —
/// [`hop_protocol::ActionKind`] has no [`std::fmt::Display`] of its own (its
/// wire spelling is `snake_case`, meant for `config.toml`/JSON, not a
/// human-facing row), so this is the one place that maps the wire
/// vocabulary onto words a user reads. Deliberately exhaustive with no `_`
/// arm: a future [`ActionKind`] variant fails this match at compile time
/// rather than silently rendering no kind hint at all, the same "a new
/// variant must be handled explicitly, not fall through" property
/// `ui::row::resolve_icon`'s three-way `IconSpec` match already relies on.
fn kind_hint(kind: &ActionKind) -> &'static str {
    match kind {
        ActionKind::Open => "Open",
        ActionKind::Focus => "Focus",
        ActionKind::Copy => "Copy",
        ActionKind::Run => "Run",
        ActionKind::CloseWindow => "Close Window",
        ActionKind::MoveToWorkspace => "Move to Workspace",
        ActionKind::OpenUrl => "Open URL",
    }
}

/// The CSS class [`ActionPanel::present`] adds — and [`ActionPanel::dismiss`]
/// removes — on the panel's own container, the moment it genuinely opens or
/// closes. See this module's top doc comment, "The state class the
/// stylesheet needs for its fade", for the fade this exists to trigger and
/// why it is `pub`, matching `ui::row::HINT_SHOWN_CLASS`'s own reason:
/// `tests/action_panel.rs` is expected to read it back to prove the
/// transition happens on a real state change, not on every `present` call
/// regardless of whether the panel was already open.
pub const PANEL_SHOWN_CLASS: &str = "hop-action-panel-shown";

/// The widget name **and** CSS class (doubled identity, `ui::row`'s own
/// precedent — see `ui::row::SUBTITLE_CHILD_NAME`'s doc comment for the full
/// argument) of the panel's own container: the fixed selector
/// `assets/stylesheet.css` is written against, per this issue's CSS
/// contract.
const PANEL_NAME: &str = "hop-action-panel";

/// The filter entry's doubled name/class — this issue's CSS contract names
/// it `.hop-action-panel-entry` specifically (not the bare `.hop-query-entry`
/// `ui::window`'s own entry carries), since the two are visually and
/// behaviorally distinct fields that happen to both be a `gtk::Entry`.
const ENTRY_NAME: &str = "hop-action-panel-entry";

/// One action row's own doubled name/class.
const ROW_NAME: &str = "hop-action-row";

/// The row's label doubled name/class.
const ROW_LABEL_NAME: &str = "hop-action-row-label";

/// The row's trailing kind/type hint doubled name/class.
const ROW_KIND_NAME: &str = "hop-action-row-kind";

/// Reads the [`Action`] a `BoxedAnyObject` wraps, cloned out from behind its
/// `RefCell` borrow — [`crate::ui::model::item_of`]'s exact shape, applied
/// to this panel's own store instead of the results list's. `pub` for the
/// same reason that function is: `tests/action_panel.rs` decodes the
/// filtered selection model's entries back into [`Action`]s to assert every
/// one of `item.actions` is listed, in order, and none invented.
///
/// # Panics
///
/// If `object` is not a `BoxedAnyObject<Action>` — which would mean this
/// module's own [`ActionPanel::populate`] put something else in the store,
/// a programming error in this module, not a condition a caller of this
/// function can otherwise produce. See `model::item_of`'s own doc comment
/// for the identical argument made about the identical shape of panic.
pub fn action_of(object: &glib::Object) -> Action {
    object
        .downcast_ref::<BoxedAnyObject>()
        .expect("hop-gtk's action panel store holds only BoxedAnyObject<Action>")
        .borrow::<Action>()
        .clone()
}

/// Builds one action row's widget — a horizontal `gtk::Box` holding the
/// label (`.hop-action-row-label`) and the trailing kind hint
/// (`.hop-action-row-kind`). Unlike `ui::row::build`, this carries no fixed
/// height request: nothing about this panel loads a row's content
/// asynchronously the way a result row's icon can, so there is no
/// layout-shift hazard for a size reservation to guard against — see this
/// module's top doc comment for the fuller contrast with `ui::row`.
fn build_row() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_widget_name(ROW_NAME);
    row.add_css_class(ROW_NAME);

    let label = gtk::Label::new(None);
    label.set_widget_name(ROW_LABEL_NAME);
    label.add_css_class(ROW_LABEL_NAME);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row.append(&label);

    let kind = gtk::Label::new(None);
    kind.set_widget_name(ROW_KIND_NAME);
    kind.add_css_class(ROW_KIND_NAME);
    kind.set_xalign(1.0);
    row.append(&kind);

    row
}

/// Populates a row built by [`build_row`] with `action`'s label and kind
/// hint — a straight-line read of two fields into two labels, matching
/// `ui::row::bind`'s own "`build` and `bind` do not blindly animate"
/// discipline: recycled rows here carry no state across a rebind that
/// needs preserving (nothing here is the shape of `ui::row`'s hint-entrance
/// fade), so there is nothing this function needs to compare before/after.
fn bind_row(widget: &gtk::Widget, action: &Action) {
    let Some(row) = widget.downcast_ref::<gtk::Box>() else {
        return;
    };
    if let Some(label) = row
        .first_child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    {
        label.set_text(&action.label);
    }
    if let Some(kind) = row
        .last_child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    {
        kind.set_text(kind_hint(&action.kind));
    }
}

/// The `GtkSignalListItemFactory` behind the panel's `gtk::ListView` —
/// `ui::view::build`'s own shape (see that module's doc comment for why
/// every callback downcasts its `&glib::Object` parameter back to
/// `gtk::ListItem` itself: GTK 4.8 widened the signal signature and this
/// workspace's gtk4-rs binding follows suit), simplified for this panel's
/// own single, un-recycled-state row: no `unbind` handler, since
/// [`bind_row`] leaves nothing behind that would need clearing before the
/// next bind — a fresh `set_text` on both labels is already a complete
/// reset.
fn build_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        list_item.set_child(Some(&build_row()));
    });

    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(item_object), Some(widget)) = (list_item.item(), list_item.child()) else {
            return;
        };
        bind_row(&widget, &action_of(&item_object));
    });

    factory
}

/// The ctrl-K action panel — see this module's top doc comment for the
/// full account of its scope, its `gtk::Popover` shape, and the zero-action
/// and filtering decisions it makes.
///
/// `Clone`, matching `ui::window::HopWindow`'s and
/// `ui::offline_indicator::OfflineIndicator`'s own field convention: every
/// field here is a cheap, reference-counted handle (a GObject clone, or the
/// `Rc<dyn Fn(ActionId)>` wrapping the caller's own callback), so cloning
/// this struct is cloning a handle to the one real panel, never building a
/// second one.
#[derive(Clone)]
pub struct ActionPanel {
    /// The popover itself — what [`ActionPanel::present`] parents and pops
    /// up, and what [`ActionPanel::dismiss`] pops down. `pub` accessor
    /// below rather than a `pub` field, matching this module's other two
    /// accessors, so every external read of this panel's state goes through
    /// one typed surface `tests/action_panel.rs` and any future caller both
    /// use identically.
    popover: gtk::Popover,
    /// The popover's own child — the actual `.hop-action-panel` container
    /// (see this module's top doc comment, "Why a `gtk::Popover`", for why
    /// the class lives here and not on the popover's own chrome).
    panel: gtk::Box,
    entry: gtk::Entry,
    /// Every action of the item this panel was last populated for, in
    /// [`Item::actions`]'s own order — the unfiltered source
    /// [`ActionPanel::selection`]'s `gtk::FilterListModel` narrows.
    store: gio::ListStore,
    /// Re-evaluated by [`ActionPanel::refilter`] on every entry keystroke
    /// and every fresh [`ActionPanel::populate`] — never rebuilt, since a
    /// `gtk::CustomFilter`'s predicate closure already reads `entry`'s
    /// *current* text on every call, so "the filter changed" only ever
    /// means "tell the filter model to re-run it," never "give it a new
    /// closure."
    filter: gtk::CustomFilter,
    /// Wraps a `gtk::FilterListModel` over `store` — see
    /// [`ActionPanel::reset_selection`] for why this panel manages its
    /// `selected` position explicitly on every filter change rather than
    /// trusting `gtk::SingleSelection`'s own autoselect.
    selection: gtk::SingleSelection,
    /// The caller's own "an action was chosen" callback — see this
    /// module's top doc comment, "Scope: this is the widget, not the
    /// wiring", for why this is a plain closure rather than
    /// `crate::ipc::CommandSender` itself.
    on_choose: Rc<dyn Fn(ActionId)>,
}

impl ActionPanel {
    /// Builds the panel with no item populated yet and nothing shown —
    /// `on_choose` is called exactly once per reported choice, with the
    /// [`ActionId`] of whichever [`Action`] [`ActionPanel::activate_selected`]
    /// (Enter, or a mouse click on a row) resolved at the moment it ran.
    pub fn new(on_choose: impl Fn(ActionId) + 'static) -> Self {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.set_widget_name(PANEL_NAME);
        panel.add_css_class(PANEL_NAME);

        let entry = gtk::Entry::builder()
            .placeholder_text("Filter actions")
            .build();
        entry.set_widget_name(ENTRY_NAME);
        entry.add_css_class(ENTRY_NAME);
        panel.append(&entry);

        let store = gio::ListStore::new::<BoxedAnyObject>();

        // The filter predicate reads `entry`'s *current* text on every
        // call — see [`filter`]'s own field doc comment for why that is
        // what lets this panel re-run filtering by calling
        // `gtk::Filter::changed` alone, with no second closure ever built.
        let filter_entry = entry.clone();
        let filter = gtk::CustomFilter::new(move |object| {
            let needle = filter_entry.text();
            label_matches(action_of(object).label.as_str(), &needle)
        });

        let filter_model = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filter_model));
        // `autoselect` off, matching `ui::window::HopWindow::build`'s own
        // reasoning for its results list exactly: a filtered-to-nothing
        // panel must show no selection, never GTK's own default of
        // reselecting *something* once the model is non-empty again later.
        // [`ActionPanel::reset_selection`] is what sets the selection
        // explicitly on every populate and every filter change instead.
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let list_view = gtk::ListView::new(Some(selection.clone()), Some(build_factory()));
        list_view.set_single_click_activate(true);

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list_view)
            .vexpand(true)
            .build();
        panel.append(&scrolled);

        let popover = gtk::Popover::new();
        popover.set_child(Some(&panel));
        // `autohide` is `gtk::Popover`'s own default (`true`), named here
        // explicitly rather than left implicit: it is what gives this
        // panel "closes on an outside click" for free, per design spec
        // decision 6, without this module wiring a click-outside detector
        // of its own.
        popover.set_autohide(true);

        let action_panel = ActionPanel {
            popover,
            panel,
            entry,
            store,
            filter,
            selection,
            on_choose: Rc::new(on_choose),
        };

        action_panel.wire_entry();
        action_panel.wire_keys();
        action_panel.wire_mouse(&list_view);

        action_panel
    }

    /// The filter entry — `tests/action_panel.rs` types into this directly
    /// (`entry().set_text(...)`), the same way it drives keyboard
    /// navigation through [`ActionPanel::handle_key`] rather than
    /// synthesizing a real `gdk::Event`; see this module's top doc comment
    /// for why calling the resolved function directly is this crate's own
    /// established precedent (`ui::window::HopWindow::dispatch_action`'s
    /// own tests take the identical shape).
    pub fn entry(&self) -> &gtk::Entry {
        &self.entry
    }

    /// The selection model over the *filtered* rows — `n_items()` is the
    /// count currently visible, `selected()` the currently highlighted
    /// position (or [`gtk::INVALID_LIST_POSITION`] when nothing is), and
    /// `item(position)` (decoded through [`action_of`]) is how a caller —
    /// or a test — reads back which [`Action`] a row actually is.
    pub fn selection(&self) -> &gtk::SingleSelection {
        &self.selection
    }

    /// The popover itself, so a caller can read `is_visible()` — or, for a
    /// future right-click slice, call `set_pointing_to` before
    /// [`ActionPanel::present`] — without this module growing a forwarding
    /// method for every `gtk::Popover` method a future caller might want.
    pub fn popover(&self) -> &gtk::Popover {
        &self.popover
    }

    /// Replaces the store's contents with `item.actions`, in
    /// [`Item::actions`]'s own order, and resets the filter and selection
    /// to a freshly opened panel's state — cleared entry text, everything
    /// visible, the first row selected if any exist. Returns whether
    /// `item` has any actions at all; [`ActionPanel::present`] is the one
    /// caller that uses that to decide whether to actually open, per this
    /// module's top doc comment, "Zero actions: no mystery box".
    ///
    /// Always repopulates fully, even for an item this panel already shows
    /// — there is no "same item, skip the rebuild" fast path, because nothing
    /// about this panel is expensive enough (bounded at
    /// [`hop_protocol::limits::MAX_ACTIONS_PER_ITEM`] rows) to make that
    /// worth the extra state a "was this already populated for this exact
    /// item" check would need to track correctly.
    fn populate(&self, item: &Item) -> bool {
        let wrapped: Vec<BoxedAnyObject> = item
            .actions
            .iter()
            .cloned()
            .map(BoxedAnyObject::new)
            .collect();
        self.store.splice(0, self.store.n_items(), &wrapped);
        // Set directly rather than through `self.entry.set_text("")`
        // relying on its `changed` signal: `gtk::Editable::set_text` does
        // not fire `changed` when the text does not actually change (a
        // panel already showing an empty filter, opened for a *second*
        // item, would otherwise skip the refilter this populate still
        // needs to run against the new store contents) — see
        // `ActionPanel::refilter`'s own doc comment for the one thing
        // both routes must always do afterward regardless.
        self.entry.set_text("");
        self.refilter();
        !item.actions.is_empty()
    }

    /// Re-runs [`filter`](ActionPanel::filter) against the store's current
    /// contents and resets the selection — the one operation both
    /// [`ActionPanel::populate`] (a fresh item) and `entry`'s own `changed`
    /// signal (the user typed) need, and the only place either ever touches
    /// [`filter`](ActionPanel::filter) or [`selection`](ActionPanel::selection)
    /// together, so the two can never drift out of step with each other.
    fn refilter(&self) {
        self.filter.changed(gtk::FilterChange::Different);
        self.reset_selection();
    }

    /// Selects the first visible row if any exist, or explicitly clears the
    /// selection if none do.
    ///
    /// The `if` arm is the half doing real work, confirmed by deliberately
    /// deleting this whole function's body while writing this issue's
    /// tests: with `autoselect(false)` (set in [`ActionPanel::new`]),
    /// `gtk::SingleSelection` does *not* select anything on its own even
    /// when a freshly non-empty model appears — `selected()` stayed
    /// [`gtk::INVALID_LIST_POSITION`] after a `populate` of three actions
    /// until this arm ran, which is what
    /// `tests/action_panel.rs`'s `assert_arrow_keys_move_and_clamp_the_selection`
    /// (its very first assertion) pins directly. The `else` arm turned out
    /// to be belt-and-braces rather than load-bearing by that same
    /// experiment: `gtk::SingleSelection` with `autoselect(false)` already
    /// clears `selected` to [`gtk::INVALID_LIST_POSITION`] on its own once
    /// its model turns genuinely empty (removing this arm alone did not
    /// break `assert_filter_matching_nothing_leaves_nothing_runnable`).
    /// Kept anyway, explicit rather than assumed: this issue's own "a
    /// filter matching nothing must not leave a stale selection that Enter
    /// would then run" requirement is exactly the property `gtk::SingleSelection`
    /// docs describe only as a consequence of `autoselect`, not as its own
    /// documented contract in those words — the same "state this crate's
    /// contract explicitly rather than lean on a GTK default's incidental
    /// behavior" judgment `ui::window::HopWindow::build`'s own comment on
    /// `set_autoselect(false)` already makes for the opposite transition
    /// (a model becoming non-empty). `gtk::SingleSelection::set_selected`
    /// accepts [`gtk::INVALID_LIST_POSITION`] as "select nothing" by GTK's
    /// own documented contract for that constant, the identical sentinel
    /// `ui::window::HopWindow::activate_selected` already checks incoming.
    fn reset_selection(&self) {
        if self.selection.n_items() > 0 {
            self.selection.set_selected(0);
        } else {
            self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
    }

    /// Moves the selection by `delta` rows over the *filtered* model,
    /// clamped to its bounds — `ui::window::HopWindow::move_selection`'s
    /// own shape, applied to [`selection`](ActionPanel::selection) instead
    /// of a results list's unfiltered store. An empty filtered list has
    /// nothing to move and is left alone (not reset to a stale "first row"
    /// that does not exist).
    fn move_selection(&self, delta: i32) {
        let len = self.selection.n_items();
        if len == 0 {
            return;
        }
        let current = self.selection.selected();
        let current = if current == gtk::INVALID_LIST_POSITION {
            0
        } else {
            current as i32
        };
        let next = (current + delta).clamp(0, len as i32 - 1);
        self.selection.set_selected(next as u32);
    }

    /// The [`Action`] at `position` in the filtered model, or `None` if
    /// `position` names no row — returns the whole `Action`, not just its
    /// `id`, since [`ActionPanel::activate_at`] needs the `id` while
    /// `tests/action_panel.rs`'s own assertions need the `label` too.
    fn action_at(&self, position: u32) -> Option<Action> {
        self.selection
            .item(position)
            .map(|object| action_of(&object))
    }

    /// Reports the currently selected action through `on_choose` and closes
    /// the panel — Enter's own effect, and (via [`ActionPanel::wire_mouse`])
    /// a mouse click's, matching `ui::window::HopWindow::activate_selected`/
    /// `activate_at`'s "both routes resolve to one function" shape exactly.
    /// A `selected()` of [`gtk::INVALID_LIST_POSITION`] — nothing selected,
    /// per [`ActionPanel::reset_selection`] whenever the filtered list is
    /// empty — reports nothing and does not close the panel, since there is
    /// nothing to report and Enter over an empty filtered list is not
    /// itself a request to dismiss.
    fn activate_selected(&self) {
        let selected = self.selection.selected();
        if selected == gtk::INVALID_LIST_POSITION {
            return;
        }
        self.activate_at(selected);
    }

    /// Reports the action at `position`, or does nothing if `position`
    /// names no row — shared by [`ActionPanel::activate_selected`] (Enter)
    /// and [`ActionPanel::wire_mouse`]'s click handler (which already has
    /// the position GTK's `activate` signal reported), the same split
    /// `ui::window`'s `activate_selected`/`activate_at` pair makes.
    fn activate_at(&self, position: u32) {
        let Some(action) = self.action_at(position) else {
            return;
        };
        (self.on_choose)(action.id);
        self.dismiss();
    }

    /// Presents the panel for `item`, anchored to `parent`, if `item` has
    /// any actions — otherwise leaves it closed. Returns whether it opened.
    ///
    /// Calling `gtk::Popover::popup` before `parent` sits under a realized
    /// [`gtk::Native`] is not a crash — verified directly against this
    /// crate's real, installed GTK 4.14 while writing this issue's tests —
    /// but it is also not a real presentation: GTK logs a `g_critical`
    /// ("widget not in toplevel") and the popover's own `visible` property
    /// never becomes `true`. This function does not guard against that
    /// case itself; it is the caller's obligation to hand `present` a
    /// `parent` that is already part of a window, the same obligation any
    /// `gtk::Popover` user has. `tests/action_panel.rs` discharges it by
    /// building a real `gtk::Window`, giving it a real child, and calling
    /// `gtk::Window::present` before this method ever runs.
    ///
    /// `parent` is re-parented every call, unconditionally: a popover
    /// already parented to a different widget from a previous `present`
    /// (design spec decision 6's right-click case, where each invocation
    /// can anchor to a different row) would make a bare `set_parent` panic
    /// against GTK's own "widget already has a parent" assertion, so any
    /// existing parent is dropped first.
    ///
    /// [`PANEL_SHOWN_CLASS`] is deliberately **not** added here, in the same
    /// turn as `popup()` — see this module's top doc comment, "Why the
    /// class add is deferred by one main-loop turn," for the defect that
    /// shape produces and why [`glib::idle_add_local_once`] instead of a
    /// direct call is the fix, not a stylistic preference.
    pub fn present(&self, item: &Item, parent: &impl IsA<gtk::Widget>) -> bool {
        let has_actions = self.populate(item);
        if !has_actions {
            return false;
        }

        if self.popover.parent().is_some() {
            self.popover.unparent();
        }
        self.popover.set_parent(parent);
        self.popover.popup();

        // Deferred by one main-loop turn, on purpose — see this module's
        // top doc comment. `popup()` above has already mapped `self.panel`
        // at its base `.hop-action-panel { opacity: 0; }` style by the time
        // this line runs; adding the class here directly would give the
        // widget no earlier, already-mapped style to fade in from, which is
        // the exact inert-fade defect this deferral exists to avoid.
        let panel = self.panel.clone();
        glib::idle_add_local_once(move || {
            panel.add_css_class(PANEL_SHOWN_CLASS);
        });

        self.entry.grab_focus();

        true
    }

    /// Closes the panel without reporting anything — Escape's own effect,
    /// `gtk::Popover`'s own `autohide` outside-click dismissal's effect
    /// (wired at construction, not by this function), and the tail end of
    /// [`ActionPanel::activate_at`] once it has already reported a choice.
    /// Safe to call on a panel that was never opened (`popdown` on an
    /// already-hidden popover is a documented no-op), which is what lets
    /// [`ActionPanel::handle_key`] route Escape here unconditionally rather
    /// than checking `is_visible()` first.
    ///
    /// Removing [`PANEL_SHOWN_CLASS`] here does **not** play a fade-out —
    /// see this module's top doc comment, "Why the class add is deferred,"
    /// closing section, for why `gtk::Popover::popdown()` unmaps
    /// synchronously and so gives no frame for any dismiss transition to
    /// paint. This removal exists to leave the panel in a genuinely
    /// unshown state for its *next* `present`, not to animate this one's
    /// close.
    pub fn dismiss(&self) {
        self.popover.popdown();
        self.panel.remove_css_class(PANEL_SHOWN_CLASS);
    }

    /// Resolves one key press into this panel's fixed navigation contract —
    /// Up/Down move the selection, Return reports it, Escape dismisses —
    /// and reports whether it claimed the key.
    ///
    /// # Why this compares `gdk::Key` literally, rather than going through
    /// `crate::keymap::Keymap`
    ///
    /// `crate::keymap` is explicitly out of this issue's scope (`keymap.rs`
    /// is one of the files this issue's brief names as belonging to a
    /// later wiring slice, not this one), and its own module doc comment
    /// is emphatic that `ui::window` "never compares a `gdk::Key` against a
    /// literal" specifically because its bindings are meant to be
    /// user-configurable. This panel's four keys are not that: they are a
    /// fixed, non-configurable navigation contract every menu-shaped
    /// overlay in this crate is expected to share (the results list itself
    /// hardcodes nothing key-related the same way — its own Up/Down/Enter/
    /// Escape all go through the keymap precisely because *that* surface
    /// is the one design spec decision 4 promises a `[keymap]` table can
    /// rebind). Comparing `gdk::Key::Up` and friends literally here is
    /// therefore not the shortcut `keymap.rs`'s own doc comment warns
    /// against — it is the correct tool for a contract nothing configures.
    ///
    /// # Why this is `pub` and takes a bare `gdk::Key`, not a real event
    ///
    /// `ui::window::HopWindow`'s own test suite settled this exact question
    /// for `dispatch_action`: rather than synthesize a real `GdkEvent`
    /// through a real `gtk::EventControllerKey` (attempted and found not
    /// worth it — see that module's test module doc comment), its tests
    /// call the resolved dispatch function directly with a plain,
    /// hand-built value. This method is that same shape, made `pub`
    /// because `tests/action_panel.rs` is a separate integration test crate
    /// (unlike `ui::window`'s own inline `#[cfg(test)]` module, which can
    /// reach a private method in the same file) and therefore needs a
    /// public entry point to drive the identical proof.
    pub fn handle_key(&self, key: gdk::Key) -> glib::Propagation {
        match key {
            gdk::Key::Up => {
                self.move_selection(-1);
                glib::Propagation::Stop
            }
            gdk::Key::Down => {
                self.move_selection(1);
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                self.activate_selected();
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                self.dismiss();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    /// Wires `entry`'s own `changed` signal to [`ActionPanel::refilter`] —
    /// the route a real keystroke takes, as opposed to
    /// [`ActionPanel::populate`]'s own direct call for a freshly presented
    /// item. See this module's top doc comment, "Self-referential
    /// closures, and why that is not a leak here", for why this clones
    /// `self` into the closure rather than only the fields it touches.
    fn wire_entry(&self) {
        let panel = self.clone();
        self.entry.connect_changed(move |_entry| {
            panel.refilter();
        });
    }

    /// Attaches the one `gtk::EventControllerKey` this panel has, to
    /// `entry` specifically (the widget expected to hold keyboard focus for
    /// as long as the panel is open — [`ActionPanel::present`] grabs focus
    /// onto it), in [`gtk::PropagationPhase::Capture`] so this panel's own
    /// Up/Down/Return/Escape contract is resolved before `gtk::Entry`'s own
    /// default key bindings ever see those four keys — `ui::window::HopWindow
    /// ::wire_keyboard`'s own reasoning for the identical phase choice,
    /// applied to this panel's entry instead of the whole window.
    fn wire_keys(&self) {
        let panel = self.clone();
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        controller.connect_key_pressed(move |_controller, key, _keycode, _modifiers| {
            panel.handle_key(key)
        });
        self.entry.add_controller(controller);
    }

    /// Wires `list_view`'s own `activate` signal (a single click, per
    /// `set_single_click_activate(true)` in [`ActionPanel::new`]) to
    /// [`ActionPanel::activate_at`] — `ui::window`'s own
    /// `wire_list_activation`, applied to this panel's list instead of the
    /// results list. Not required by this issue's own test list (mouse
    /// activation of an action row is design spec decision 6's territory,
    /// not decision 5's), but wiring it costs one shared function call and
    /// leaves no half-built affordance for a later slice to notice missing.
    fn wire_mouse(&self, list_view: &gtk::ListView) {
        let panel = self.clone();
        list_view.connect_activate(move |_list_view, position| {
            panel.activate_at(position);
        });
    }
}

#[cfg(test)]
mod tests {
    use hop_protocol::ActionKind;

    use super::*;

    #[test]
    fn label_matches_is_case_insensitive_substring() {
        assert!(label_matches("Copy Path", "path"));
        assert!(label_matches("Copy Path", "COPY"));
        assert!(label_matches("Copy Path", ""));
        assert!(!label_matches("Copy Path", "xyz"));
    }

    #[test]
    fn kind_hint_names_every_action_kind() {
        assert_eq!(kind_hint(&ActionKind::Open), "Open");
        assert_eq!(kind_hint(&ActionKind::Focus), "Focus");
        assert_eq!(kind_hint(&ActionKind::Copy), "Copy");
        assert_eq!(kind_hint(&ActionKind::Run), "Run");
        assert_eq!(kind_hint(&ActionKind::CloseWindow), "Close Window");
        assert_eq!(kind_hint(&ActionKind::MoveToWorkspace), "Move to Workspace");
        assert_eq!(kind_hint(&ActionKind::OpenUrl), "Open URL");
    }
}
