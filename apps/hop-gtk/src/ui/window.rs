//! The pre-built launcher window: constructed once at startup, presented on
//! demand rather than rebuilt per presentation — acceptance criterion 2.
//! See `app`'s module doc for what "on demand" resolves to for this issue.
//!
//! # Key dispatch is keymap-driven, not hardcoded — issue #182
//!
//! [`HopWindow::build`] takes a [`crate::keymap::Keymap`] and attaches one
//! [`gtk::EventControllerKey`] to the window itself, in
//! [`gtk::PropagationPhase::Capture`]. Every key press asks that keymap
//! which [`crate::keymap::Action`] (if any) it means and, for a recognized
//! one, calls [`HopWindow::dispatch_action`] — the single place every
//! action's effect lives. **No handler here compares a `gdk::Key` against a
//! literal**; the only thing this module knows about any specific key is
//! that a `key-pressed` signal exists to report one.
//!
//! Capture, rather than the default Bubble phase, is deliberate: GTK4
//! propagates one key event root-to-target (Capture), then target
//! (Target), then target-to-root (Bubble). `GtkEntry`'s own default
//! bindings — Tab moving focus, for instance — run as part of its own
//! Target/Bubble handling, which is *after* the window's Capture phase in
//! that ordering regardless of which widget has focus. Attaching this
//! crate's controller at Capture on the window is what lets it see every
//! key press first and decide, itself, whether GTK's own default binding
//! for that key should ever run at all: [`glib::Propagation::Stop`] for a
//! key the keymap resolves to an action (so, for example, Tab never falls
//! through to GTK's focus-move binding once [`crate::keymap::Action::CompletePrefix`]
//! claims it), [`glib::Propagation::Proceed`] for everything else, so
//! ordinary typing, Backspace, and text-cursor movement in the query entry
//! are completely unaffected — this controller never even learns those key
//! presses happened beyond deciding to let them through.
//!
//! One consequence worth naming: because Return is now resolved through the
//! keymap like every other key, `GtkEntry`'s own built-in `activate` signal
//! (this module's previous, pre-#182 doc comment described it as "not a
//! comparison this code makes") is no longer wired to anything — Capture
//! consumes Return before that signal would ever fire, since the window's
//! controller runs first and returns `Stop` the moment the keymap resolves
//! Return to [`crate::keymap::Action::Activate`]. `activate_selected` is
//! reached exactly once per Enter press, through the keymap, whichever key
//! the keymap currently binds to that action.
//!
//! # Selection is one indicator that moves, not a per-row class
//!
//! [`HopWindow::selection`] (a `GtkSingleSelection`) is the *model* the list
//! view and keyboard navigation act on — GTK requires a selection model for
//! `GtkListView` regardless of how selection is drawn. What matters
//! structurally is [`HopWindow::indicator`]: one persistent, unstyled widget,
//! repositioned over whichever row is currently selected by moving its top
//! margin, rather than every row toggling a "selected" CSS class on itself
//! as it is bound.
//!
//! The second shape is the one the design spec rules out (issue #179's
//! brief: "a single indicator that moves, not a per-row style class"). A
//! per-row class has to be re-synced on every `bind` as rows recycle
//! underneath a selected index that does not move with them — the row that
//! *was* selected can be recycled away to show a different item entirely
//! while still carrying the class, unless every bind re-checks "is my new
//! position the selected one" and toggles accordingly. That check would work,
//! but it is exactly the kind of per-row bookkeeping recycling exists to
//! avoid needing. One widget, repositioned on [`gtk::SingleSelection`]'s
//! `notify::selected` and on the list's scroll offset changing, has nothing
//! to resync: it is either drawn at the right `y` for the current selection
//! or it is not, and both signal handlers below only ever do that one
//! computation.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gio::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, glib};

use hop_protocol::{
    Action as WireAction, ActionId, ActionKind, CopyText, ExecOutcome, Item, ItemId, ItemSubtitle,
    ItemTitle, Kind, MAX_TITLE, RecentItem,
};

use crate::ipc::{CommandSender, IpcCommand, IpcEvent};
use crate::keymap::{Action, Keymap};
use crate::tokens;
use crate::ui::action_panel::ActionPanel;
use crate::ui::offline_indicator::OfflineIndicator;
use crate::ui::{marker_highlight, mode_label, model, row, view};

/// Rows moved per [`Action::PageUp`]/[`Action::PageDown`]. A fixed step
/// rather than one derived from the scrolled window's currently allocated
/// height (which would need a layout query at press time, and would change
/// with every resize) — five is a plain, easy-to-reason-about jump that
/// gives Page Up/Page Down a distinct feel from Up/Down without pretending
/// to track "one screenful", a refinement nothing in §8 asks this issue to
/// build.
const PAGE_STEP: i64 = 5;

/// The query `gtk::Entry`'s widget name and CSS class — one string serving
/// both, the doubled-identity precedent `ui::row`'s `SUBTITLE_CHILD_NAME`
/// doc comment documents. `assets/stylesheet.css`'s `.hop-query-entry`
/// rule (issue #253's accent caret) selects on it.

#[derive(Clone)]
enum LocalAction {
    WebSearch(String),
    Copy(String),
}

#[derive(Default)]
struct LocalActionRegistry {
    generation: u64,
    rendered_generation: Option<u64>,
    actions: HashMap<String, LocalAction>,
}

impl LocalActionRegistry {
    fn begin_query(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.clear();
    }

    fn clear(&mut self) {
        self.actions.clear();
        self.rendered_generation = None;
    }

    fn install(&mut self, actions: impl IntoIterator<Item = (String, LocalAction)>) {
        self.clear();
        self.actions.extend(actions);
        self.rendered_generation = Some(self.generation);
    }

    fn action_for(&self, item_id: &ItemId) -> Option<LocalAction> {
        (self.rendered_generation == Some(self.generation))
            .then(|| self.actions.get(item_id.as_str()).cloned())
            .flatten()
    }
}

trait UserActionSink {
    fn copy_text(&self, text: &str);
    fn launch_uri(&self, uri: &str) -> Result<(), String>;
}

struct GtkUserActionSink;

impl UserActionSink for GtkUserActionSink {
    fn copy_text(&self, text: &str) {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(text);
        }
    }

    fn launch_uri(&self, uri: &str) -> Result<(), String> {
        gtk::gio::AppInfo::launch_default_for_uri(uri, gtk::gio::AppLaunchContext::NONE)
            .map_err(|err| err.to_string())
    }
}

struct ScreenshotUserActionSink;

impl UserActionSink for ScreenshotUserActionSink {
    fn copy_text(&self, _text: &str) {}

    fn launch_uri(&self, _uri: &str) -> Result<(), String> {
        Err("navigation is disabled while capturing a screenshot".to_string())
    }
}

struct PendingProvider {
    id: String,
    attribution: gtk::Label,
    row: gtk::Box,
    bars: [gtk::Box; 2],
}
const QUERY_ENTRY_NAME: &str = "hop-query-entry";

/// Which of two run purposes a built window serves: the ordinary interactive
/// launcher, or a one-shot `--screenshot` capture. "Run purpose", not "mode"
/// — mode is reserved vocabulary (it names how a query is interpreted); the
/// disambiguation follows the same pattern as [`crate::keymap::Action`].
///
/// The distinction gates exactly one wiring decision — close-on-focus-loss
/// ([`Self::wire_dismiss_on_focus_loss`], issue #232's X11 and GNOME
/// Wayland rows). That behavior presumes a user who can click away and
/// expects the overlay to follow. A `--screenshot` run has no user, so the
/// acceptance criterion is flat: a capture harness's window must not be
/// dismissible at all, whatever strategy the session resolves. `Screenshot`
/// therefore skips that wiring regardless of what
/// [`crate::session::OverlayStrategy`] asks for.
///
/// This is not what caused the flake issue #261 reports — that signature
/// (silent exit 1, no error print) traces to an Xvfb display-number race in
/// `tests/x11_smoke.rs`'s harness, fixed alongside this. But reproducing
/// that flake locally surfaced this as a separate latent failure mode of
/// its own: a background focus loss really did hide a wired capture window
/// and hung the run to its own printed timeout. Unwiring dismissal fixes
/// that on its own merits rather than as a workaround for the reported one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPurpose {
    /// The interactive launcher (`hop-gtk` with no mode flags).
    Interactive,
    /// A one-shot `hop-gtk --screenshot <path>` capture run.
    Screenshot,
}

/// The pre-built window and everything it owns. `Clone` and cheap to clone —
/// every field is a GTK/glib reference-counted handle — so `app`'s
/// `glib::spawn_future_local` event loop can hold one across `.await`
/// points without fighting the borrow checker.
#[derive(Clone)]
pub struct HopWindow {
    pub window: adw::ApplicationWindow,
    entry: gtk::Entry,
    mode_label: gtk::Label,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    // Kept mainly so this module's own `#[cfg(test)]` dispatch tests can
    // reach the real `activate` signal `wire_list_activation` connects in
    // `build` (D5's mouse-activation regression test emits it directly,
    // the same technique `tests/view_tree_renderer.rs` uses for `setup` —
    // see this struct's own doc comment). Nothing in production code reads
    // this field back out after `build` constructs it; the wiring itself
    // is done before `HopWindow` even exists, against the local `list_view`
    #[cfg_attr(not(test), allow(dead_code))]
    list_view: gtk::ListView,
    indicator: gtk::Widget,
    scrolled: gtk::ScrolledWindow,
    status: gtk::Label,
    state_header: gtk::Label,
    state_stack: gtk::Stack,
    pending_surface: gtk::Box,
    error_pin: gtk::Box,
    error_title: gtk::Label,
    error_subtitle: gtk::Label,
    local_actions: Rc<RefCell<LocalActionRegistry>>,
    user_actions: Rc<dyn UserActionSink>,
    pending_user_executions: Rc<Cell<u32>>,
    pending_providers: Rc<RefCell<Vec<PendingProvider>>>,
    query_had_results: Rc<Cell<bool>>,
    query_pending: Rc<Cell<bool>>,
    state_items: Rc<RefCell<Vec<Item>>>,
    cached_items: Rc<RefCell<Vec<Item>>>,
    /// `ui::offline_indicator`'s module doc for what it is and
    /// [`HopWindow::apply_event`] for the one place it is ever shown or
    /// hidden.
    offline_indicator: OfflineIndicator,
    /// Issue #254's ctrl-K action panel — built once here, alongside every
    /// other widget `build` constructs, per this module's "never rebuilt"
    /// convention (this struct's own top doc comment) and
    /// `ui::action_panel`'s own doc comment on why the panel is built once
    /// and presented on demand rather than reconstructed per open. See
    /// [`HopWindow::open_secondary_action_menu`] for the one place this is
    /// ever presented, and [`HopWindow::dispatch_action`]'s `Action::Dismiss`
    /// arm for the one place it is ever dismissed by a key press.
    action_panel: ActionPanel,
    /// The item [`HopWindow::open_secondary_action_menu`] most recently
    /// opened [`Self::action_panel`] for — pinned at *open* time, read at
    /// *choose* time, by the `on_choose` closure [`HopWindow::build`] wires
    /// into `action_panel` itself. See
    /// [`HopWindow::open_secondary_action_menu`]'s own doc comment,
    /// "Pinning the item at open time, not choose time," for why this
    /// indirection exists rather than that closure re-reading
    /// `self.selection.selected()` when a choice is reported.
    ///
    /// `Rc<RefCell<..>>`, not a plain field: the closure captured into
    /// `action_panel` at construction and the `&self` methods on this
    /// struct both need to reach the *same* cell, and the closure outlives
    /// any one `&self` borrow — the identical shape
    /// `ui::offline_indicator::OfflineIndicator`'s own `stamp` field would
    /// need if two independent closures ever had to share one mutable slot
    /// (today they do not; this is the first field in this window that
    /// does). This workspace denies `unsafe_code`, so `RefCell`'s runtime
    /// borrow check is the only interior-mutability tool available — safe
    /// here because every borrow is short-lived and non-overlapping: one
    /// `borrow_mut` to set it in [`HopWindow::open_secondary_action_menu`],
    /// one `borrow_mut().take()` to read-and-clear it in the `on_choose`
    /// closure, each call synchronous and single-threaded on the GTK main
    /// loop, never nested.
    pinned_action_item: Rc<RefCell<Option<Item>>>,
    cmd_tx: CommandSender,
}

impl HopWindow {
    /// Builds the window once. Not shown — `app`'s `activate` handler is
    /// what calls [`HopWindow::present_with_token`], per the
    /// pre-built-hidden-window requirement.
    ///
    /// `keymap` is consumed here, moved into the `key-pressed` closure
    /// [`HopWindow::wire_keyboard`] installs — it is not kept on
    /// [`HopWindow`] itself, since nothing after construction needs to
    /// consult it directly; every action it can resolve to already has a
    /// name ([`crate::keymap::Action`]) that [`HopWindow::dispatch_action`]
    /// switches on without ever asking the keymap "which key was that"
    /// again.
    ///
    /// `strategy` is the overlay decision `app` resolved and logged at
    /// startup (issue #232) — see `session`'s module doc for why the
    /// decision is made once there rather than re-derived here. All three
    /// arms wire behavior onto the window below: X11's self-positioning
    /// (delegated entirely to `x11::apply_self_positioning`),
    /// close-on-focus-loss in the two sessions that ask for it (GNOME
    /// Wayland's documented shape, and X11's parity with it) — gated on
    /// `purpose`, so a `--screenshot` run never wires it (see
    /// [`RunPurpose`] for why) — and — since issue #233 — the layer-shell
    /// arm, which applies
    /// `layer_shell::apply_or_fallback` when the strategy is LayerShell:
    /// the compositor owns placement and focus for a layer surface, and
    /// the probe inside decides supported-versus-fallback.
    pub fn build(
        app: &adw::Application,
        cmd_tx: CommandSender,
        keymap: Keymap,
        strategy: crate::session::OverlayStrategy,
        purpose: RunPurpose,
    ) -> Self {
        let user_actions: Rc<dyn UserActionSink> = match purpose {
            RunPurpose::Interactive => Rc::new(GtkUserActionSink),
            RunPurpose::Screenshot => Rc::new(ScreenshotUserActionSink),
        };
        Self::build_with_user_actions(app, cmd_tx, keymap, strategy, purpose, user_actions)
    }

    fn build_with_user_actions(
        app: &adw::Application,
        cmd_tx: CommandSender,
        keymap: Keymap,
        strategy: crate::session::OverlayStrategy,
        purpose: RunPurpose,
        user_actions: Rc<dyn UserActionSink>,
    ) -> Self {
        let (window_w, window_h) = *tokens::WINDOW_SIZE_PX;
        let row_h = *tokens::ROW_HEIGHT_PX;

        let entry = gtk::Entry::builder()
            .placeholder_text("type, or ? for prefixes")
            .build();
        // Doubled identity (widget name + CSS class, one string serving
        // both — the same precedent `ui::row`'s `SUBTITLE_CHILD_NAME` doc
        // comment documents): `assets/stylesheet.css`'s `.hop-query-entry`
        // rule needs a selector to hang the accent caret colour on (issue
        // #253), and the name keeps a future `find_named_child` caller from
        // having to invent a second string for the same widget.
        entry.set_widget_name(QUERY_ENTRY_NAME);
        entry.add_css_class(QUERY_ENTRY_NAME);

        // The mode label (issue #184) sits as an overlay child over `entry`
        // rather than a sibling beside it — see `ui::mode_label::build`'s own
        // doc comment, "No layout shift", for why an overlay is what makes
        // its appearing, disappearing, and its text changing length none of
        // them ever move anything else in this window.
        let mode_label = mode_label::build();
        let entry_overlay = gtk::Overlay::new();
        entry_overlay.set_child(Some(&entry));
        entry_overlay.add_overlay(&mode_label);

        let store = model::new_store();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        // `autoselect` off: an empty result list, or one the user has not
        // touched yet, should have nothing highlighted. GTK's default is to
        // always keep something selected once the model is non-empty, which
        // is the wrong empty-state/no-results behavior for a launcher (§8a
        // names both as designed, not defaulted, states) — `apply_event`
        // below sets the selection explicitly per state instead of trusting
        // this default.
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        // `keymap` is cloned here, not moved: `view::build` needs its own
        // copy to resolve every row's action hint's display string, exactly
        // once, before handing the factory back (issue #197 — see
        // `ui::view::build`'s own doc comment, and `ui::view::Node`'s "why
        // `Row` carries an already-resolved `Option<String>`, not a
        // `Keymap`"), and `wire_keyboard` below still needs the original to
        // resolve key presses into `Action`s. `Keymap` is `Clone` exactly
        // so both consumers can each own a copy rather than one borrowing
        // from the other for the window's whole lifetime — this one-time
        // clone, at startup, is unrelated to the per-bind clone finding 3
        // removed from `view::build`'s own closures.
        let query_had_results = Rc::new(Cell::new(false));
        let query_pending = Rc::new(Cell::new(false));
        let local_actions = Rc::new(RefCell::new(LocalActionRegistry::default()));
        let pending_providers = Rc::new(RefCell::new(Vec::new()));
        let pending_user_executions = Rc::new(Cell::new(0));
        let factory = view::build(keymap.clone());
        let list_view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        // Single click activates a row rather than GTK's own double-click
        // default — the launcher convention §8 names ("mouse click still
        // activates a row") reads as one click, matching how a result list
        // in a launcher behaves everywhere else this UI takes cues from,
        // and D5 of the plan this issue implements found no
        // `connect_activate` anywhere in this crate before this change: this
        // is new wiring, not a preserved default.
        list_view.set_single_click_activate(true);
        wire_list_activation(
            &list_view,
            &selection,
            &cmd_tx,
            &local_actions,
            &user_actions,
            &pending_user_executions,
        );

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list_view)
            .vexpand(true)
            .build();

        // Unstyled per this issue's scope (§8a's design pass owns every
        // visual value); the class name is wired in now so that pass has
        // something to hook a rule to without touching this file again.
        let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        indicator.add_css_class("hop-selection-indicator");
        indicator.set_height_request(row_h);
        indicator.set_valign(gtk::Align::Start);
        indicator.set_halign(gtk::Align::Fill);
        indicator.set_can_target(false);
        indicator.set_visible(false);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scrolled));
        overlay.add_overlay(&indicator);

        let (error_pin, error_title, error_subtitle) = build_error_pin();
        error_pin.set_halign(gtk::Align::Fill);
        error_pin.set_valign(gtk::Align::End);
        error_pin.set_margin_start(*tokens::OFFLINE_ROW_GAP_PX);
        error_pin.set_margin_end(*tokens::OFFLINE_ROW_GAP_PX);
        error_pin.set_margin_bottom(*tokens::OFFLINE_ROW_GAP_PX);
        overlay.add_overlay(&error_pin);

        let pending_surface = build_pending_surface(Rc::clone(&pending_providers));
        pending_surface.set_halign(gtk::Align::Fill);
        pending_surface.set_valign(gtk::Align::End);
        pending_surface.set_margin_bottom(*tokens::OFFLINE_ROW_GAP_PX);
        overlay.add_overlay(&pending_surface);

        let state_stack = gtk::Stack::new();
        state_stack.set_vexpand(true);
        state_stack.add_named(&overlay, Some("results"));
        state_stack.set_visible_child_name("results");

        let state_header = gtk::Label::new(None);
        state_header.add_css_class("hop-state-header");
        state_header.set_xalign(0.0);
        state_header.set_visible(false);

        let status = gtk::Label::new(None);
        status.add_css_class("hop-status");
        status.set_xalign(0.0);
        status.set_visible(false);
        status.set_wrap(true);

        // Issue #200's offline indicator — built once, alongside every other
        // widget here, and starts hidden (`OfflineIndicator::build`'s own doc
        // comment).
        let offline_indicator = OfflineIndicator::build();

        let state_items = Rc::new(RefCell::new(Vec::new()));
        let cached_items = Rc::new(RefCell::new(Vec::new()));

        // Issue #254's ctrl-K action panel — built once, like every other
        // widget above, never rebuilt per open (see
        // `HopWindow::action_panel`'s own field doc comment). Not appended
        // to `content`: it presents itself as a `gtk::Popover` anchored to
        // `indicator` from `open_secondary_action_menu`, not as a
        // permanent member of this window's own layout.
        //
        // `pinned_action_item` is created before `action_panel` because the
        // `on_choose` closure below needs its own clone of the `Rc` before
        // the panel that closure belongs to exists — the same
        // build-the-shared-cell-first ordering `wire_keyboard`'s own
        // `keymap` capture uses for an unrelated reason (there, avoiding a
        // second `Keymap` load; here, giving two independent closures/
        // methods a handle to the one cell they must agree on).
        let pinned_action_item: Rc<RefCell<Option<Item>>> = Rc::new(RefCell::new(None));
        let action_panel = {
            let cmd_tx = cmd_tx.clone();
            let pinned_action_item = Rc::clone(&pinned_action_item);
            let local_actions = Rc::clone(&local_actions);
            let user_actions = Rc::clone(&user_actions);
            let pending_user_executions = Rc::clone(&pending_user_executions);
            ActionPanel::new(move |action_id| {
                // `take()`, not a borrow-and-clone: once a choice is
                // reported the pin has done its job for this `present`
                // call, and clearing it here is what keeps
                // `pinned_action_item` truthful as "the item the
                // *currently open* panel was opened for" rather than a
                // stale answer surviving after the panel it named has
                // already closed. `ActionPanel::activate_at` calls
                // `on_choose` at most once per `present` (it dismisses
                // immediately after), so there is no second call this
                // `take` could wrongly starve.
                let Some(item) = pinned_action_item.borrow_mut().take() else {
                    // Structurally unreachable in production —
                    // `present_action_panel_for_selected` below (the shared
                    // core of both `open_secondary_action_menu`, ctrl-K's
                    // handler, and `open_secondary_action_menu_at`, issue
                    // #254 AC2's right-click handler) always pins an item
                    // before the panel can be opened at all, so `on_choose`
                    // cannot fire with nothing pinned — but not `unwrap`,
                    // matching this crate's `clippy::unwrap_used` lint on a
                    // value this function cannot itself prove is `Some`
                    // from its own signature alone.
                    return;
                };
                dispatch_item_action(
                    &cmd_tx,
                    &item,
                    &local_actions,
                    action_id,
                    &user_actions,
                    &pending_user_executions,
                );
            })
        };
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&entry_overlay);
        content.append(&status);
        content.append(&offline_indicator.widget);
        content.append(&state_header);
        content.append(&state_stack);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(window_w)
            .default_height(window_h)
            .content(&content)
            .hide_on_close(true)
            .build();
        // Issue #253: the material mode (translucent vs. opaque window
        // ground) is decided and applied here — before layer-shell/X11
        // wiring below, and well before `app`'s `present_with_token` ever
        // shows this window — satisfying `assets/stylesheet.css`'s own
        // MATERIAL MODES comment that the decision is made and the class
        // applied once, before presentation. `material::resolve`
        // re-detects the session from the live display rather than taking
        // `strategy` as an input: the two decisions are independent (a
        // Wayland session's overlay *strategy* depends on layer-shell
        // support; its material *mode* never does — see `material`'s own
        // module doc for why Wayland is always opaque here), so nothing is
        // gained by threading one through the other.
        crate::material::apply(&window, crate::material::resolve());
        // Issue #233: the strategy — not a second probe — decides whether
        // this window becomes a layer surface. `apply_or_fallback` still
        // re-checks the probe internally (a documented no-op unless the
        // compositor answered "supported"), but gating on
        // `uses_layer_shell()` keeps exactly one decision authoritative:
        // the one `resolve_overlay_strategy` logged to stderr above.
        // X11's and every fallback row never reach it, so the ordinary
        // window those rows describe is what actually maps.
        if strategy.uses_layer_shell() {
            crate::layer_shell::apply_or_fallback(&window);
        }

        // Issue #232: the one remaining strategy arm that adds behavior to
        // the plain window. Layer-shell (when a feature-on build meets a
        // supporting compositor) owns placement and focus itself, and
        // `session` never pairs it with this arm.
        if strategy.self_positions() {
            crate::x11::apply_self_positioning(&window);
        }

        // Issue #254 AC2: per-row action-icon buttons (`ui::row::build`)
        // invoke a GAction named `ROW_ACTION_GROUP_PREFIX.ROW_ACTION_NAME`
        // ("row.run-action") rather than a plain `connect_clicked` closure
        // holding its own, separately-tracked mutable state — see
        // `ui::row`'s own top doc comment, "How a click runs the right
        // action", for why. This is where that name resolves to something
        // real: one parameterized `gio::SimpleAction`, installed on the
        // window itself (via `insert_action_group`) so every row
        // descendant can reach it by name regardless of which recycled
        // slot it currently lives in — action names resolve by walking up
        // the widget tree looking for a matching group, and the window is
        // the ancestor every row shares.
        // `.ok()`, not an `.expect()`: `ROW_ACTION_TARGET_TYPE` is the fixed
        // literal `"(ss)"`, a valid GVariant type string by construction, so
        // `Err` here is structurally unreachable — but a corrupted constant
        // should fail by silently registering a parameter-less action
        // (whose `connect_activate` below would then simply never receive a
        // parameter) rather than by an `.expect()`, which this crate's
        // `unwrap_used` lint (promoted to a hard error by `-D warnings` in
        // CI) refuses to spend on a one-time, build-time value like this
        // one.
        let row_action_target_type = glib::VariantTy::new(row::ROW_ACTION_TARGET_TYPE).ok();
        let row_run_action = gio::SimpleAction::new(row::ROW_ACTION_NAME, row_action_target_type);
        {
            let cmd_tx = cmd_tx.clone();
            let local_actions = Rc::clone(&local_actions);
            let user_actions = Rc::clone(&user_actions);
            let pending_user_executions = Rc::clone(&pending_user_executions);
            row_run_action.connect_activate(move |_action, parameter| {
                let Some(parameter) = parameter else {
                    return;
                };
                let Some((item_id, action_id)) = parameter.get::<(String, String)>() else {
                    return;
                };
                // Round-tripping strings that already satisfied
                // `ItemId`/`ActionId`'s own length bounds when
                // `ui::row::resolve_action_icons` packed them into this
                // exact target — re-validated here anyway rather than
                // trusted, the same "do not assume a value already passed
                // one check elsewhere" posture this crate takes with wire
                // data generally.
                let (Ok(item_id), Ok(action_id)) = (ItemId::new(item_id), ActionId::new(action_id))
                else {
                    return;
                };
                dispatch_id_action(
                    &cmd_tx,
                    &item_id,
                    &action_id,
                    &local_actions,
                    &user_actions,
                    &pending_user_executions,
                );
            });
        }
        let row_action_group = gio::SimpleActionGroup::new();
        row_action_group.add_action(&row_run_action);
        window.insert_action_group(row::ROW_ACTION_GROUP_PREFIX, Some(&row_action_group));

        let hop_window = HopWindow {
            window,
            entry,
            mode_label,
            store,
            selection,
            list_view,
            indicator: indicator.upcast(),
            scrolled,
            status,
            state_header,
            state_stack,
            pending_surface,
            error_pin,
            error_title,
            error_subtitle,
            local_actions: Rc::clone(&local_actions),
            user_actions,
            pending_user_executions,
            pending_providers,
            query_had_results,
            query_pending,
            state_items,
            cached_items,
            offline_indicator,
            action_panel,
            pinned_action_item,
            cmd_tx,
        };

        // Issue #254 review, finding 4: the row's own overflow chevron
        // (`ui::row::overflow_button_widget`) invokes a *second* GAction,
        // `row.open-actions` — see `ui::row`'s top doc comment, "A second
        // GAction, not a third `(item_id, action_id)` target", for why
        // this is not folded into `row_run_action` above. Registered here,
        // after `hop_window` exists rather than alongside `row_run_action`
        // above: its own handler,
        // [`HopWindow::open_action_panel_for_overflow`], needs a real
        // `&HopWindow` to select a row and present the panel on, which
        // does not exist yet at the point `row_run_action` is built — that
        // handler only ever needed `cmd_tx`, a plain clone available long
        // before `hop_window` is. `row_action_group` is still the same
        // `gio::SimpleActionGroup` already installed on `window` above (by
        // reference, not by value), so adding a second action to it here
        // extends the same, already-inserted group rather than installing
        // a second one under the same prefix.
        let row_open_actions_target_type =
            glib::VariantTy::new(row::ROW_OPEN_ACTIONS_TARGET_TYPE).ok();
        let row_open_actions =
            gio::SimpleAction::new(row::ROW_OPEN_ACTIONS_NAME, row_open_actions_target_type);
        {
            let hop_window = hop_window.clone();
            row_open_actions.connect_activate(move |_action, parameter| {
                let Some(parameter) = parameter else {
                    return;
                };
                let Some(item_id) = parameter.get::<String>() else {
                    return;
                };
                // Re-validated rather than trusted, matching
                // `row_run_action`'s own posture on the identical strings
                // just above.
                let Ok(item_id) = ItemId::new(item_id) else {
                    return;
                };
                hop_window.open_action_panel_for_overflow(&item_id);
            });
        }
        row_action_group.add_action(&row_open_actions);
        hop_window.render_empty_state();
        hop_window.wire_entry();
        hop_window.wire_selection_indicator();
        hop_window.wire_row_right_click();
        hop_window.wire_keyboard(keymap);
        if strategy.dismisses_on_focus_loss() && purpose == RunPurpose::Interactive {
            hop_window.wire_dismiss_on_focus_loss();
        }

        hop_window
    }

    /// Closes the window when it loses keyboard input focus — the
    /// close-on-focus-loss behavior design spec §2 documents for the GNOME
    /// Wayland row and issue #232 extends to X11 as parity. `close()` on a
    /// `hide_on_close` window hides rather than destroys it (see
    /// [`Self::dispatch_action`]'s `Action::Dismiss` arm), so dismissing
    /// keeps the pre-built window's "never rebuilt" property intact: the
    /// next toggle re-presents this same instance.
    ///
    /// The `is_visible` guard covers the one ordering where the notify
    /// fires without a user-visible focus loss: hiding the window itself
    /// ends any focus it had, which flips `is-active` to false on its way
    /// down. Closing an already-hidden window would be harmless, but the
    /// guard makes that path a no-op instead of relying on GTK treating it
    /// as one.
    fn wire_dismiss_on_focus_loss(&self) {
        self.window.connect_is_active_notify(|window| {
            if !window.is_active() && window.is_visible() {
                window.close();
            }
        });
    }

    /// Types `text` into the query entry, exactly as a keystroke would.
    ///
    /// This exists for `app`'s `--screenshot --query` path, and it
    /// deliberately does not take a shortcut past the UI: setting the entry's
    /// text fires the same `connect_changed` handler [`wire_entry`] installs,
    /// so the query reaches `ipc` by the route a real keystroke uses rather
    /// than by a second, capture-only one. Two things follow from that. The
    /// captured frame shows the query that produced it, instead of a
    /// placeholder above results nothing on screen appears to have asked for
    /// — and §8a compares mock frames against these captures, so a capture
    /// that misrepresents its own state is worse than no capture. And the
    /// path under test is the real one; a capture-only send would prove the
    /// socket works while leaving the entry-to-socket wiring unexercised.
    ///
    /// [`wire_entry`]: HopWindow::wire_entry
    pub fn set_query_text(&self, text: &str) {
        self.entry.set_text(text);
    }

    fn wire_entry(&self) {
        let cmd_tx = self.cmd_tx.clone();
        let hop_window = self.clone();
        self.entry.connect_changed(move |entry| {
            hop_window.begin_query(entry.text().as_str());
            cmd_tx.send(IpcCommand::Query(entry.text().to_string()));
        });
        // Enter running the selection's default action (acceptance
        // criterion 6) is wired in `wire_keyboard` now, through the keymap —
        // see this module's top doc comment, "Key dispatch is keymap-driven,
        // not hardcoded", for why `GtkEntry`'s own `activate` signal is no
        // longer connected to anything here.
    }

    /// Requests the daemon's persisted recents for the empty query. Unlike
    /// `set_query_text`, this sends even when the entry is already empty,
    /// which is required when a fresh window connects.
    pub fn request_empty_query(&self) {
        self.begin_query("");
        self.cmd_tx.send(IpcCommand::Query(String::new()));
    }

    /// Attaches the one [`gtk::EventControllerKey`] this window has, in
    /// [`gtk::PropagationPhase::Capture`] — see this module's top doc
    /// comment for why Capture, and for the Propagation choice each arm
    /// below makes. `keymap` is moved into the closure: it lives exactly as
    /// long as this window does, which is the process's whole lifetime (the
    /// pre-built window is never rebuilt — see this module's own top doc
    /// comment, "acceptance criterion 2").
    fn wire_keyboard(&self, keymap: Keymap) {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);

        let hop_window = self.clone();
        controller.connect_key_pressed(move |_controller, key, _keycode, modifiers| {
            match keymap.lookup(key, modifiers) {
                Some(action) => {
                    hop_window.dispatch_action(action);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });

        self.window.add_controller(controller);
    }

    /// Carries out `action` — the single place every [`Action`] this slice
    /// gives real behavior to takes effect, reached from [`wire_keyboard`]'s
    /// closure for a key press and, for [`Action::Activate`] specifically,
    /// also from [`wire_list_activation`] for a mouse click on a row (D5):
    /// both routes resolve to this one function rather than to two
    /// independently-written effects that happen to agree today.
    ///
    /// [`Action::SecondaryAction`] is dispatched here to
    /// [`HopWindow::open_secondary_action_menu`] — issue #254's real
    /// handler, replacing the do-nothing stub issue #182 left in its place
    /// (see that method's own doc comment for what it does now).
    /// [`Action::CompletePrefix`] still dispatches to
    /// [`HopWindow::complete_prefix`], still empty on purpose — its own
    /// unbuilt slice, untouched by this issue; see that method's own doc
    /// comment.
    ///
    /// [`Action::Dismiss`]'s own arm below is no longer a bare
    /// `self.window.close()` either: an open [`Self::action_panel`] claims
    /// Escape (Dismiss's default binding) for itself first — see that arm's
    /// own comment for why.
    fn dispatch_action(&self, action: Action) {
        match action {
            Action::NavigateUp => self.move_selection(-1),
            Action::NavigateDown => self.move_selection(1),
            Action::PageUp => self.move_selection(-PAGE_STEP),
            Action::PageDown => self.move_selection(PAGE_STEP),
            Action::Home => self.select_first(),
            Action::End => self.select_last(),
            Action::Activate => {
                activate_selected(
                    &self.selection,
                    &self.cmd_tx,
                    &self.local_actions,
                    &self.user_actions,
                    &self.pending_user_executions,
                );
            }
            Action::SecondaryAction => self.open_secondary_action_menu(),
            Action::CompletePrefix => self.complete_prefix(),
            Action::Dismiss => {
                if self.action_panel.popover().is_visible() {
                    // Issue #254: Escape closes the panel and returns focus
                    // to the list — it must not *also* dismiss the window
                    // underneath it. `ActionPanel::dismiss` is documented
                    // safe to call on an already-closed panel, so the
                    // `is_visible` guard here is only about *which* thing
                    // Escape closes, never about whether `dismiss` itself
                    // is safe to call unconditionally.
                    //
                    // This guard lives here, in `dispatch_action`, rather
                    // than relying on `gtk::Popover`'s own surface (a
                    // `gtk::Native` distinct from this window's — confirmed
                    // against this workspace's installed gtk4-rs: `Popover`
                    // implements `Native`) to keep this window's own
                    // `EventControllerKey` from ever seeing an Escape aimed
                    // at the panel's focused entry in the first place. That
                    // surface separation is real and is why a live keypress
                    // is expected to route to the panel's own
                    // `ActionPanel::handle_key` without this arm's help at
                    // all — but this test suite proves its behavior by
                    // calling `dispatch_action` directly (this file's own
                    // top doc comment gives the reason: no GTK4 backend in
                    // this environment can synthesize a real key event), so
                    // the *only* thing that suite can exercise, and the
                    // only thing this arm can be held to regardless of
                    // whatever the real surface routing does, is what
                    // `dispatch_action(Action::Dismiss)` itself does when
                    // called. Guarding here makes the invariant hold
                    // unconditionally rather than incidentally.
                    self.action_panel.dismiss();
                } else {
                    // `hide_on_close(true)` (set in `build`) is what makes
                    // `close()` hide the pre-built window rather than
                    // destroy it — the same "never rebuilt" property
                    // `present_with_token` relies on to `present()` this
                    // exact window again later.
                    self.window.close();
                }
            }
        }
    }

    /// Moves the list selection by `delta` rows, clamped to the store's
    /// bounds. An empty store, or no current selection (the empty/no-results
    /// states `apply_event` sets `INVALID_LIST_POSITION` for), has nothing
    /// to move — treated as "start from the first row" rather than doing
    /// nothing, so Down on a freshly-populated, not-yet-touched list starts
    /// the same way pressing it a second time would continue.
    fn move_selection(&self, delta: i64) {
        let len = self.store.n_items();
        if len == 0 {
            return;
        }
        let current = self.selection.selected();
        let current = if current == gtk::INVALID_LIST_POSITION {
            0
        } else {
            current as i64
        };
        let next = (current + delta).clamp(0, len as i64 - 1);
        self.selection.set_selected(next as u32);
    }

    /// Moves the list selection to the first row, or does nothing if the
    /// list is empty.
    fn select_first(&self) {
        if self.store.n_items() == 0 {
            return;
        }
        self.selection.set_selected(0);
    }

    /// Moves the list selection to the last row, or does nothing if the
    /// list is empty.
    fn select_last(&self) {
        let len = self.store.n_items();
        if len == 0 {
            return;
        }
        self.selection.set_selected(len - 1);
    }

    /// [`Action::SecondaryAction`]'s handler — bound in the keymap (`ctrl+k`
    /// by default) and reached from [`HopWindow::dispatch_action`]. Issue
    /// #182/#197 left this as a deliberate no-op, on the grounds that
    /// `hop-gtk` had no secondary-action menu to open yet; issue #254 built
    /// that menu (`ui::action_panel::ActionPanel`) and this is the "later
    /// issue" that do-nothing stub's own doc comment named — giving this
    /// function a real body.
    ///
    /// Resolves the selected item exactly the way [`activate_selected`]
    /// does — `self.selection.selected()`, guarded against
    /// [`gtk::INVALID_LIST_POSITION`], then [`gtk::SingleSelection::item`]
    /// decoded through [`model::item_of`] — rather than growing a second,
    /// independent "what item is selected" lookup for this one caller.
    /// Does nothing, gracefully, in exactly the same "nothing to act on"
    /// case `activate_selected` already treats as a no-op (an empty or
    /// no-results list), and again if the resolved item has no actions at
    /// all: [`ActionPanel::present`] itself refuses to open for that case
    /// (its own doc comment, "Zero actions: no mystery box") and reports
    /// `false`, which this method reads back to keep
    /// [`Self::pinned_action_item`] truthful rather than pinning an item no
    /// panel is actually showing.
    ///
    /// Anchored to [`Self::indicator`] — the one widget in this window that
    /// tracks the selected row's on-screen position
    /// ([`position_indicator`], driven by [`wire_selection_indicator`]) —
    /// rather than a `GtkListView` row widget reached by walking the list's
    /// own recycled children. `ui::view` and `ui::row` own that recycling
    /// machinery, and pulling a live row widget back out of it here would
    /// mean either growing a new, public seam on those modules for one
    /// caller or duplicating their own position bookkeeping. Design spec
    /// decision 6 backs this choice independently: ctrl-K is named there as
    /// opening the panel "as a general overlay," in contrast with a
    /// right-click, which opens it pinned to the literal cursor point — see
    /// [`Self::open_secondary_action_menu_at`], issue #254 AC2's own
    /// handler for that path, for why *it* anchors to `self.list_view` and
    /// a real `gtk::Popover::set_pointing_to` rectangle instead. A popover
    /// parented to `indicator` reads as "near the selected row," which is
    /// what "general overlay" asks for, without this method claiming the
    /// pixel-exact row anchoring decision 6 reserves for the cursor path.
    ///
    /// # Issue #254 review, finding 3: the row must be scrolled into view
    /// *before* `indicator` is trusted
    ///
    /// `indicator` tracks the selected row's position only for a row that
    /// is actually within the scrolled window's own viewport —
    /// [`position_indicator`]'s `offset = selected*row_h - scroll_value`
    /// is a real on-screen `y` only when that subtraction comes out
    /// non-negative. [`HopWindow::move_selection`] moves `self.selection`
    /// directly and does not itself keep the moved-to row scrolled into
    /// view, so a selection reached by holding `Down` can easily sit above
    /// the current viewport by the time ctrl-K runs. Before this review
    /// finding, this method anchored to `indicator` regardless: `position_
    /// indicator`'s own `.max(0)` clamp (correct for *its* job — see that
    /// function's own doc comment, and [`scroll_value_to_reveal_row`]'s,
    /// "Why this scrolls the list rather than changing `position_
    /// indicator`'s own clamp") then pinned the indicator to the
    /// viewport's literal top instead of leaving it honestly off-screen,
    /// and the panel opened anchored there — floating over whichever row
    /// actually occupied that pixel, never the selected item the panel was
    /// about to act on.
    ///
    /// [`Self::ensure_selected_row_visible`], called first below, is the
    /// fix: it scrolls `self.scrolled` so the selected row's own band is
    /// fully within the viewport *before* `indicator`'s position is ever
    /// read, which is what makes the "already tracks the selected row's
    /// on-screen position" claim above actually true by the time this
    /// method reaches [`Self::present_action_panel_for_selected`], rather
    /// than true only for a selection that already happened to be in view.
    ///
    /// The actual "resolve the selected item, pin it, present the panel"
    /// work lives in [`Self::present_action_panel_for_selected`] — shared
    /// with [`Self::open_secondary_action_menu_at`], since both paths do
    /// exactly that, differing only in *what* they anchor to and whether
    /// they select a row first. See that method's own doc comment,
    /// "Pinning the item at open time, not choose time," for the reasoning
    /// this split carries forward unchanged from before it existed.
    /// [`Self::open_secondary_action_menu_at`] needs no equivalent call to
    /// [`Self::ensure_selected_row_visible`] of its own: a real right-click
    /// can only ever land on a row already painted somewhere in the
    /// viewport, so the row it selects is never the out-of-view case this
    /// method's own fix exists for.
    fn open_secondary_action_menu(&self) {
        self.ensure_selected_row_visible();
        self.present_action_panel_for_selected(&self.indicator, None);
    }

    /// Scrolls `self.scrolled` so the selected row's own band is fully
    /// within its viewport, or does nothing if there is no selection or it
    /// is already fully visible — [`scroll_value_to_reveal_row`]'s own doc
    /// comment has the full account of the anchor-detachment bug this
    /// exists to fix (issue #254 review, finding 3) and why scrolling the
    /// list, rather than loosening [`position_indicator`]'s own clamp, is
    /// the fix. Called once, by [`Self::open_secondary_action_menu`],
    /// before it ever reads `self.indicator`'s position.
    fn ensure_selected_row_visible(&self) {
        let selected = self.selection.selected();
        if selected == gtk::INVALID_LIST_POSITION {
            return;
        }
        let adjustment = self.scrolled.vadjustment();
        if let Some(value) = scroll_value_to_reveal_row(
            selected,
            *tokens::ROW_HEIGHT_PX,
            adjustment.value(),
            adjustment.page_size(),
        ) {
            adjustment.set_value(value);
        }
    }

    /// Issue #254 review, finding 4's own window-layer handler: the row's
    /// overflow chevron (`ui::row::overflow_button_widget`) invokes
    /// `row.open-actions` with its row's own item id as target, and this is
    /// where that name resolves to real behavior —
    /// `ui::window::HopWindow::build`'s registered `gio::SimpleAction`
    /// calls this directly from its `connect_activate` closure.
    ///
    /// Reuses the identical "select, then present" shape
    /// [`Self::open_secondary_action_menu_at`] (right-click) already
    /// establishes, per this review finding's own "reusing the same
    /// select-then-present path right-click already uses ... do not grow a
    /// third copy of that logic" instruction — the one real difference is
    /// *how* the target row is found: a right-click already knows which
    /// row it landed on from a pixel `y`
    /// ([`row_index_at_y`]); this chevron instead names its own row's item
    /// by id (a GAction target survives a recycle the same way
    /// `ui::row`'s own dedicated action-icon buttons already rely on — see
    /// `ui::row`'s top doc comment, "How a click runs the right action"),
    /// so [`position_of_item_id`] is the one new lookup this path needs:
    /// turning that id back into the position [`gtk::SingleSelection::
    /// set_selected`] and [`Self::ensure_selected_row_visible`] both need.
    ///
    /// `item_id` naming no row currently in the store — stale by the time
    /// this runs, in principle, though `ui::row::resolve_overflow_button`'s
    /// own recycling constraint should never actually produce one — does
    /// nothing, the same "nothing honest to open a panel for" judgment
    /// [`Self::open_secondary_action_menu_at`] already makes for a click
    /// landing outside every real row.
    ///
    /// # Anchoring: the row's own trailing edge, not the chevron's literal
    /// pixel bounds
    ///
    /// `parent`/`pointing_to` are `self.list_view`/a `gdk::Rectangle` built
    /// from the *row's* own known geometry (`tokens::ROW_HEIGHT_PX`, the
    /// selected position, and `self.scrolled`'s own scroll offset — the
    /// identical arithmetic [`position_indicator`] and [`row_index_at_y`]
    /// already share), at the row's trailing edge
    /// (`self.list_view.width()`). This is a deliberate approximation, not
    /// the chevron button's own precise allocated rectangle: `ui::row` owns
    /// the recycled per-button widget instances, and reaching into that
    /// layer from here to read one button's real on-screen bounds would
    /// mean growing a new, public seam on a module whose own top doc
    /// comment already draws this exact line for a live row widget
    /// (`Self::open_secondary_action_menu`'s own doc comment makes the
    /// identical call, for `self.indicator`, one paragraph over). Every row
    /// lays its action icons and this chevron out flush against its own
    /// trailing edge (`ui::row::build`'s own `hexpand`-carries-the-
    /// trailing-child layout), so anchoring at that edge, at the selected
    /// row's own vertical band, reads as "opened from that row" without
    /// this module needing to know the chevron's exact pixel rectangle.
    fn open_action_panel_for_overflow(&self, item_id: &ItemId) {
        let Some(position) = position_of_item_id(&self.store, item_id) else {
            return;
        };
        self.selection.set_selected(position);
        self.ensure_selected_row_visible();

        let row_h = *tokens::ROW_HEIGHT_PX;
        let row_top = (position as i32) * row_h - self.scrolled.vadjustment().value() as i32;
        let rect = gdk::Rectangle::new(self.list_view.width(), row_top.max(0), 1, row_h);
        self.present_action_panel_for_selected(&self.list_view, Some(&rect));
    }

    /// Issue #254 AC2's own handler: a right-click on a row selects that
    /// row and opens [`Self::action_panel`] anchored at the exact cursor
    /// position, `(x, y)` — SPEC decision 6's "right-click row = action
    /// panel at cursor," in contrast with ctrl-K's "general overlay" anchor
    /// [`Self::open_secondary_action_menu`]'s own doc comment describes.
    /// `x`/`y` arrive from [`Self::wire_row_right_click`]'s own
    /// `gtk::GestureClick`, already in `self.list_view`'s own coordinate
    /// space — see that method's own doc comment for why that is exactly
    /// the space this function needs both halves of its own job to agree
    /// on.
    ///
    /// # Select, then open — atomically, in one synchronous call
    ///
    /// This issue's own brief names this as the sharpest edge in the
    /// slice: a right-click must select the row under the cursor *before*
    /// the panel opens, or the panel would act on whatever the results
    /// selection already happened to be — a different item than the one
    /// actually under the pointer. [`row_index_at_y`] turns `y` alone into
    /// "which row" (rows are fixed-height — `tokens::ROW_HEIGHT_PX` — so no
    /// widget lookup is needed, only arithmetic, the same trick
    /// [`position_indicator`] already relies on in the opposite direction),
    /// `self.selection.set_selected` moves the selection to it, and only
    /// *then* does this function call
    /// [`Self::present_action_panel_for_selected`], which reads
    /// `self.selection.selected()` straight back out — the identical read
    /// [`Self::open_secondary_action_menu`]'s own path already trusts.
    /// Nothing here is two steps a second event could interleave with:
    /// GTK is single-threaded, and both the select and the resolve-and-
    /// present run inside this one synchronous function call, driven
    /// directly by [`tests::assert_right_click_selects_the_row_under_the_
    /// cursor_before_opening_the_panel_at_that_point`] rather than assumed.
    ///
    /// A click landing outside every real row — an empty list, or a click
    /// in the blank space below the last row — resolves to `None` and this
    /// function does nothing: there is no row to select and nothing honest
    /// to open a panel for, the same judgment [`ActionPanel::present`]'s
    /// own "Zero actions: no mystery box" section makes for a different
    /// empty case.
    fn open_secondary_action_menu_at(&self, x: f64, y: f64) {
        let row_h = *tokens::ROW_HEIGHT_PX;
        let scroll_offset = self.scrolled.vadjustment().value();
        let Some(index) = row_index_at_y(y, scroll_offset, row_h, self.store.n_items()) else {
            return;
        };
        self.selection.set_selected(index);

        let rect = gdk::Rectangle::new(x.round() as i32, y.round() as i32, 1, 1);
        self.present_action_panel_for_selected(&self.list_view, Some(&rect));
    }

    /// The shared core [`Self::open_secondary_action_menu`] and
    /// [`Self::open_secondary_action_menu_at`] both delegate to: resolves
    /// the currently selected item exactly the way [`activate_selected`]
    /// does — `self.selection.selected()`, guarded against
    /// [`gtk::INVALID_LIST_POSITION`], then [`gtk::SingleSelection::item`]
    /// decoded through [`model::item_of`] — rather than growing a second,
    /// independent "what item is selected" lookup for either caller. Does
    /// nothing, gracefully, in exactly the same "nothing to act on" case
    /// `activate_selected` already treats as a no-op (an empty or no-
    /// results list), and again if the resolved item has no actions at
    /// all: [`ActionPanel::present`] itself refuses to open for that case
    /// (its own doc comment, "Zero actions: no mystery box") and reports
    /// `false`, which this method reads back to keep
    /// [`Self::pinned_action_item`] truthful rather than pinning an item no
    /// panel is actually showing.
    ///
    /// `pointing_to` is forwarded to [`gtk::Popover::set_pointing_to`]
    /// unconditionally, on every call — including `None` for
    /// [`Self::open_secondary_action_menu`]'s own ctrl-K path. That `None`
    /// is load-bearing, not a default left implicit: [`Self::action_panel`]
    /// wraps one popover, built once and reused (`ActionPanel`'s own doc
    /// comment, "Why a `gtk::Popover`"), so a *previous* right-click's
    /// rectangle would otherwise still be set the next time ctrl-K opens
    /// it, silently anchoring a "general overlay" open to a stale cursor
    /// point instead of centering on `parent` the way an unset
    /// `pointing_to` does.
    ///
    /// # Pinning the item at open time, not choose time
    ///
    /// [`Self::pinned_action_item`] is set here, once, before
    /// [`ActionPanel::present`] is ever called — not read fresh from
    /// `self.selection` inside the `on_choose` closure [`HopWindow::build`]
    /// wires into `action_panel`. The results selection is free to move
    /// while the panel stays open (a mouse click elsewhere in the results
    /// list, or any future feature that touches `self.selection`, is not
    /// blocked by an open panel today), so reading
    /// `self.selection.selected()` again at choose time would run the
    /// chosen action against whichever item happens to be selected *at that
    /// later moment* — silently wrong the instant the two diverge, and
    /// invisible to every test that never moves the selection after
    /// opening. Pinning once, here, before `present` runs, is what makes
    /// "which item does a choice run against" a question with exactly one
    /// answer for the whole time one `present` call's panel stays open —
    /// proven directly by
    /// `tests::assert_choosing_pins_the_item_opened_for_not_whatever_is_selected_later`,
    /// which moves the selection mid-flight specifically to catch a
    /// choose-time implementation that a same-item test could not tell
    /// apart from this one.
    fn present_action_panel_for_selected(
        &self,
        parent: &impl IsA<gtk::Widget>,
        pointing_to: Option<&gdk::Rectangle>,
    ) {
        let selected = self.selection.selected();
        if selected == gtk::INVALID_LIST_POSITION {
            return;
        }
        let Some(object) = self.selection.item(selected) else {
            return;
        };
        let item: Item = model::item_of(&object);

        *self.pinned_action_item.borrow_mut() = Some(item.clone());
        self.action_panel.popover().set_pointing_to(pointing_to);
        if !self.action_panel.present(&item, parent) {
            // `present` refused to open (the item has no actions) — the pin
            // just set can never be read by anything real, since
            // `on_choose` only ever runs as a result of a choice made
            // inside a panel that actually opened. Clearing it anyway keeps
            // the field's own documented meaning ("the item the *currently
            // open* panel was opened for") true rather than leaving a stale
            // answer behind for a panel nothing is showing.
            *self.pinned_action_item.borrow_mut() = None;
        }
    }

    /// [`Action::CompletePrefix`]'s handler — the same honesty as
    /// [`HopWindow::open_secondary_action_menu`] above: bound, reachable,
    /// and empty, because prefix completion (matching the query against the
    /// longest shared prefix among the current results) is its own unbuilt
    /// slice, not this issue's to invent.
    fn complete_prefix(&self) {}

    fn wire_selection_indicator(&self) {
        let row_h = *tokens::ROW_HEIGHT_PX;

        let indicator = self.indicator.clone();
        let scrolled = self.scrolled.clone();
        self.selection.connect_selected_notify(move |selection| {
            position_indicator(&indicator, selection, &scrolled, row_h);
        });

        let indicator = self.indicator.clone();
        let selection = self.selection.clone();
        let scrolled = self.scrolled.clone();
        self.scrolled.vadjustment().connect_value_changed(move |_| {
            position_indicator(&indicator, &selection, &scrolled, row_h);
        });
    }

    /// Attaches the one right-click `gtk::GestureClick` this window has, to
    /// `self.list_view` itself — never to any individual row widget.
    /// `ui::row`'s own row content is recycled (`GtkListView`'s whole
    /// reason to exist), so a gesture attached to one recycled row widget
    /// would face the identical "which item does this widget currently
    /// show" problem `ui::row`'s own action-icon buttons solve with a
    /// GAction target — except a row's *position* has no equivalent
    /// widget-stored answer to reach for the same way. Attaching once, to
    /// `list_view`, and deriving the clicked row from the click's own `y`
    /// via [`row_index_at_y`] sidesteps that entirely: one gesture answers
    /// for every row, current and future, with no per-row wiring added to
    /// `ui::row` at all.
    ///
    /// A plain, default-phase (Bubble) `GtkGestureClick` restricted to
    /// [`gdk::BUTTON_SECONDARY`] needs no propagation-phase tuning to see
    /// every secondary click — checked directly against GTK 4.14.5's own
    /// source (`gtk/gtklistfactorywidget.c`,
    /// `gtk_list_factory_widget_init`) while building this: the row's own
    /// internal click gesture (the one `set_single_click_activate` in
    /// [`build`] governs) is restricted to `GDK_BUTTON_PRIMARY` only
    /// (`gtk_gesture_single_set_button (GTK_GESTURE_SINGLE (gesture),
    /// GDK_BUTTON_PRIMARY)`), so a secondary-button press is never claimed
    /// there and bubbles up to this controller on `list_view` untouched.
    ///
    /// `n_press == 1` only: a right *double*-click must not open the panel
    /// twice, or once per press — the same "double-click = single click"
    /// mouse-contract line SPEC decision 6 states for the left-click
    /// activation path, applied here to the one other click-driven affordance
    /// this window wires by hand.
    ///
    /// `x`/`y`, as GTK reports them to a controller added directly to
    /// `list_view`, are in `list_view`'s own coordinate space — the
    /// viewport's, not the full virtual scrolled content's, since
    /// `GtkListView` implements `GtkScrollable` directly
    /// (`assets/stylesheet.css`'s own LIST VIEW GROUND comment already
    /// established there is no intervening `GtkViewport` node). That is
    /// exactly the space [`HopWindow::open_secondary_action_menu_at`]
    /// needs both halves of its own job to agree on: [`row_index_at_y`]'s
    /// arithmetic already assumes a viewport-relative `y` (matching
    /// [`position_indicator`]'s own inverse), and `list_view` is also the
    /// `parent` that call anchors [`gtk::Popover::set_pointing_to`]'s
    /// rectangle to — a rectangle and a parent drawn from two different
    /// coordinate spaces would place the popover somewhere other than the
    /// literal cursor point decision 6 asks for.
    fn wire_row_right_click(&self) {
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gdk::BUTTON_SECONDARY);

        let hop_window = self.clone();
        gesture.connect_pressed(move |_gesture, n_press, x, y| {
            if n_press != 1 {
                return;
            }
            hop_window.open_secondary_action_menu_at(x, y);
        });

        self.list_view.add_controller(gesture);
    }

    /// Presents the window, optionally with an activation token — the
    /// `hop toggle` → control message → `present()` path (acceptance
    /// criterion 2). See `app`'s module doc, "Activation token handoff",
    /// for exactly what is and is not wired up and verified here.
    pub fn present_with_token(&self, activation_token: Option<&str>) {
        if let Some(token) = activation_token {
            // GTK/GDK's Wayland backend picks up `XDG_ACTIVATION_TOKEN` from
            // the environment when a surface asks for focus during
            // `present()` — see `app`'s doc comment for the fuller account
            // of what this does and does not cover.
            //
            // SAFETY: `set_var` is unsafe only against a concurrent read or
            // write of the environment from another thread. This runs on
            // the GTK main thread, synchronously, immediately before the
            // `present()` call below reads it back; no other thread in this
            // process touches environment variables (`ipc`'s background
            // thread only ever touches the socket and its channels — see
            // that module's doc comment for why it cannot reach anything
            // else).
            #[expect(
                unsafe_code,
                reason = "GTK reads the activation token back out of the environment \
                          during present(); set_var is the only way to put it there, and \
                          this runs single-threaded on the GTK main thread"
            )]
            unsafe {
                std::env::set_var("XDG_ACTIVATION_TOKEN", token);
            }
        }
        self.window.present();
    }

    /// Applies one [`IpcEvent`] to the window's visible state. Called from
    /// `app`'s `glib::spawn_future_local` event loop — see `ipc`'s module
    /// doc for why that is guaranteed to run on the GTK main thread despite
    /// the event having originated on the IPC thread.
    pub fn apply_event(&self, event: IpcEvent) {
        match event {
            IpcEvent::Connected => {
                self.pending_user_executions.set(0);
                row::set_offline_state(false, None);
                self.status.set_visible(false);
                self.offline_indicator.apply(None);
                self.list_view.remove_css_class("hop-state-offline");
                self.rebind_current_items();
            }
            IpcEvent::ConnectFailed(reason) => {
                self.set_status(&format!("Can't reach hopd: {reason}"));
            }
            IpcEvent::Disconnected => {
                self.pending_user_executions.set(0);
                // Issue #200: `IpcEvent::Disconnected` — a connection that
                // *was* established and has now been lost, `ipc`'s own
                // reconnect loop already retrying in the background (per
                // that event's own doc comment) — is the offline indicator's
                // event, not only `.hop-status`'s. `IpcEvent::ConnectFailed`
                // (never yet connected at all) deliberately keeps its own,
                // separate `.hop-status` treatment above, unchanged: this
                // issue's own brief scopes it to one widget, the offline
                // indicator, and treats the error row (which
                // `ConnectFailed`/`Error` most naturally belong to) as
                // explicitly out of scope — see `ui::offline_indicator`'s
                // module doc comment for why the two are not the same
                // signal.
                //
                // A code-review pass on this issue caught a real
                // regression an earlier version of this arm introduced: it
                // replaced `.hop-status`'s pre-existing "Lost connection to
                // hopd, reconnecting…" text outright, rather than adding
                // the offline indicator alongside it. Nothing in this
                // issue's acceptance criteria asked for that replacement,
                // and it quietly cost real information — the offline
                // indicator's own words are fixed to the single, constant
                // truth `OFFLINE_TEXT` names ("Offline"; see
                // `ui::offline_indicator`'s own doc comment for why that
                // string never varies), so a user who only saw the
                // indicator could no longer tell "offline and hopd is
                // actively retrying the connection" apart from "offline,
                // full stop" — a distinction they could read directly off
                // `.hop-status` before this branch touched this arm.
                //
                // Restored as a second signal *alongside* the offline
                // indicator, not folded into the indicator's own wording:
                // `OFFLINE_TEXT` is honesty-critical, locked text by the
                // same contract that locks its colour and size (this
                // issue's Fix 1), so it is exactly the wrong place to
                // encode a transient, retry-loop-specific detail that has
                // nothing to do with the truthfulness guarantee — the
                // indicator's job is "are we offline right now", not "what
                // is `ipc` doing about it". `.hop-status` already is the
                // ordinary, non-locked surface this crate uses for
                // connection prose (see `ConnectFailed`'s own arm above),
                // so reusing it here for exactly the same kind of message
                // reads as the smaller, more consistent change in code —
                // one call to the method this arm already used before this
                // issue existed, not a new literal grafted onto a
                // deliberately-fixed honesty-critical string.
                let as_of = current_local_hh_mm();
                row::set_offline_state(true, Some(as_of.as_str()));
                self.set_status("Lost connection to hopd, reconnecting…");
                self.offline_indicator.apply(Some(as_of.as_str()));
                self.list_view.add_css_class("hop-state-offline");
                self.error_pin.set_visible(false);
                self.pending_surface.set_visible(false);
                self.clear_pending_providers();
                self.state_stack.set_visible_child_name("results");
                let cached = self.cached_items.borrow().clone();
                if !cached.is_empty() {
                    self.replace_state_items(cached);
                    self.state_header.set_text("Cached · daemon unreachable");
                    self.state_header.set_visible(true);
                    self.selection.set_selected(0);
                }
            }
            IpcEvent::Routed {
                mode,
                exclusive,
                marker_span,
                query_text,
                pending_providers,
            } => {
                // D3's "mirrors `exclusive`, and nothing else" rule, made
                // concrete right here: the mode label is shown only when
                // `exclusive` is true, computed once and handed to
                // `mode_label::apply` as the single `Option<Mode>` it acts
                // on — that function has no `exclusive` of its own to
                // consult. See `IpcEvent::Routed`'s own doc comment for how
                // `marker_span` is bound to `query_text`, and
                // `ui::marker_highlight`'s doc comment for what
                // `marker_highlight::apply` still checks before trusting
                // that binding against this window's *current* entry text.
                mode_label::apply(&self.mode_label, exclusive.then_some(mode));
                marker_highlight::apply(&self.entry, &query_text, marker_span);
                if !query_text.is_empty() && self.query_pending.get() {
                    self.set_pending_providers(pending_providers);
                }
            }
            IpcEvent::Results(items) => {
                row::set_offline_state(false, None);
                self.local_actions.borrow_mut().clear();
                self.list_view.remove_css_class("hop-state-offline");
                self.resolve_pending_providers(&items);
                if items.is_empty() {
                    if self.entry.text().is_empty() && !self.query_pending.get() {
                        self.render_empty_state();
                    }
                } else if !self.entry.text().is_empty() {
                    self.query_had_results.set(true);
                    *self.cached_items.borrow_mut() = items.clone();
                    self.show_results(items);
                }
            }
            IpcEvent::RecentItems(items) => {
                if self.entry.text().is_empty() {
                    self.query_had_results.set(!items.is_empty());
                    self.render_recent_items(items);
                }
            }
            IpcEvent::QueryDone => {
                if !self.query_pending.replace(false) {
                    return;
                }
                self.clear_pending_providers();
                if self.entry.text().is_empty() {
                    if !self.query_had_results.get() {
                        self.render_empty_state();
                    } else {
                        self.pending_surface.set_visible(false);
                    }
                } else if !self.query_had_results.get() {
                    self.render_no_results_state(self.entry.text().as_str());
                } else {
                    self.pending_surface.set_visible(false);
                    self.state_header.set_visible(false);
                }
            }
            IpcEvent::Executed(outcome) => self.handle_outcome(outcome),
            IpcEvent::Error(message) => self.show_error(&message),
        }
    }

    fn begin_query(&self, text: &str) {
        self.pending_user_executions.set(0);
        row::set_offline_state(false, None);
        self.query_had_results.set(false);
        self.query_pending.set(true);
        self.local_actions.borrow_mut().begin_query();
        self.clear_pending_providers();
        self.error_pin.set_visible(false);
        self.list_view.remove_css_class("hop-state-offline");
        if text.is_empty() {
            self.render_empty_state();
            self.query_pending.set(true);
        } else {
            self.show_pending();
        }
    }

    fn show_pending(&self) {
        self.replace_state_items(Vec::new());
        self.clear_pending_providers();
        self.state_header.set_text("Working…");
        self.state_header.set_visible(true);
        self.pending_surface.add_css_class("hop-state-pending");
        self.update_pending_motion_class();
        self.state_stack.set_visible_child_name("results");
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
    }

    fn set_pending_providers(&self, provider_ids: Vec<String>) {
        self.clear_pending_providers();
        let mut seen = HashSet::with_capacity(provider_ids.len());
        let mut pending = self.pending_providers.borrow_mut();
        for provider_id in provider_ids {
            if !seen.insert(provider_id.clone()) {
                continue;
            }
            let provider = build_pending_provider(provider_id);
            self.pending_surface.append(&provider.attribution);
            self.pending_surface.append(&provider.row);
            pending.push(provider);
        }
        self.pending_surface.set_visible(!pending.is_empty());
    }

    fn resolve_pending_providers(&self, items: &[Item]) {
        let pending = self.pending_providers.borrow();
        for provider in pending.iter() {
            if provider.attribution.get_visible()
                && items
                    .iter()
                    .any(|item| item.provider.as_str() == provider.id)
            {
                provider.attribution.set_visible(false);
                provider.row.set_visible(false);
            }
        }
        let any_pending = pending
            .iter()
            .any(|provider| provider.attribution.get_visible());
        self.pending_surface
            .set_visible(self.query_pending.get() && any_pending);
    }

    fn clear_pending_providers(&self) {
        self.pending_providers.borrow_mut().clear();
        while let Some(child) = self.pending_surface.first_child() {
            self.pending_surface.remove(&child);
        }
        self.pending_surface.set_visible(false);
    }

    fn show_results(&self, items: Vec<Item>) {
        let keep_pending = self.query_pending.get();
        self.replace_state_items(items);
        self.state_stack.set_visible_child_name("results");
        self.status.set_visible(false);
        if keep_pending {
            self.state_header.set_text("Working…");
            self.state_header.set_visible(true);
            self.pending_surface.set_visible(
                self.pending_providers
                    .borrow()
                    .iter()
                    .any(|provider| provider.attribution.get_visible()),
            );
        } else {
            self.state_header.set_visible(false);
            self.pending_surface.set_visible(false);
        }
        if self.store.n_items() > 0 {
            self.selection.set_selected(0);
        } else {
            self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
    }
    fn render_empty_state(&self) {
        self.query_pending.set(false);
        self.show_results(empty_state_items(&[]));
        self.state_header.set_text("Recent");
        self.state_header.set_visible(true);
    }

    fn render_recent_items(&self, recents: Vec<RecentItem>) {
        self.replace_state_items(empty_state_items(&recents));
        self.state_stack.set_visible_child_name("results");
        self.status.set_visible(false);
        self.pending_surface.set_visible(false);
        self.state_header.set_text("Recent");
        self.state_header.set_visible(true);
        if self.store.n_items() > 0 {
            self.selection.set_selected(0);
        } else {
            self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
    }

    fn render_no_results_state(&self, query: &str) {
        let full_query = query.to_string();
        let display_query = truncate_query(query);
        self.query_pending.set(false);
        self.local_actions.borrow_mut().install([
            (
                "hop:fallback-web-search".to_string(),
                LocalAction::WebSearch(full_query.clone()),
            ),
            (
                "hop:fallback-copy".to_string(),
                LocalAction::Copy(full_query),
            ),
        ]);
        self.show_results(no_results_state_items(display_query.as_str(), query));
        self.state_header.set_text("No local matches");
        self.state_header.set_visible(true);
    }
    fn show_error(&self, message: &str) {
        row::set_offline_state(false, None);
        self.query_pending.set(false);
        self.pending_surface.set_visible(false);
        self.state_stack.set_visible_child_name("results");
        self.error_title.set_text(message);
        self.error_subtitle
            .set_text("provider isolated; other results unaffected");
        self.error_pin.set_visible(true);
    }

    fn replace_state_items(&self, items: Vec<Item>) {
        *self.state_items.borrow_mut() = items.clone();
        model::replace(&self.store, items);
    }

    fn rebind_current_items(&self) {
        let items: Vec<Item> = (0..self.store.n_items())
            .filter_map(|position| self.store.item(position))
            .map(|object| model::item_of(&object))
            .collect();
        model::replace(&self.store, items);
    }

    fn update_pending_motion_class(&self) {
        let reduced =
            gtk::Settings::default().is_some_and(|settings| !settings.is_gtk_enable_animations());
        if reduced {
            self.pending_surface
                .add_css_class("hop-state-reduced-motion");
        } else {
            self.pending_surface
                .remove_css_class("hop-state-reduced-motion");
        }
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
        self.status.set_visible(true);
    }
    /// Carries out an [`ExecOutcome`] through the injected user-action
    /// boundary. Interactive windows use the real clipboard/URI launcher;
    /// screenshot and widget-test windows use a non-launching sink.
    fn handle_outcome(&self, outcome: ExecOutcome) {
        let pending = self.pending_user_executions.get();
        if pending == 0 {
            return;
        }
        self.pending_user_executions.set(pending - 1);
        match outcome {
            ExecOutcome::Done => {}
            ExecOutcome::CopyText(text) => self.user_actions.copy_text(text.as_str()),
            ExecOutcome::OpenUrl(url) => {
                if let Err(err) = self.user_actions.launch_uri(url.as_str()) {
                    self.set_status(&format!("couldn't open {}: {err}", url.as_str()));
                }
            }
        }
    }
}

fn build_error_pin() -> (gtk::Box, gtk::Label, gtk::Label) {
    let pin = gtk::Box::new(gtk::Orientation::Horizontal, *tokens::OFFLINE_ROW_GAP_PX);
    pin.add_css_class("hop-honesty");
    pin.add_css_class("hop-error-pin");
    pin.set_visible(false);

    let icon = gtk::Label::new(Some("!"));
    icon.add_css_class("hop-error-pin-icon");
    pin.append(&icon);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let title = gtk::Label::new(None);
    title.add_css_class("hop-error-pin-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("hop-honesty-text");
    let subtitle = gtk::Label::new(None);
    subtitle.add_css_class("hop-error-pin-subtitle");
    subtitle.add_css_class("hop-honesty-text");
    subtitle.set_xalign(0.0);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&title);
    text.append(&subtitle);
    pin.append(&text);

    (pin, title, subtitle)
}

fn build_pending_surface(pending_providers: Rc<RefCell<Vec<PendingProvider>>>) -> gtk::Box {
    let surface = gtk::Box::new(gtk::Orientation::Vertical, *tokens::HINT_CHIP_GAP_PX);
    surface.add_css_class("hop-honesty");
    surface.add_css_class("hop-state-pending");
    surface.set_margin_start(*tokens::OFFLINE_ROW_GAP_PX);
    surface.set_margin_end(*tokens::OFFLINE_ROW_GAP_PX);
    surface.set_visible(false);

    let settings = gtk::Settings::default();
    let mut active = 0usize;
    glib::timeout_add_local(Duration::from_millis(180), move || {
        let reduced = settings
            .as_ref()
            .is_some_and(|settings| !settings.is_gtk_enable_animations());
        let pending = pending_providers.borrow();
        let mut bar_count = 0;
        for provider in pending.iter() {
            for bar in &provider.bars {
                if reduced {
                    bar.remove_css_class("hop-shimmer-active");
                } else if bar_count == active {
                    bar.add_css_class("hop-shimmer-active");
                } else {
                    bar.remove_css_class("hop-shimmer-active");
                }
                bar_count += 1;
            }
        }
        if !reduced && bar_count != 0 {
            active = (active + 1) % bar_count;
        }
        glib::ControlFlow::Continue
    });

    surface
}

fn build_pending_provider(id: String) -> PendingProvider {
    let attribution = gtk::Label::new(Some(&id));
    attribution.add_css_class("hop-pending-attribution");
    attribution.add_css_class("hop-honesty-text");
    attribution.set_xalign(0.0);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, *tokens::HINT_CHIP_GAP_PX);
    row.add_css_class("hop-pending-row");
    row.set_homogeneous(true);
    let first = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    first.add_css_class("hop-skeleton");
    first.add_css_class("hop-pending-bar");
    first.add_css_class("hop-honesty");
    first.set_hexpand(true);
    row.append(&first);
    let second = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    second.add_css_class("hop-skeleton");
    second.add_css_class("hop-pending-bar");
    second.add_css_class("hop-honesty");
    second.set_hexpand(true);
    row.append(&second);

    PendingProvider {
        id,
        attribution,
        row,
        bars: [first, second],
    }
}

fn state_item(
    id: &str,
    kind: Kind,
    title: &str,
    subtitle: Option<&str>,
    action: Option<(&str, ActionKind, &str)>,
    copy_text: Option<&str>,
    metadata: (&str, bool),
) -> Option<Item> {
    let (provider, append_to_end) = metadata;
    let (actions, default_action) = match action {
        Some((id, kind, label)) => {
            let id = ActionId::new(id).ok()?;
            let action = WireAction {
                id: id.clone(),
                kind,
                label: label.to_string(),
            };
            (vec![action], id)
        }
        None => (Vec::new(), ActionId::new("none").ok()?),
    };
    Some(Item {
        id: ItemId::new(id).ok()?,
        kind,
        title: ItemTitle::new(title).ok()?,
        subtitle: subtitle.and_then(|text| ItemSubtitle::new(text).ok()),
        icon: None,
        actions,
        default_action,
        copy_text: copy_text.and_then(|text| CopyText::new(text).ok()),
        append_to_end,
        provider: provider.to_string(),
    })
}

fn empty_state_items(recents: &[RecentItem]) -> Vec<Item> {
    let mut items = recents
        .iter()
        .map(|recent| {
            let mut item = recent.item.clone();
            item.subtitle = relative_subtitle_ms(&item.provider, recent.launched_at_ms);
            item
        })
        .collect::<Vec<_>>();
    if let Some(prefixes) = state_item(
        "hop:prefixes",
        Kind::Action,
        "w windows · a apps · f files · = math · : emoji",
        None,
        None,
        None,
        ("ui", false),
    ) {
        items.push(prefixes);
    }
    items
}

fn relative_subtitle_ms(provider: &str, launched_at_ms: u64) -> Option<ItemSubtitle> {
    let launched_at = SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(launched_at_ms))?;
    relative_subtitle(provider, launched_at)
}

fn relative_subtitle(provider: &str, launched_at: SystemTime) -> Option<ItemSubtitle> {
    let age = SystemTime::now()
        .duration_since(launched_at)
        .unwrap_or_default();
    let suffix = if age.as_secs() < 60 {
        "just now".to_string()
    } else if age.as_secs() < 3_600 {
        format!("{}m ago", age.as_secs() / 60)
    } else if age.as_secs() < 86_400 {
        format!("{}h ago", age.as_secs() / 3_600)
    } else if age.as_secs() < 172_800 {
        "yesterday".to_string()
    } else {
        format!("{}d ago", age.as_secs() / 86_400)
    };
    ItemSubtitle::new(format!("{provider} · {suffix}")).ok()
}

fn no_results_state_items(display_query: &str, copy_query: &str) -> Vec<Item> {
    [
        state_item(
            "hop:fallback-web-search",
            Kind::WebSearch,
            &format!("Search the web for “{display_query}”"),
            Some("fallback · GNOME Web"),
            Some(("open", ActionKind::OpenUrl, "Open")),
            None,
            ("web-search", true),
        ),
        state_item(
            "hop:fallback-copy",
            Kind::Action,
            &format!("Copy “{display_query}” to clipboard"),
            Some("fallback · clipboard"),
            Some(("copy", ActionKind::Copy, "Copy")),
            Some(copy_query),
            ("clipboard", true),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn truncate_query(query: &str) -> String {
    const PREFIX: &str = "Search the web for “";
    const SUFFIX: &str = "”";
    let budget = MAX_TITLE.saturating_sub(PREFIX.len() + SUFFIX.len());
    let mut end = query.len().min(budget);
    while end > 0 && !query.is_char_boundary(end) {
        end -= 1;
    }
    query[..end].to_string()
}

/// The current wall-clock time, formatted `HH:MM` in the local timezone —
/// [`HopWindow::apply_event`]'s `IpcEvent::Disconnected` arm's one call
/// site, and the only place in this crate that reads a clock at all.
///
/// Deliberately a plain, free function rather than a method: it takes no
/// part of `self`, and giving it one would wrongly suggest the offline
/// row's own "as of" wording depends on window state, when the real
/// dependency is only ever "what time is it right now" — the same reason
/// `ui::row`'s pure helpers (`hint_entered_shown`, `default_action_label`)
/// are free functions rather than methods on a type that happens to be
/// nearby.
///
/// # Why a degraded stamp, not a panic, on a formatting failure
///
/// [`glib::DateTime::now_local`] and [`glib::DateTime::format`] are both
/// fallible ([`glib::BoolError`]) — GLib's own docs name a missing or
/// broken timezone database as the realistic cause, an environment
/// condition, not a programming error this crate's code caused. That is a
/// different shape of failure than the ones this crate's "fail loudly"
/// precedent (`tokens.rs`, `stylesheet.rs`, this module's own
/// `gtk_enable_animations`) exists for: those all catch a *build-time*
/// defect in a file this crate ships (a malformed token, a broken
/// placeholder) that a panic surfaces to whoever broke it, immediately,
/// long before a user could hit it. A broken system clock or timezone
/// database is neither — hop did not cause it and cannot fix it, and it can
/// only ever be discovered at runtime, on a real user's machine, the moment
/// they lose their connection to `hopd`. Panicking there would turn "the
/// clock is unavailable" into "the whole launcher crashes", which is a
/// strictly worse honesty failure than a stamp that reads `"as of --:--"` —
/// the row itself, and the words naming it offline, still render exactly
/// as truthfully either way. Neither `.unwrap()` nor `.expect()` appears
/// here, matching this crate's `clippy::unwrap_used` lint on a fallible
/// runtime value.
fn current_local_hh_mm() -> glib::GString {
    glib::DateTime::now_local()
        .and_then(|now| now.format("%H:%M"))
        .unwrap_or_else(|_| glib::GString::from("--:--"))
}

/// Moves `indicator` to sit over `selection`'s current row, accounting for
/// how far `scrolled` has scrolled — the "one indicator that moves" this
/// module's doc comment describes, recomputed fresh on every call rather
/// than incrementally adjusted, so there is exactly one thing this function
/// does with a selected position: turn it into a `y` and set it.
fn position_indicator(
    indicator: &gtk::Widget,
    selection: &gtk::SingleSelection,
    scrolled: &gtk::ScrolledWindow,
    row_h: i32,
) {
    let selected = selection.selected();
    if selected == gtk::INVALID_LIST_POSITION {
        indicator.set_visible(false);
        return;
    }
    let offset = (selected as i32) * row_h - scrolled.vadjustment().value() as i32;
    indicator.set_margin_top(offset.max(0));
    indicator.set_visible(true);
}

/// The index of the row whose fixed-height (`row_h`) band contains
/// viewport-relative `y` — the arithmetic inverse of [`position_indicator`]'s
/// own `offset = selected*row_h - scroll_offset`, used by
/// [`HopWindow::open_secondary_action_menu_at`] to turn a right-click's
/// pixel position into "which row is under the cursor".
///
/// `None` if `y` names no real row: above the very first one (a negative
/// index — a click landing above the viewport, which should not happen for
/// a gesture attached directly to `list_view`, but this function makes no
/// assumption about that), or at or past `item_count` (a click in the
/// blank space below the last row, or into an empty list altogether) —
/// both cases are treated identically as "nothing under the cursor",
/// leaving the caller to decide that means "do nothing" rather than this
/// function clamping to a row that is not actually there. `row_h <= 0`
/// (never true for a real [`tokens::ROW_HEIGHT_PX`], but this function
/// takes a bare `i32` rather than that type directly) answers `None` for
/// the same reason: dividing by a non-positive height cannot honestly name
/// a row either.
///
/// GTK-free and independently unit-tested
/// (`tests::assert_row_index_at_y_resolves_the_row_under_a_viewport_relative_y`)
/// for the identical reason `ui::row::hint_entered_shown` is: a pure
/// arithmetic decision, isolated from the real `gtk::ScrolledWindow`/
/// `gtk::ListView` [`HopWindow::open_secondary_action_menu_at`] drives it
/// against.
fn row_index_at_y(y: f64, scroll_offset: f64, row_h: i32, item_count: u32) -> Option<u32> {
    if row_h <= 0 {
        return None;
    }
    let absolute_y = scroll_offset + y;
    if absolute_y < 0.0 {
        return None;
    }
    let index = (absolute_y / f64::from(row_h)).floor();
    if index >= f64::from(item_count) {
        return None;
    }
    Some(index as u32)
}

/// The `vadjustment` value that would bring `selected`'s own row band fully
/// into a viewport of height `page_size` currently scrolled to
/// `current_value` — or `None` if it is already fully visible and no scroll
/// is needed. Issue #254 review, finding 3.
///
/// # The bug this exists to fix
///
/// [`HopWindow::open_secondary_action_menu`] (ctrl-K) anchors the action
/// panel to [`HopWindow::indicator`] — the persistent highlight
/// [`position_indicator`] moves to sit over the selected row. Before this
/// function existed, ctrl-K opened the panel with the scroll position
/// exactly wherever the user had left it: if `selected`'s own row had
/// scrolled above the viewport (a keyboard `Down` past the last visible
/// row — `HopWindow::move_selection` moves `self.selection` directly and
/// does not itself keep the selection scrolled into view), `position_
/// indicator`'s own `offset = selected*row_h - scroll_value` came out
/// negative and its `.max(0)` clamp pinned the indicator to the viewport's
/// literal top instead. The panel then opened anchored there — over
/// whichever row actually occupied that pixel, never the selected item the
/// panel was about to act on, with nothing on screen to explain the
/// mismatch.
///
/// # Why this scrolls the list rather than changing [`position_indicator`]'s
/// own clamp
///
/// `position_indicator`'s `.max(0)` is correct for what it protects today:
/// nothing currently asks it to place the indicator at a genuinely
/// negative margin, which would push part of the highlight above the
/// scrolled window's own visible area and clip it. Loosening that clamp
/// would not fix the panel's anchor either — an indicator honestly
/// rendered off-screen is still not a widget a `gtk::Popover::set_pointing_to`-
/// free `set_parent` anchor can point at meaningfully. The real defect is
/// upstream of the indicator entirely: the *selection* was allowed to
/// scroll out of view in the first place. [`HopWindow::open_secondary_
/// action_menu`] calling this function to scroll the row back into view
/// before it ever reads `self.indicator`'s position is what makes
/// `position_indicator`'s existing arithmetic honest again — `offset` comes
/// out non-negative on its own once the row is genuinely back in the
/// viewport, with no change needed to `position_indicator` itself, and
/// nothing about its own contract for a row that is *already* in view
/// changes at all.
///
/// # The two directions, and why both are handled the same way
///
/// `row_index_at_y`, `position_indicator`, and this function all agree on
/// one coordinate space: `0` is the very first item's own top, and every
/// row's own band is `[selected*row_h, selected*row_h + row_h)` within it.
/// A row named by `selected` can be out of view two ways — its top can sit
/// above `current_value` (scrolled past it, the finding's own named case),
/// or its bottom can sit below `current_value + page_size` (scrolled short
/// of it, the direction `HopWindow::move_selection`'s `Down` can reach the
/// same way just by moving past the last visible row without this
/// function). Revealing either means moving the *nearer* edge of the
/// viewport to meet the row: scrolling up until the row's own top is the
/// viewport's top, or down until the row's own bottom is the viewport's
/// bottom — never further than that, so a row already partially visible
/// moves the shortest distance that makes it fully visible rather than
/// re-centering it. A row that already satisfies both bounds needs neither
/// adjustment, which is `None`, not `Some(current_value)`: `HopWindow::
/// open_secondary_action_menu` only calls `gtk::Adjustment::set_value` when
/// this returns `Some`, so an already-visible selection produces no
/// redundant scroll event at all.
///
/// GTK-free and independently unit-tested
/// (`tests::scroll_value_to_reveal_row_only_moves_when_the_row_is_not_
/// already_fully_visible`), matching [`row_index_at_y`]'s own precedent for
/// isolating this file's pure row-geometry arithmetic from the real
/// `gtk::Adjustment` [`HopWindow::open_secondary_action_menu`] drives it
/// against.
fn scroll_value_to_reveal_row(
    selected: u32,
    row_h: i32,
    current_value: f64,
    page_size: f64,
) -> Option<f64> {
    let row_top = f64::from(selected) * f64::from(row_h);
    let row_bottom = row_top + f64::from(row_h);
    if row_top < current_value {
        Some(row_top)
    } else if row_bottom > current_value + page_size {
        Some(row_bottom - page_size)
    } else {
        None
    }
}

/// The position of the item whose id is `item_id` in `store`'s current
/// contents, or `None` if no item there carries it — issue #254 review,
/// finding 4's own overflow-chevron handler
/// ([`HopWindow::open_action_panel_for_overflow`]) is the one caller: the
/// chevron's own GAction target names an item id, not a position (the
/// identical choice `ui::row`'s dedicated action-icon buttons already make
/// for their own `(item_id, action_id)` target — see that module's top doc
/// comment for why a position would not survive the list reordering a
/// later query can produce between a bind and a click, where an id still
/// names the same item), so this is the one place that id is turned back
/// into a position [`gtk::SingleSelection::set_selected`] can use.
///
/// A plain linear scan, not a lookup table this module maintains
/// alongside `store`: hop's own results list is bounded for exactly the
/// reason a per-frame, human-driven click never needs faster than this —
/// nothing here runs on a hot path measured in anything other than mouse
/// clicks.
fn position_of_item_id(store: &gio::ListStore, item_id: &ItemId) -> Option<u32> {
    for position in 0..store.n_items() {
        let object = store.item(position)?;
        let item: Item = model::item_of(&object);
        if &item.id == item_id {
            return Some(position);
        }
    }
    None
}

/// Sends `cmd_tx.send(IpcCommand::Execute { item_id, action_id })` for a
/// daemon-owned item, or performs a locally rendered fallback action only
/// while the action registry still belongs to the current query generation.
fn dispatch_id_action(
    cmd_tx: &CommandSender,
    item_id: &ItemId,
    action_id: &ActionId,
    local_actions: &Rc<RefCell<LocalActionRegistry>>,
    user_actions: &Rc<dyn UserActionSink>,
    pending_user_executions: &Rc<Cell<u32>>,
) {
    if let Some(action) = local_actions.borrow().action_for(item_id) {
        perform_local_action(action, user_actions.as_ref());
    } else {
        send_execute(
            cmd_tx,
            item_id.clone(),
            action_id.clone(),
            pending_user_executions,
        );
    }
}

fn dispatch_item_action(
    cmd_tx: &CommandSender,
    item: &Item,
    local_actions: &Rc<RefCell<LocalActionRegistry>>,
    action_id: ActionId,
    user_actions: &Rc<dyn UserActionSink>,
    pending_user_executions: &Rc<Cell<u32>>,
) {
    dispatch_id_action(
        cmd_tx,
        &item.id,
        &action_id,
        local_actions,
        user_actions,
        pending_user_executions,
    );
}

fn perform_local_action(action: LocalAction, user_actions: &dyn UserActionSink) {
    match action {
        LocalAction::Copy(text) => user_actions.copy_text(&text),
        LocalAction::WebSearch(query) => {
            let encoded = percent_encode_query(&query);
            let uri = format!("https://www.google.com/search?q={encoded}");
            let _ = user_actions.launch_uri(&uri);
        }
    }
}

fn percent_encode_query(query: &str) -> String {
    query
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn send_execute(
    cmd_tx: &CommandSender,
    item_id: ItemId,
    action_id: ActionId,
    pending_user_executions: &Rc<Cell<u32>>,
) {
    if cmd_tx.send(IpcCommand::Execute { item_id, action_id }) {
        pending_user_executions.set(pending_user_executions.get().saturating_add(1));
    }
}

fn activate_selected(
    selection: &gtk::SingleSelection,
    cmd_tx: &CommandSender,
    local_actions: &Rc<RefCell<LocalActionRegistry>>,
    user_actions: &Rc<dyn UserActionSink>,
    pending_user_executions: &Rc<Cell<u32>>,
) {
    let selected = selection.selected();
    if selected == gtk::INVALID_LIST_POSITION {
        return;
    }
    activate_at(
        selection,
        cmd_tx,
        local_actions,
        user_actions,
        pending_user_executions,
        selected,
    );
}

fn activate_at(
    selection: &gtk::SingleSelection,
    cmd_tx: &CommandSender,
    local_actions: &Rc<RefCell<LocalActionRegistry>>,
    user_actions: &Rc<dyn UserActionSink>,
    pending_user_executions: &Rc<Cell<u32>>,
    position: u32,
) {
    let Some(object) = selection.item(position) else {
        return;
    };
    let item: Item = model::item_of(&object);
    if !item
        .actions
        .iter()
        .any(|action| action.id == item.default_action)
    {
        return;
    }
    dispatch_item_action(
        cmd_tx,
        &item,
        local_actions,
        item.default_action.clone(),
        user_actions,
        pending_user_executions,
    );
}

fn wire_list_activation(
    list_view: &gtk::ListView,
    selection: &gtk::SingleSelection,
    cmd_tx: &CommandSender,
    local_actions: &Rc<RefCell<LocalActionRegistry>>,
    user_actions: &Rc<dyn UserActionSink>,
    pending_user_executions: &Rc<Cell<u32>>,
) {
    let selection = selection.clone();
    let cmd_tx = cmd_tx.clone();
    let local_actions = Rc::clone(local_actions);
    let user_actions = Rc::clone(user_actions);
    let pending_user_executions = Rc::clone(pending_user_executions);
    list_view.connect_activate(move |_list_view, position| {
        activate_at(
            &selection,
            &cmd_tx,
            &local_actions,
            &user_actions,
            &pending_user_executions,
            position,
        );
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    // These are the GTK-dependent half of issue #182's test suite —
    // `crate::keymap`'s own tests cover the keymap's pure lookup (parsing,
    // defaults, criterion 3's rebinding, criterion 4's two refusal shapes)
    // with no display at all. What is left to prove here needs a real
    // `HopWindow`, built through the real `HopWindow::build`, so it needs a
    // real GTK display — both run inside
    // `keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window`:
    // `assert_dispatch_action_moves_selection_and_activates` proves
    // `dispatch_action` — the function every keymap-resolved `Action` and
    // (via `super::activate_at`) every mouse click reaches — actually
    // drives the real `GtkSingleSelection` and sends the real `IpcCommand`s
    // the design promises, and `assert_mouse_click_activates_the_clicked_row`
    // is D5's regression test: the list view's own `activate` signal, fired
    // exactly as GTK fires it for a real single click, must produce the
    // same `Execute` command a keyboard `Activate` would.
    //
    // Neither test below drives a real GDK key event through the real
    // `EventControllerKey` `wire_keyboard` installs — both call
    // `dispatch_action` (or, for the mouse test, the list view's real
    // `activate` signal) directly. That gap was raised in review as a real
    // one, worth its own account rather than a wave at
    // `tests/view_tree_renderer.rs`'s "trivial glue, checked by reading the
    // diff" precedent: unlike `Node::for_item`, this glue sits directly in
    // the path of the app's single most important interaction (typing a
    // query, pressing Enter), and this issue removed
    // `entry.connect_activate`'s own prior coverage of part of that path.
    // The right response was to actually try synthesizing a real key event
    // through this display and see what happens — not to keep citing the
    // precedent.
    //
    // # What was tried, and why it does not work in this environment
    //
    // GTK3 had `gtk_test_widget_send_key(widget, keyval, modifiers)` in
    // `gtktestutils.h` — build a synthetic `GdkEventKey` and hand it to
    // `gtk_main_do_event` as though a backend had produced it. **GTK4
    // removed it, with no replacement.** Confirmed two ways on this exact
    // machine while investigating this: `grep -n "send_key" /usr/include/gtk-4.0/gtk/gtktestutils.h`
    // matches nothing (the GTK3 copy at `/usr/include/gtk-3.0/gtk/gtktestutils.h`
    // has it; the GTK4 one does not), and `gtk4-sys 0.11.4`'s generated
    // bindings carry only `gtk_test_accessible_*`, `gtk_test_init`,
    // `gtk_test_list_all_types`, `gtk_test_register_all_types`, and
    // `gtk_test_widget_wait_for_draw` — no `send_key`, no `send_button`, no
    // synthetic-event constructor of any kind.
    //
    // The lower-level route would be building a `GdkEvent`/`GdkKeyEvent` by
    // hand and pushing it onto the display's queue with
    // `gdk_display_put_event` — which *is* still present in `gdk4-sys`
    // (`fn gdk_display_put_event(display: *mut GdkDisplay, event: *mut
    // GdkEvent)`). It goes nowhere: GDK4's events are immutable and, by
    // design, constructed only by each backend's own real input path.
    // `/usr/include/gtk-4.0/gdk/gdkevents.h` declares no `_new` for
    // `GdkEvent` at all, and `gdk4-sys` exposes only *getters* for
    // `GdkKeyEvent` (`gdk_key_event_get_keyval`, `_get_keycode`,
    // `_get_consumed_modifiers`, …) — nothing that builds one. There is
    // nothing to hand `gdk_display_put_event` in the first place. This is a
    // GDK4-wide removal, not a broadway-specific gap: the same absence
    // would block this on X11 or Wayland too.
    //
    // Broadway's own client-side headers were checked on the chance the
    // backend itself exposed something backend-specific for this — it does
    // not. `/usr/include/gtk-4.0/gdk/broadway/gdkbroadwaydisplay.h` has
    // exactly two pairs of functions:
    // `gdk_broadway_display_show_keyboard`/`hide_keyboard` (toggling an
    // on-screen keyboard prompt in the browser, for touch devices) and
    // `get_surface_scale`/`set_surface_scale`. `gdkbroadwaysurface.h` adds
    // nothing input-related either. Architecturally this makes sense once
    // stated plainly: under broadway, this test process is the GTK
    // *client*, not the source of input. The only thing that ever produces
    // a real key event on a broadway display is an actual web browser,
    // running broadway's own JavaScript client (served by `gtk4-broadwayd`
    // itself) and forwarding real DOM keyboard events over broadway's own
    // WebSocket wire protocol to the server, which is what turns them into
    // GDK events for this process. A real browser (Chromium via Playwright,
    // both of which happen to be present on this development machine)
    // could in principle drive that whole path end to end — but doing so
    // would mean this crate's test suite silently growing a dependency on a
    // browser and a browser-automation framework being installed wherever
    // `cargo test` runs, which is not established anywhere else in this
    // repository's test infrastructure (`gtk4-broadwayd` is the one
    // documented, minimal, `libgtk-4-bin`-provided requirement every
    // display-needing test in this crate already relies on) and is not
    // guaranteed to be present in this project's actual CI. That trade was
    // not taken. If a future issue wants this coverage badly enough to
    // justify it, `gtk4-broadwayd`'s HTTP endpoint plus a real browser
    // driven by Playwright is the concrete path — reachable, just not
    // reached in this pass, the same distinction issue #179 drew about
    // `exec_round_trip.rs`'s own pre-existing gap.
    //
    // So: **synthesizing a real key event through this display is
    // unreachable via any public GDK4/GTK4 API in this environment**, for
    // any backend, confirmed by inspecting the actual headers and generated
    // bindings this crate builds against rather than assumed. What remains
    // untested end-to-end, honestly: that a real hardware Enter key press,
    // with focus in the query entry, is dispatched by GTK to this
    // controller before `GtkEntry`'s own input handling — the tests below
    // cover everything downstream of that (the controller's own dispatch
    // logic, `dispatch_action`'s effects, the mouse `activate` signal) but
    // not GTK's own event-routing decision to deliver the press to this
    // controller in the first place. `exec_round_trip.rs`'s own doc comment
    // already discloses the sibling gap on the IPC side; this is the same
    // kind of honest disclosure for the input side.
    //
    // # Why a re-exec'd subprocess, and why `gtk4-broadwayd`
    //
    // Identical reasoning to `tests/view_tree_renderer.rs`'s own module
    // doc, which this file does not repeat in full: `gtk::init()` only ever
    // reads `GDK_BACKEND`/`BROADWAY_DISPLAY` from *this process's own*
    // environment, so they must be set before this test binary's `main`
    // runs — the only sound way to do that without `std::env::set_var`
    // (`unsafe` under edition 2024, and this crate denies `unsafe_code`) is
    // on a child process's environment via `Command::env`. The child
    // recognizes `CHILD_MARKER` and runs the real assertions directly
    // instead of re-execing a second time; its exit status becomes this
    // test's own pass/fail. `gtk4-broadwayd`, not the `broadwayd` on
    // `$PATH` (Debian/Ubuntu's is `libgtk-3-bin`'s incompatible GTK3
    // server) — see `tests/headless_smoke.rs`'s top doc comment for how
    // that was diagnosed.

    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use adw::prelude::AdwApplicationWindowExt;
    use gtk::gdk;
    use hop_protocol::{
        Action as WireAction, ActionId, ActionKind, ItemId, ItemTitle, Kind, MarkerSpan, Mode,
    };

    use super::*;
    use crate::keymap::{Action, Keymap};
    #[derive(Default)]
    struct FakeUserActionSink {
        copies: RefCell<Vec<String>>,
        uris: RefCell<Vec<String>>,
    }

    impl UserActionSink for FakeUserActionSink {
        fn copy_text(&self, text: &str) {
            self.copies.borrow_mut().push(text.to_string());
        }

        fn launch_uri(&self, uri: &str) -> Result<(), String> {
            self.uris.borrow_mut().push(uri.to_string());
            Ok(())
        }
    }
    fn capture_six_state(window: &HopWindow, state: &str) {
        let context = glib::MainContext::default();
        let path = PathBuf::from(format!("/tmp/hop-258-sixstates-{state}.png"));
        let content = window
            .window
            .content()
            .expect("presented test window has a content root");
        for _ in 0..100 {
            while context.iteration(false) {}
            match crate::screenshot::capture(content.upcast_ref(), &path) {
                Ok(()) => {
                    println!("captured {state} state at {}", path.display());
                    return;
                }
                Err(crate::screenshot::ScreenshotError::NoRenderNode) => {
                    std::thread::sleep(Duration::from_millis(30));
                }
                Err(err) => panic!("capturing {state} state at {path:?}: {err}"),
            }
        }
        panic!("capturing {state} state at {path:?}: widget produced no drawing");
    }

    /// [`row_index_at_y`]'s own truth table — GTK-free, no display needed,
    /// matching `ui::row::tests`'s own precedent of pinning a pure
    /// arithmetic/logic decision as plain assertions before ever touching
    /// the real widget this file's broadway-gated tests drive it against.
    #[test]
    fn row_index_at_y_resolves_the_row_under_a_viewport_relative_y() {
        // No scroll offset: row 0 spans [0, 56), row 1 spans [56, 112), ...
        assert_eq!(row_index_at_y(0.0, 0.0, 56, 3), Some(0));
        assert_eq!(row_index_at_y(55.9, 0.0, 56, 3), Some(0));
        assert_eq!(
            row_index_at_y(56.0, 0.0, 56, 3),
            Some(1),
            "a y exactly on a row boundary must belong to the row below it"
        );
        assert_eq!(row_index_at_y(140.0, 0.0, 56, 3), Some(2));

        // Scrolled down by exactly one row's height: the same on-screen
        // y=0 now names row 1, not row 0 — the whole reason this function
        // takes a scroll offset at all rather than just a raw pixel `y`.
        assert_eq!(row_index_at_y(0.0, 56.0, 56, 3), Some(1));

        // Above the viewport (should not occur for a gesture attached
        // directly to list_view, but this function makes no assumption
        // about that) and at-or-past the last real row both name no row.
        assert_eq!(row_index_at_y(-1.0, 0.0, 56, 3), None);
        assert_eq!(
            row_index_at_y(168.0, 0.0, 56, 3),
            None,
            "row index 3 does not exist for a 3-item list"
        );
        assert_eq!(
            row_index_at_y(0.0, 0.0, 56, 0),
            None,
            "an empty list has no row for any y to resolve to"
        );

        // A non-positive row height cannot honestly name a row either.
        assert_eq!(row_index_at_y(0.0, 0.0, 0, 3), None);
    }

    /// [`scroll_value_to_reveal_row`]'s own truth table — GTK-free, the
    /// same shape [`row_index_at_y`]'s own test above already establishes
    /// for a different pure decision this file makes about row geometry.
    /// Issue #254 review, finding 3: this is the function
    /// [`HopWindow::open_secondary_action_menu`] calls before anchoring the
    /// panel to [`HopWindow::indicator`], so that indicator (driven by
    /// [`position_indicator`]) is never left describing a row that is not
    /// actually on screen.
    #[test]
    fn scroll_value_to_reveal_row_only_moves_when_the_row_is_not_already_fully_visible() {
        // A 5-row-tall viewport (`page_size = 5 * 56`), scrolled to its very
        // top. Row 2 (spanning [112, 168)) is already fully inside
        // [0, 280) — nothing to do.
        assert_eq!(
            scroll_value_to_reveal_row(2, 56, 0.0, 280.0),
            None,
            "a row already fully within the viewport must not trigger a scroll"
        );

        // The exact scenario this issue's own finding names: the list has
        // been scrolled well past row 2's own band (current value = 8 rows
        // down), so row 2's top (112) sits *above* the viewport's own top
        // (448) — scrolled out of view above it, not below.
        assert_eq!(
            scroll_value_to_reveal_row(2, 56, 8.0 * 56.0, 280.0),
            Some(112.0),
            "a row scrolled above the viewport must reveal it by scrolling up to the row's \
             own top, not leave the scroll position untouched"
        );

        // The opposite direction: row 9 (spanning [504, 560)) sits below a
        // viewport currently showing [0, 280) — its own bottom, not top,
        // is what must land exactly on the viewport's trailing edge.
        assert_eq!(
            scroll_value_to_reveal_row(9, 56, 0.0, 280.0),
            Some(280.0),
            "a row scrolled below the viewport must reveal it by scrolling down until the \
             row's own bottom is flush with the viewport's trailing edge"
        );

        // A row exactly flush with either edge already is already fully
        // visible — the boundary itself must not be treated as "out of
        // view" in either direction.
        assert_eq!(
            scroll_value_to_reveal_row(0, 56, 0.0, 280.0),
            None,
            "a row flush with the viewport's own top edge is already fully visible"
        );
        assert_eq!(
            scroll_value_to_reveal_row(4, 56, 0.0, 280.0),
            None,
            "a row flush with the viewport's own bottom edge is already fully visible"
        );
    }

    /// Set on the re-exec'd child so it knows to run the real assertions
    /// in-process instead of spawning a second child.
    const CHILD_MARKER: &str = "HOP_GTK_WINDOW_TEST_CHILD";

    /// A spawned `gtk4-broadwayd`, killed on drop. Display number derived
    /// from this process's own pid, offset by a caller-chosen `base` so
    /// parallel `cargo test` runs of every other file under `tests/` — and,
    /// since issue #200, a *second* `#[test]` fn in *this* file too — do
    /// not collide on the same display. `base` used to be a hardcoded `400`
    /// this struct alone knew; it is a parameter now because
    /// `offline_indicator_reflects_connection_state` below needed its own,
    /// distinct base (`450` was already `tests/motion_setting.rs`'s) rather
    /// than sharing `400 + process::id()` with
    /// `keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window` —
    /// see this struct's own former "One test, one display" comment there
    /// for why two `#[test]` fns computing the *identical* display number
    /// (same base, same pid — `std::process::id()` is one value per test
    /// *binary*, shared by every `#[test]` fn cargo runs as a thread inside
    /// it) would race to bind the same broadway socket and one would fail.
    /// Two different bases sidestep that without needing to merge this
    /// file's now-two broadway-dependent tests into one function the way
    /// that comment originally did.
    struct BroadwayServer {
        child: Child,
        display: u32,
    }

    impl BroadwayServer {
        fn start(base: u32) -> Self {
            let display = base + (std::process::id() % 5000);
            let child = Command::new("gtk4-broadwayd")
                .arg(format!(":{display}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect(
                    "failed to spawn gtk4-broadwayd — it ships in libgtk-4-bin (NOT \
                     `broadwayd` on $PATH; see tests/headless_smoke.rs's top doc comment)",
                );
            // Asynchronous socket creation — see `tests/headless_smoke.rs`'s
            // identical comment; the socket lives in the abstract namespace
            // so it cannot be polled for by `Path::exists`.
            std::thread::sleep(Duration::from_millis(300));
            BroadwayServer { child, display }
        }
    }

    impl Drop for BroadwayServer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// A minimal, valid [`Item`] — `n` differentiates ids, `title`
    /// differentiates what a row would show, matching
    /// `tests/view_tree_renderer.rs`'s own `test_item` helper.
    fn test_item(n: usize, title: &str) -> Item {
        Item {
            id: ItemId::new(format!("test:{n}")).unwrap(),
            kind: Kind::Action,
            title: ItemTitle::new(title).unwrap(),
            subtitle: None,
            icon: None,
            actions: vec![WireAction {
                id: ActionId::new("open").unwrap(),
                kind: ActionKind::Open,
                label: "Open".to_string(),
            }],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "test".to_string(),
        }
    }

    /// A variant of [`test_item`] carrying more than one action, in
    /// `action_ids` order — [`test_item`] alone (exactly one action,
    /// doubling as its own `default_action`) cannot exercise "the chosen
    /// action's own id, not `default_action`", which is exactly the
    /// distinction issue #254's action-panel tests below need. The first id
    /// in `action_ids` is always `default_action`, matching [`test_item`]'s
    /// own convention of using its one (and here, first) action as the
    /// default.
    fn test_item_with_actions(n: usize, title: &str, action_ids: &[&str]) -> Item {
        let actions: Vec<WireAction> = action_ids
            .iter()
            .map(|id| WireAction {
                id: ActionId::new(*id).unwrap(),
                kind: ActionKind::Open,
                label: (*id).to_string(),
            })
            .collect();
        let default_action = actions
            .first()
            .expect("test fixture must name at least one action id")
            .id
            .clone();
        Item {
            id: ItemId::new(format!("test:{n}")).unwrap(),
            kind: Kind::Action,
            title: ItemTitle::new(title).unwrap(),
            subtitle: None,
            icon: None,
            actions,
            default_action,
            copy_text: None,
            append_to_end: false,
            provider: "test".to_string(),
        }
    }

    /// An item with zero actions — [`ActionPanel::present`]'s own "no
    /// mystery box" rule (see that method's doc comment) means this must
    /// never open the panel. Matches `tests/action_panel.rs`'s own
    /// `zero_action_item` fixture: `default_action` still names an id
    /// (`hop_protocol::Item`'s own field is not `Option`), even though no
    /// action with that id exists in `actions` — a wire shape this crate's
    /// own daemon is trusted never to send, exercised here only because a
    /// test fixture has to fill every field regardless.
    fn test_item_without_actions(n: usize, title: &str) -> Item {
        Item {
            id: ItemId::new(format!("test:{n}")).unwrap(),
            kind: Kind::Action,
            title: ItemTitle::new(title).unwrap(),
            subtitle: None,
            icon: None,
            actions: vec![],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "test".to_string(),
        }
    }

    /// Re-execs this test binary under a headless `broadway` display and
    /// asserts the child's real-assertion run succeeded — see this module's
    /// doc comment.
    fn run_under_broadway(test_name: &str, base: u32) {
        if std::env::var_os(CHILD_MARKER).is_some() {
            // Already the re-exec'd child; the `#[test]` fn that called this
            // has already run its real assertions before reaching here in
            // that case — see each test fn below.
            return;
        }

        let broadway = BroadwayServer::start(base);
        let output = Command::new(std::env::current_exe().unwrap())
            .env("GDK_BACKEND", "broadway")
            .env("BROADWAY_DISPLAY", format!(":{}", broadway.display))
            .env(CHILD_MARKER, "1")
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .output()
            .expect("failed to re-exec this test binary under the headless broadway display");

        assert!(
            output.status.success(),
            "the headless child process failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// Builds a [`HopWindow`] against a throwaway [`adw::Application`] and a
    /// [`crate::ipc::test_channel`] rather than a real IPC thread — every
    /// test below only cares which [`IpcCommand`] a dispatch produced, never
    /// whether a real `hopd` received it (`ipc`'s own module doc comment is
    /// the case for why that boundary is safe to substitute in a test:
    /// `CommandSender`/`IpcCommand` carry no socket, only plain data).
    /// `app_id` must be distinct across calls within one process: both of
    /// this module's assertion functions call this from the *same*
    /// re-exec'd child, and `adw::Application::register` exports a D-Bus
    /// object at a path derived from the application id — a second
    /// `register` under an id already exported in this process fails with
    /// "An object is already exported", `NON_UNIQUE` notwithstanding (that
    /// flag only disables cross-process single-instance forwarding, not
    /// this process-local export).
    fn build_test_window(app_id: &str) -> (HopWindow, async_channel::Receiver<IpcCommand>) {
        // These widget tests run under broadway — `SessionKind::Other`, the
        // one strategy that deliberately wires nothing session-specific
        // onto the window (no self-positioning, no focus-loss dismissal),
        // which is exactly what a capture harness wants.
        build_configured_window(
            app_id,
            crate::session::SessionKind::Other.overlay_strategy(crate::layer_shell::probe()),
            RunPurpose::Interactive,
        )
    }
    fn build_test_window_with_action_sink(
        app_id: &str,
        actions: Rc<FakeUserActionSink>,
    ) -> (HopWindow, async_channel::Receiver<IpcCommand>) {
        let app = adw::Application::new(Some(app_id), gio::ApplicationFlags::NON_UNIQUE);
        app.register(gio::Cancellable::NONE)
            .expect("registering a NON_UNIQUE test application must not fail");
        let (cmd_tx, cmd_rx) = crate::ipc::test_channel();
        let strategy =
            crate::session::SessionKind::Other.overlay_strategy(crate::layer_shell::probe());
        let user_actions: Rc<dyn UserActionSink> = actions;
        let window = HopWindow::build_with_user_actions(
            &app,
            cmd_tx,
            Keymap::defaults(),
            strategy,
            RunPurpose::Interactive,
            user_actions,
        );
        (window, cmd_rx)
    }

    /// [`build_test_window`] with the overlay strategy and run purpose
    /// spelled out — the shape
    /// [`screenshot_window_never_wires_close_on_focus_loss`] needs, pinning
    /// issue #261's wiring decision across both purposes under a strategy
    /// that *does* ask for dismissal.
    fn build_configured_window(
        app_id: &str,
        strategy: crate::session::OverlayStrategy,
        purpose: RunPurpose,
    ) -> (HopWindow, async_channel::Receiver<IpcCommand>) {
        let app = adw::Application::new(Some(app_id), gio::ApplicationFlags::NON_UNIQUE);
        // GTK asserts "New application windows must be added after the
        // GApplication::startup signal has been emitted" the moment
        // `HopWindow::build` constructs an `adw::ApplicationWindow` with
        // `.application(app)` set — `app::run`'s own production code never
        // hits this because `app.run_with_args` always emits `startup`
        // before its own `activate` handler (the one that calls
        // `HopWindow::build`) ever runs. This test builds a window directly,
        // with no `app.run()` main loop around it, so it has to trigger that
        // same signal itself first: `register` does exactly that,
        // synchronously, with no main loop iteration required.
        app.register(gio::Cancellable::NONE)
            .expect("registering a NON_UNIQUE test application must not fail");
        let (cmd_tx, cmd_rx) = crate::ipc::test_channel();
        let window = HopWindow::build(&app, cmd_tx, Keymap::defaults(), strategy, purpose);
        (window, cmd_rx)
    }

    // Both GTK-dependent checks below run from *one* `#[test]` function,
    // sharing one re-exec'd child and one `BroadwayServer` — not two. Each
    // `BroadwayServer::start` derives its display number from
    // `std::process::id()`, which is identical for every `#[test]` fn in
    // this one binary (cargo runs them as threads within a single process,
    // not separate processes), so two independently re-exec'd tests using
    // the *same* base would race to bind the *same* broadway display and one
    // would fail with "Unable to write to server" — verified directly while
    // writing this suite: splitting these into two `#[test]` fns
    // intermittently failed exactly that way under `cargo test
    // --workspace`'s default parallelism. One test, one display, both
    // checks — for these two specifically, since both exercise the same
    // pre-built `HopWindow` construction path and neither needs anything
    // the other's fixture does not already set up.
    //
    // `offline_indicator_reflects_connection_state` further down is issue #200's
    // own, later addition, and deliberately does *not* join this function:
    // it is a different, later concern (a widget's response to
    // `IpcEvent`s, not keyboard/mouse dispatch), and — per
    // `BroadwayServer::start`'s own updated doc comment — a distinct `base`
    // is what actually resolves the collision this comment describes,
    // making a second `#[test]` fn safe again without needing every
    // GTK-dependent check in this file to share one function forever.
    #[test]
    fn keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window() {
        run_under_broadway(
            "ui::window::tests::keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window",
            400,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        assert_dispatch_action_moves_selection_and_activates();
        assert_mouse_click_activates_the_clicked_row();
        assert_activate_signal_produces_exactly_one_execute_per_emission();
        assert_mode_label_mirrors_exclusive_and_nothing_else();
        assert_marker_highlight_covers_exactly_the_reported_span();
        assert_stale_marker_span_is_never_applied_to_newer_text();
    }

    /// [`HopWindow::dispatch_action`] is the single place every keymap-
    /// resolved [`Action`] takes effect — this drives it directly (rather
    /// than through a real key press) per this module's own doc comment's
    /// argument for why the `key-pressed` closure itself is checked by
    /// reading the diff instead.
    fn assert_dispatch_action_moves_selection_and_activates() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.Dispatch");
        let item_a = test_item(1, "first result");
        let item_b = test_item(2, "second result");
        model::replace(&window.store, vec![item_a.clone(), item_b.clone()]);
        window.selection.set_selected(0);

        // NavigateDown/NavigateUp move the real GtkSingleSelection.
        window.dispatch_action(Action::NavigateDown);
        assert_eq!(
            window.selection.selected(),
            1,
            "NavigateDown must move forward"
        );
        window.dispatch_action(Action::NavigateUp);
        assert_eq!(window.selection.selected(), 0, "NavigateUp must move back");

        // End/Home jump to the ends; clamped rather than panicking on a
        // short (two-item) list.
        window.dispatch_action(Action::End);
        assert_eq!(window.selection.selected(), 1);
        window.dispatch_action(Action::PageDown);
        assert_eq!(
            window.selection.selected(),
            1,
            "PageDown past the last row must clamp, not wrap or panic"
        );
        window.dispatch_action(Action::Home);
        assert_eq!(window.selection.selected(), 0);
        window.dispatch_action(Action::PageUp);
        assert_eq!(
            window.selection.selected(),
            0,
            "PageUp before the first row must clamp, not wrap or panic"
        );

        // Activate sends Execute for whichever row is currently selected.
        window.selection.set_selected(1);
        window.dispatch_action(Action::Activate);
        match cmd_rx
            .try_recv()
            .expect("Activate must send an Execute command")
        {
            IpcCommand::Execute { item_id, .. } => assert_eq!(item_id, item_b.id),
            other => panic!("expected Execute, got {other:?}"),
        }

        // `CompletePrefix` is still the D4 behaviorless stub: callable
        // without panicking, no `IpcCommand`. `SecondaryAction` is not
        // behaviorless any more (issue #254) — it opens the action panel
        // for `item_b`, still selected from the Activate check above — but
        // opening the panel alone must still send no `IpcCommand` either;
        // only choosing an action inside it does that (see
        // `ctrl_k_action_panel_opens_pins_and_dispatches_through_the_keymap`'s
        // own assertions for that real behavior, exercised properly against
        // a *presented* window rather than this one, which is not yet
        // presented at this point in the test).
        window.dispatch_action(Action::SecondaryAction);
        window.dispatch_action(Action::CompletePrefix);
        assert!(
            cmd_rx.try_recv().is_err(),
            "opening the action panel, and the still-behaviorless CompletePrefix, must not \
             themselves send any IpcCommand"
        );
        // Reset: `self.indicator` is already parented under this window's
        // own `adw::ApplicationWindow` from `build` onward (it does not
        // wait for `present()`), so the `SecondaryAction` dispatch above
        // really did open the panel, even though this window itself is not
        // yet on screen. Left open, it would make the Dismiss check below
        // close only the panel (per its own new guard) instead of the
        // window, which is not what this section of the test is about —
        // that interaction has its own dedicated coverage in
        // `assert_escape_with_the_panel_open_closes_only_the_panel` below.
        window.action_panel.dismiss();

        // Dismiss closes the pre-built (hide_on_close) window.
        window.present_with_token(None);
        assert!(
            window.window.is_visible(),
            "the window must be visible once presented"
        );
        window.dispatch_action(Action::Dismiss);
        assert!(
            !window.window.is_visible(),
            "Dismiss must hide the window rather than leaving it on screen"
        );

        println!("dispatch_action assertions passed");
    }

    /// D5's regression test: mouse-click activation. Fires the real
    /// `GtkListView` `activate` signal — the same one a real single click
    /// fires now that `build` sets `set_single_click_activate(true)` and
    /// `wire_list_activation` connects it — for a specific row position,
    /// and asserts the resulting `Execute` names *that* row's item, not
    /// whatever the selection model separately holds.
    fn assert_mouse_click_activates_the_clicked_row() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.MouseClick");
        let item_a = test_item(1, "first result");
        let item_b = test_item(2, "second result");
        model::replace(&window.store, vec![item_a.clone(), item_b.clone()]);
        // Selection deliberately left on the first row while the simulated
        // click below targets the second, so a wiring bug that reads
        // `selection.selected()` instead of the signal's own `position`
        // argument would be caught rather than accidentally passing.
        window.selection.set_selected(0);

        // The "activate" signal is a real, named GObject signal —
        // `ListView::connect_activate` only ever backs an actual
        // `g_signal_connect` — so emitting it by hand exercises the exact
        // closure `wire_list_activation` installed, not a copy of its
        // logic. Same technique `tests/view_tree_renderer.rs` uses for the
        // factory's `setup` signal.
        window.list_view.emit_by_name::<()>("activate", &[&1u32]);

        match cmd_rx
            .try_recv()
            .expect("a row activation must send an Execute command")
        {
            IpcCommand::Execute { item_id, .. } => assert_eq!(
                item_id, item_b.id,
                "the clicked row's own item must be activated, not the selection model's"
            ),
            other => panic!("expected Execute, got {other:?}"),
        }

        println!("mouse-click activation assertions passed");
    }

    /// SPEC decision 6: "double-click = single click." Checked directly
    /// against GTK 4.14.5's own source
    /// (`gtk/gtklistfactorywidget.c`,
    /// `gtk_list_factory_widget_click_gesture_released`) while writing this
    /// test: with `single-click-activate` true (`build` sets this), the
    /// guard is `if (n_press == 1 && priv->single_click_activate)` — an
    /// *exact* equality on `n_press`, not `>=` — so only the *first* press
    /// of a genuine double-click ever satisfies it. The second press
    /// (`n_press == 2`) falls through to the `selectable` branch
    /// (`list.select-item`, a plain re-select) instead of firing
    /// `list.activate-item` a second time. A real double-click therefore
    /// activates exactly once at the GTK gesture level, *before* this
    /// crate's own `wire_list_activation` closure ever runs — there is
    /// nothing for that closure to debounce, and nothing it could debounce
    /// even if it tried, since GTK itself never emits a second `activate`
    /// for the second press.
    ///
    /// This crate's own test environment cannot synthesize a real
    /// two-press click gesture to prove that GTK-level guard directly (this
    /// file's own top doc comment makes the identical finding for
    /// synthetic key events, and the same absence of a public GDK4
    /// event-construction API applies to pointer events too — confirmed
    /// while investigating this: no `gtk_test_widget_send_key`-shaped
    /// helper, nor its click equivalent, exists in this workspace's
    /// `gtk4-sys`/`gdk4-sys` bindings). What this test *can*, and does,
    /// prove is the half that is this crate's own code rather than GTK's:
    /// `wire_list_activation`'s closure sends exactly one `Execute` per
    /// `activate` emission it receives, with no accidental double-
    /// registration (a future edit connecting a second handler to the same
    /// signal, say) multiplying one genuine activation into two commands.
    fn assert_activate_signal_produces_exactly_one_execute_per_emission() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.ActivateOnce");
        model::replace(&window.store, vec![test_item(1, "only row")]);

        window.list_view.emit_by_name::<()>("activate", &[&0u32]);

        assert!(
            cmd_rx.try_recv().is_ok(),
            "one activate emission must produce one Execute command"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "one activate emission must never produce a second Execute command — this is the \
             wiring's own idempotency, complementing the GTK-source-verified guarantee (this \
             function's own doc comment) that a real double-click never emits activate twice \
             in the first place"
        );

        println!("assert_activate_signal_produces_exactly_one_execute_per_emission passed");
    }

    /// Reads back the one attribute [`marker_highlight::apply`] would have
    /// set on `entry`, if any — `(start, end)` byte offsets, or `None` if
    /// nothing is currently highlighted. `gtk::Entry::attributes()` needs a
    /// real display to be meaningful the same way constructing the entry
    /// itself does, which is why this lives beside the broadway-gated
    /// assertions below rather than in `ui::marker_highlight`'s own
    /// (display-free) unit tests.
    fn entry_highlighted_range(entry: &gtk::Entry) -> Option<(u32, u32)> {
        let list = entry.attributes()?;
        let attr = list.iterator().attrs().iter().next().cloned()?;
        Some((attr.start_index(), attr.end_index()))
    }

    /// Issue #184, criterion 1 / D3: the mode label is shown, naming the
    /// mode, exactly when a `Routed` frame reports an exclusive route, and
    /// is entirely absent — hidden, not merely empty-text — otherwise.
    ///
    /// Reads `get_visible()` (the label's own `visible` property,
    /// `gtk_widget_get_visible`), not `is_visible()`
    /// (`gtk_widget_is_visible`) — the latter also asks whether every
    /// *ancestor* is visible, and `build_test_window` never presents the
    /// window (this check has nothing to do with on-screen presentation), so
    /// it would read `false` regardless of what `mode_label::apply` actually
    /// set.
    fn assert_mode_label_mirrors_exclusive_and_nothing_else() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.ModeLabel");

        assert!(
            !window.mode_label.get_visible(),
            "a freshly built window must show no mode label before any route arrives"
        );

        window.apply_event(IpcEvent::Routed {
            mode: Mode::Weather,
            exclusive: true,
            marker_span: None,
            query_text: String::new(),
            pending_providers: vec![],
        });
        assert!(
            window.mode_label.get_visible(),
            "an exclusive route must show the mode label"
        );
        assert_eq!(window.mode_label.text(), "Weather");

        window.apply_event(IpcEvent::Routed {
            mode: Mode::Weather,
            exclusive: false,
            marker_span: None,
            query_text: String::new(),
            pending_providers: vec![],
        });
        assert!(
            !window.mode_label.get_visible(),
            "a non-exclusive route must show no label at all, mirroring `exclusive` exactly"
        );
        assert_eq!(
            window.mode_label.text(),
            "",
            "the label's text must be cleared, not just hidden, once absent"
        );

        println!("mode label assertions passed");
    }

    /// Issue #184, criterion 2: the consumed-marker highlight covers exactly
    /// the byte range a `Routed` frame reports, and a `None` span highlights
    /// nothing.
    fn assert_marker_highlight_covers_exactly_the_reported_span() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.MarkerHighlight");

        window.entry.set_text("w firefox");
        assert!(
            entry_highlighted_range(&window.entry).is_none(),
            "a freshly built entry must start with nothing highlighted"
        );

        window.apply_event(IpcEvent::Routed {
            mode: Mode::Windows,
            exclusive: true,
            marker_span: Some(MarkerSpan::new(0, 2).unwrap()),
            query_text: "w firefox".to_string(),
            pending_providers: vec![],
        });
        assert_eq!(
            entry_highlighted_range(&window.entry),
            Some((0, 2)),
            "the highlight must cover exactly the reported span"
        );

        // A later route that consumed no marker (`marker_span: None`) clears
        // whatever highlight the previous route left — the entry's own text
        // is unchanged here, only what routed it.
        window.apply_event(IpcEvent::Routed {
            mode: Mode::All,
            exclusive: false,
            marker_span: None,
            query_text: "w firefox".to_string(),
            pending_providers: vec![],
        });
        assert!(
            entry_highlighted_range(&window.entry).is_none(),
            "a `None` span must clear any previous highlight, not leave it stale"
        );

        println!("marker highlight assertions passed");
    }

    /// The stale-span risk Task 1's review handed to this task, and the one
    /// this suite treats as its most important check: a `marker_span`
    /// computed against a query's text must never be applied once the entry
    /// has moved on to different text — see `ui::marker_highlight`'s module
    /// doc comment and `IpcEvent::Routed`'s own doc comment for the full
    /// argument this test is the executable form of.
    ///
    /// The scenario: the user typed `"w "` (routing exclusively to Windows,
    /// with a marker span over the leading `"w "`), then kept typing before
    /// that frame's response arrived, so by the time `apply_event` runs, the
    /// entry already shows `"wx firefox"` — a *different* mode's territory
    /// (`wx ` is the weather prefix; see D7's own confusability example).
    /// Applying `"w "`'s span (`[0, 2)`) to `"wx firefox"` would not panic —
    /// both offsets land on real character boundaries of the new text too —
    /// it would just silently highlight the wrong two bytes. This must not
    /// happen: the stale frame's own `query_text` no longer matches
    /// `entry.text()`, and that mismatch is exactly what
    /// `marker_highlight::apply` checks for.
    fn assert_stale_marker_span_is_never_applied_to_newer_text() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.StaleSpan");

        // The user has moved on: the entry now holds text a *later* query
        // produced, not the one this (stale) frame was computed against.
        window.entry.set_text("wx firefox");

        window.apply_event(IpcEvent::Routed {
            mode: Mode::Windows,
            exclusive: true,
            marker_span: Some(MarkerSpan::new(0, 2).unwrap()),
            query_text: "w ".to_string(),
            pending_providers: vec![],
        });

        assert!(
            entry_highlighted_range(&window.entry).is_none(),
            "a span bound to superseded text must never be applied to newer text, \
             even though its offsets also happen to be valid against the new text"
        );

        println!("stale marker span guard assertions passed");
    }

    /// Issue #200's own production-wiring proof: [`HopWindow::apply_event`]
    /// is the one call site that ever shows or hides the offline indicator, and
    /// this is what proves it actually does — not
    /// `tests/honesty_locked_provider.rs`'s job, which builds an
    /// [`OfflineIndicator`] directly and never touches `apply_event` at all. Uses
    /// its own `BroadwayServer` at base `550` — the first base not already
    /// claimed by another file under `tests/` or by this file's own first
    /// test above (`400`) — rather than joining that test, per
    /// `BroadwayServer::start`'s own updated doc comment.
    #[test]
    fn offline_indicator_reflects_connection_state() {
        run_under_broadway(
            "ui::window::tests::offline_indicator_reflects_connection_state",
            550,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.OfflineIndicator");
        // `gtk::Widget::is_visible` (unlike `set_visible`'s own paired
        // property alone) also checks every ancestor's own visibility —
        // and a pre-built `HopWindow`'s toplevel starts unpresented, itself
        // not visible, per this whole crate's "pre-built hidden window"
        // design (`app`'s own module doc). Without `present_with_token`
        // here, *every* visibility assertion below — shown or hidden —
        // would trivially read `false` regardless of what
        // `OfflineIndicator::apply` actually did, proving nothing.
        // `assert_dispatch_action_moves_selection_and_activates` above hits
        // the identical requirement for `window.window.is_visible()` at its
        // own Dismiss check.
        window.present_with_token(None);

        assert!(
            !window.offline_indicator.widget.is_visible(),
            "a freshly built window must not show the offline indicator before any IpcEvent has \
             arrived — there is nothing honest to show yet"
        );

        window.apply_event(IpcEvent::Disconnected);
        assert!(
            window.offline_indicator.widget.is_visible(),
            "IpcEvent::Disconnected must show the offline indicator"
        );

        // No privileged access to `OfflineIndicator`'s own private `stamp`
        // field needed here — the same plain `first_child`/`next_sibling`
        // widget-tree walk `tests/honesty_locked_provider.rs` already uses
        // finds it back out: `OfflineIndicator::build`'s own doc comment fixes
        // the stamp label as the container's second child, right after the
        // (never-changing) text label.
        let stamp_label = window
            .offline_indicator
            .widget
            .first_child()
            .and_then(|text| text.next_sibling())
            .and_then(|stamp| stamp.downcast::<gtk::Label>().ok())
            .expect("the offline indicator must have a stamp label as its second child");
        assert!(
            stamp_label.text().starts_with("as of "),
            "the offline indicator's stamp must read \"as of HH:MM\" once shown, got: {:?}",
            stamp_label.text()
        );

        window.apply_event(IpcEvent::Connected);
        assert!(
            !window.offline_indicator.widget.is_visible(),
            "IpcEvent::Connected must hide the offline indicator again"
        );

        println!("the offline indicator shows on Disconnected and hides on Connected");
    }

    /// Issue #254's own wiring slice: `Action::SecondaryAction`'s dispatch
    /// (ctrl+K by default — `crate::keymap::Action::default_spelling`) now
    /// opens `HopWindow`'s own [`ActionPanel`], and `Action::Dismiss` (the
    /// default Escape binding) must close only that panel when one is open,
    /// never the window underneath it. Bundled into one `#[test]` fn
    /// sharing one `BroadwayServer`/re-exec'd child, for the identical
    /// reason `keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window`
    /// and `offline_indicator_reflects_connection_state` already are: every
    /// `#[test]` fn in this one binary shares one process id, so each needs
    /// its own `base` to avoid two of them racing to bind the same
    /// broadway display. `700` is the first base this file has not already
    /// claimed (`400`, `550`).
    ///
    /// Every assertion below drives `HopWindow::dispatch_action` directly,
    /// not a real `GdkEvent` — this file's own top doc comment already
    /// makes the case for why (GTK4 exposes no synthetic-key-event
    /// constructor on any backend in this environment), and
    /// `ActionPanel::handle_key` (issue #254's own panel) takes the
    /// identical "call the resolved function, not a synthesized event"
    /// shape for the same reason, one file over.
    #[test]
    fn ctrl_k_action_panel_opens_pins_and_dispatches_through_the_keymap() {
        run_under_broadway(
            "ui::window::tests::ctrl_k_action_panel_opens_pins_and_dispatches_through_the_keymap",
            700,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        assert_secondary_action_opens_the_panel_for_a_selected_item_with_actions();
        assert_secondary_action_does_nothing_without_a_selection();
        assert_secondary_action_does_not_open_for_an_item_with_no_actions();
        assert_choosing_an_action_sends_execute_with_that_actions_id_not_the_default();
        assert_choosing_pins_the_item_opened_for_not_whatever_is_selected_later();
        assert_escape_with_the_panel_open_closes_only_the_panel();
        assert_escape_with_the_panel_closed_still_dismisses_the_window();
        assert_ctrl_k_scrolls_a_row_selected_above_the_viewport_back_into_view();

        println!("ctrl-K action panel wiring assertions passed");
    }

    /// `ActionPanel::present`'s own "a popover needs a realized `gtk::Native`"
    /// requirement (see that method's doc comment) is why every assertion
    /// function below calls `window.present_with_token(None)` before
    /// dispatching `SecondaryAction` — without it, `popup()` would silently
    /// no-op (a logged `g_critical`, not a panic) and every "must open"
    /// assertion would read `false` regardless of whether dispatch itself
    /// was correct, proving nothing. `assert_dispatch_action_moves_selection_and_activates`
    /// above hits the identical requirement for `window.window.is_visible()`.
    fn assert_secondary_action_opens_the_panel_for_a_selected_item_with_actions() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.PanelOpens");
        window.present_with_token(None);
        model::replace(&window.store, vec![test_item(1, "has actions")]);
        window.selection.set_selected(0);

        window.dispatch_action(Action::SecondaryAction);

        assert!(
            window.action_panel.popover().is_visible(),
            "SecondaryAction must open the panel for a selected item that has actions"
        );

        println!("assert_secondary_action_opens_the_panel_for_a_selected_item_with_actions passed");
    }

    fn assert_secondary_action_does_nothing_without_a_selection() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.PanelNoSelection");
        window.present_with_token(None);
        // An empty store: `selection.selected()` reads `INVALID_LIST_POSITION`,
        // the same "nothing to act on" state `activate_selected` already
        // treats as a no-op.
        model::replace(&window.store, vec![]);

        window.dispatch_action(Action::SecondaryAction);

        assert!(
            !window.action_panel.popover().is_visible(),
            "SecondaryAction with no selection must not open the panel"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "no selection must send no IpcCommand either"
        );

        println!("assert_secondary_action_does_nothing_without_a_selection passed");
    }

    fn assert_secondary_action_does_not_open_for_an_item_with_no_actions() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.PanelZeroActions");
        window.present_with_token(None);
        model::replace(
            &window.store,
            vec![test_item_without_actions(1, "no actions")],
        );
        window.selection.set_selected(0);

        window.dispatch_action(Action::SecondaryAction);

        assert!(
            !window.action_panel.popover().is_visible(),
            "an item with zero actions must not open the panel — ActionPanel::present's own \
             'no mystery box' rule"
        );

        println!("assert_secondary_action_does_not_open_for_an_item_with_no_actions passed");
    }

    /// The panel's own callback must be turned into the same `IpcCommand::Execute`
    /// shape `activate_at` sends for the default action — but carrying
    /// *this* choice's id, not `item.default_action`. Row 0 of a freshly
    /// presented panel is `item.actions[0]` (`"open"`, the fixture's own
    /// default); this test moves off it before choosing.
    fn assert_choosing_an_action_sends_execute_with_that_actions_id_not_the_default() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.PanelChoose");
        window.present_with_token(None);
        let item = test_item_with_actions(1, "multi", &["open", "reveal"]);
        model::replace(&window.store, vec![item.clone()]);
        window.selection.set_selected(0);

        window.dispatch_action(Action::SecondaryAction);
        assert!(window.action_panel.popover().is_visible());

        window.action_panel.handle_key(gdk::Key::Down); // row 1: "reveal"
        window.action_panel.handle_key(gdk::Key::Return);

        match cmd_rx
            .try_recv()
            .expect("choosing an action must send an Execute command")
        {
            IpcCommand::Execute { item_id, action_id } => {
                assert_eq!(item_id, item.id);
                assert_eq!(
                    action_id.as_str(),
                    "reveal",
                    "the chosen action's own id must be sent, not item.default_action (\"open\")"
                );
            }
            other => panic!("expected Execute, got {other:?}"),
        }

        println!(
            "assert_choosing_an_action_sends_execute_with_that_actions_id_not_the_default passed"
        );
    }

    /// The sharpest edge in this slice, per this issue's own brief: the
    /// item a choice runs against is the one the panel was *opened* for,
    /// fixed at that moment, even if the results selection moves to a
    /// different item while the panel stays open. A naive implementation
    /// that re-read `self.selection.selected()` inside the panel's
    /// `on_choose` callback would pass every other test above (none of them
    /// move the selection after opening) and fail only this one.
    fn assert_choosing_pins_the_item_opened_for_not_whatever_is_selected_later() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.PanelPinned");
        window.present_with_token(None);
        let item_a = test_item(1, "item A");
        let item_b = test_item(2, "item B");
        model::replace(&window.store, vec![item_a.clone(), item_b.clone()]);
        window.selection.set_selected(0); // item_a

        window.dispatch_action(Action::SecondaryAction); // opens the panel for item_a
        assert!(window.action_panel.popover().is_visible());

        // The results selection moves to item_b *while the panel is still
        // open* — nothing about `ActionPanel` or this window's own key
        // dispatch freezes `self.selection` for as long as a panel is
        // presented.
        window.selection.set_selected(1);

        window.action_panel.handle_key(gdk::Key::Return); // chooses item_a's only action

        match cmd_rx
            .try_recv()
            .expect("choosing an action must send an Execute command")
        {
            IpcCommand::Execute { item_id, .. } => assert_eq!(
                item_id, item_a.id,
                "the action must run against the item the panel was opened for (item_a), not \
                 whatever the results selection moved to while the panel stayed open (item_b)"
            ),
            other => panic!("expected Execute, got {other:?}"),
        }

        println!("assert_choosing_pins_the_item_opened_for_not_whatever_is_selected_later passed");
    }

    /// Issue #254: Escape must close an open panel without also closing the
    /// window underneath it — `Action::Dismiss`'s arm in `dispatch_action`
    /// is what this pins, driven directly rather than through a real
    /// Escape keypress, per this test module's own established precedent.
    fn assert_escape_with_the_panel_open_closes_only_the_panel() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.EscapePanelOpen");
        window.present_with_token(None);
        model::replace(&window.store, vec![test_item(1, "has actions")]);
        window.selection.set_selected(0);
        window.dispatch_action(Action::SecondaryAction);
        assert!(
            window.action_panel.popover().is_visible(),
            "setup: the panel must be open before this test's own assertion means anything"
        );

        window.dispatch_action(Action::Dismiss);

        assert!(
            !window.action_panel.popover().is_visible(),
            "Dismiss (Escape's default binding) must close an open panel"
        );
        assert!(
            window.window.is_visible(),
            "Dismiss must not also close the window while the panel was open — Escape returns \
             to the list, it does not dismiss hop"
        );

        println!("assert_escape_with_the_panel_open_closes_only_the_panel passed");
    }

    /// The other direction of the same contract: with no panel open,
    /// Dismiss must still behave exactly as it did before this issue —
    /// closing the window.
    fn assert_escape_with_the_panel_closed_still_dismisses_the_window() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.EscapeWindowOnly");
        window.present_with_token(None);
        assert!(
            !window.action_panel.popover().is_visible(),
            "setup: the panel must be closed before this test's own assertion means anything"
        );

        window.dispatch_action(Action::Dismiss);

        assert!(
            !window.window.is_visible(),
            "Dismiss with no panel open must still dismiss the window, exactly as before this \
             issue"
        );

        println!("assert_escape_with_the_panel_closed_still_dismisses_the_window passed");
    }

    /// Issue #254 review, finding 3: a real anchor bug. `self.scrolled`'s
    /// own `vadjustment` is configured directly, via
    /// [`gtk::Adjustment::configure`], rather than trusted to arrive at a
    /// particular value from real widget layout — this suite deliberately
    /// runs no `glib::MainContext::iteration` anywhere (see this module's
    /// own top doc comment for why every other geometry-shaped assertion
    /// here already drives a resolved pure function or reads a value
    /// GtkAdjustment initializes to `0.0` on its own), and a real broadway
    /// layout pass's *timing* is not this test's own concern — only that
    /// [`HopWindow::open_secondary_action_menu`] reacts correctly to
    /// whatever the adjustment currently reports.
    ///
    /// A 5-row-tall viewport (`page_size = 5 * row_h`) scrolled to its
    /// 9th row's own band (`value = 8 * row_h`) puts row 2 — the one this
    /// test selects — eight rows above the visible viewport, the exact
    /// "selected row scrolled above the viewport" scenario this review
    /// finding names. Before [`HopWindow::ensure_selected_row_visible`]
    /// existed, `open_secondary_action_menu` left `value` untouched and
    /// anchored the panel to `self.indicator`, which `position_indicator`'s
    /// own `.max(0)` clamp had pinned to the viewport's literal top —
    /// wherever *that* pixel actually was, it was never row 2, since the
    /// scroll position itself never moved to bring row 2 there. This test
    /// pins the actual, load-bearing evidence that it now does.
    fn assert_ctrl_k_scrolls_a_row_selected_above_the_viewport_back_into_view() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.CtrlKScrollIntoView");
        window.present_with_token(None);
        let items: Vec<Item> = (0..10)
            .map(|n| test_item(n, &format!("item {n}")))
            .collect();
        model::replace(&window.store, items);
        window.selection.set_selected(2);

        let row_h = f64::from(*tokens::ROW_HEIGHT_PX);
        let page_size = row_h * 5.0;
        window.scrolled.vadjustment().configure(
            row_h * 8.0,
            0.0,
            row_h * 10.0,
            row_h,
            page_size,
            page_size,
        );

        window.dispatch_action(Action::SecondaryAction);

        assert_eq!(
            window.scrolled.vadjustment().value(),
            row_h * 2.0,
            "opening the panel for a row selected above the viewport must scroll the list so \
             that row's own top becomes the viewport's own top, not leave the scroll position \
             untouched and let the panel anchor to whatever row the indicator's own clamp left \
             it pinned over instead"
        );
        assert!(
            window.action_panel.popover().is_visible(),
            "the panel must still open once the selected row has been scrolled back into view"
        );

        println!("assert_ctrl_k_scrolls_a_row_selected_above_the_viewport_back_into_view passed");
    }

    /// Issue #254 AC2's own wiring slice: a right-click selects the row
    /// under the cursor and opens [`ActionPanel`] anchored at the exact
    /// click point, atomically — the sharpest edge this issue's own brief
    /// names. `850` is the first base this file has not already claimed
    /// (`400`, `550`, `700`), matching every other `#[test]` fn's own
    /// reasoning for why it needs one (`BroadwayServer::start`'s doc
    /// comment).
    ///
    /// Every assertion below drives [`HopWindow::open_secondary_action_menu_at`]
    /// directly — this file's own top doc comment already makes the case
    /// for calling a resolved function rather than synthesizing a real
    /// `GdkEvent` (unreachable via any public GDK4/GTK4 API in this
    /// environment); [`HopWindow::wire_row_right_click`]'s own doc comment
    /// is the (separately-checked, against GTK 4.14.5's real source) proof
    /// that a real secondary-button press reaches this exact function with
    /// `(x, y)` in `list_view`'s own coordinate space, which is what lets
    /// this test's own hand-picked `(x, y)` values stand in for a real
    /// click faithfully.
    #[test]
    fn right_click_selects_the_row_under_the_cursor_and_opens_the_panel_at_that_point() {
        run_under_broadway(
            "ui::window::tests::right_click_selects_the_row_under_the_cursor_and_opens_the_panel_at_that_point",
            850,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        assert_right_click_selects_the_row_under_the_cursor_and_opens_at_that_point();
        assert_right_click_with_no_row_under_the_cursor_does_nothing();
        assert_ctrl_k_after_a_right_click_clears_the_stale_pointing_to();

        println!("right-click action-panel assertions passed");
    }

    /// `ActionPanel::present`'s own "a popover needs a realized `gtk::Native`"
    /// requirement is why this, like every `SecondaryAction`-dispatching
    /// assertion elsewhere in this file, calls `window.present_with_token(None)`
    /// first.
    fn assert_right_click_selects_the_row_under_the_cursor_and_opens_at_that_point() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.RightClick");
        window.present_with_token(None);
        let item_a = test_item(1, "item A");
        let item_b = test_item(2, "item B");
        model::replace(&window.store, vec![item_a.clone(), item_b.clone()]);
        // Selected row is item_a, but the click below lands in item_b's own
        // row band — the exact divergence this issue's brief calls the
        // sharpest edge: a naive handler that opened the panel for
        // whatever was *already* selected, without moving the selection
        // first, would pass every other assertion in this file and fail
        // only this one.
        window.selection.set_selected(0);

        let row_h = *tokens::ROW_HEIGHT_PX;
        let click_x = 37.0;
        let click_y = f64::from(row_h) + 5.0; // inside row 1's own band
        window.open_secondary_action_menu_at(click_x, click_y);

        assert_eq!(
            window.selection.selected(),
            1,
            "a right-click on the second row must select that row, not leave whatever was \
             selected before the click"
        );
        assert!(
            window.action_panel.popover().is_visible(),
            "a right-click on a row with actions must open the panel"
        );
        let (has_point, rect) = window.action_panel.popover().pointing_to();
        assert!(
            has_point,
            "a right-click open must set a real pointing-to rectangle, not leave the popover \
             anchored generically to its parent"
        );
        assert_eq!(rect.x(), click_x.round() as i32);
        assert_eq!(rect.y(), click_y.round() as i32);

        // Choosing the panel's only action must run against item_b — the
        // row genuinely under the cursor — never item_a, which was
        // selected before the right-click landed.
        window.action_panel.handle_key(gdk::Key::Return);
        match cmd_rx
            .try_recv()
            .expect("choosing the action must send an Execute command")
        {
            IpcCommand::Execute { item_id, .. } => assert_eq!(
                item_id, item_b.id,
                "the action must run against the item under the cursor (item_b), never \
                 whatever was selected before the right-click (item_a)"
            ),
            other => panic!("expected Execute, got {other:?}"),
        }

        println!(
            "assert_right_click_selects_the_row_under_the_cursor_and_opens_at_that_point passed"
        );
    }

    /// A click landing outside every real row — here, the blank space
    /// below a one-row list's only row — must select nothing and open
    /// nothing, matching [`row_index_at_y`]'s own documented `None` for
    /// exactly this case.
    fn assert_right_click_with_no_row_under_the_cursor_does_nothing() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.RightClickEmptySpace");
        window.present_with_token(None);
        model::replace(&window.store, vec![test_item(1, "only row")]);
        window.selection.set_selected(0);

        let row_h = *tokens::ROW_HEIGHT_PX;
        window.open_secondary_action_menu_at(10.0, f64::from(row_h) + 10.0);

        assert_eq!(
            window.selection.selected(),
            0,
            "a right-click below the last real row must not change the selection"
        );
        assert!(
            !window.action_panel.popover().is_visible(),
            "a right-click with no row under the cursor must not open the panel"
        );

        println!("assert_right_click_with_no_row_under_the_cursor_does_nothing passed");
    }

    /// [`HopWindow::present_action_panel_for_selected`]'s own documented
    /// load-bearing detail: a ctrl-K open that follows a right-click must
    /// not inherit that click's `pointing_to` rectangle. Without this,
    /// [`ActionPanel`]'s single, built-once popover (never rebuilt per
    /// open) would keep anchoring a "general overlay" ctrl-K open to a
    /// stale cursor position from whichever row was right-clicked earlier.
    fn assert_ctrl_k_after_a_right_click_clears_the_stale_pointing_to() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.RightClickThenCtrlK");
        window.present_with_token(None);
        model::replace(&window.store, vec![test_item(1, "only row")]);
        window.selection.set_selected(0);

        window.open_secondary_action_menu_at(20.0, 5.0);
        assert!(
            window.action_panel.popover().is_visible(),
            "setup: the right-click must have opened the panel before this test's own \
             assertion means anything"
        );
        window.action_panel.dismiss();

        window.dispatch_action(Action::SecondaryAction);
        assert!(
            window.action_panel.popover().is_visible(),
            "setup: ctrl-K must reopen the panel for this test's own assertion to mean anything"
        );
        let (has_point, _rect) = window.action_panel.popover().pointing_to();
        assert!(
            !has_point,
            "ctrl-K opening after a previous right-click must clear that click's pointing-to \
             rectangle, not silently keep anchoring to the stale cursor point"
        );

        println!("assert_ctrl_k_after_a_right_click_clears_the_stale_pointing_to passed");
    }

    /// Issue #254 review, finding 4 (maintainer decision, 2026-08-23): the
    /// row's own overflow chevron (`ui::row::overflow_button_widget`)
    /// invokes `row.open-actions` with its row's own item id as target —
    /// [`HopWindow::open_action_panel_for_overflow`] is where that name
    /// resolves to real behavior, reusing
    /// [`HopWindow::present_action_panel_for_selected`] exactly the way
    /// [`HopWindow::open_secondary_action_menu_at`] (right-click) already
    /// does, per this review finding's own "do not grow a third copy of
    /// that logic" instruction. `1000` is the first base this file has not
    /// already claimed (`400`, `550`, `700`, `850`).
    #[test]
    fn overflow_chevron_selects_the_rows_item_and_opens_the_panel_anchored_there() {
        run_under_broadway(
            "ui::window::tests::overflow_chevron_selects_the_rows_item_and_opens_the_panel_anchored_there",
            1000,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        assert_overflow_chevron_selects_the_rows_item_and_opens_the_panel_at_that_point();
        assert_overflow_chevron_does_nothing_for_an_unknown_item_id();

        println!("overflow chevron wiring assertions passed");
    }

    /// The sharpest edge this review finding shares with the right-click
    /// path: the chevron's own target names *its* row's item, which is not
    /// necessarily whatever `self.selection` already points at — a naive
    /// handler that opened the panel for the current selection, ignoring
    /// the target entirely, would pass every ctrl-K assertion above and
    /// fail only this one.
    fn assert_overflow_chevron_selects_the_rows_item_and_opens_the_panel_at_that_point() {
        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.OverflowChevron");
        window.present_with_token(None);
        let item_a = test_item(1, "item A");
        // Three actions — one more than `ui::row::ROW_ACTION_ICON_CAP` —
        // so this is genuinely the item a real overflow chevron would show
        // on: `ui::row::resolve_overflow_button`'s own condition.
        let item_b = test_item_with_actions(2, "item B", &["open", "reveal", "copy"]);
        model::replace(&window.store, vec![item_a.clone(), item_b.clone()]);
        window.selection.set_selected(0); // item_a — deliberately not item_b

        window.open_action_panel_for_overflow(&item_b.id);

        assert_eq!(
            window.selection.selected(),
            1,
            "the overflow chevron must select the row it actually belongs to (item_b), not \
             leave whatever the results selection already happened to be (item_a)"
        );
        assert!(
            window.action_panel.popover().is_visible(),
            "the overflow chevron must open the panel for an item that has actions"
        );
        let (has_point, _rect) = window.action_panel.popover().pointing_to();
        assert!(
            has_point,
            "the overflow chevron must anchor the panel at a real point on that row, reusing \
             the same present_action_panel_for_selected(..., Some(pointing_to)) path a \
             right-click already uses — not ctrl-K's own pointing_to-free \"general overlay\" \
             anchor"
        );

        // Choosing an action must run against item_b — the row the
        // chevron actually belonged to — never item_a, which was selected
        // before this call ran.
        window.action_panel.handle_key(gdk::Key::Return);
        match cmd_rx
            .try_recv()
            .expect("choosing an action must send an Execute command")
        {
            IpcCommand::Execute { item_id, .. } => assert_eq!(
                item_id, item_b.id,
                "the action must run against the item the chevron belonged to (item_b), never \
                 whatever was selected before the chevron was clicked (item_a)"
            ),
            other => panic!("expected Execute, got {other:?}"),
        }

        println!(
            "assert_overflow_chevron_selects_the_rows_item_and_opens_the_panel_at_that_point \
             passed"
        );
    }

    /// An item id naming no row currently in the store — stale by the time
    /// the click is processed, in principle, though `ui::row`'s own
    /// recycling constraint should never actually produce one — must do
    /// nothing, not panic and not open a panel for whatever the selection
    /// already was.
    fn assert_overflow_chevron_does_nothing_for_an_unknown_item_id() {
        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.OverflowChevronUnknown");
        window.present_with_token(None);
        model::replace(&window.store, vec![test_item(1, "only row")]);
        window.selection.set_selected(0);

        let unknown_id = ItemId::new("test:does-not-exist").unwrap();
        window.open_action_panel_for_overflow(&unknown_id);

        assert_eq!(
            window.selection.selected(),
            0,
            "an unknown item id must not change the current selection"
        );
        assert!(
            !window.action_panel.popover().is_visible(),
            "an unknown item id must not open the panel"
        );

        println!("assert_overflow_chevron_does_nothing_for_an_unknown_item_id passed");
    }

    /// Issue #261's wiring decision, pinned across both run purposes under
    /// [`OverlayStrategy::SelfPositioned`] — the X11 strategy, the one whose
    /// row asks for close-on-focus-loss and the one the flaking CI arm
    /// actually ran under. `1150` is the next base this file has not
    /// already claimed (`400`, `550`, `700`, `850`, `1000`).
    ///
    /// The focus loss itself is simulated with `ObjectExt::notify("is-active")`,
    /// which fires `connect_is_active_notify`'s handler without needing a real
    /// X focus change broadway cannot produce: what this pins is what the
    /// handler does, not how GTK decides the property. Under a real server the
    /// same notify arrives whenever input focus leaves the window — exactly
    /// what Xvfb's WM-less map/unmap races can produce mid-capture.
    #[test]
    fn screenshot_window_never_wires_close_on_focus_loss() {
        run_under_broadway(
            "ui::window::tests::screenshot_window_never_wires_close_on_focus_loss",
            1150,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        let x11_strategy = crate::session::OverlayStrategy::SelfPositioned;
        assert!(x11_strategy.dismisses_on_focus_loss());

        // The `--screenshot` purpose: a background focus loss must leave the
        // window mapped for the capture.
        let (window, _cmd_rx) = build_configured_window(
            "dev.hop.WindowTest.ScreenshotFocusLoss",
            x11_strategy,
            RunPurpose::Screenshot,
        );
        window.present_with_token(None);
        assert!(
            window.window.is_visible(),
            "setup: the window must be visible once presented"
        );
        window.window.notify("is-active");
        assert!(
            window.window.is_visible(),
            "a --screenshot window must stay mapped through a focus loss — dismissing a \
             capture harness's only window hides it mid-capture (issue #261)"
        );

        // The interactive purpose: the same focus loss must still dismiss,
        // guarding this pin against drifting into "never wire dismissal at
        // all" — that would regress issue #232's documented behavior.
        let (window, _cmd_rx) = build_configured_window(
            "dev.hop.WindowTest.InteractiveFocusLoss",
            x11_strategy,
            RunPurpose::Interactive,
        );
        window.present_with_token(None);
        assert!(
            window.window.is_visible(),
            "setup: the window must be visible once presented"
        );
        window.window.notify("is-active");
        assert!(
            !window.window.is_visible(),
            "an interactive window must dismiss on focus loss (issue #232)"
        );

        println!("screenshot_window_never_wires_close_on_focus_loss passed");
    }
    /// The interactive app loop must request persisted recents from the real
    /// connection event path, not only from screenshot driving. This exercises
    /// the shared connection driver against a fresh widget: one authoritative
    /// `Connected` sends one empty query, duplicate `Connected` events do not
    /// resend it, `RecentItems` reaches the visible Empty frame, and a
    /// `Disconnected`/`Connected` reconnect gets exactly one fresh request.
    #[test]
    fn interactive_connection_requests_empty_query_once_and_surfaces_recents() {
        run_under_broadway(
            "ui::window::tests::interactive_connection_requests_empty_query_once_and_surfaces_recents",
            1450,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        let (window, cmd_rx) = build_test_window("dev.hop.WindowTest.InteractiveRecents");
        let mut connection = crate::app::InteractiveConnection::new("");

        connection.apply(&window, IpcEvent::Connected);
        match cmd_rx
            .try_recv()
            .expect("the first Connected event must request empty recents")
        {
            IpcCommand::Query(text) => assert!(text.is_empty()),
            other => panic!("expected Query(\"\") from first Connected, got {other:?}"),
        }
        connection.apply(&window, IpcEvent::Connected);
        // A duplicate Connected above must not enqueue another command.
        assert!(
            cmd_rx.try_recv().is_err(),
            "duplicate Connected events for one connection must not resend Query(\"\")"
        );

        let recent = test_item(9, "Persisted launch");
        connection.apply(
            &window,
            IpcEvent::RecentItems(vec![RecentItem {
                item: recent,
                launched_at_ms: 1,
            }]),
        );
        assert_eq!(
            window.state_items.borrow()[0].title.as_str(),
            "Persisted launch"
        );
        assert_eq!(window.state_header.text(), "Recent");

        connection.apply(&window, IpcEvent::Disconnected);
        connection.apply(&window, IpcEvent::Connected);
        match cmd_rx
            .try_recv()
            .expect("a reconnect must request one fresh empty recents query")
        {
            IpcCommand::Query(text) => assert!(text.is_empty()),
            other => panic!("expected Query(\"\") after reconnect, got {other:?}"),
        }
        assert!(
            cmd_rx.try_recv().is_err(),
            "one reconnect must still produce exactly one empty query"
        );

        println!("interactive connection requests one empty recents query per connection");
    }

    fn pending_provider_ids(surface: &gtk::Box) -> Vec<String> {
        let mut providers = Vec::new();
        let mut child = surface.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(label) = widget.downcast::<gtk::Label>()
                && label.has_css_class("hop-pending-attribution")
                && label.get_visible()
            {
                providers.push(label.text().to_string());
            }
        }
        providers
    }

    #[test]
    fn pending_provider_attribution_tracks_the_routed_selection_until_done() {
        run_under_broadway(
            "ui::window::tests::pending_provider_attribution_tracks_the_routed_selection_until_done",
            1600,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        let (window, _cmd_rx) = build_test_window("dev.hop.WindowTest.PendingProviders");
        window.set_query_text("par");
        window.apply_event(IpcEvent::Routed {
            mode: Mode::All,
            exclusive: false,
            marker_span: None,
            query_text: "par".to_string(),
            pending_providers: vec![
                "calculator".to_string(),
                "files".to_string(),
                "zero".to_string(),
            ],
        });
        assert_eq!(
            pending_provider_ids(&window.pending_surface),
            vec!["calculator", "files", "zero"],
            "the skeleton rows must come from the daemon's real selection, not a fixed UI list"
        );
        let first_pending_row = window
            .pending_surface
            .first_child()
            .and_then(|child| child.next_sibling())
            .and_then(|child| child.downcast::<gtk::Box>().ok())
            .expect("pending attribution must be followed by its material row");
        let first_pending_bar = first_pending_row
            .first_child()
            .and_then(|child| child.downcast::<gtk::Box>().ok())
            .expect("pending material row must contain its first shimmer bar");
        assert!(
            first_pending_bar.has_css_class("hop-honesty"),
            "every provider shimmer bar must carry the honesty lock class"
        );
        assert!(
            first_pending_bar.has_css_class("hop-pending-bar"),
            "provider rows must expose the pending-bar selector"
        );

        let mut calculator = test_item(1, "Parity");
        calculator.provider = "calculator".to_string();
        window.apply_event(IpcEvent::Results(vec![calculator]));
        assert_eq!(
            pending_provider_ids(&window.pending_surface),
            vec!["files", "zero"],
            "a partial replacement list resolves only the provider it names"
        );

        window.apply_event(IpcEvent::Results(vec![]));
        assert_eq!(
            pending_provider_ids(&window.pending_surface),
            vec!["files", "zero"],
            "an empty replacement list cannot claim that a provider completed"
        );

        let mut files = test_item(2, "Manifest");
        files.provider = "files".to_string();
        window.apply_event(IpcEvent::Results(vec![files]));
        assert_eq!(pending_provider_ids(&window.pending_surface), vec!["zero"]);
        assert!(window.pending_surface.get_visible());

        window.apply_event(IpcEvent::QueryDone);
        assert!(!window.pending_surface.get_visible());
    }

    #[test]
    fn local_fallbacks_keep_the_full_query_and_cannot_capture_provider_rows() {
        run_under_broadway(
            "ui::window::tests::local_fallbacks_keep_the_full_query_and_cannot_capture_provider_rows",
            1750,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        let actions = Rc::new(FakeUserActionSink::default());
        let (window, cmd_rx) = build_test_window_with_action_sink(
            "dev.hop.WindowTest.LocalFallbacks",
            Rc::clone(&actions),
        );

        let full_query = format!("{} /?&", "x".repeat(MAX_TITLE));
        window.set_query_text(&full_query);
        window.apply_event(IpcEvent::QueryDone);
        assert!(
            !window.state_items.borrow()[1]
                .title
                .as_str()
                .contains(full_query.as_str()),
            "the visible fallback copy must truncate independently of its payload"
        );
        window.list_view.emit_by_name::<()>("activate", &[&1u32]);
        assert_eq!(actions.copies.borrow().as_slice(), [full_query.as_str()]);

        window.set_query_text("a b/c?&");
        window.apply_event(IpcEvent::QueryDone);
        window.list_view.emit_by_name::<()>("activate", &[&0u32]);
        assert_eq!(
            actions.uris.borrow().as_slice(),
            ["https://www.google.com/search?q=a%20b%2Fc%3F%26"]
        );
        while cmd_rx.try_recv().is_ok() {}

        let mut provider_item = test_item(3, "Provider-owned collision");
        provider_item.id = ItemId::new("hop:fallback-web-search").unwrap();
        provider_item.provider = "provider".to_string();
        window.apply_event(IpcEvent::Results(vec![provider_item]));
        window.list_view.emit_by_name::<()>("activate", &[&0u32]);
        match cmd_rx
            .try_recv()
            .expect("a provider row that collides with a fallback id must execute through hopd")
        {
            IpcCommand::Execute { item_id, action_id } => {
                assert_eq!(item_id.as_str(), "hop:fallback-web-search");
                assert_eq!(action_id.as_str(), "open");
            }
            other => panic!("expected daemon Execute for provider row, got {other:?}"),
        }
        assert_eq!(
            actions.uris.borrow().as_slice(),
            ["https://www.google.com/search?q=a%20b%2Fc%3F%26"],
            "provider-controlled ids must never invoke a stale local fallback"
        );

        window.apply_event(IpcEvent::Executed(ExecOutcome::OpenUrl(
            hop_protocol::OpenUrl::new("https://example.test/from-provider").unwrap(),
        )));
        assert_eq!(
            actions.uris.borrow().as_slice(),
            [
                "https://www.google.com/search?q=a%20b%2Fc%3F%26",
                "https://example.test/from-provider",
            ]
        );

        while cmd_rx.try_recv().is_ok() {}
        window.set_query_text("");
        while cmd_rx.try_recv().is_ok() {}
        window.apply_event(IpcEvent::Results(vec![]));
        window.list_view.emit_by_name::<()>("activate", &[&0u32]);
        assert!(
            cmd_rx.try_recv().is_err(),
            "the prefix helper is a non-action affordance, never an Execute"
        );
    }

    #[test]
    fn execute_outcomes_require_a_successful_user_activation() {
        run_under_broadway(
            "ui::window::tests::execute_outcomes_require_a_successful_user_activation",
            1850,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        let actions = Rc::new(FakeUserActionSink::default());
        let (window, cmd_rx) = build_test_window_with_action_sink(
            "dev.hop.WindowTest.ExecuteOutcomeGate",
            Rc::clone(&actions),
        );
        let outcome = || {
            ExecOutcome::OpenUrl(
                hop_protocol::OpenUrl::new("https://example.test/authorized").unwrap(),
            )
        };

        // A daemon outcome for the active query is not authorization by
        // itself; only a user-triggered Execute may open an external URI.
        window.set_query_text("authorized");
        while cmd_rx.try_recv().is_ok() {}
        window.apply_event(IpcEvent::Results(vec![test_item(1, "authorized")]));
        window.apply_event(IpcEvent::Executed(outcome()));
        assert!(actions.uris.borrow().is_empty());

        // One successful command send permits one matching outcome.
        window.list_view.emit_by_name::<()>("activate", &[&0u32]);
        assert!(matches!(cmd_rx.try_recv(), Ok(IpcCommand::Execute { .. })));
        window.apply_event(IpcEvent::Executed(outcome()));
        assert_eq!(
            actions.uris.borrow().as_slice(),
            ["https://example.test/authorized"]
        );

        // The permission is consumed; an extra outcome cannot launch again.
        window.apply_event(IpcEvent::Executed(outcome()));
        assert_eq!(
            actions.uris.borrow().as_slice(),
            ["https://example.test/authorized"]
        );

        // A new query clears an outstanding permission before its outcome.
        window.list_view.emit_by_name::<()>("activate", &[&0u32]);
        assert!(matches!(cmd_rx.try_recv(), Ok(IpcCommand::Execute { .. })));
        window.set_query_text("new-query");
        while cmd_rx.try_recv().is_ok() {}
        window.apply_event(IpcEvent::Executed(outcome()));
        assert_eq!(
            actions.uris.borrow().as_slice(),
            ["https://example.test/authorized"]
        );

        // Disconnect also clears an outstanding permission.
        window.set_query_text("authorized-again");
        while cmd_rx.try_recv().is_ok() {}
        window.apply_event(IpcEvent::Results(vec![test_item(2, "authorized again")]));
        window.list_view.emit_by_name::<()>("activate", &[&0u32]);
        assert!(matches!(cmd_rx.try_recv(), Ok(IpcCommand::Execute { .. })));
        window.apply_event(IpcEvent::Disconnected);
        window.apply_event(IpcEvent::Executed(outcome()));
        assert_eq!(
            actions.uris.borrow().as_slice(),
            ["https://example.test/authorized"]
        );

        // A closed command channel does not grant permission merely because
        // activation was attempted.
        let failed_actions = Rc::new(FakeUserActionSink::default());
        let (failed_window, failed_rx) = build_test_window_with_action_sink(
            "dev.hop.WindowTest.ExecuteOutcomeSendFailure",
            Rc::clone(&failed_actions),
        );
        drop(failed_rx);
        failed_window.set_query_text("closed");
        failed_window.apply_event(IpcEvent::Results(vec![test_item(3, "closed")]));
        failed_window
            .list_view
            .emit_by_name::<()>("activate", &[&0u32]);
        failed_window.apply_event(IpcEvent::Executed(outcome()));
        assert!(failed_actions.uris.borrow().is_empty());
    }

    /// Issue #258's approved frame contract. Keep the assertions at the
    /// widget boundary: this is where the six state choices become visible,
    /// rather than a string-only test of the event reducer.
    #[test]
    fn six_states_render_the_approved_material_frames() {
        run_under_broadway(
            "ui::window::tests::six_states_render_the_approved_material_frames",
            1300,
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");
        let display = gtk::gdk::Display::default().expect("Broadway provides a display");
        crate::style::install(&display);
        crate::style::install_locked(&display);

        let actions = Rc::new(FakeUserActionSink::default());
        let (window, cmd_rx) =
            build_test_window_with_action_sink("dev.hop.WindowTest.SixStates", Rc::clone(&actions));
        window.present_with_token(None);

        // Empty: a fresh learning store has no fake history; only the
        // always-available inline prefix cheatsheet is shown.
        window.set_query_text("");
        window.apply_event(IpcEvent::Results(vec![]));
        assert_eq!(window.state_header.text(), "Recent");
        assert_eq!(window.store.n_items(), 1);
        assert_eq!(
            window.state_items.borrow()[0].title.as_str(),
            "w windows · a apps · f files · = math · : emoji"
        );

        // A successful real launch becomes the next Empty-state recent with
        // a computed relative-time subtitle rather than a hard-coded row.
        window.set_query_text("launch");
        window.apply_event(IpcEvent::Results(vec![test_item(7, "Learned app")]));
        window.apply_event(IpcEvent::QueryDone);
        capture_six_state(&window, "results");
        window.apply_event(IpcEvent::Executed(ExecOutcome::Done));
        window.set_query_text("");
        window.apply_event(IpcEvent::RecentItems(vec![hop_protocol::RecentItem {
            item: test_item(7, "Learned app"),
            launched_at_ms: (SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_millis() as u64)
                .saturating_sub(7_200_000),
        }]));
        assert_eq!(window.state_items.borrow()[0].title.as_str(), "Learned app");
        assert!(
            window.state_items.borrow()[0]
                .subtitle
                .as_ref()
                .is_some_and(|subtitle| subtitle.as_str().contains("2h ago"))
        );
        let two_hours_ago = SystemTime::now()
            .checked_sub(Duration::from_secs(7_200))
            .expect("test clock supports subtracting two hours");
        assert_eq!(
            relative_subtitle("apps", two_hours_ago)
                .expect("bounded relative subtitle")
                .as_str(),
            "apps · 2h ago"
        );
        capture_six_state(&window, "empty");

        // Pending: provider attribution remains visible until that provider
        // answers, then its own material collapses while other providers can
        // continue resolving until QueryDone.
        window.set_query_text("par");
        window.apply_event(IpcEvent::Routed {
            mode: Mode::All,
            exclusive: false,
            marker_span: None,
            query_text: "par".to_string(),
            pending_providers: vec!["calculator".to_string(), "files".to_string()],
        });
        assert!(window.pending_surface.get_visible());
        capture_six_state(&window, "pending");
        assert!(window.pending_surface.has_css_class("hop-state-pending"));
        assert!(
            window
                .pending_surface
                .first_child()
                .and_then(|child| child.downcast::<gtk::Label>().ok())
                .is_some_and(|label| label.text().contains("calculator"))
        );
        let mut parity = test_item(1, "Parity");
        parity.provider = "calculator".to_string();
        window.apply_event(IpcEvent::Results(vec![parity]));
        assert!(
            !window
                .pending_surface
                .first_child()
                .is_some_and(|provider| provider.get_visible()),
            "a responding provider's attribution must collapse its pending material"
        );
        assert!(
            window
                .pending_surface
                .first_child()
                .and_then(|provider| provider.next_sibling())
                .is_some_and(|row| !row.get_visible())
        );
        assert!(window.pending_surface.get_visible());
        window.apply_event(IpcEvent::QueryDone);
        assert!(!window.pending_surface.get_visible());

        // No-results: both fallback handlers are ordinary selectable items.
        window.set_query_text("zzqq");
        window.apply_event(IpcEvent::Results(vec![]));
        window.apply_event(IpcEvent::QueryDone);
        assert_eq!(window.state_header.text(), "No local matches");
        assert_eq!(window.store.n_items(), 2);
        assert_eq!(
            window.state_items.borrow()[0].title.as_str(),
            "Search the web for “zzqq”"
        );
        assert_eq!(
            window.state_items.borrow()[1].title.as_str(),
            "Copy “zzqq” to clipboard"
        );
        capture_six_state(&window, "no-results");
        // A query that receives no Results frame still resolves to
        // No-results when its terminal QueryDone arrives.
        window.set_query_text("terminal-only");
        window.apply_event(IpcEvent::QueryDone);
        assert_eq!(window.state_header.text(), "No local matches");
        assert_eq!(
            window.state_items.borrow()[0].title.as_str(),
            "Search the web for “terminal-only”"
        );
        assert_eq!(window.selection.selected(), 0);
        while cmd_rx.try_recv().is_ok() {}
        window.list_view.emit_by_name::<()>("activate", &[&1u32]);
        assert!(
            cmd_rx.try_recv().is_err(),
            "frontend fallback activation must not reach the daemon"
        );
        assert_eq!(
            actions.copies.borrow().as_slice(),
            ["terminal-only"],
            "copy fallback must use the injected action sink in widget tests"
        );
        window.set_query_text("webonly");
        window.apply_event(IpcEvent::QueryDone);
        while cmd_rx.try_recv().is_ok() {}
        window.list_view.emit_by_name::<()>("activate", &[&0u32]);
        assert!(cmd_rx.try_recv().is_err());

        // Error: provider and reason are pinned below the real result list.
        window.apply_event(IpcEvent::Error(
            "weather failed — budget exceeded".to_string(),
        ));
        assert!(window.error_pin.get_visible());
        assert_eq!(
            window.error_title.text(),
            "weather failed — budget exceeded"
        );
        assert_eq!(
            window.error_subtitle.text(),
            "provider isolated; other results unaffected"
        );
        assert!(window.error_title.has_css_class("hop-honesty-text"));
        assert!(window.error_subtitle.has_css_class("hop-honesty-text"));
        assert!(
            window.error_pin.has_css_class("hop-honesty"),
            "the pinned provider error must use the honesty-critical surface"
        );
        capture_six_state(&window, "error");

        // Offline: cached results retain their rows and gain the honest
        // connection state plus per-row as-of treatment.

        window.apply_event(IpcEvent::Results(vec![test_item(2, "last launch")]));
        window.apply_event(IpcEvent::Disconnected);
        assert_eq!(window.state_header.text(), "Cached · daemon unreachable");
        assert!(window.list_view.has_css_class("hop-state-offline"));
        assert!(window.offline_indicator.widget.get_visible());
        let cached_row = row::build();
        row::bind(cached_row.upcast_ref(), &test_item(2, "last launch"), None);
        let cached_stamp = row::stamp_widget(&cached_row).expect("cached row stamp");
        assert!(
            cached_stamp
                .parent()
                .and_then(|metadata| metadata.parent())
                .is_some_and(|parent| parent == cached_row.clone().upcast::<gtk::Widget>()),
            "cached age metadata must be a trailing row child, not part of the text column"
        );
        assert!(cached_stamp.has_css_class("hop-honesty-stamp"));
        let first_stamp = cached_stamp.text().to_string();
        row::bind(cached_row.upcast_ref(), &test_item(2, "last launch"), None);
        assert_eq!(
            cached_stamp.text(),
            first_stamp,
            "the cache snapshot time must remain stable when a recycled row rebinds"
        );
        capture_six_state(&window, "offline");
        assert!(
            row::stamp_widget(&cached_row)
                .is_some_and(|stamp| stamp.get_visible() && stamp.text().starts_with("as of ")),
            "offline cached rows must expose a visible mono as-of timestamp"
        );

        // Reduced motion makes pending bars static dim rather than animated.
        let settings = gtk::Settings::default().expect("broadway settings");
        settings.set_gtk_enable_animations(false);
        window.set_query_text("par");
        assert!(
            window
                .pending_surface
                .has_css_class("hop-state-reduced-motion")
        );
        settings.set_gtk_enable_animations(true);

        println!("six approved material states render at the widget boundary");
    }
}
