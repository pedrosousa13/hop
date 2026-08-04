//! One connection's protocol loop: the handshake gate, then the query
//! lifecycle — streamed results, server-side cancellation, and the bounded
//! retained result set.
//!
//! Everything here trusts nothing about the bytes it reads: [`payload_len`]
//! decides whether a frame's declared length is even worth allocating for
//! before this module reads a byte of payload, and every message this process
//! sends back to a peer goes through [`encode_frame`] the same way
//! [`hop_protocol::framing`]'s docs describe — this module never redefines
//! either the frame cap or the codec, only calls into them.
//!
//! # Why reading happens on its own task
//!
//! After the handshake this loop must watch two things at once: the peer's
//! next frame and the active source's next batch. `tokio::select!` cancels
//! the losing branch, and a cancelled `read_exact` mid-frame loses the bytes
//! it had already read — the stream desyncs and every later frame parses as
//! garbage. `mpsc::Receiver::recv` is cancel-safe, so the reads move to a
//! dedicated reader task that owns the read half and forwards [`ReadEvent`]s
//! over a channel; the driver selects over two receivers and stays sound.
//!
//! # Why the driver waits in two shapes rather than one guarded select
//!
//! [`drive`] chooses its wait on whether a query is streaming: with none, it
//! awaits the peer's channel alone, so the source branch is not merely
//! disabled but absent; with one, it selects over both. The select is written
//! as an *expression* whose arms only classify what woke it — the work runs
//! after it, once the borrow of the exchange has ended.
//!
//! The obvious alternative is one `select!` with a `, if active.is_some()`
//! guard on the source arm. That shape has to name the receiver inside the
//! arm's future while the other arm's handler mutates the same state, which
//! costs either a fight with the borrow checker or a runtime `expect` on an
//! invariant threaded through a loop — the kind this crate keeps out of
//! production code. Two shapes and a `Step` enum buy the same behaviour with
//! neither.

use std::io;

use hop_protocol::framing::{
    FRAME_PREFIX_LEN, FrameError, decode_payload, encode_frame, payload_len,
};
use hop_protocol::limits::MAX_ITEMS_PER_RESULTS_FRAME;
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, ErrorCode, Item, ProtoError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::source::ResultSource;

/// A connection's position in the handshake gate every frame passes through.
///
/// Starts at `AwaitingHello` on every new connection and moves to `Ready`
/// exactly once, on a `Hello` whose `api_version` matches
/// [`API_VERSION`](hop_protocol::API_VERSION). Nothing moves a `Ready`
/// connection back to `AwaitingHello` — a second `Hello` is refused, not
/// treated as a re-handshake, per the brief's behavior spec.
enum HandshakeState {
    AwaitingHello,
    Ready,
}

/// What the reader task forwards to the driver, one per frame read.
enum ReadEvent {
    /// A frame that parsed as a [`ClientMsg`].
    Message(ClientMsg),
    /// A frame this connection refuses, and why — the driver sends the error
    /// and closes.
    Refused { code: ErrorCode, message: String },
    /// The transport failed mid-read. The driver surfaces it to
    /// [`crate::server::serve`]'s log seam; there is no peer left worth
    /// answering.
    Failed(io::Error),
}

/// One query id's exchange: the source still producing for it, if any, and
/// the last assembled list it sent.
///
/// The two halves are one struct because they are one invariant. `delivered`
/// is the state issue #59's `execute` binding resolves against, and it has to
/// stay readable *while* the query streams as well as after it ends — so it
/// cannot live inside a value that is dropped when the source stops, and
/// holding it in a second `Option` alongside would mean two fields that must
/// agree on a query id with nothing but a comment (and, at the point of use,
/// an `expect`) saying they do. Here the id is stored once and the two can
/// only disagree if this file stops compiling.
///
/// `source` going to `None` is what ends an exchange — naturally, at the
/// per-frame bound, or on a `Cancel` — and dropping the receiver is what
/// tells the source to stop working. The exchange itself outlives that: what
/// was delivered stays resolvable until a new `Query` replaces it whole,
/// because an item this daemon has already shown the client must not become
/// unresolvable just because the query that produced it finished. What
/// changes under replacement is that `delivered` is not everything this
/// exchange has ever sent — it is only the *last* list, because each arrival
/// replaces it rather than adding to it, so an item the daemon has since
/// replaced away is no longer resolvable either. That is not a gap this
/// struct leaves open; it is what criterion 6 (issue #103) asks for.
struct Exchange {
    /// The `query_id` every frame of this exchange carries.
    id: u64,
    /// The live source, or `None` once this exchange has ended.
    source: Option<mpsc::Receiver<Vec<Item>>>,
    /// The last list [`forward_batch`] sent, bounded by
    /// [`MAX_ITEMS_PER_RESULTS_FRAME`] — this struct's own defensive bound on
    /// one assembled list, since [`ResultSource`]'s obligations say a source
    /// is not trusted. What a source may *accumulate* for one query is
    /// [`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY),
    /// enforced in `source.rs` before a batch ever reaches this connection —
    /// this field only ever holds one arrival's worth.
    delivered: Vec<Item>,
}

/// What one turn of the driver's wait produced.
///
/// The driver's `select!` arms build one of these and nothing else; the work
/// happens in the `match` below the select, where the exchange is no longer
/// borrowed by the future that was reading from it.
enum Step {
    /// The peer's channel woke: an event, or `None` for the reader task
    /// having finished.
    Peer(Option<ReadEvent>),
    /// The active source woke: a batch, or `None` for the source finishing.
    Batch(Option<Vec<Item>>),
}

/// Serves one accepted connection to completion.
///
/// Every frame's length prefix is checked against the frame cap, via
/// [`hop_protocol::framing::payload_len`] inside [`read_frame`], before this
/// connection reads or allocates a single byte of that frame's payload — the
/// pre-allocation gate `hop_protocol::framing`'s docs describe, applied here
/// rather than re-implemented.
pub(crate) async fn handle_connection<S: ResultSource>(
    stream: UnixStream,
    source: S,
) -> io::Result<()> {
    let (read_half, write_half) = stream.into_split();
    // Capacity 1: the driver is the only consumer and handles one frame at a
    // time, so a deeper queue would only buy the peer the right to read
    // further ahead of the work it is queueing.
    let (events_tx, events_rx) = mpsc::channel(1);
    let reader = tokio::spawn(read_loop(read_half, events_tx));
    let result = drive(events_rx, write_half, source).await;
    // A mute peer leaves the reader parked in `read_exact` forever; the
    // driver returning must not leave that task (and the fd's read half)
    // behind. Aborting is safe: nothing downstream consumes its events now.
    reader.abort();
    result
}

/// Reads frames until EOF, a refusal, or an IO error, forwarding each as a
/// [`ReadEvent`]. Returning drops `events`, which is how the driver learns
/// the peer is gone.
async fn read_loop(mut read_half: OwnedReadHalf, events: mpsc::Sender<ReadEvent>) {
    loop {
        let event = match read_frame(&mut read_half).await {
            Ok(Some(ReadOutcome::Message(msg))) => ReadEvent::Message(msg),
            Ok(Some(ReadOutcome::Refused { code, message })) => {
                ReadEvent::Refused { code, message }
            }
            Ok(None) => return, // EOF: the peer closed its end.
            Err(err) => {
                let _ = events.send(ReadEvent::Failed(err)).await;
                return;
            }
        };
        let refused = matches!(event, ReadEvent::Refused { .. });
        if events.send(event).await.is_err() {
            return; // The driver is gone; nothing to read for.
        }
        if refused {
            return; // A refusal closes the connection; stop reading behind it.
        }
    }
}

/// The driver: waits on the peer's frames and, while a query is streaming,
/// on that source's batches, owning the handshake state and the exchange.
async fn drive<S: ResultSource>(
    mut events: mpsc::Receiver<ReadEvent>,
    mut write_half: OwnedWriteHalf,
    source: S,
) -> io::Result<()> {
    let mut state = HandshakeState::AwaitingHello;
    let mut exchange: Option<Exchange> = None;

    loop {
        let step = match exchange.as_mut().and_then(|active| active.source.as_mut()) {
            // Nothing is streaming, so the peer's next frame is the only
            // thing that can move this connection forward: there is no
            // second future to wait on, and the source branch is absent
            // rather than merely disabled.
            None => Step::Peer(events.recv().await),
            // Both arms only classify what woke the select. Doing the work
            // here instead would mean mutating the exchange while this
            // borrow of its receiver is still live — see the module docs.
            //
            // `select!` picks at random between branches that are both
            // ready, which is what this loop wants: a source producing
            // batches as fast as the driver can forward them must not be
            // able to starve the peer's arm, or a `Cancel` would only be
            // noticed once the flood it is cancelling had finished.
            Some(batches) => tokio::select! {
                event = events.recv() => Step::Peer(event),
                batch = batches.recv() => Step::Batch(batch),
            },
        };

        match step {
            Step::Peer(None) => return Ok(()), // EOF: the peer closed its end.
            Step::Peer(Some(ReadEvent::Failed(err))) => return Err(err),
            Step::Peer(Some(ReadEvent::Refused { code, message })) => {
                send_error(&mut write_half, None, code, message).await?;
                return Ok(());
            }
            Step::Peer(Some(ReadEvent::Message(msg))) => {
                if handle_message(&mut state, &mut exchange, &mut write_half, &source, msg).await? {
                    return Ok(());
                }
            }
            Step::Batch(batch) => forward_batch(&mut exchange, &mut write_half, batch).await?,
        }
    }
}

/// Applies one client frame to the connection's state. `Ok(true)` means the
/// connection is done and the driver should return.
async fn handle_message<S: ResultSource>(
    state: &mut HandshakeState,
    exchange: &mut Option<Exchange>,
    write_half: &mut OwnedWriteHalf,
    source: &S,
    msg: ClientMsg,
) -> io::Result<bool> {
    match (&*state, msg) {
        (HandshakeState::AwaitingHello, ClientMsg::Hello { api_version })
            if api_version == API_VERSION =>
        {
            send_msg(
                write_half,
                &DaemonMsg::HelloAck {
                    api_version: API_VERSION,
                },
            )
            .await?;
            *state = HandshakeState::Ready;
            Ok(false)
        }
        (HandshakeState::AwaitingHello, ClientMsg::Hello { api_version }) => {
            send_error(
                write_half,
                None,
                ErrorCode::VersionMismatch,
                format!("hopd speaks api_version {API_VERSION}, client sent {api_version}"),
            )
            .await?;
            Ok(true)
        }
        (HandshakeState::AwaitingHello, _other) => {
            send_error(
                write_half,
                None,
                ErrorCode::HandshakeRequired,
                "the first frame on a connection must be hello".to_string(),
            )
            .await?;
            Ok(true)
        }
        (HandshakeState::Ready, ClientMsg::Query { id, text }) => {
            // Replacing the exchange drops the previous query's receiver, and
            // that *is* the server-side cancellation: the source's next send
            // fails and it stops. No frames follow for the superseded id —
            // not even `QueryDone`; the client that issued this query has
            // moved on and would drop them as stale anyway. The retained set
            // is replaced along with it, because what the client is looking
            // at is now this query's results and nothing else.
            *exchange = Some(Exchange {
                id,
                source: Some(source.start(text)),
                delivered: Vec::new(),
            });
            Ok(false)
        }
        (HandshakeState::Ready, ClientMsg::Cancel { id }) => {
            // A cancel naming anything but the live query — a stale id, or
            // one whose `QueryDone` is already on the wire — is ordinary
            // traffic and is dropped silently, per `ClientMsg::Cancel`'s
            // contract.
            if let Some(active) = exchange
                .as_mut()
                .filter(|active| active.id == id && active.source.is_some())
            {
                // Same mechanism as supersession, but acknowledged: the
                // canceller is still waiting on this id, so it gets the
                // exchange's terminal frame. What was delivered stays
                // retained — cancelling a query does not unshow its results.
                active.source = None;
                send_msg(write_half, &DaemonMsg::QueryDone { query_id: id }).await?;
            }
            Ok(false)
        }
        (HandshakeState::Ready, _other) => {
            // A second `hello`, or `execute` (issue #59's slice): refused per
            // frame, the connection stays open. This is a refusal of one
            // frame, not of the peer.
            send_error(
                write_half,
                None,
                ErrorCode::Internal,
                "not implemented yet".to_string(),
            )
            .await?;
            Ok(false)
        }
    }
}

/// Forwards one source event — a batch, or the source finishing — to the
/// peer, replacing [`Exchange::delivered`] whole and enforcing
/// [`MAX_ITEMS_PER_RESULTS_FRAME`].
///
/// A batch here is already the complete current list, per the replace-frame
/// contract [`ResultSource`]'s docs describe — never an increment to append.
/// That is what makes at most one `Results` frame per arrival correct rather
/// than merely convenient: an increment could always be carried across
/// several frames because the client was going to append them anyway, but a
/// replacement cannot be, because a client receiving a second frame for the
/// same arrival would have no way to tell "the rest of this list" from "a
/// new list replacing it" — nothing on the wire distinguishes them (Design
/// decision 3). So an over-long list has exactly two honest answers, truncate
/// or refuse, and truncate-and-terminate is the one this daemon already uses
/// at every other bound: truncate to the frame's capacity, deliver that, and
/// end the exchange with its terminal frame — the same rule
/// `CONTEXT.md`'s truncate-and-terminate entry states, applied here at
/// [`MAX_ITEMS_PER_RESULTS_FRAME`] instead of at the per-query cap, because
/// the per-query cap is no longer this connection's to enforce — see
/// `source.rs`, where [`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY)
/// now bounds what a source accumulates before a batch ever reaches here.
/// This connection's [`MAX_ITEMS_PER_RESULTS_FRAME`] check stays regardless,
/// because [`ResultSource`]'s obligations section is explicit that a source
/// is not trusted to honour it on its own.
async fn forward_batch(
    exchange: &mut Option<Exchange>,
    write_half: &mut OwnedWriteHalf,
    batch: Option<Vec<Item>>,
) -> io::Result<()> {
    let Some(active) = exchange.as_mut() else {
        // Unreachable: the receiver this batch came from lives inside the
        // exchange, so there is no way to be woken by a source the exchange
        // no longer holds.
        return Ok(());
    };
    let query_id = active.id;

    let Some(mut batch) = batch else {
        // The source finished. Clearing it is what takes this query out of
        // the driver's wait — a closed receiver is permanently ready, so
        // leaving it in place would spin — and `QueryDone` is the exchange's
        // terminal frame, never a `partial: false` results frame.
        active.source = None;
        return send_msg(write_half, &DaemonMsg::QueryDone { query_id }).await;
    };

    let capped = batch.len() > MAX_ITEMS_PER_RESULTS_FRAME;
    if capped {
        batch.truncate(MAX_ITEMS_PER_RESULTS_FRAME);
    }

    // Retained before it is sent, not after: a write that fails partway
    // leaves the connection dead either way, and the state that matters is
    // "what this daemon committed to delivering under this id".
    active.delivered = batch;

    send_msg(
        write_half,
        &DaemonMsg::Results {
            query_id,
            partial: true,
            items: active.delivered.clone(),
        },
    )
    .await?;

    if capped {
        // Dropping the receiver stops the source; QueryDone is what tells
        // the client the exchange is over. The two halves of
        // truncate-and-terminate: everything delivered stays retained and
        // resolvable, and what did not fit was never delivered at all.
        active.source = None;
        send_msg(write_half, &DaemonMsg::QueryDone { query_id }).await?;
    }
    Ok(())
}

/// What reading one frame produced.
enum ReadOutcome {
    /// A frame that parsed as a [`ClientMsg`].
    Message(ClientMsg),
    /// A frame this connection refuses, and why — the caller sends the error
    /// and closes.
    Refused { code: ErrorCode, message: String },
}

/// Reads one length-prefixed frame off `read_half`, or reports why it refuses
/// to.
///
/// Returns `Ok(None)` on a clean EOF at the frame boundary — the peer closed
/// its end between frames, which ends the connection with no error to send.
/// An `io::Error` other than EOF (a read that fails mid-frame, for instance)
/// travels to the driver as [`ReadEvent::Failed`] and, from there, out of
/// [`handle_connection`] to the `eprintln!` in [`crate::server::serve`]'s
/// spawned task — the same "log and move on" path an accept error takes.
async fn read_frame(read_half: &mut OwnedReadHalf) -> io::Result<Option<ReadOutcome>> {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    match read_half.read_exact(&mut prefix).await {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }

    let len = match payload_len(prefix) {
        Ok(len) => len,
        Err(err @ FrameError::TooLarge { .. }) => {
            // The refusal happens here, on the prefix alone: nothing below
            // this arm reads or allocates a buffer sized by the peer's
            // claimed length, which is the whole point of `payload_len`
            // being the pre-allocation gate.
            return Ok(Some(ReadOutcome::Refused {
                code: ErrorCode::FrameTooLarge,
                message: err.to_string(),
            }));
        }
        Err(other) => {
            // `payload_len` only ever constructs `TooLarge` — see its own
            // doc comment — so this arm exists as a compile-time reminder
            // rather than a case this server expects to hit: a future
            // variant added there is a match to update here, not a silent
            // fallthrough.
            return Ok(Some(ReadOutcome::Refused {
                code: ErrorCode::Internal,
                message: other.to_string(),
            }));
        }
    };

    // `len` is already checked against MAX_FRAME_BYTES by `payload_len`
    // above, so this allocation is capped per frame — but nothing here caps
    // how many such allocations one connection can rack up over its
    // lifetime, or across every connection this daemon is serving at once,
    // and `read_exact` below has no timeout, so a peer that sends a valid
    // prefix and then never finishes the payload holds this buffer and this
    // task open indefinitely. Aggregate memory bounds and read timeouts are
    // issue #98's, not this slice's.
    let mut payload = vec![0u8; len];
    read_half.read_exact(&mut payload).await?;

    match decode_payload::<ClientMsg>(&payload) {
        Ok(msg) => Ok(Some(ReadOutcome::Message(msg))),
        Err(err) => Ok(Some(ReadOutcome::Refused {
            // A payload this connection read in full and still could not
            // parse as a `ClientMsg` is bytes the peer sent, not a bug in
            // this daemon — `ErrorCode::MalformedFrame`'s doc comment makes
            // that split explicit. `ErrorCode::Internal` stays reserved for
            // a failure this process caused itself.
            code: ErrorCode::MalformedFrame,
            message: err.to_string(),
        })),
    }
}

/// Encodes and writes one [`DaemonMsg`].
///
/// A [`FrameError`] here means this process failed to serialize a message it
/// built itself — every variant this server sends is small and fixed-shape,
/// so this is a bug rather than anything a peer triggered — and is folded
/// into an `io::Error` so the caller's single `?` covers both that and a
/// genuine write failure.
async fn send_msg(write_half: &mut OwnedWriteHalf, msg: &DaemonMsg) -> io::Result<()> {
    let frame = encode_frame(msg).map_err(|err| io::Error::other(err.to_string()))?;
    write_half.write_all(&frame).await
}

/// Sends a [`DaemonMsg::Error`] built from `code` and `message`.
async fn send_error(
    write_half: &mut OwnedWriteHalf,
    query_id: Option<u64>,
    code: ErrorCode,
    message: String,
) -> io::Result<()> {
    send_msg(
        write_half,
        &DaemonMsg::Error {
            query_id,
            error: ProtoError { code, message },
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// An item whose id names it, so a test can tell two lists' items apart
    /// by identity rather than by count alone.
    fn item_named(id: &str) -> Item {
        Item {
            id: hop_protocol::ItemId::new(id).unwrap(),
            kind: hop_protocol::Kind::Action,
            title: id.to_string(),
            subtitle: None,
            icon: None,
            actions: vec![],
            default_action: hop_protocol::ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "test".to_string(),
        }
    }

    /// A live socket pair to drive [`forward_batch`] against directly: it
    /// needs a real `OwnedWriteHalf` to write frames into. The peer half is
    /// kept alive (never read from) for the tests below — each writes only a
    /// couple of small frames, well under what the kernel buffers before a
    /// write would block.
    fn write_half_pair() -> (tokio::net::UnixStream, OwnedWriteHalf) {
        let (near, far) = tokio::net::UnixStream::pair().expect("unix socket pair");
        let (_read, write_half) = near.into_split();
        (far, write_half)
    }

    #[tokio::test]
    async fn the_retained_set_is_the_last_list_not_the_union() {
        // Two forward_batch calls, each a complete replace-frame list. The
        // first holds an item the second does not ("only-in-first"), so a
        // retained set that was the *union* of every list sent — the bug
        // this test exists to catch — would still contain it after the
        // second call; the correct implementation's retained set holds
        // exactly the second list.
        let (_peer, mut write_half) = write_half_pair();
        let mut exchange = Some(Exchange {
            id: 1,
            source: None,
            delivered: Vec::new(),
        });

        let first = vec![item_named("only-in-first"), item_named("shared")];
        let second = vec![item_named("shared"), item_named("only-in-second")];

        forward_batch(&mut exchange, &mut write_half, Some(first))
            .await
            .unwrap();
        forward_batch(&mut exchange, &mut write_half, Some(second.clone()))
            .await
            .unwrap();

        let delivered = &exchange.as_ref().unwrap().delivered;
        assert_eq!(
            delivered, &second,
            "the retained set must equal the last list exactly, not the \
             union of every list this exchange has sent"
        );
        assert!(
            delivered.iter().all(|i| i.id.as_str() != "only-in-first"),
            "an item only the first list held must not survive a replacement"
        );
    }
}
