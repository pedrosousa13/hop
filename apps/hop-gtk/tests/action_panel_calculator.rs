//! Issue #254 review, finding 2: acceptance criterion 3 — "Calculator
//! result exposes Copy (and any provider-declared extras) through the
//! panel" — was asserted nowhere. `crates/hopd/src/calculator.rs` ships
//! exactly one [`ActionKind::Copy`] action per result, and
//! `ActionPanel::present`/`populate` are generic over `item.actions`, so
//! this already worked — but every panel test up to this point (`tests/
//! action_panel.rs`, `ui::window`'s own ctrl-K/right-click tests) drove
//! synthetic fixtures built by hand in the test file itself, never a real
//! calculator-shaped [`Item`]. An acceptance criterion that names a
//! specific provider by name deserves a test that actually drives that
//! provider's own output through the panel, not a fixture merely *shaped*
//! like it.
//!
//! # Why a real `hopd`, not `hopd::calculator::build_item` called directly
//!
//! `hopd::calculator::build_item` is `pub(crate)` inside the `hopd` crate
//! — reachable from `hopd`'s own tests, not from `hop-gtk`'s, and widening
//! it to `pub` for one test elsewhere is a visibility change to a crate
//! this issue's ownership does not extend to. `hop-gtk`'s own
//! `[dev-dependencies]` already lists `hopd` (see that entry's own
//! comment: `tests/headless_smoke.rs` needs the built `hopd` *binary* as
//! its own sibling executable, not any Rust item from the crate) — this
//! file spawns that same real binary and talks to it over the real IPC
//! socket, the identical shape `tests/exec_round_trip.rs` already
//! establishes for proving a different acceptance criterion end to end.
//! That is "ideally built from hopd's own calculator output" in the most
//! literal sense available without a cross-crate visibility change this
//! issue does not own: the [`Item`] this file drives through
//! [`ActionPanel`] is decoded from bytes a real `hopd` process actually
//! sent over its own socket, in response to a real `IpcCommand::Query`.
//!
//! # Combining two harnesses this workspace has never combined in one file
//! before
//!
//! `tests/action_panel.rs` re-execs this binary under `GDK_BACKEND=broadway`
//! to get a real, mapped `gtk::Window` a `gtk::Popover` can anchor to.
//! `tests/exec_round_trip.rs` spawns a real `hopd` and drives it purely
//! over `hop_gtk::ipc`, no GTK involved at all. This file needs both in the
//! same process: a real calculator [`Item`], and a real popover to present
//! it in. The daemon is spawned, queried, and killed entirely *before*
//! `gtk::init()` ever runs — `hop_gtk::ipc::spawn`'s own background thread
//! and this file's own short-lived `tokio::runtime::Builder::
//! new_current_thread` runtime are independent of GTK's main context
//! (`async_channel`, not `glib::MainContext`, is what `ipc::spawn` uses to
//! hand events back — see that module's own doc comment), so blocking on
//! them here is no different from `tests/exec_round_trip.rs` doing the
//! identical blocking with no GTK in the process at all.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gtk::gdk;
use gtk::prelude::*;

use hop_gtk::ipc::{self, IpcCommand, IpcEvent};
use hop_gtk::ui::action_panel::{ActionPanel, action_of};
use hop_protocol::{ActionKind, Item, Kind};

/// Set on the re-exec'd child so it knows to run the real assertions
/// in-process instead of spawning a second child — `tests/action_panel.rs`'s
/// own precedent.
const CHILD_MARKER: &str = "HOP_GTK_ACTION_PANEL_CALCULATOR_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — `tests/action_panel.rs`'s
/// own `BroadwayServer`, duplicated here rather than shared: every
/// integration test file under `tests/` compiles as its own separate
/// crate, with no shared module unless routed through `tests/common`
/// (`tests/action_panel.rs`'s own module doc comment gives the fuller
/// account of why this workspace accepts that duplication rather than
/// building a shared harness crate for it).
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

/// A spawned real `hopd`, killed on drop — `tests/exec_round_trip.rs`'s own
/// `DaemonProcess`, duplicated here for the identical "every integration
/// test file owns its own harness" reason [`BroadwayServer`]'s doc comment
/// gives.
struct DaemonProcess {
    child: Child,
    socket_path: PathBuf,
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hopd_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_hop-gtk"));
    path.set_file_name(if cfg!(windows) { "hopd.exe" } else { "hopd" });
    path
}

fn spawn_daemon(runtime_dir: &Path) -> DaemonProcess {
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-config-home")).unwrap();

    let child = Command::new(hopd_path())
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", runtime_dir.join("isolated-home"))
        .env("XDG_DATA_HOME", runtime_dir.join("isolated-xdg-data-home"))
        .env("XDG_DATA_DIRS", "")
        .env(
            "XDG_CONFIG_HOME",
            runtime_dir.join("isolated-xdg-config-home"),
        )
        .env(
            "XDG_STATE_HOME",
            runtime_dir.join("isolated-xdg-state-home"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn hopd");

    let socket_path = runtime_dir.join("hop").join("hopd.sock");
    let process = DaemonProcess { child, socket_path };
    for _ in 0..50 {
        if process.socket_path.exists() {
            return process;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("hopd did not create its socket in time");
}

/// Queries a real, freshly spawned `hopd` for `term` and returns the one
/// [`Item`] it answers with — `tests/exec_round_trip.rs`'s own
/// `query_then_execute_the_default_action_round_trips_against_real_hopd`
/// drives the identical query/response pair; this function only stops
/// short of also driving `Execute`, since this file's own job is the panel,
/// not the IPC round trip that file already owns proving.
fn query_calculator_item(runtime_dir: &Path, term: &str) -> Item {
    let daemon = spawn_daemon(runtime_dir);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (cmd_tx, evt_rx) = ipc::spawn(daemon.socket_path.clone());

    let item = runtime.block_on(async {
        loop {
            match evt_rx.recv().await.expect("ipc thread exited early") {
                IpcEvent::Connected => break,
                IpcEvent::ConnectFailed(reason) => panic!("connect failed: {reason}"),
                _ => {}
            }
        }

        cmd_tx.send(IpcCommand::Query(term.to_string()));

        let mut items = Vec::new();
        loop {
            match evt_rx.recv().await.expect("ipc thread exited early") {
                IpcEvent::Results(new_items) => items = new_items,
                IpcEvent::QueryDone => break,
                IpcEvent::Error(msg) => panic!("query failed: {msg}"),
                _ => {}
            }
        }
        items
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the calculator provider must answer {term:?} with one item"))
    });

    // `daemon` is dropped here, at the end of this function, killing the
    // real process — `query_calculator_item`'s caller only ever needs the
    // `Item` this returns, and holding a real `hopd` alive any longer than
    // the query itself takes buys nothing: `ActionPanel` never touches
    // `ipc` at all (this module's own top doc comment; `ui::action_panel`'s
    // own "Scope: this is the widget, not the wiring").
    item
}

#[test]
fn calculator_result_exposes_copy_through_the_action_panel() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_assertions();
        return;
    }

    // A different base than `tests/action_panel.rs`'s own `600` and every
    // other broadway-gated file's, so a full `cargo test --workspace` run
    // cannot collide on the same display even if two of these processes
    // land on the same pid-derived offset.
    let (broadway, display) = BroadwayServer::start(1100);

    let output = Command::new(std::env::current_exe().unwrap())
        .env("GDK_BACKEND", "broadway")
        .env("BROADWAY_DISPLAY", format!(":{display}"))
        .env(CHILD_MARKER, "1")
        .arg("--exact")
        .arg("calculator_result_exposes_copy_through_the_action_panel")
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
/// once `GDK_BACKEND=broadway`/`BROADWAY_DISPLAY` are already set on its
/// own environment. Queries the real `hopd` *before* `gtk::init()` runs —
/// see this file's own top doc comment, "Combining two harnesses", for why
/// that ordering is safe.
fn run_assertions() {
    let runtime_dir = tempfile::tempdir().expect("failed to create a temp runtime dir");
    let item = query_calculator_item(runtime_dir.path(), "2+2");

    // Genuinely calculator-shaped, not merely a fixture that happens to
    // look like one — this is what "at minimum a fixture asserted to
    // match its real shape" (this review finding's own fallback, not
    // needed here) would have had to assert by hand instead.
    assert_eq!(
        item.kind,
        Kind::Calculator,
        "a real hopd must answer a math-looking query with a Kind::Calculator item"
    );
    assert_eq!(
        item.actions.len(),
        1,
        "the calculator provider's own module doc comment promises exactly one action per \
         result — if a future change ever adds a provider-declared extra, this count (and this \
         test) must grow with it, not silently stay green over one"
    );
    assert_eq!(
        item.actions[0].kind,
        ActionKind::Copy,
        "the calculator's one action must be Copy — AC3's own wording"
    );

    gtk::init().expect("gtk init under the broadway display this process's environment selects");

    let window = gtk::Window::new();
    let anchor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    window.set_child(Some(&anchor));
    window.present();

    let chosen: std::rc::Rc<std::cell::RefCell<Vec<hop_protocol::ActionId>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = chosen.clone();
    let panel = ActionPanel::new(move |action_id| recorded.borrow_mut().push(action_id));

    let opened = panel.present(&item, &anchor);
    assert!(
        opened,
        "AC3: a real calculator result must open the panel — it is not the zero-action item \
         ActionPanel::present refuses"
    );
    assert_eq!(
        panel.selection().n_items(),
        1,
        "AC3: the panel must list exactly the calculator item's own one action, none invented \
         and none omitted"
    );
    let listed = action_of(
        &panel
            .selection()
            .item(0)
            .expect("a panel that opened for a one-action item must have a row at position 0"),
    );
    assert_eq!(
        listed.id, item.actions[0].id,
        "the listed row must be the calculator's own Copy action, not a different one"
    );
    assert_eq!(listed.kind, ActionKind::Copy);

    // AC3 says "exposes ... through the panel", not merely "lists" — Enter
    // must actually be able to run it, the same "listed but not runnable"
    // gap a filter-matching-nothing state would otherwise leave.
    panel.handle_key(gdk::Key::Return);
    assert_eq!(
        chosen.borrow().as_slice(),
        [item.actions[0].id.clone()],
        "choosing the panel's row for a real calculator result must report that action's own \
         id through on_choose, exactly once"
    );
    assert!(
        !panel.popover().is_visible(),
        "choosing the action must close the panel, the same as every other item this panel \
         is presented for"
    );

    println!("calculator_result_exposes_copy_through_the_action_panel passed");
}
