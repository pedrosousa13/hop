//! The tokio task that owns the socket: connect, handshake, and the query
//! lifecycle — the async mirror of `hop-cli`'s blocking client
//! (`crates/hop-cli/src/lib.rs`), kept alive for the process's lifetime and
//! reconnecting rather than exiting on the first error.
//!
//! Nothing in this file is `pub` outside `super` — see `ipc`'s module doc for
//! why that privacy is load-bearing rather than incidental.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, QueryText};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use super::{IpcCommand, IpcEvent};

/// How long a dropped connection waits before the next connect attempt.
/// Fixed rather than backed off: a v1 walking skeleton talks to a `hopd` on
/// the same machine, which is either up (reconnect succeeds immediately) or
/// down for a reason a fixed short delay does not make worse (there is no
/// remote peer here to overwhelm with retries).
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// What the reader task forwards to [`run`]'s driver loop, one per frame —
/// the same shape `hopd::connection`'s `ReadEvent` exists for and the same
/// reason: `read_exact` is not cancel-safe, so a frame read racing a
/// `tokio::select!` against the command channel must happen on a task of its
/// own, forwarding over an `mpsc::Receiver` (which *is* cancel-safe) rather
/// than being one of the `select!` branches directly. See
/// `crates/hopd/src/connection.rs`'s "Why reading happens on its own task".
enum ReadEvent {
    Message(DaemonMsg),
    Failed(std::io::Error),
}

/// Reads exactly one length-prefixed frame and forwards it, looping until
/// the socket errors or closes — at which point this task ends and its
/// sender drop closes `reader_rx` in [`run`], which is what tells the driver
/// loop the connection is gone.
async fn read_loop(mut read_half: OwnedReadHalf, tx: mpsc::Sender<ReadEvent>) {
    loop {
        let mut prefix = [0u8; FRAME_PREFIX_LEN];
        if let Err(err) = read_half.read_exact(&mut prefix).await {
            let _ = tx.send(ReadEvent::Failed(err)).await;
            return;
        }
        let len = match payload_len(prefix) {
            Ok(len) => len,
            Err(err) => {
                let _ = tx.send(ReadEvent::Failed(std::io::Error::other(err))).await;
                return;
            }
        };
        let mut payload = vec![0u8; len];
        if let Err(err) = read_half.read_exact(&mut payload).await {
            let _ = tx.send(ReadEvent::Failed(err)).await;
            return;
        }
        match decode_payload::<DaemonMsg>(&payload) {
            Ok(msg) => {
                if tx.send(ReadEvent::Message(msg)).await.is_err() {
                    // The driver loop is gone — nothing left to forward to.
                    return;
                }
            }
            Err(err) => {
                let _ = tx.send(ReadEvent::Failed(std::io::Error::other(err))).await;
                return;
            }
        }
    }
}

async fn send_frame(write_half: &mut OwnedWriteHalf, msg: &ClientMsg) -> std::io::Result<()> {
    let frame = encode_frame(msg).map_err(std::io::Error::other)?;
    write_half.write_all(&frame).await
}

/// Connects once and runs the handshake. `Ok` hands back the split stream
/// halves; `Err` is a human-readable reason [`IpcEvent::ConnectFailed`]
/// carries as-is.
async fn connect_and_handshake(
    socket_path: &Path,
) -> Result<(OwnedReadHalf, OwnedWriteHalf), String> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|err| format!("failed to connect to hopd: {err}"))?;
    let (mut read_half, mut write_half) = stream.into_split();

    send_frame(
        &mut write_half,
        &ClientMsg::Hello {
            api_version: API_VERSION,
        },
    )
    .await
    .map_err(|err| format!("failed to send handshake: {err}"))?;

    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    read_half
        .read_exact(&mut prefix)
        .await
        .map_err(|err| format!("lost connection during handshake: {err}"))?;
    let len = payload_len(prefix).map_err(|err| format!("handshake reply malformed: {err}"))?;
    let mut payload = vec![0u8; len];
    read_half
        .read_exact(&mut payload)
        .await
        .map_err(|err| format!("lost connection during handshake: {err}"))?;
    match decode_payload::<DaemonMsg>(&payload) {
        Ok(DaemonMsg::HelloAck { .. }) => Ok((read_half, write_half)),
        Ok(other) => Err(format!(
            "hopd did not acknowledge the handshake, got {other:?}"
        )),
        Err(err) => Err(format!("handshake reply malformed: {err}")),
    }
}

/// One connected session: drives the command channel and the reader task
/// until either the socket fails or `cmd_rx` closes (process shutdown).
/// Returns `true` if the caller should reconnect and keep serving, `false`
/// if it should stop entirely (shutdown).
async fn serve_one_connection(
    read_half: OwnedReadHalf,
    mut write_half: OwnedWriteHalf,
    cmd_rx: &async_channel::Receiver<IpcCommand>,
    evt_tx: &async_channel::Sender<IpcEvent>,
) -> bool {
    let _ = evt_tx.send(IpcEvent::Connected).await;

    let (reader_tx, mut reader_rx) = mpsc::channel::<ReadEvent>(8);
    let reader = tokio::spawn(read_loop(read_half, reader_tx));

    // The id `hop-cli` calls `QUERY_ID`, chosen the same way that crate's
    // one is: this client's, freshly incremented for every `Query` it sends,
    // so a superseded id's late frames are recognizable and dropped exactly
    // as `ClientMsg::Query`'s doc comment describes.
    let mut next_id: u64 = 1;
    let mut current_id: Option<u64> = None;
    // The raw text that produced `current_id` — set in the same match arm
    // that assigns `current_id`, and nowhere else, so the two never drift
    // apart. This is what lets the `QueryRouted` arm below hand
    // `IpcEvent::Routed` a `query_text` that is *provably* the text
    // `marker_span` was computed against, rather than something the UI would
    // otherwise have to reconstruct — see that event variant's own doc
    // comment (`ipc::IpcEvent::Routed`) for why this binding is issue #184's
    // central correctness requirement.
    let mut current_query_text: Option<String> = None;

    let outcome = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Ok(IpcCommand::Query(text)) => {
                        // Cloned before `QueryText::new` consumes `text` for
                        // the wire: this crate's own copy of exactly what was
                        // sent, kept for `QueryRouted` to bind below —
                        // see `current_query_text`'s own comment.
                        let raw_text = text.clone();
                        let Ok(query_text) = QueryText::new(text) else {
                            let _ = evt_tx.send(IpcEvent::Error(
                                "query text rejected locally (over bound)".to_string(),
                            )).await;
                            continue;
                        };
                        let id = next_id;
                        next_id += 1;
                        current_id = Some(id);
                        current_query_text = Some(raw_text);
                        if send_frame(&mut write_half, &ClientMsg::Query { id, text: query_text }).await.is_err() {
                            break true;
                        }
                    }
                    Ok(IpcCommand::Execute { item_id, action_id }) => {
                        let Some(query_id) = current_id else {
                            let _ = evt_tx.send(IpcEvent::Error(
                                "no active query to execute against".to_string(),
                            )).await;
                            continue;
                        };
                        if send_frame(&mut write_half, &ClientMsg::Execute { query_id, item_id, action_id }).await.is_err() {
                            break true;
                        }
                    }
                    // Every `CommandSender` clone was dropped: the UI is
                    // shutting down. Stop reconnecting.
                    Err(_) => break false,
                }
            }
            frame = reader_rx.recv() => {
                match frame {
                    Some(ReadEvent::Message(DaemonMsg::QueryRouted { query_id, mode, exclusive, marker_span })) if Some(query_id) == current_id => {
                        // `current_query_text` was set in the very same
                        // `IpcCommand::Query` arm above that produced
                        // `current_id`, and nothing else in this loop ever
                        // assigns either one independently — so this guard
                        // (`Some(query_id) == current_id`, identical to every
                        // other stale-frame check below) guarantees
                        // `current_query_text`, if present, is the exact text
                        // that produced *this* frame's `query_id`. It is
                        // always present by the time any `query_id` can equal
                        // `current_id` at all (both are `Some` or `None`
                        // together), so `unwrap_or_default` here is a
                        // never-taken fallback, not a real possibility — kept
                        // only to avoid an `unwrap()` this crate's lints warn
                        // on for a case that cannot actually occur.
                        let query_text = current_query_text.clone().unwrap_or_default();
                        let _ = evt_tx.send(IpcEvent::Routed { mode, exclusive, marker_span, query_text }).await;
                    }
                    Some(ReadEvent::Message(DaemonMsg::Results { query_id, items, .. })) if Some(query_id) == current_id => {
                        let _ = evt_tx.send(IpcEvent::Results(items)).await;
                    }
                    Some(ReadEvent::Message(DaemonMsg::QueryDone { query_id })) if Some(query_id) == current_id => {
                        let _ = evt_tx.send(IpcEvent::QueryDone).await;
                    }
                    Some(ReadEvent::Message(DaemonMsg::Executed { query_id, outcome })) if Some(query_id) == current_id => {
                        let _ = evt_tx.send(IpcEvent::Executed(outcome)).await;
                    }
                    Some(ReadEvent::Message(DaemonMsg::Error { query_id: None, error })) => {
                        let _ = evt_tx.send(IpcEvent::Error(error.message().to_string())).await;
                        // Connection-scoped: nothing promises this
                        // connection stays usable — see `DaemonMsg::Error`'s
                        // contract in `hop_protocol::wire`. Reconnect.
                        break true;
                    }
                    Some(ReadEvent::Message(DaemonMsg::Error { query_id: Some(id), error })) if Some(id) == current_id => {
                        let _ = evt_tx.send(IpcEvent::Error(error.message().to_string())).await;
                    }
                    // A frame for a superseded query id, or a message type
                    // this client draws no UI from — dropped, exactly the
                    // stale-frame rule `hop-cli`'s `connect_and_query` and
                    // `try_run_exec` apply on their own read loops.
                    Some(ReadEvent::Message(_)) => {}
                    Some(ReadEvent::Failed(err)) => {
                        // stderr, not the `IpcEvent` the UI sees: the reason
                        // a socket read failed (EOF, a reset connection) is
                        // diagnostic detail for a developer, not something a
                        // launcher's status row should show a user — the UI
                        // gets the human-readable `Disconnected` state below
                        // either way.
                        eprintln!("hop-gtk: lost connection to hopd: {err}");
                        let _ = evt_tx.send(IpcEvent::Disconnected).await;
                        break true;
                    }
                    None => {
                        let _ = evt_tx.send(IpcEvent::Disconnected).await;
                        break true;
                    }
                }
            }
        }
    };

    reader.abort();
    outcome
}

/// Runs for the process's lifetime: connect, serve, and on any disconnect
/// (network failure, a connection-scoped error, `hopd` restarting) wait
/// [`RECONNECT_DELAY`] and try again — until `cmd_rx` closes, which only
/// happens when every [`super::CommandSender`] clone has been dropped
/// (process shutdown).
///
/// This is the only function in this crate that constructs a `UnixStream` or
/// calls [`hop_protocol::framing`]'s decode/encode pair directly; see `ipc`'s
/// module doc comment for why that is a structural guarantee about where
/// socket IO can happen, not a fact this comment merely reports.
pub(super) async fn run(
    socket_path: PathBuf,
    cmd_rx: async_channel::Receiver<IpcCommand>,
    evt_tx: async_channel::Sender<IpcEvent>,
) {
    loop {
        match connect_and_handshake(&socket_path).await {
            Ok((read_half, write_half)) => {
                let keep_going =
                    serve_one_connection(read_half, write_half, &cmd_rx, &evt_tx).await;
                if !keep_going {
                    return;
                }
            }
            Err(reason) => {
                let _ = evt_tx.send(IpcEvent::ConnectFailed(reason)).await;
            }
        }

        // Stay responsive to shutdown even while `hopd` is unreachable,
        // rather than sleeping uninterruptibly: a `Query`/`Execute` sent
        // during this window is dropped (documented on
        // `CommandSender::send`), but a dropped `CommandSender` still ends
        // this task promptly instead of after a stale reconnect delay.
        tokio::select! {
            _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            cmd = cmd_rx.recv() => {
                if cmd.is_err() {
                    return;
                }
            }
        }
    }
}
