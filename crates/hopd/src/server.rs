//! Binding the socket and the per-connection protocol loop.
//!
//! Everything here trusts nothing about the bytes it reads: [`payload_len`]
//! decides whether a frame's declared length is even worth allocating for
//! before this module reads a byte of payload, and every message this
//! process sends back to a peer goes through [`encode_frame`] the same way
//! [`hop_protocol::framing`]'s docs describe — this module never redefines
//! either the frame cap or the codec, only calls into them.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hop_protocol::framing::{
    FRAME_PREFIX_LEN, FrameError, decode_payload, encode_frame, payload_len,
};
use hop_protocol::{
    API_VERSION, Action, ActionId, ActionKind, ClientMsg, DaemonMsg, ErrorCode, Item, ItemId, Kind,
    ProtoError,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// The socket's file name inside the runtime directory
/// [`crate::runtime_dir::resolve`] returns.
const SOCKET_FILE_NAME: &str = "hopd.sock";

/// Binds `<runtime_dir>/hopd.sock` and serves connections until an error
/// stops the accept loop or the process is killed — whichever comes first.
///
/// `runtime_dir` is assumed already created at 0700 by
/// [`crate::runtime_dir::resolve`]; this function does not create it, only
/// the socket file inside it.
///
/// # Stale-socket removal is provisional
///
/// If a file already sits at the socket path, it is removed before binding.
/// This is what makes restarting hopd after a crash work at all — `bind`
/// otherwise fails with `AddrInUse` against a leftover socket file, live or
/// not — but it is not a single-instance guard: nothing here checks whether
/// another `hopd` is still listening on that path before unlinking it out
/// from under it. That check is a later M2 slice's job, not this walking
/// skeleton's.
///
/// # The socket's mode is decided, not inherited
///
/// The v1 spec fixes the runtime directory's mode at 0700 (which grants or
/// withholds *traverse*) and says nothing about the socket file's own mode,
/// which is what grants or withholds *connect* once traverse is granted —
/// left unstated, that mode would be whatever the process's umask happens to
/// produce. The threat model
/// (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "The
/// boundary") calls that out as a decision this slice must make rather than
/// inherit, so the socket file is explicitly narrowed to 0600 with
/// `set_permissions` right after `bind`. Between `bind` returning and that
/// call completing there is a brief window where the socket's own mode is
/// whatever the umask left it at — but the *directory* is already 0700 by
/// the time this function runs, and reaching a path inside it requires
/// traverse on every component, so the parent directory's mode is what
/// carries the access control during that window, not the socket file's.
pub async fn serve(runtime_dir: &Path) -> io::Result<()> {
    let socket_path = runtime_dir.join(SOCKET_FILE_NAME);

    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;

    // No accept-loop exit beyond an unrecoverable startup error above: a
    // per-connection failure is logged and the loop keeps accepting, so the
    // only way out of this loop is the process being killed. Signal handling
    // and any orderly shutdown belong to issue #62 (socket activation and
    // lifecycle), not this slice.
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                // One task per connection, per the brief's acceptance
                // criterion that the runtime be multi-threaded: unbounded
                // today, since a per-connection or per-daemon cap on
                // concurrent connections is issue #55's, not this walking
                // skeleton's.
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(stream).await {
                        // The logging seam is issue #34, blocked on a later
                        // slice; this `eprintln!` is deliberately the only
                        // place this crate reports an error, per the
                        // brief's behavior spec.
                        eprintln!("hopd: connection error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("hopd: accept error: {err}"),
        }
    }
}

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

/// Reads frames from one connection until EOF or a refusal closes it.
///
/// Every frame's length prefix is checked against the frame cap, via
/// [`hop_protocol::framing::payload_len`] inside [`read_frame`], before this
/// function reads or allocates a single byte of that frame's payload — the
/// pre-allocation gate `hop_protocol::framing`'s docs describe, applied
/// here rather than re-implemented.
async fn handle_connection(mut stream: UnixStream) -> io::Result<()> {
    let mut state = HandshakeState::AwaitingHello;

    loop {
        let msg = match read_frame(&mut stream).await? {
            Some(ReadOutcome::Message(msg)) => msg,
            Some(ReadOutcome::Refused { code, message }) => {
                send_error(&mut stream, None, code, message).await?;
                return Ok(());
            }
            None => return Ok(()), // EOF: the peer closed its end.
        };

        match (&state, msg) {
            (HandshakeState::AwaitingHello, ClientMsg::Hello { api_version })
                if api_version == API_VERSION =>
            {
                send_msg(
                    &mut stream,
                    &DaemonMsg::HelloAck {
                        api_version: API_VERSION,
                    },
                )
                .await?;
                state = HandshakeState::Ready;
            }
            (HandshakeState::AwaitingHello, ClientMsg::Hello { api_version }) => {
                send_error(
                    &mut stream,
                    None,
                    ErrorCode::VersionMismatch,
                    format!("hopd speaks api_version {API_VERSION}, client sent {api_version}"),
                )
                .await?;
                return Ok(());
            }
            (HandshakeState::AwaitingHello, _other) => {
                send_error(
                    &mut stream,
                    None,
                    ErrorCode::HandshakeRequired,
                    "the first frame on a connection must be hello".to_string(),
                )
                .await?;
                return Ok(());
            }
            (HandshakeState::Ready, ClientMsg::Query { id, text: _ }) => {
                // `text` is unused: the walking skeleton answers every query
                // with the same hardcoded item, regardless of what was
                // typed. A real query path is a later M2/M3 slice's.
                send_msg(
                    &mut stream,
                    &DaemonMsg::Results {
                        query_id: id,
                        partial: false,
                        items: vec![hardcoded_item()],
                    },
                )
                .await?;
                send_msg(&mut stream, &DaemonMsg::QueryDone { query_id: id }).await?;
            }
            (HandshakeState::Ready, _other) => {
                // A second `hello`, or `cancel`/`execute`: none of these are
                // implemented in the walking skeleton. The connection stays
                // open — this is a refusal of one frame, not of the peer.
                send_error(
                    &mut stream,
                    None,
                    ErrorCode::Internal,
                    "not implemented in the walking skeleton".to_string(),
                )
                .await?;
            }
        }
    }
}

/// What reading one frame produced.
enum ReadOutcome {
    /// A frame that parsed as a [`ClientMsg`].
    Message(ClientMsg),
    /// A frame this connection refuses, and why — the caller sends the error
    /// and closes.
    Refused { code: ErrorCode, message: String },
}

/// Reads one length-prefixed frame off `stream`, or reports why it refuses
/// to.
///
/// Returns `Ok(None)` on a clean EOF at the frame boundary — the peer closed
/// its end between frames, which ends the connection with no error to send.
/// An `io::Error` other than EOF (a read that fails mid-frame, for instance)
/// propagates to the caller, which is `handle_connection`'s `?` and, above
/// that, the `eprintln!` in [`serve`]'s spawned task — the same "log and
/// move on" path an accept error takes.
async fn read_frame(stream: &mut UnixStream) -> io::Result<Option<ReadOutcome>> {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    match stream.read_exact(&mut prefix).await {
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

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await?;

    match decode_payload::<ClientMsg>(&payload) {
        Ok(msg) => Ok(Some(ReadOutcome::Message(msg))),
        Err(err) => Ok(Some(ReadOutcome::Refused {
            code: ErrorCode::Internal,
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
async fn send_msg(stream: &mut UnixStream, msg: &DaemonMsg) -> io::Result<()> {
    let frame = encode_frame(msg).map_err(|err| io::Error::other(err.to_string()))?;
    stream.write_all(&frame).await
}

/// Sends a [`DaemonMsg::Error`] built from `code` and `message`.
async fn send_error(
    stream: &mut UnixStream,
    query_id: Option<u64>,
    code: ErrorCode,
    message: String,
) -> io::Result<()> {
    send_msg(
        stream,
        &DaemonMsg::Error {
            query_id,
            error: ProtoError { code, message },
        },
    )
    .await
}

/// The walking skeleton's one and only result: every `query` frame gets
/// exactly this item back, regardless of what was typed.
fn hardcoded_item() -> Item {
    Item {
        id: ItemId::new("hop:walking-skeleton").expect("within bounds by construction"),
        kind: Kind::Action,
        title: "Hello from hopd".to_string(),
        subtitle: Some("M2.2 walking skeleton".to_string()),
        icon: None,
        actions: vec![Action {
            id: ActionId::new("open").expect("within bounds by construction"),
            kind: ActionKind::Open,
            label: "Open".to_string(),
        }],
        default_action: ActionId::new("open").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: "skeleton".to_string(),
    }
}
