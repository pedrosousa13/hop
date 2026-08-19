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

use gio::prelude::*;
use gtk::prelude::*;

use hop_protocol::{ExecOutcome, Item};

use crate::ipc::{CommandSender, IpcCommand, IpcEvent};
use crate::keymap::{Action, Keymap};
use crate::tokens;
use crate::ui::{marker_highlight, mode_label, model, view};

/// Rows moved per [`Action::PageUp`]/[`Action::PageDown`]. A fixed step
/// rather than one derived from the scrolled window's currently allocated
/// height (which would need a layout query at press time, and would change
/// with every resize) — five is a plain, easy-to-reason-about jump that
/// gives Page Up/Page Down a distinct feel from Up/Down without pretending
/// to track "one screenful", a refinement nothing in §8 asks this issue to
/// build.
const PAGE_STEP: i64 = 5;

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
    // `build` still holds at that point — hence the `cfg_attr` below: a
    // production build (where only `build`'s own local variable is ever
    // touched) would otherwise trip `dead_code` on a field that a `#[cfg(test)]`
    // build genuinely does read.
    #[cfg_attr(not(test), allow(dead_code))]
    list_view: gtk::ListView,
    indicator: gtk::Widget,
    scrolled: gtk::ScrolledWindow,
    status: gtk::Label,
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
    pub fn build(app: &adw::Application, cmd_tx: CommandSender, keymap: Keymap) -> Self {
        let (window_w, window_h) = *tokens::WINDOW_SIZE_PX;
        let row_h = *tokens::ROW_HEIGHT_PX;

        let entry = gtk::Entry::builder()
            .placeholder_text("Type to search")
            .build();

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

        let factory = view::build();
        let list_view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        // Single click activates a row rather than GTK's own double-click
        // default — the launcher convention §8 names ("mouse click still
        // activates a row") reads as one click, matching how a result list
        // in a launcher behaves everywhere else this UI takes cues from,
        // and D5 of the plan this issue implements found no
        // `connect_activate` anywhere in this crate before this change: this
        // is new wiring, not a preserved default.
        list_view.set_single_click_activate(true);
        wire_list_activation(&list_view, &selection, &cmd_tx);

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

        let status = gtk::Label::new(None);
        status.add_css_class("hop-status");
        status.set_xalign(0.0);
        status.set_visible(false);
        status.set_wrap(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&entry_overlay);
        content.append(&status);
        content.append(&overlay);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(window_w)
            .default_height(window_h)
            .content(&content)
            .hide_on_close(true)
            .build();

        crate::layer_shell::apply_or_fallback(&window);

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
            cmd_tx,
        };

        hop_window.wire_entry();
        hop_window.wire_selection_indicator();
        hop_window.wire_keyboard(keymap);

        hop_window
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
        self.entry.connect_changed(move |entry| {
            cmd_tx.send(IpcCommand::Query(entry.text().to_string()));
        });

        // Enter running the selection's default action (acceptance
        // criterion 6) is wired in `wire_keyboard` now, through the keymap —
        // see this module's top doc comment, "Key dispatch is keymap-driven,
        // not hardcoded", for why `GtkEntry`'s own `activate` signal is no
        // longer connected to anything here.
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
    /// [`Action::SecondaryAction`] and [`Action::CompletePrefix`] are
    /// dispatched here too, to [`HopWindow::open_secondary_action_menu`] and
    /// [`HopWindow::complete_prefix`] — both real, reachable handlers that do
    /// nothing visible yet. See each one's own doc comment for why (D4):
    /// the binding must exist now so a later issue only has to write the
    /// feature, not also retrofit the keymap plumbing around it.
    fn dispatch_action(&self, action: Action) {
        match action {
            Action::NavigateUp => self.move_selection(-1),
            Action::NavigateDown => self.move_selection(1),
            Action::PageUp => self.move_selection(-PAGE_STEP),
            Action::PageDown => self.move_selection(PAGE_STEP),
            Action::Home => self.select_first(),
            Action::End => self.select_last(),
            Action::Activate => activate_selected(&self.selection, &self.cmd_tx),
            Action::SecondaryAction => self.open_secondary_action_menu(),
            Action::CompletePrefix => self.complete_prefix(),
            // `hide_on_close(true)` (set in `build`) is what makes `close()`
            // hide the pre-built window rather than destroy it — the same
            // "never rebuilt" property `present_with_token` relies on to
            // `present()` this exact window again later.
            Action::Dismiss => self.window.close(),
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

    /// [`Action::SecondaryAction`]'s handler — bound in the keymap and
    /// reached from [`HopWindow::dispatch_action`], and empty on purpose.
    /// §8 names a secondary-action menu key as one of the launcher's default
    /// bindings, but `hop-gtk` has no secondary-action menu to open: no
    /// widget, no per-item action list surfaced anywhere in this crate
    /// today. D4 of the plan this issue implements requires the *binding*
    /// to exist regardless — leaving it out of the keymap now would be
    /// exactly the hardcoded-handler retrofit this issue exists to prevent
    /// — while ruling out building the menu itself here: that is its own
    /// slice, scoped separately so it can pick its own widget and
    /// interaction model without this issue's binding-only brief
    /// constraining it. A future issue building that menu starts by giving
    /// this function a body.
    fn open_secondary_action_menu(&self) {}

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
                self.status.set_visible(false);
            }
            IpcEvent::ConnectFailed(reason) => {
                self.set_status(&format!("Can't reach hopd: {reason}"));
            }
            IpcEvent::Disconnected => {
                self.set_status("Lost connection to hopd, reconnecting…");
            }
            IpcEvent::Routed {
                mode,
                exclusive,
                marker_span,
                query_text,
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
            }
            IpcEvent::Results(items) => {
                let has_results = !items.is_empty();
                model::replace(&self.store, items);
                if has_results {
                    self.status.set_visible(false);
                    self.selection.set_selected(0);
                } else {
                    self.selection.set_selected(gtk::INVALID_LIST_POSITION);
                }
            }
            IpcEvent::QueryDone => {}
            IpcEvent::Executed(outcome) => self.handle_outcome(outcome),
            IpcEvent::Error(message) => self.set_status(&message),
        }
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
        self.status.set_visible(true);
    }

    /// Carries out an [`ExecOutcome`] the daemon reported for the last
    /// `Execute` — the two variants that ask the *client* to act
    /// (`CopyText`, `OpenUrl`; see that type's doc comment in
    /// `hop_protocol::wire`). `Done` means the provider already performed
    /// the action itself (an app launch, a window focus) and there is
    /// nothing left for this process to do.
    fn handle_outcome(&self, outcome: ExecOutcome) {
        match outcome {
            ExecOutcome::Done => {}
            ExecOutcome::CopyText(text) => {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(text.as_str());
                }
            }
            ExecOutcome::OpenUrl(url) => {
                if let Err(err) = gtk::gio::AppInfo::launch_default_for_uri(
                    url.as_str(),
                    gtk::gio::AppLaunchContext::NONE,
                ) {
                    self.set_status(&format!("couldn't open {}: {err}", url.as_str()));
                }
            }
        }
    }
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

/// Sends an [`IpcCommand::Execute`] for the currently selected item's
/// default action, or does nothing if no item is selected — an empty or
/// no-results list, where Enter has nothing to run. [`Action::Activate`]'s
/// own handler, reached from [`HopWindow::dispatch_action`]; mouse-click
/// activation ([`wire_list_activation`]) reaches [`activate_at`] directly
/// instead, since a click already carries the row's position and has no
/// need to re-derive it from `selection.selected()`.
fn activate_selected(selection: &gtk::SingleSelection, cmd_tx: &CommandSender) {
    let selected = selection.selected();
    if selected == gtk::INVALID_LIST_POSITION {
        return;
    }
    activate_at(selection, cmd_tx, selected);
}

/// Sends an [`IpcCommand::Execute`] for the item at `position` in
/// `selection`'s underlying model, or does nothing if `position` names no
/// item — shared by [`activate_selected`] (which resolves `position` from
/// the model's own current selection) and [`wire_list_activation`]'s mouse
/// click handler (which already has the position GTK's `activate` signal
/// reported), so both routes run the identical "turn a chosen item into an
/// `Execute`" lookup instead of growing two copies of it.
fn activate_at(selection: &gtk::SingleSelection, cmd_tx: &CommandSender, position: u32) {
    let Some(object) = selection.item(position) else {
        return;
    };
    let item: Item = model::item_of(&object);
    cmd_tx.send(IpcCommand::Execute {
        item_id: item.id,
        action_id: item.default_action,
    });
}

/// Wires `list_view`'s own `activate` signal — GTK's name for "the user
/// chose this row", fired on a single click here (`set_single_click_activate(true)`
/// in `build`) or on Enter while a row itself has focus — to the same
/// default-action `Execute` [`Action::Activate`] sends. D5 of the plan this
/// issue implements: no `connect_activate` existed anywhere in this crate
/// before this change, confirmed by grepping the crate before writing this
/// function, so this is new wiring closing a real gap, not a preserved
/// default — §8 already named mouse-click activation as "an extension gap
/// hop deliberately closes".
fn wire_list_activation(
    list_view: &gtk::ListView,
    selection: &gtk::SingleSelection,
    cmd_tx: &CommandSender,
) {
    let selection = selection.clone();
    let cmd_tx = cmd_tx.clone();
    list_view.connect_activate(move |_list_view, position| {
        activate_at(&selection, &cmd_tx, position);
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

    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use hop_protocol::{
        Action as WireAction, ActionId, ActionKind, ItemId, ItemTitle, Kind, MarkerSpan, Mode,
    };

    use super::*;
    use crate::keymap::{Action, Keymap};

    /// Set on the re-exec'd child so it knows to run the real assertions
    /// in-process instead of spawning a second child.
    const CHILD_MARKER: &str = "HOP_GTK_WINDOW_TEST_CHILD";

    /// A spawned `gtk4-broadwayd`, killed on drop. Display number derived
    /// from this process's own pid, offset from `tests/headless_smoke.rs`'s
    /// and `tests/view_tree_renderer.rs`'s own ranges so parallel `cargo
    /// test` runs of all three files do not collide on the same display.
    struct BroadwayServer {
        child: Child,
        display: u32,
    }

    impl BroadwayServer {
        fn start() -> Self {
            let display = 400 + (std::process::id() % 5000);
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

    /// Re-execs this test binary under a headless `broadway` display and
    /// asserts the child's real-assertion run succeeded — see this module's
    /// doc comment.
    fn run_under_broadway(test_name: &str) {
        if std::env::var_os(CHILD_MARKER).is_some() {
            // Already the re-exec'd child; the `#[test]` fn that called this
            // has already run its real assertions before reaching here in
            // that case — see each test fn below.
            return;
        }

        let broadway = BroadwayServer::start();
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
        let window = HopWindow::build(&app, cmd_tx, Keymap::defaults());
        (window, cmd_rx)
    }

    // Both GTK-dependent checks below run from *one* `#[test]` function,
    // sharing one re-exec'd child and one `BroadwayServer` — not two. Each
    // `BroadwayServer::start` derives its display number from
    // `std::process::id()`, which is identical for every `#[test]` fn in
    // this one binary (cargo runs them as threads within a single process,
    // not separate processes), so two independently re-exec'd tests would
    // race to bind the *same* broadway display and one would fail with
    // "Unable to write to server" — verified directly while writing this
    // suite: splitting these into two `#[test]` fns intermittently failed
    // exactly that way under `cargo test --workspace`'s default parallelism.
    // One test, one display, both checks.
    #[test]
    fn keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window() {
        run_under_broadway(
            "ui::window::tests::keyboard_and_mouse_dispatch_use_the_keymap_and_the_real_window",
        );
        if std::env::var_os(CHILD_MARKER).is_none() {
            return;
        }
        gtk::init()
            .expect("gtk init under the broadway display this process's environment selects");

        assert_dispatch_action_moves_selection_and_activates();
        assert_mouse_click_activates_the_clicked_row();
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

        // The two behaviorless actions (D4) must be callable without
        // panicking and must produce no `IpcCommand` — they do nothing
        // visible yet, on purpose; see `HopWindow::open_secondary_action_menu`
        // and `HopWindow::complete_prefix`.
        window.dispatch_action(Action::SecondaryAction);
        window.dispatch_action(Action::CompletePrefix);
        assert!(
            cmd_rx.try_recv().is_err(),
            "the two behaviorless actions must not send any IpcCommand"
        );

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
        });

        assert!(
            entry_highlighted_range(&window.entry).is_none(),
            "a span bound to superseded text must never be applied to newer text, \
             even though its offsets also happen to be valid against the new text"
        );

        println!("stale marker span guard assertions passed");
    }
}
