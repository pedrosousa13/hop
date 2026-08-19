//! Proves acceptance criterion 6's second half — "Enter launches the
//! default action" — at the layer that actually matters: the exact
//! `hop_gtk::ipc` calls `ui::window::activate_selected` makes
//! (`IpcCommand::Query`, then `IpcCommand::Execute { item_id,
//! action_id: item.default_action }`), driven against a real `hopd` built
//! from this workspace, over the real socket, with no GTK involved.
//!
//! GTK itself is exercised separately: `tests/headless_smoke.rs` proves the
//! query half visually (a results-state screenshot with `"2+2 = 4"`
//! rendered), and `ui::window::wire_entry` wires `Enter` to call the exact
//! same `activate_selected` function this test calls indirectly through
//! `ipc`. What is not exercised anywhere in this workspace's automated
//! tests is a synthetic Enter keypress landing on a real, mapped
//! `GtkEntry` — GTK's own key-event injection APIs exist
//! (`gtk_test_widget_send_key` and similar) but were not reached for in
//! this issue's walking skeleton; see this crate's top-level report for
//! that explicitly named gap rather than left implicit.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_protocol::ExecOutcome;

use hop_gtk::ipc::{self, IpcCommand, IpcEvent};

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

#[test]
fn query_then_execute_the_default_action_round_trips_against_real_hopd() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_daemon(runtime_dir.path());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (cmd_tx, evt_rx) = ipc::spawn(daemon.socket_path.clone());

    let item = runtime.block_on(async {
        // Wait for the handshake, same as `ui::window`'s real event loop
        // would while showing a "connecting" status.
        loop {
            match evt_rx.recv().await.expect("ipc thread exited early") {
                IpcEvent::Connected => break,
                IpcEvent::ConnectFailed(reason) => panic!("connect failed: {reason}"),
                _ => {}
            }
        }

        cmd_tx.send(IpcCommand::Query("2+2".to_string()));

        // The same deterministic calculator query
        // `crates/hopd/tests/calculator.rs` drives — one real item, no
        // external state.
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
            .expect("the calculator provider must answer \"2+2\" with one item")
    });

    // Exactly what `ui::window::activate_selected` sends on Enter: the
    // selected item's id and its own `default_action` — nothing this test
    // chose independently.
    let item_id = item.id.clone();
    let action_id = item.default_action.clone();

    let outcome = runtime.block_on(async move {
        cmd_tx.send(IpcCommand::Execute { item_id, action_id });
        loop {
            match evt_rx.recv().await.expect("ipc thread exited early") {
                IpcEvent::Executed(outcome) => return outcome,
                IpcEvent::Error(msg) => panic!("execute failed: {msg}"),
                _ => {}
            }
        }
    });

    // The calculator provider's default action copies the formatted
    // result — see `crates/hopd/src/calculator.rs`'s module doc ("offers
    // the formatted result as a single, copyable item"). Asserting the
    // variant (not just "it returned something") is what proves this ran
    // the calculator's real execute path, not merely that a frame arrived.
    match outcome {
        ExecOutcome::CopyText(text) => assert_eq!(text.as_str(), "4"),
        other => panic!("expected CopyText(\"4\"), got {other:?}"),
    }
}
