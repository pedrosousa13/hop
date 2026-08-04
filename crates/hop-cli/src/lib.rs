//! `hop` — the walking-skeleton CLI for the hop launcher daemon.
//!
//! Two subcommands exist today: `hop version`, which needs no daemon, and
//! `hop query <text>`, which speaks the same length-prefixed JSON framing
//! [`hopd`](../hopd/index.html) does over the same Unix socket, using
//! `hop_protocol`'s IO-free codec on both ends of the pipe (see that crate's
//! `framing` module docs for why the codec itself has no socket code in it).
//!
//! # Why no tokio
//!
//! This binary opens one socket, sends two frames, and reads until the
//! daemon says `query_done` — a strictly sequential, single-connection
//! conversation with nothing else for a runtime to schedule concurrently.
//! `std::os::unix::net::UnixStream` blocks the one thread this process has
//! for exactly as long as `hopd` takes to answer, which is the same amount
//! of wall-clock time an async client would spend awaiting the same reads —
//! the only difference an async runtime would buy here is a scheduler with
//! nothing to schedule. `hopd` needs tokio because it serves many
//! connections at once (see `hopd::server`'s docs); this binary is one
//! connection, once, and pulling in an async runtime, its `Cargo.lock`
//! surface, and a `#[tokio::main]` wrapper to block on a single blocking
//! call would be weight with no behavior behind it.
//!
//! # Why no clap
//!
//! Two subcommands, one of which takes a single required string argument,
//! is little enough surface that hand-rolling the match in [`parse`] is
//! fewer lines than a derive macro's attributes would be, and it keeps this
//! crate's only dependency beyond `hop-protocol` and `serde_json` at zero.
//! That trade tips the other way once `exec`, `toggle`, and `doctor` land —
//! multiple flags, subcommand-specific options, and `--help` text generated
//! rather than hand-maintained are what a parser earns its dependency
//! weight back on. Until then, a parser here would be solving a problem
//! this binary does not have yet.

use std::env;
use std::fmt;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use hop_protocol::framing::{
    FRAME_PREFIX_LEN, FrameError, decode_payload, encode_frame, payload_len,
};
use hop_protocol::{API_VERSION, BoundError, ClientMsg, DaemonMsg, ProtoError, QueryText};

/// The `id` this CLI sends on its one `Query` frame per process. There is
/// only ever one query in flight on this connection, so a fixed id (rather
/// than a counter) is enough to tell "this query's answer" apart from a
/// stray frame — see the stale-frame comment in [`try_run_query`].
const QUERY_ID: u64 = 1;

/// What `hop`'s argument list resolved to. Kept separate from the code that
/// acts on it — [`parse`] never touches a socket or prints anything — so the
/// five `*_is_usage` / `*_parses` tests below exercise the parsing rule
/// alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `hop version`.
    Version,
    /// `hop query <text>`, carrying the text as typed.
    Query(String),
    /// Anything else: no arguments, an unrecognized subcommand, or `query`
    /// with nothing after it.
    Usage,
}

/// The line `main` prints to stderr for [`Command::Usage`].
pub const USAGE: &str = "usage: hop version | hop query <text>";

/// Parses `args` — the process's arguments with `argv[0]` already stripped —
/// into a [`Command`].
pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Command {
    match args.next().as_deref() {
        Some("version") => Command::Version,
        Some("query") => match args.next() {
            Some(text) => Command::Query(text),
            None => Command::Usage,
        },
        _ => Command::Usage,
    }
}

/// Prints `hop <CARGO_PKG_VERSION>` and `protocol <API_VERSION>` to stdout.
///
/// `CARGO_PKG_VERSION` is resolved by `env!` against *this* crate's
/// manifest, so it reports `hop-cli`'s own version, not `hopd`'s or the
/// workspace's — the two need not move together.
pub fn print_version() {
    println!("hop {}", env!("CARGO_PKG_VERSION"));
    println!("protocol {API_VERSION}");
}

/// Runs `hop query <text>`: connects to `hopd`, performs the handshake,
/// sends the query, and prints each returned item as one line of JSON.
///
/// Returns the process's exit code rather than a `Result` — every error this
/// flow can hit is reported to stderr and mapped to exit code 1 right here,
/// per the behavior spec, so there is nothing left for `main` to decide.
pub fn run_query(text: &str) -> ExitCode {
    match try_run_query(text) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hop: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Everything that can go wrong on the query flow, each variant naming the
/// step it failed at so the stderr line says something more useful than
/// "it broke".
#[derive(Debug)]
enum QueryError {
    RuntimeDirUnset,
    Connect(std::io::Error),
    Io(std::io::Error),
    Frame(FrameError),
    Bound(BoundError),
    Encode(serde_json::Error),
    UnexpectedHandshakeReply(DaemonMsg),
    Daemon(ProtoError),
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::RuntimeDirUnset => write!(f, "XDG_RUNTIME_DIR is not set"),
            QueryError::Connect(err) => write!(f, "failed to connect to hopd: {err}"),
            QueryError::Io(err) => write!(f, "lost the connection to hopd: {err}"),
            QueryError::Frame(err) => write!(f, "{err}"),
            QueryError::Bound(err) => write!(f, "{err}"),
            QueryError::Encode(err) => write!(f, "failed to encode a returned item: {err}"),
            QueryError::UnexpectedHandshakeReply(msg) => {
                write!(f, "hopd did not acknowledge the handshake, got {msg:?}")
            }
            QueryError::Daemon(err) => write!(f, "hopd reported {:?}: {}", err.code, err.message),
        }
    }
}

/// Derives `hopd`'s socket path from `XDG_RUNTIME_DIR`, the same convention
/// `hopd::runtime_dir` uses to create it.
fn socket_path() -> Result<PathBuf, QueryError> {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").map_err(|_| QueryError::RuntimeDirUnset)?;
    Ok(PathBuf::from(runtime_dir).join("hop").join("hopd.sock"))
}

fn send(stream: &mut UnixStream, msg: &ClientMsg) -> Result<(), QueryError> {
    let frame = encode_frame(msg).map_err(QueryError::Frame)?;
    stream.write_all(&frame).map_err(QueryError::Io)
}

fn recv(stream: &mut UnixStream) -> Result<DaemonMsg, QueryError> {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream.read_exact(&mut prefix).map_err(QueryError::Io)?;
    let len = payload_len(prefix).map_err(QueryError::Frame)?;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(QueryError::Io)?;
    decode_payload(&payload).map_err(QueryError::Frame)
}

fn try_run_query(text: &str) -> Result<(), QueryError> {
    let socket_path = socket_path()?;
    let mut stream = UnixStream::connect(&socket_path).map_err(QueryError::Connect)?;

    send(
        &mut stream,
        &ClientMsg::Hello {
            api_version: API_VERSION,
        },
    )?;
    match recv(&mut stream)? {
        DaemonMsg::HelloAck { .. } => {}
        DaemonMsg::Error { error, .. } => return Err(QueryError::Daemon(error)),
        other => return Err(QueryError::UnexpectedHandshakeReply(other)),
    }

    let query_text = QueryText::new(text).map_err(QueryError::Bound)?;
    send(
        &mut stream,
        &ClientMsg::Query {
            id: QUERY_ID,
            text: query_text,
        },
    )?;

    loop {
        match recv(&mut stream)? {
            DaemonMsg::Results {
                query_id, items, ..
            } if query_id == QUERY_ID => {
                for item in &items {
                    let line = serde_json::to_string(item).map_err(QueryError::Encode)?;
                    println!("{line}");
                }
            }
            DaemonMsg::QueryDone { query_id } if query_id == QUERY_ID => return Ok(()),
            DaemonMsg::Error { error, .. } => return Err(QueryError::Daemon(error)),
            // A frame naming a `query_id` other than this connection's one
            // outstanding query is not this query's answer. This CLI never
            // sends `Cancel`, so it should never see one in practice; a real
            // multi-query client's frame-demultiplexing is issue #55's
            // slice, not this walking skeleton's.
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn version_parses() {
        assert_eq!(parse(["version".to_string()].into_iter()), Command::Version);
    }

    #[test]
    fn query_with_text_parses() {
        assert_eq!(
            parse(["query".to_string(), "hello".to_string()].into_iter()),
            Command::Query("hello".to_string())
        );
    }

    #[test]
    fn query_without_text_is_usage() {
        assert_eq!(parse(["query".to_string()].into_iter()), Command::Usage);
    }

    #[test]
    fn no_args_is_usage() {
        assert_eq!(parse(std::iter::empty::<String>()), Command::Usage);
    }

    #[test]
    fn unknown_subcommand_is_usage() {
        assert_eq!(
            parse(["frobnicate".to_string()].into_iter()),
            Command::Usage
        );
    }
}
