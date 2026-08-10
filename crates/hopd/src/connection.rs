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

use hop_core::router::route;
use hop_protocol::framing::{
    FRAME_PREFIX_LEN, FrameError, decode_payload, encode_frame, payload_len,
};
use hop_protocol::limits::MAX_ITEMS_PER_RESULTS_FRAME;
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, ErrorCode, ErrorDetail, Item, ProtoError};
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
    Refused {
        code: ErrorCode,
        detail: ErrorDetail,
    },
    /// The transport failed mid-read. The driver surfaces it to
    /// [`crate::server::serve_with`]'s log seam; there is no peer left worth
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
    /// The accepted text of this exchange's query, retained for
    /// [`ResultSource::record_launch`]: the learning store keys on
    /// `(query, item_id)`, and this connection is the only place that holds
    /// both the query it accepted and the item an `Execute` frame resolves
    /// against. Set once, from the `Query` arm's `ClientMsg::Query::text`,
    /// and never mutated afterward — a new `Query` replaces the whole
    /// exchange rather than this field alone, which is what keeps it in
    /// agreement with `id` and `delivered` for the same reason those two
    /// live in one struct (see this struct's own docs).
    text: String,
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
            Ok(Some(ReadOutcome::Refused { code, detail })) => ReadEvent::Refused { code, detail },
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
            Step::Peer(Some(ReadEvent::Refused { code, detail })) => {
                send_error(&mut write_half, None, code, detail).await?;
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
                ErrorDetail::Version {
                    expected: API_VERSION,
                    actual: api_version,
                },
            )
            .await?;
            Ok(true)
        }
        (HandshakeState::AwaitingHello, _other) => {
            send_error(
                write_half,
                None,
                ErrorCode::HandshakeRequired,
                ErrorDetail::Fixed("the first frame on a connection must be hello"),
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
            let text_owned = text.clone().into_string();

            // `QueryRouted` goes out before the source is even started, which
            // is what makes its ordering guarantee hold trivially: no
            // `Results` or `QueryDone` for this id can be written until this
            // function returns to the driver's poll loop, so nothing can
            // overtake it. Issue #127.
            //
            // Routing here is a *third* call on the same text — `HostSource`
            // routes once for the host's `RoutedQuery` and `Pipeline::assemble`
            // routes again per arrival, a tradeoff `source`'s own docs weigh
            // and accept because `route` is pure and cheap enough to run on
            // every keystroke. The same purity is what makes this call safe to
            // add rather than threading a value out through `ResultSource`,
            // whose `start` returns only a receiver: identical input, identical
            // answer, so the mode the client is told is necessarily the mode
            // the providers were asked under.
            let routed = route(&text_owned);
            send_msg(
                write_half,
                &DaemonMsg::QueryRouted {
                    query_id: id,
                    mode: routed.mode,
                    exclusive: routed.exclusive,
                },
            )
            .await?;

            // Replacing the exchange drops the previous query's receiver, and
            // that *is* the server-side cancellation — see the comment above.
            *exchange = Some(Exchange {
                id,
                text: text_owned,
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
        (
            HandshakeState::Ready,
            ClientMsg::Execute {
                query_id,
                item_id,
                action_id,
            },
        ) => {
            // Resolve against the retained set (`Exchange::delivered`), which
            // is the live-result-set binding issue #25's threat-model shape
            // and issue #59 choose: an execute frame acts only on an item this
            // daemon actually delivered under `query_id` — never a stale
            // query, never an id it never emitted. Every refusal below is
            // query-scoped (`Some(query_id)`) and non-terminal to the
            // connection, per `DaemonMsg::Error`'s contract.
            //
            // Cap-vs-never-emitted (#53/#55, design decision 4): since #103,
            // `delivered` is the *last* assembled list, bounded at
            // `MAX_ITEMS_PER_RESULTS_FRAME` and replaced whole per frame, while
            // per-query accumulation is bounded upstream at
            // `MAX_ITEMS_PER_QUERY` in `source.rs`. So an id absent from
            // `delivered` was never what the client was shown under this
            // `query_id` — whether the daemon never emitted it, a later frame
            // replaced it away, or a per-query cap dropped it. Execute binds to
            // what the client was shown, and retirement *is* removal from that
            // live set, so all three are honestly the same `UnknownItem`,
            // never a silent fall-through: there is no separate retained
            // "lost to the cap" state to distinguish, and claiming one would
            // be a fiction. This one refusal is the honest answer #53 asks for.
            let active = match exchange {
                Some(active) if active.id == query_id => active,
                _ => {
                    send_error(
                        &mut *write_half,
                        Some(query_id),
                        ErrorCode::UnknownItem,
                        // `query_id` is not repeated here: the enclosing
                        // frame's own `query_id` field already carries it
                        // (see `DaemonMsg::Error`'s docs), so this message
                        // has nothing left to interpolate.
                        ErrorDetail::Fixed("no such query or stale query id"),
                    )
                    .await?;
                    return Ok(false);
                }
            };

            let Some(item) = active.delivered.iter().find(|i| i.id == item_id) else {
                send_error(
                    &mut *write_half,
                    Some(query_id),
                    ErrorCode::UnknownItem,
                    ErrorDetail::Item(item_id),
                )
                .await?;
                return Ok(false);
            };

            if !item.actions.iter().any(|a| a.id == action_id) {
                send_error(
                    &mut *write_half,
                    Some(query_id),
                    ErrorCode::UnknownAction,
                    ErrorDetail::Action(action_id),
                )
                .await?;
                return Ok(false);
            }

            // Resolution succeeded; dispatch to the provider that produced the
            // item. The provider error is *not* forwarded verbatim — a
            // `ProviderError::Failed(String)` carries provider-authored text
            // that is not bounds-checked here, and the client only needs the
            // classification, not the payload.
            let provider = item.provider.clone();
            match source.execute(&provider, item_id.clone(), action_id).await {
                Ok(outcome) => {
                    // A launch is a successful action, not an attempted one:
                    // this fires only once `execute` has already answered
                    // `Ok`, and before `Executed` goes out — see
                    // `ResultSource::record_launch`'s docs for why the
                    // connection is what drives this rather than `execute`
                    // itself.
                    source.record_launch(&active.text, &item_id).await;
                    send_msg(write_half, &DaemonMsg::Executed { query_id, outcome }).await?
                }
                Err(_) => {
                    send_error(
                        &mut *write_half,
                        Some(query_id),
                        ErrorCode::ProviderFailed,
                        ErrorDetail::Provider(provider),
                    )
                    .await?
                }
            }
            Ok(false)
        }
        (HandshakeState::Ready, _other) => {
            // The only frame left to reach here on a `Ready` connection is a
            // second `Hello` — the peer asking to re-handshake a connection
            // that is already past that gate. Refused per frame, and the
            // connection stays open: this is a refusal of one frame, not of
            // the peer (mirroring how the connection's handshake docs describe
            // a second `Hello`).
            //
            // `ErrorCode::Internal` is kept deliberately: no dedicated code
            // exists for "already handshaken", and `Internal` is the code this
            // path has always emitted for a `Ready`-state frame the daemon has
            // no handler for. It is not the *execute* refusal path (issue
            // #59's slice), which now has its own arm above with
            // `UnknownItem`/`UnknownAction`/`ProviderFailed`.
            send_error(
                write_half,
                None,
                ErrorCode::Internal,
                ErrorDetail::Fixed("a connection may complete its handshake only once"),
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
    Refused {
        code: ErrorCode,
        detail: ErrorDetail,
    },
}

/// Reads one length-prefixed frame off `read_half`, or reports why it refuses
/// to.
///
/// Returns `Ok(None)` on a clean EOF at the frame boundary — the peer closed
/// its end between frames, which ends the connection with no error to send.
/// An `io::Error` other than EOF (a read that fails mid-frame, for instance)
/// travels to the driver as [`ReadEvent::Failed`] and, from there, out of
/// [`handle_connection`] to the `eprintln!` in [`crate::server::serve_with`]'s
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
        Err(FrameError::TooLarge { len }) => {
            // The refusal happens here, on the prefix alone: nothing below
            // this arm reads or allocates a buffer sized by the peer's
            // claimed length, which is the whole point of `payload_len`
            // being the pre-allocation gate. `len` is the peer's own claimed
            // prefix value — a bare number, not text — so it travels to the
            // client as a typed `ErrorDetail::FrameTooLarge` rather than
            // through `FrameError::TooLarge`'s own `Display`.
            return Ok(Some(ReadOutcome::Refused {
                code: ErrorCode::FrameTooLarge,
                detail: ErrorDetail::FrameTooLarge { len },
            }));
        }
        Err(other) => {
            // `payload_len` only ever constructs `TooLarge` — see its own
            // doc comment — so this arm exists as a compile-time reminder
            // rather than a case this server expects to hit: a future
            // variant added there is a match to update here, not a silent
            // fallthrough. `other`'s `Display` is logged here, daemon-side,
            // rather than reaching the client — the same split the
            // `decode_payload` arm below makes, for the same reason.
            eprintln!("hopd: unexpected error decoding a frame prefix: {other}");
            return Ok(Some(ReadOutcome::Refused {
                code: ErrorCode::Internal,
                detail: ErrorDetail::Fixed("an internal error occurred decoding a frame"),
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
        Err(err) => {
            // `err` is a `serde_json::Error` and is daemon-internal by
            // construction (issue #84): its `Display` can echo back
            // whatever the peer's bytes happened to contain — an unknown
            // `type` tag's exact text, for one — which is peer input, not a
            // daemon secret, but is exactly the shape of thing a *future*
            // parse failure could leak real internals through if this arm
            // kept forwarding it verbatim. It is logged here, daemon-side,
            // where it is actually useful for debugging a malformed peer,
            // and never reaches `message`, which becomes a fixed,
            // code-derived string instead.
            eprintln!("hopd: refused a frame that failed to parse as a client message: {err}");
            Ok(Some(ReadOutcome::Refused {
                // A payload this connection read in full and still could not
                // parse as a `ClientMsg` is bytes the peer sent, not a bug in
                // this daemon — `ErrorCode::MalformedFrame`'s doc comment
                // makes that split explicit. `ErrorCode::Internal` stays
                // reserved for a failure this process caused itself.
                code: ErrorCode::MalformedFrame,
                detail: ErrorDetail::Fixed("the frame payload could not be parsed"),
            }))
        }
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

/// Sends a [`DaemonMsg::Error`] whose message [`ProtoError::new`] derives
/// from `code` and `detail` — see that constructor's docs for why this is
/// the only way this daemon builds one.
async fn send_error(
    write_half: &mut OwnedWriteHalf,
    query_id: Option<u64>,
    code: ErrorCode,
    detail: ErrorDetail,
) -> io::Result<()> {
    send_msg(
        write_half,
        &DaemonMsg::Error {
            query_id,
            error: ProtoError::new(code, detail),
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
            text: "q".to_string(),
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

    // --- Execute resolution (issue #59) ---

    use std::future::Future;
    use std::sync::Arc;
    use std::sync::Mutex;

    use hop_core::provider::ProviderError;
    use hop_protocol::{Action, ActionId, ActionKind, ExecOutcome, ItemId, Kind, QueryText};

    /// An item whose `actions` carry exactly the named action ids, so a test
    /// can exercise the unknown-action path independently of the unknown-item
    /// one.
    fn item_with_action(id: &str, action_ids: &[&str]) -> Item {
        Item {
            id: ItemId::new(id).unwrap(),
            kind: Kind::Action,
            title: id.to_string(),
            subtitle: None,
            icon: None,
            actions: action_ids
                .iter()
                .map(|&a| Action {
                    id: ActionId::new(a).unwrap(),
                    kind: ActionKind::Open,
                    label: a.to_string(),
                })
                .collect(),
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "test".to_string(),
        }
    }

    /// A source that records every `execute` call it is handed and either
    /// succeeds with [`ExecOutcome::Done`] or fails, per its construction.
    /// `start` never emits anything — these tests drive `handle_message`
    /// directly against an exchange whose `delivered` the test populated.
    ///
    /// `launches` records every `record_launch` call the same way `calls`
    /// records `execute` calls, so a test can make a genuine observation
    /// about whether the Execute arm invoked it — in particular, that it
    /// does not on a refused or failed execute (see
    /// `a_provider_execute_error_is_query_scoped_provider_failed`), rather
    /// than asserting something that would hold even if the wiring were
    /// missing entirely.
    #[derive(Clone)]
    struct ScriptedSource {
        calls: Arc<Mutex<Vec<String>>>,
        launches: Arc<Mutex<Vec<String>>>,
        fail: bool,
    }

    impl ResultSource for ScriptedSource {
        fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
            let (_tx, rx) = mpsc::channel(1);
            rx
        }

        fn execute(
            &self,
            provider: &str,
            item_id: ItemId,
            action_id: ActionId,
        ) -> impl Future<Output = Result<ExecOutcome, ProviderError>> + Send {
            let provider = provider.to_string();
            let calls = self.calls.clone();
            let fail = self.fail;
            async move {
                calls
                    .lock()
                    .expect("no test panics holding this")
                    .push(format!("{provider}|{item_id}|{action_id}"));
                if fail {
                    Err(ProviderError::Failed("boom".to_string()))
                } else {
                    Ok(ExecOutcome::Done)
                }
            }
        }

        fn record_launch(&self, query: &str, item_id: &ItemId) -> impl Future<Output = ()> + Send {
            let launches = self.launches.clone();
            let entry = format!("{query}|{item_id}");
            async move {
                launches
                    .lock()
                    .expect("no test panics holding this")
                    .push(entry);
            }
        }
    }

    /// Reads one frame off `peer` and decodes it as a [`DaemonMsg`], so a test
    /// can assert on exactly what `handle_message` sent.
    async fn read_daemon_msg(peer: &mut tokio::net::UnixStream) -> DaemonMsg {
        use tokio::io::AsyncReadExt;
        let mut prefix = [0u8; FRAME_PREFIX_LEN];
        peer.read_exact(&mut prefix).await.expect("read prefix");
        let len = payload_len(prefix).expect("in-cap prefix");
        let mut payload = vec![0u8; len];
        peer.read_exact(&mut payload).await.expect("read payload");
        decode_payload(&payload).expect("decode as DaemonMsg")
    }

    #[tokio::test]
    async fn an_execute_resolves_against_delivered_and_sends_executed() {
        let mut state = HandshakeState::Ready;
        let source = ScriptedSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            launches: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        };
        let mut exchange = Some(Exchange {
            id: 7,
            text: "hello world".to_string(),
            source: None,
            delivered: vec![item_with_action("app:1", &["open"])],
        });
        let (mut peer, mut write_half) = write_half_pair();

        let done = handle_message(
            &mut state,
            &mut exchange,
            &mut write_half,
            &source,
            ClientMsg::Execute {
                query_id: 7,
                item_id: ItemId::new("app:1").unwrap(),
                action_id: ActionId::new("open").unwrap(),
            },
        )
        .await
        .unwrap();

        assert!(!done, "a successful execute must not end the connection");
        assert_eq!(
            read_daemon_msg(&mut peer).await,
            DaemonMsg::Executed {
                query_id: 7,
                outcome: ExecOutcome::Done,
            }
        );
        assert_eq!(
            source.calls.lock().expect("test lock").as_slice(),
            &["test|app:1|open".to_string()],
            "the item's provider and both resolved ids must reach the source"
        );
        assert_eq!(
            source.launches.lock().expect("test lock").as_slice(),
            &["hello world|app:1".to_string()],
            "a successful execute must record a launch keyed on the \
             exchange's accepted query text and the resolved item id"
        );
    }

    #[tokio::test]
    async fn an_execute_for_a_stale_query_id_is_query_scoped_unknown_item() {
        let mut state = HandshakeState::Ready;
        let source = ScriptedSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            launches: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        };
        // The live exchange is id 7; the frame names id 8.
        let mut exchange = Some(Exchange {
            id: 7,
            text: "q".to_string(),
            source: None,
            delivered: vec![item_with_action("app:1", &["open"])],
        });
        let (mut peer, mut write_half) = write_half_pair();

        let done = handle_message(
            &mut state,
            &mut exchange,
            &mut write_half,
            &source,
            ClientMsg::Execute {
                query_id: 8,
                item_id: ItemId::new("app:1").unwrap(),
                action_id: ActionId::new("open").unwrap(),
            },
        )
        .await
        .unwrap();

        assert!(!done, "a stale-query refusal must not end the connection");
        assert_eq!(
            read_daemon_msg(&mut peer).await,
            DaemonMsg::Error {
                query_id: Some(8),
                error: ProtoError::new(
                    ErrorCode::UnknownItem,
                    ErrorDetail::Fixed("no such query or stale query id"),
                ),
            }
        );
        assert!(
            source.calls.lock().expect("test lock").is_empty(),
            "a refused execute must never dispatch to the source"
        );
    }

    #[tokio::test]
    async fn an_execute_with_no_active_exchange_is_query_scoped_unknown_item() {
        let mut state = HandshakeState::Ready;
        let source = ScriptedSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            launches: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        };
        let mut exchange: Option<Exchange> = None;
        let (mut peer, mut write_half) = write_half_pair();

        let done = handle_message(
            &mut state,
            &mut exchange,
            &mut write_half,
            &source,
            ClientMsg::Execute {
                query_id: 1,
                item_id: ItemId::new("app:1").unwrap(),
                action_id: ActionId::new("open").unwrap(),
            },
        )
        .await
        .unwrap();

        assert!(!done);
        assert_eq!(
            read_daemon_msg(&mut peer).await,
            DaemonMsg::Error {
                query_id: Some(1),
                error: ProtoError::new(
                    ErrorCode::UnknownItem,
                    ErrorDetail::Fixed("no such query or stale query id"),
                ),
            }
        );
    }

    #[tokio::test]
    async fn an_execute_for_an_undelivered_item_is_query_scoped_unknown_item() {
        let mut state = HandshakeState::Ready;
        let source = ScriptedSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            launches: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        };
        // The live query delivered "app:1" only, so "app:2" was never shown.
        let mut exchange = Some(Exchange {
            id: 7,
            text: "q".to_string(),
            source: None,
            delivered: vec![item_with_action("app:1", &["open"])],
        });
        let (mut peer, mut write_half) = write_half_pair();

        let done = handle_message(
            &mut state,
            &mut exchange,
            &mut write_half,
            &source,
            ClientMsg::Execute {
                query_id: 7,
                item_id: ItemId::new("app:2").unwrap(),
                action_id: ActionId::new("open").unwrap(),
            },
        )
        .await
        .unwrap();

        assert!(!done);
        assert_eq!(
            read_daemon_msg(&mut peer).await,
            DaemonMsg::Error {
                query_id: Some(7),
                error: ProtoError::new(
                    ErrorCode::UnknownItem,
                    ErrorDetail::Item(ItemId::new("app:2").unwrap()),
                ),
            }
        );
        assert!(source.calls.lock().expect("test lock").is_empty());
    }

    #[tokio::test]
    async fn an_execute_for_an_unknown_action_is_query_scoped_unknown_action() {
        let mut state = HandshakeState::Ready;
        let source = ScriptedSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            launches: Arc::new(Mutex::new(Vec::new())),
            fail: false,
        };
        // The item only offers "open"; the frame asks for "delete".
        let mut exchange = Some(Exchange {
            id: 7,
            text: "q".to_string(),
            source: None,
            delivered: vec![item_with_action("app:1", &["open"])],
        });
        let (mut peer, mut write_half) = write_half_pair();

        let done = handle_message(
            &mut state,
            &mut exchange,
            &mut write_half,
            &source,
            ClientMsg::Execute {
                query_id: 7,
                item_id: ItemId::new("app:1").unwrap(),
                action_id: ActionId::new("delete").unwrap(),
            },
        )
        .await
        .unwrap();

        assert!(!done);
        assert_eq!(
            read_daemon_msg(&mut peer).await,
            DaemonMsg::Error {
                query_id: Some(7),
                error: ProtoError::new(
                    ErrorCode::UnknownAction,
                    ErrorDetail::Action(ActionId::new("delete").unwrap()),
                ),
            }
        );
        assert!(source.calls.lock().expect("test lock").is_empty());
    }

    #[tokio::test]
    async fn a_provider_execute_error_is_query_scoped_provider_failed() {
        let mut state = HandshakeState::Ready;
        let source = ScriptedSource {
            calls: Arc::new(Mutex::new(Vec::new())),
            launches: Arc::new(Mutex::new(Vec::new())),
            fail: true,
        };
        let mut exchange = Some(Exchange {
            id: 7,
            text: "q".to_string(),
            source: None,
            delivered: vec![item_with_action("app:1", &["open"])],
        });
        let (mut peer, mut write_half) = write_half_pair();

        let done = handle_message(
            &mut state,
            &mut exchange,
            &mut write_half,
            &source,
            ClientMsg::Execute {
                query_id: 7,
                item_id: ItemId::new("app:1").unwrap(),
                action_id: ActionId::new("open").unwrap(),
            },
        )
        .await
        .unwrap();

        assert!(!done, "a provider failure is query-scoped, not terminal");
        assert_eq!(
            read_daemon_msg(&mut peer).await,
            DaemonMsg::Error {
                query_id: Some(7),
                error: ProtoError::new(
                    ErrorCode::ProviderFailed,
                    ErrorDetail::Provider("test".to_string()),
                ),
            }
        );
        assert_eq!(
            source.calls.lock().expect("test lock").as_slice(),
            &["test|app:1|open".to_string()],
            "the provider must be reached before it can fail"
        );
        // The negative case this seam owes: a launch is a *successful*
        // action, so a provider failure — which the source above genuinely
        // received and answered `Err` to, not merely a refusal that never
        // reached it — must not produce a recorded launch either. Asserting
        // `is_empty()` here is a real observation because `source.launches`
        // would show exactly what went wrong if the Execute arm called
        // `record_launch` unconditionally instead of only on `Ok`.
        assert!(
            source.launches.lock().expect("test lock").is_empty(),
            "a provider failure must never record a launch"
        );
    }
}
