//! Widget-level proof of issue #254's ctrl-K action panel
//! (`hop_gtk::ui::action_panel::ActionPanel`): every action listed, typed
//! text filtering the visible rows, arrow-key navigation clamped at both
//! ends, Enter reporting the selected action through the panel's own
//! callback, Escape reporting nothing, a zero-action item handled without
//! presenting an empty panel, and — the defect this file's own
//! `assert_present_defers_the_shown_class_until_the_widget_is_already_mapped`
//! guards — [`hop_gtk::ui::action_panel::PANEL_SHOWN_CLASS`] landing on the
//! panel widget only on a *later* main-loop turn than [`ActionPanel::present`]
//! itself, never synchronously inside it. See `action_panel.rs`'s own top
//! doc comment, "The state class the stylesheet needs for its fade", for
//! why: the class must land after the widget's first, unshown-opacity style
//! has already been computed while mapped, or `assets/stylesheet.css`'s
//! `.hop-action-panel.hop-action-panel-shown` transition has no earlier
//! value to interpolate from and the "fade" is really an instant snap that
//! merely parses as a transition.
//!
//! This file duplicates `tests/view_tree_renderer.rs`'s own broadway
//! harness shape (`BroadwayServer`, the `CHILD_MARKER` re-exec, one
//! `#[test]` function that re-execs itself and then calls
//! [`run_assertions`]) rather than sharing it — that file's own module doc
//! comment gives the reason this crate's other harness duplicates one
//! level down (`tests/headless_smoke.rs`'s own `DaemonProcess`): every
//! integration test file under `tests/` compiles as its own separate
//! crate, with no shared module unless routed through `tests/common`, and
//! nothing here needs anything else `view_tree_renderer.rs` has (in
//! particular, no icon fixtures — the action panel renders no icon at all,
//! so this file's child needs no `XDG_DATA_HOME` tempdir).
//!
//! See `view_tree_renderer.rs`'s own module doc comment for the fuller
//! argument (verified directly against this machine's GTK) for why
//! `gtk::init()` must run in a re-exec'd child with `GDK_BACKEND`/
//! `BROADWAY_DISPLAY` set on its own environment before it runs, rather
//! than in this process directly.

#![allow(clippy::unwrap_used)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};

use hop_gtk::ui::action_panel::{ActionPanel, PANEL_SHOWN_CLASS, action_of};
use hop_protocol::{Action, ActionId, ActionKind, Item, ItemId, ItemTitle, Kind};

/// Set on the re-exec'd child so it knows to run [`run_assertions`] in
/// process instead of spawning a second child — see this file's module doc.
const CHILD_MARKER: &str = "HOP_GTK_ACTION_PANEL_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop. See
/// `tests/view_tree_renderer.rs`'s own `BroadwayServer` for why the display
/// number is derived from this process's own pid (so parallel `cargo test`
/// runs of different test binaries do not collide on the same display) and
/// why the offset base differs between files that might otherwise run at
/// the same time.
struct BroadwayServer {
    child: Child,
}

impl BroadwayServer {
    fn start(base: u32) -> (Self, u32) {
        let display = base + (std::process::id() % 5000);
        let child = Command::new("gtk4-broadwayd")
            .arg(format!(":{display}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect(
                "failed to spawn gtk4-broadwayd — it ships in libgtk-4-bin, see \
                 tests/headless_smoke.rs's top doc comment for how this was diagnosed",
            );
        std::thread::sleep(Duration::from_millis(300));
        (BroadwayServer { child }, display)
    }
}

impl Drop for BroadwayServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn action_panel_lists_filters_navigates_and_reports() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_assertions();
        return;
    }

    // A different base than `view_tree_renderer.rs`'s `200` and
    // `headless_smoke.rs`'s own, so a full `cargo test --workspace` run
    // (which can schedule every integration test binary's `#[test]`
    // functions concurrently) cannot collide on the same broadway display
    // even if two of these processes land on the same pid-derived offset.
    let (broadway, display) = BroadwayServer::start(600);

    let output = Command::new(std::env::current_exe().unwrap())
        .env("GDK_BACKEND", "broadway")
        .env("BROADWAY_DISPLAY", format!(":{display}"))
        .env(CHILD_MARKER, "1")
        .arg("--exact")
        .arg("action_panel_lists_filters_navigates_and_reports")
        .arg("--nocapture")
        .output()
        .expect("failed to re-exec this test binary under the headless broadway display");

    drop(broadway);

    assert!(
        output.status.success(),
        "the headless child process failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The real assertions, run inside the re-exec'd child described above,
/// once `GDK_BACKEND=broadway` and `BROADWAY_DISPLAY` are already set on
/// its environment.
fn run_assertions() {
    gtk::init().expect("gtk init under the broadway display this process's environment selects");

    // A real, presented top-level window with a real child widget to anchor
    // the panel's popover to — `ActionPanel::present`'s own doc comment
    // explains why a `gtk::Popover` needs a widget that is actually part of
    // a realized `gtk::Native` before `popup()` does anything real.
    let window = gtk::Window::new();
    let anchor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    window.set_child(Some(&anchor));
    window.present();

    assert_every_action_is_listed_in_order(&anchor);
    assert_filter_narrows_by_case_insensitive_substring(&anchor);
    assert_filter_matching_nothing_leaves_nothing_runnable(&anchor);
    assert_arrow_keys_move_and_clamp_the_selection(&anchor);
    assert_enter_reports_the_selected_action_and_closes(&anchor);
    assert_escape_reports_nothing_and_closes(&anchor);
    assert_zero_action_item_does_not_open(&anchor);
    assert_present_defers_the_shown_class_until_the_widget_is_already_mapped(&anchor);

    println!("action panel assertions passed");
}

/// Records every [`ActionId`] an [`ActionPanel`]'s callback ever reports,
/// in order — the one thing every assertion below needs to read back after
/// driving the panel through [`ActionPanel::handle_key`].
#[derive(Clone, Default)]
struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<ActionId>>>);

impl Recorder {
    fn on_choose(&self) -> impl Fn(ActionId) + 'static {
        let recorded = self.0.clone();
        move |id| recorded.borrow_mut().push(id)
    }

    fn chosen(&self) -> Vec<ActionId> {
        self.0.borrow().clone()
    }
}

fn assert_every_action_is_listed_in_order(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = multi_action_item();

    let opened = panel.present(&item, anchor);
    assert!(opened, "an item with actions must open the panel");
    assert_eq!(
        panel.selection().n_items(),
        item.actions.len() as u32,
        "every action in item.actions must be listed, none omitted and none invented"
    );
    for (position, expected) in item.actions.iter().enumerate() {
        let object = panel
            .selection()
            .item(position as u32)
            .expect("a position under n_items() must resolve to an item");
        let action = action_of(&object);
        assert_eq!(
            action.id, expected.id,
            "row {position} must be item.actions[{position}], in the same order"
        );
    }

    println!("assert_every_action_is_listed_in_order passed");
}

fn assert_filter_narrows_by_case_insensitive_substring(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = multi_action_item();
    panel.present(&item, anchor);

    panel.entry().set_text("PATH");
    assert_eq!(
        panel.selection().n_items(),
        1,
        "a case-insensitive substring of exactly one label must narrow to that one row"
    );
    assert_eq!(
        panel.selection().selected(),
        0,
        "narrowing to a non-empty result must reselect the (new) first visible row, not \
         leave the selection wherever it was pointing before the filter ran"
    );
    let only = action_of(
        &panel
            .selection()
            .item(0)
            .expect("one row must remain after filtering"),
    );
    assert_eq!(only.label, "Copy Path");

    panel.entry().set_text("open");
    assert_eq!(
        panel.selection().n_items(),
        2,
        "\"open\" must match both \"Open\" and \"Open in New Window\", case-insensitively"
    );
    assert_eq!(panel.selection().selected(), 0);

    panel.entry().set_text("");
    assert_eq!(
        panel.selection().n_items(),
        item.actions.len() as u32,
        "clearing the filter must restore every action"
    );

    println!("assert_filter_narrows_by_case_insensitive_substring passed");
}

/// The sharp edge this issue's brief names directly: a filter matching
/// nothing must not leave a stale selection Enter would still run.
fn assert_filter_matching_nothing_leaves_nothing_runnable(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = multi_action_item();
    panel.present(&item, anchor);

    panel.entry().set_text("zzz-does-not-match-anything");
    assert_eq!(panel.selection().n_items(), 0, "no action must match");
    assert_eq!(
        panel.selection().selected(),
        gtk::INVALID_LIST_POSITION,
        "a filter matching nothing must clear the selection, not leave a stale index behind"
    );

    let propagation = panel.handle_key(gdk::Key::Return);
    assert!(
        recorder.chosen().is_empty(),
        "Enter over an empty filtered list must not report any action \
         (got {:?}) — this is the hazard where Enter would otherwise run \
         whatever the last real selection happened to be",
        recorder.chosen()
    );
    assert_eq!(
        propagation,
        glib::Propagation::Stop,
        "Return is still a key this panel claims, even when it has nothing to report"
    );

    println!("assert_filter_matching_nothing_leaves_nothing_runnable passed");
}

fn assert_arrow_keys_move_and_clamp_the_selection(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = multi_action_item();
    panel.present(&item, anchor);

    assert_eq!(
        panel.selection().selected(),
        0,
        "presenting a non-empty item must select its first action"
    );

    // Five Downs over three rows must clamp at the last row, not wrap and
    // not walk past it.
    for _ in 0..5 {
        panel.handle_key(gdk::Key::Down);
    }
    assert_eq!(
        panel.selection().selected(),
        2,
        "Down must clamp at the last row"
    );

    // Ten Ups over three rows must clamp at the first row.
    for _ in 0..10 {
        panel.handle_key(gdk::Key::Up);
    }
    assert_eq!(
        panel.selection().selected(),
        0,
        "Up must clamp at the first row"
    );

    println!("assert_arrow_keys_move_and_clamp_the_selection passed");
}

fn assert_enter_reports_the_selected_action_and_closes(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = multi_action_item();
    panel.present(&item, anchor);
    assert!(
        panel.popover().is_visible(),
        "present must open the popover"
    );

    panel.handle_key(gdk::Key::Down); // selects item.actions[1]
    panel.handle_key(gdk::Key::Return);

    assert_eq!(
        recorder.chosen(),
        vec![item.actions[1].id.clone()],
        "Enter must report exactly the currently selected action's id, once"
    );
    assert!(
        !panel.popover().is_visible(),
        "Enter must close the panel after reporting the chosen action"
    );

    println!("assert_enter_reports_the_selected_action_and_closes passed");
}

fn assert_escape_reports_nothing_and_closes(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = multi_action_item();
    panel.present(&item, anchor);
    panel.handle_key(gdk::Key::Down);

    panel.handle_key(gdk::Key::Escape);

    assert!(
        recorder.chosen().is_empty(),
        "Escape must never report an action"
    );
    assert!(
        !panel.popover().is_visible(),
        "Escape must dismiss the panel"
    );

    println!("assert_escape_reports_nothing_and_closes passed");
}

fn assert_zero_action_item_does_not_open(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = zero_action_item();

    let opened = panel.present(&item, anchor);

    assert!(!opened, "present must report that it did not open");
    assert!(
        !panel.popover().is_visible(),
        "an item with zero actions must not show an empty mystery panel"
    );
    assert_eq!(
        panel.selection().n_items(),
        0,
        "the list must stay empty rather than showing stale rows from a previous item"
    );

    // Enter/Escape/arrows must still be inert, not panic, against an
    // unopened, empty panel.
    panel.handle_key(gdk::Key::Down);
    panel.handle_key(gdk::Key::Return);
    assert!(recorder.chosen().is_empty());

    println!("assert_zero_action_item_does_not_open passed");
}

/// Guards the fix for the verified defect this issue's own follow-up filed:
/// [`PANEL_SHOWN_CLASS`] must land on the panel widget on a *later* turn of
/// the main loop than [`ActionPanel::present`] itself runs, never inside the
/// same call — see `action_panel.rs`'s own top doc comment, "The state class
/// the stylesheet needs for its fade", for the mechanism this proves.
///
/// A version of `present` that (re-)added the class synchronously — the
/// original, inert shape this test is written to catch — would make this
/// test's *first* assertion fail: the class would already be present the
/// instant `present` returns, with no earlier "mapped, but not yet shown"
/// turn for `assets/stylesheet.css`'s `.hop-action-panel.hop-action-panel-shown`
/// transition to interpolate away from. This is deliberately not a CSS-only
/// test (`tests/action_panel_material.rs` already owns proving the
/// stylesheet's own rule shape) — a resolved-stylesheet string check would
/// stay green even if this widget-side deferral were deleted, since the two
/// halves of the fix live in different crates entirely and neither alone
/// proves the other still holds.
fn assert_present_defers_the_shown_class_until_the_widget_is_already_mapped(anchor: &gtk::Box) {
    let recorder = Recorder::default();
    let panel = ActionPanel::new(recorder.on_choose());
    let item = multi_action_item();

    panel.present(&item, anchor);

    let panel_widget = panel
        .popover()
        .child()
        .expect("present must have set the popover's child to the .hop-action-panel container");
    assert!(
        !panel_widget.has_css_class(PANEL_SHOWN_CLASS),
        "present() must not add {PANEL_SHOWN_CLASS:?} synchronously — doing so would apply it \
         in the same turn the widget first becomes mapped, leaving no earlier \
         opacity:0 style for the stylesheet's open-fade transition to interpolate from, which \
         makes the 'fade' an instant, undetectable snap"
    );

    // Flush the main loop so `ActionPanel::present`'s deferred
    // `glib::idle_add_local_once` callback actually runs. Idle sources are
    // always ready (no timeout to wait out), so a handful of non-blocking
    // iterations is enough to drain it without this test needing to spin a
    // real `glib::MainLoop::run`.
    let context = glib::MainContext::default();
    for _ in 0..10 {
        context.iteration(false);
    }

    assert!(
        panel_widget.has_css_class(PANEL_SHOWN_CLASS),
        "present() must add {PANEL_SHOWN_CLASS:?} once the main loop has had a chance to run \
         the deferred idle callback — a fix that dropped the deferred re-add entirely (rather \
         than merely making it synchronous again) would fail here instead of at the assertion \
         above"
    );

    println!("assert_present_defers_the_shown_class_until_the_widget_is_already_mapped passed");
}

fn action(id: &str, kind: ActionKind, label: &str) -> Action {
    Action {
        id: ActionId::new(id).expect("test action id must pass ActionId's own rules"),
        kind,
        label: label.to_string(),
    }
}

/// Three actions: two whose labels both contain "open" and one whose label
/// contains "path" — built to exercise both the "matches many" and
/// "matches exactly one" filter cases with a single fixture.
fn multi_action_item() -> Item {
    Item {
        id: ItemId::new("test:multi").expect("test item id must pass ItemId's own rules"),
        kind: Kind::Action,
        title: ItemTitle::new("Test Item").expect("test title must pass ItemTitle's own rules"),
        subtitle: None,
        icon: None,
        actions: vec![
            action("open", ActionKind::Open, "Open"),
            action("open_new", ActionKind::Open, "Open in New Window"),
            action("copy_path", ActionKind::Copy, "Copy Path"),
        ],
        default_action: ActionId::new("open").expect("literal action id must parse"),
        copy_text: None,
        append_to_end: false,
        provider: "test".to_string(),
    }
}

/// An item with no actions at all — the case
/// `assert_zero_action_item_does_not_open` exercises.
fn zero_action_item() -> Item {
    Item {
        id: ItemId::new("test:zero").expect("test item id must pass ItemId's own rules"),
        kind: Kind::Action,
        title: ItemTitle::new("No Actions").expect("test title must pass ItemTitle's own rules"),
        subtitle: None,
        icon: None,
        actions: vec![],
        default_action: ActionId::new("open").expect("literal action id must parse"),
        copy_text: None,
        append_to_end: false,
        provider: "test".to_string(),
    }
}
