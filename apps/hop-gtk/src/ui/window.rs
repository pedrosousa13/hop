//! The pre-built launcher window: constructed once at startup, presented on
//! demand rather than rebuilt per presentation — acceptance criterion 2.
//! See `app`'s module doc for what "on demand" resolves to for this issue.
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
use crate::tokens;
use crate::ui::{model, view};

/// The pre-built window and everything it owns. `Clone` and cheap to clone —
/// every field is a GTK/glib reference-counted handle — so `app`'s
/// `glib::spawn_future_local` event loop can hold one across `.await`
/// points without fighting the borrow checker.
#[derive(Clone)]
pub struct HopWindow {
    pub window: adw::ApplicationWindow,
    entry: gtk::Entry,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    indicator: gtk::Widget,
    scrolled: gtk::ScrolledWindow,
    status: gtk::Label,
    cmd_tx: CommandSender,
}

impl HopWindow {
    /// Builds the window once. Not shown — `app`'s `activate` handler is
    /// what calls [`HopWindow::present_with_token`], per the
    /// pre-built-hidden-window requirement.
    pub fn build(app: &adw::Application, cmd_tx: CommandSender) -> Self {
        let (window_w, window_h) = *tokens::WINDOW_SIZE_PX;
        let row_h = *tokens::ROW_HEIGHT_PX;

        let entry = gtk::Entry::builder()
            .placeholder_text("Type to search")
            .build();

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
        content.append(&entry);
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
            store,
            selection,
            indicator: indicator.upcast(),
            scrolled,
            status,
            cmd_tx,
        };

        hop_window.wire_entry();
        hop_window.wire_selection_indicator();

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

        // Enter runs the current selection's default action — acceptance
        // criterion 6.
        let cmd_tx = self.cmd_tx.clone();
        let selection = self.selection.clone();
        self.entry.connect_activate(move |_| {
            activate_selected(&selection, &cmd_tx);
        });
    }

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
            IpcEvent::Routed { .. } => {
                // No mode label in this slice — issue #184's seam. See this
                // crate's top-level doc comment's out-of-scope list.
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
/// no-results list, where Enter has nothing to run.
fn activate_selected(selection: &gtk::SingleSelection, cmd_tx: &CommandSender) {
    let Some(object) = selection.selected_item() else {
        return;
    };
    let item: Item = model::item_of(&object);
    cmd_tx.send(IpcCommand::Execute {
        item_id: item.id,
        action_id: item.default_action,
    });
}
