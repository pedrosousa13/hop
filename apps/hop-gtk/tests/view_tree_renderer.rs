//! Unit-level proof of issue #181's view-tree dispatch seam: `ui::view`'s
//! `Node` enum, the `gtk::Stack`-based dispatch container `setup` builds,
//! and the `bind`/`unbind` functions that select and populate its pages.
//!
//! This is a different proof than `tests/headless_smoke.rs`'s: that test
//! captures a rendered PNG and diffs two frames, which can show a title
//! painted on screen but cannot show *which widget instance* is on screen,
//! or *which named page* of a `gtk::Stack` is currently selected. Criterion
//! 5 (recycling) and D4's "same widget instance across two binds" claim are
//! about identity and structure, not pixels, so this file drives the actual
//! `gtk::Stack`/`gtk::ListItem` objects `ui::view` builds and inspects them
//! directly.
//!
//! # Why this file re-execs itself as a subprocess rather than calling
//! `gtk::init()` straight from `#[test]`
//!
//! Verified directly while writing this file: `gtk::init()`'s success is
//! decided entirely by GDK's backend/display auto-probe, which only ever
//! reads it from the process environment (`GDK_BACKEND`, `BROADWAY_DISPLAY`
//! — the same two `tests/headless_smoke.rs` sets on the `hop-gtk`
//! subprocess it spawns). `gdk::set_allowed_backends("broadway")` plus an
//! explicit `gdk::Display::open(Some(":N"))` can open a broadway connection
//! by function argument alone, with no environment variable involved — but
//! `gtk::init()` itself still refuses with "Failed to initialize GTK"
//! unless its own default-display resolution (env-only) succeeds, and once
//! it has refused, every widget constructor afterward panics with "GTK has
//! not been initialized", regardless of a display opened by hand
//! afterward. So `GDK_BACKEND`/`BROADWAY_DISPLAY` have to be set in *this*
//! process's own environment before `gtk::init()` runs — and the only sound
//! way to do that here is on a child process's environment:
//! `std::process::Command::env` sets a *child's* environment and needs no
//! `unsafe`, whereas mutating this process's own environment in place would
//! need `std::env::set_var`, which is an `unsafe fn` on this toolchain, and
//! this crate denies `unsafe_code` — including in tests, per this issue's
//! brief ("No new `unsafe`"). The `#[test]` function below re-execs
//! [`std::env::current_exe`] (this same test binary) with those two
//! variables set on the child via `Command::env`, filtered with `--exact`
//! down to just this one test and a marker variable so the child recognizes
//! it should run [`run_assertions`] directly instead of re-execing a
//! second time; the child's exit status becomes this test's own pass/fail.
#![allow(clippy::unwrap_used)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gtk::prelude::*;

use hop_gtk::ui::view::{self, Node};
use hop_protocol::{Action, ActionId, ActionKind, Item, ItemId, ItemTitle, Kind};

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_VIEW_TREE_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — the same shape as
/// `tests/headless_smoke.rs`'s own `BroadwayServer`, duplicated rather than
/// shared for the same reason that file's `DaemonProcess` duplicates
/// `hopd/tests/socket.rs`'s helper: each integration test file under
/// `tests/` compiles as its own separate crate, with no shared module
/// unless routed through `tests/common`, and this is the only piece this
/// file needs from it. Display number is derived from this process's own
/// pid, exactly as in `headless_smoke.rs`, so parallel `cargo test`
/// invocations of *this* file and of `headless_smoke.rs` do not collide on
/// the same broadway display.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 200 + (std::process::id() % 5000);
        let child = Command::new("gtk4-broadwayd")
            .arg(format!(":{display}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect(
                "failed to spawn gtk4-broadwayd — it ships in libgtk-4-bin \
                 (NOT `broadwayd` on $PATH, which on Debian/Ubuntu is \
                 libgtk-3-bin's incompatible GTK3 server; see \
                 headless_smoke.rs's top doc comment for how this was \
                 diagnosed)",
            );
        // Asynchronous socket creation — see `headless_smoke.rs`'s
        // `BroadwayServer::start` for why this is a fixed sleep rather than
        // a `Path::exists` poll (the socket lives in the abstract
        // namespace).
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

#[test]
fn setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_assertions();
        return;
    }

    let broadway = BroadwayServer::start();

    let output = Command::new(std::env::current_exe().unwrap())
        .env("GDK_BACKEND", "broadway")
        .env("BROADWAY_DISPLAY", format!(":{}", broadway.display))
        .env(CHILD_MARKER, "1")
        .arg("--exact")
        .arg("setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget")
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

/// The real assertions, run inside the re-exec'd child process described in
/// this file's module doc, once `GDK_BACKEND=broadway` and
/// `BROADWAY_DISPLAY` are already set in its environment.
fn run_assertions() {
    gtk::init().expect("gtk init under the broadway display this process's environment selects");

    // --- brief test 1: the slot's child after setup is the dispatch
    // container, not a bare label. Driven through the real factory
    // `ui::view::build` returns — the same one `ui::window::HopWindow`
    // wires into its `GtkListView` — and a manufactured `gtk::ListItem`
    // (GTK exposes no public constructor for the item a real list view
    // would hand `setup`, but `glib::Object::new` builds one all the same,
    // since GTK does not mark the type non-instantiable — only its `item`
    // property, bound internally by a real list view, is read-only). The
    // "setup" signal `connect_setup` installs a handler for is a real named
    // GObject signal (`SignalListItemFactory`'s `connect_setup` returns a
    // `SignalHandlerId`, which only backs an actual `g_signal_connect`), so
    // emitting it by hand exercises the exact closure production wires in,
    // not a copy of its logic.
    let factory = view::build();
    let list_item: gtk::ListItem = glib::Object::new();
    factory.emit_by_name::<()>("setup", &[&list_item]);

    let stack = list_item
        .child()
        .and_then(|widget| widget.downcast::<gtk::Stack>().ok())
        .expect(
            "setup must give the slot a gtk::Stack dispatch container, not a bare label — the \
             shape ui/row.rs built directly before this issue's seam",
        );

    // --- brief tests 2-4, at the level D4 of the plan names as the right
    // one: `ui::view::bind`/`unbind` directly against the stack `setup`
    // already built, not through a second real `gtk::ListItem` — a real
    // list item's `item` property has no public setter (GTK gives it one
    // only via a bound `GtkListView`), so there is no way to manufacture
    // "a list item bound to item X" from application code the way this
    // test could manufacture the item above. The dispatch function's own
    // signature is what makes that unreachable path a non-issue: `bind`
    // takes `&gtk::Stack`, not `&gtk::ListItem`, so there is no slot-level
    // `set_child` in scope for it to reach for even by mistake — see
    // `ui::view::bind`'s doc comment for why that shape was chosen deliberately.
    let item_a = test_item(1, "first result");
    let item_b = test_item(2, "second result");

    view::bind(&stack, &Node::Row(item_a.clone()));
    assert_eq!(
        stack.visible_child_name().as_deref(),
        Some("row"),
        "bind must select the Row page by name, not replace the slot's child"
    );
    let widget_after_first_bind = stack
        .visible_child()
        .expect("a page must be the stack's visible child once bind has run");
    let label_after_first_bind = widget_after_first_bind
        .downcast_ref::<gtk::Label>()
        .expect("the Row page's widget is the label ui/row.rs builds");
    assert_eq!(
        label_after_first_bind.text(),
        item_a.title.as_str(),
        "the rendered row must show the bound item's title"
    );

    view::bind(&stack, &Node::Row(item_b.clone()));
    let widget_after_second_bind = stack
        .visible_child()
        .expect("a page must still be the stack's visible child after the second bind");
    assert_eq!(
        widget_after_second_bind, widget_after_first_bind,
        "recycling: binding a slot to a second item must reuse the same widget instance, \
         never destroy and rebuild it — acceptance criterion 5"
    );
    let label_after_second_bind = widget_after_second_bind
        .downcast_ref::<gtk::Label>()
        .unwrap();
    assert_eq!(
        label_after_second_bind.text(),
        item_b.title.as_str(),
        "the same recycled label must now show the second item's title"
    );

    // Pins the fix that came out of review: `unbind` takes the `&Node` it
    // is clearing, dispatching on it exactly like `bind` does, rather than
    // reaching for a hardcoded page name — the earlier shape assumed GTK's
    // `unbind` signal carries no item to build a `Node` from, which
    // `ui::view::unbind`'s doc comment now documents as having been wrong
    // (checked against GTK's own `SignalListItemFactory::unbind`
    // documentation). Passing `item_b` here — the item most recently
    // bound, exactly what `list_item.item()` would still return inside a
    // real `connect_unbind` handler at this point — is what this test can
    // do to stand in for that handler's own call.
    view::unbind(&stack, &Node::Row(item_b.clone()));
    assert_eq!(
        label_after_second_bind.text(),
        "",
        "unbind must clear the row's text, exactly as ui/row.rs's connect_unbind did before \
         this refactor"
    );

    println!("view-tree dispatch and recycling assertions passed");
}

/// A tiny [`Item`]; `n` differentiates ids so a future assertion could tell
/// two instances apart, matching the shape `crates/hopd/tests/lifecycle.rs`'s
/// own `item` helper uses for the same reason.
fn test_item(n: usize, title: &str) -> Item {
    Item {
        id: ItemId::new(format!("test:{n}")).unwrap(),
        kind: Kind::Action,
        title: ItemTitle::new(title).unwrap(),
        subtitle: None,
        icon: None,
        actions: vec![Action {
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
