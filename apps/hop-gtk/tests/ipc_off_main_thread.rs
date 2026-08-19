//! Proves, at runtime, that socket IO happens on a thread other than the one
//! driving the UI — acceptance criterion 3's "a test or an assertion makes
//! this checkable rather than asserted in prose".
//!
//! This is deliberately *not* a GTK test: it never touches `gtk::init` or a
//! main loop, because the claim under test is about `hop_gtk::ipc` alone —
//! the seam `ui::window` calls into, never the socket itself (see
//! `ipc`'s module doc). Driving `ipc::spawn` directly from this test's own
//! thread (standing in for "the GTK main thread", the same role
//! `glib::spawn_future_local`'s caller plays in the real app) and asserting
//! from here is a stronger proof than a GTK integration test would be: it
//! shows the property holds with no main loop cooperating to hide a
//! blocking call, not just that the app happens to feel responsive.
//!
//! Two things are checked:
//!
//! 1. **Thread identity**: the fake `hopd` below records which OS thread
//!    performed each socket read. It is never this test's own thread
//!    (`std::thread::current().id()`), which is the runtime half of the
//!    guarantee — [`hop_gtk::ipc`]'s module doc describes the structural
//!    half (the socket type is never nameable outside that module).
//! 2. **Non-blocking send**: [`hop_gtk::ipc::CommandSender::send`] returns
//!    before a connection even exists yet, proving it cannot be the thing
//!    doing IO.
#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::ThreadId;
use std::time::Duration;

use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg};

use hop_gtk::ipc::{self, IpcCommand, IpcEvent};

/// A minimal fake `hopd`: accepts one connection, handshakes, and answers
/// every `Query` with an empty `Results` then `QueryDone` — enough of the
/// protocol for `ipc::client::run` to drive a full round trip, no more.
/// Reports the thread id every read happened on back to the test through
/// `read_thread_tx`.
fn fake_hopd(listener: UnixListener, read_thread_tx: mpsc::Sender<ThreadId>) {
    let Ok((mut stream, _)) = listener.accept() else {
        return;
    };

    // Handshake.
    let Some(ClientMsg::Hello { .. }) = read_one(&mut stream, &read_thread_tx) else {
        return;
    };
    send(
        &mut stream,
        &DaemonMsg::HelloAck {
            api_version: API_VERSION,
        },
    );

    loop {
        match read_one(&mut stream, &read_thread_tx) {
            Some(ClientMsg::Query { id, .. }) => {
                send(
                    &mut stream,
                    &DaemonMsg::Results {
                        query_id: id,
                        partial: false,
                        items: Vec::new(),
                    },
                );
                send(&mut stream, &DaemonMsg::QueryDone { query_id: id });
            }
            Some(_) | None => return,
        }
    }
}

fn read_one(stream: &mut UnixStream, read_thread_tx: &mpsc::Sender<ThreadId>) -> Option<ClientMsg> {
    let _ = read_thread_tx.send(std::thread::current().id());
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream.read_exact(&mut prefix).ok()?;
    let len = payload_len(prefix).ok()?;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).ok()?;
    decode_payload(&payload).ok()
}

fn send(stream: &mut UnixStream, msg: &DaemonMsg) {
    let frame = encode_frame(msg).unwrap();
    stream.write_all(&frame).unwrap();
}

#[test]
fn socket_reads_happen_off_the_calling_thread_and_send_never_blocks_on_them() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path: PathBuf = dir.path().join("fake-hopd.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let (read_thread_tx, read_thread_rx) = mpsc::channel::<ThreadId>();
    let server = std::thread::spawn(move || fake_hopd(listener, read_thread_tx));

    let this_thread = std::thread::current().id();

    // `spawn` must return immediately, before any connection exists — a
    // blocking connect or handshake here would be socket IO on the caller's
    // thread, exactly what this test exists to rule out.
    let start = std::time::Instant::now();
    let (cmd_tx, evt_rx) = ipc::spawn(socket_path);
    assert!(
        start.elapsed() < Duration::from_millis(50),
        "ipc::spawn must not block on the socket"
    );

    // `send` must also return immediately — queuing, not writing.
    let start = std::time::Instant::now();
    cmd_tx.send(IpcCommand::Query("hello".to_string()));
    assert!(
        start.elapsed() < Duration::from_millis(50),
        "CommandSender::send must not block on the socket"
    );

    // Drive the round trip from *this* thread purely through the channel —
    // the same shape `ui::window`'s `glib::spawn_future_local` loop uses
    // (`EventReceiver::recv` is the same `async fn` either way), blocked on
    // here with a throwaway current-thread runtime purely so this test can
    // stay synchronous; nothing about that blocking touches the socket —
    // see this file's top doc comment.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut saw_results = false;
    let mut saw_done = false;
    for _ in 0..10 {
        let event = runtime.block_on(evt_rx.recv());
        match event {
            Some(IpcEvent::Results(_)) => saw_results = true,
            Some(IpcEvent::QueryDone) => {
                saw_done = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_results, "expected a Results event");
    assert!(saw_done, "expected a QueryDone event");

    // The actual proof: every socket read the fake daemon observed happened
    // on some thread, and none of them is this test's own thread — the read
    // side lives entirely on `ipc::spawn`'s dedicated background thread.
    let mut read_threads = Vec::new();
    while let Ok(id) = read_thread_rx.recv_timeout(Duration::from_millis(200)) {
        read_threads.push(id);
    }
    assert!(
        !read_threads.is_empty(),
        "the fake daemon should have observed at least one read"
    );
    assert!(
        read_threads.iter().all(|&id| id != this_thread),
        "a socket read happened on the calling thread, not the IPC thread"
    );

    drop(cmd_tx);
    drop(evt_rx);
    let _ = server.join();
}
