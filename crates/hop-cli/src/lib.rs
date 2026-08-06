//! `hop` — the command-line client for the hop launcher daemon.
//!
//! Three subcommands exist today: `hop version`, which needs no daemon,
//! `hop query <text>...`, and `hop exec <query> <item-id> <action-id>`, which
//! runs an action on one of the query's results. All three (except `version`)
//! speak the same length-prefixed JSON framing [`hopd`](../hopd/index.html)
//! does over the same Unix socket, using `hop_protocol`'s IO-free codec on
//! both ends of the pipe (see that crate's `framing` module docs for why the
//! codec itself has no socket code in it).
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
//! Meanwhile the hand-rolled match in [`parse`] stays in: `query` and `exec`
//! both take positional arguments (joined free text plus a fixed tail of ids
//! for `exec`), with no flags or `--help` to generate. That trade tips the
//! other way once `toggle` and `doctor` land — multiple flags,
//! subcommand-specific options, and `--help` text generated rather than
//! hand-maintained are what a parser earns its dependency weight back on.
//! Until then, a parser here would be solving a problem this binary does not
//! have yet, and it keeps this crate's only dependency beyond `hop-protocol`
//! and `serde_json` at zero.

use std::env;
use std::fmt;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use hop_protocol::framing::{
    FRAME_PREFIX_LEN, FrameError, decode_payload, encode_frame, payload_len,
};
use hop_protocol::{
    API_VERSION, ActionId, BoundError, ClientMsg, DaemonMsg, ErrorCode, ExecOutcome, Item, ItemId,
    ProtoError, QueryText,
};

/// The `id` this CLI sends on its one `Query` frame per process. There is
/// only ever one query in flight on this connection, so a fixed id (rather
/// than a counter) is enough to tell "this query's answer" apart from a
/// stray frame — see the stale-frame comment in [`try_run_query`].
const QUERY_ID: u64 = 1;

/// What `hop`'s argument list resolved to. Kept separate from the code that
/// acts on it — [`parse`] never touches a socket or prints anything — so the
/// `*_parses` / `*_is_usage` tests below exercise the parsing rule alone.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `hop version`.
    Version,
    /// `hop query <text>...`, carrying every argument after `query` joined
    /// with single spaces into one query string — see [`parse`]'s doc
    /// comment for why a single token would silently drop words.
    Query(String),
    /// `hop exec <query> <item-id> <action-id>`: run `action-id` on
    /// `item-id`, an item the query returns. The query is every token except
    /// the trailing two, joined with single spaces; the trailing two are the
    /// validated item and action ids.
    Exec {
        query: String,
        item_id: ItemId,
        action_id: ActionId,
    },
    /// Anything else: no arguments, an unrecognized subcommand, `query` or
    /// `exec` with too few arguments, or an out-of-bounds id.
    Usage,
}

/// The line `main` prints to stderr for [`Command::Usage`].
pub const USAGE: &str =
    "usage: hop version | hop query <text>... | hop exec <query> <item-id> <action-id>";

/// Parses `args` — the process's arguments with `argv[0]` already stripped —
/// into a [`Command`].
///
/// `hop query hello world`'s two tokens after `query` are joined with single
/// spaces into one query string (`"hello world"`), not just the first token
/// — `query` takes free text, and a shell hands that text over unquoted as
/// one argument per word, so treating only `args.next()` as the query would
/// silently drop every word after the first.
pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Command {
    match args.next().as_deref() {
        Some("version") => Command::Version,
        Some("query") => {
            let tokens: Vec<String> = args.collect();
            if tokens.is_empty() {
                Command::Usage
            } else {
                Command::Query(tokens.join(" "))
            }
        }
        Some("exec") => {
            // `exec` needs a query and exactly two ids: the item id and the
            // action id. Everything after `exec` except the trailing two is
            // the query (joined with single spaces, exactly like `query`), so
            // the last two tokens are popped off and validated as ids. At
            // least one token must remain as the query and at least three
            // tokens total — `hop exec <item> <action>` does not name a query
            // and is refused as usage.
            let mut tokens: Vec<String> = args.collect();
            let Some(action_id) = tokens.pop() else {
                return Command::Usage;
            };
            let Some(item_id) = tokens.pop() else {
                return Command::Usage;
            };
            if tokens.is_empty() {
                return Command::Usage;
            }
            let query = tokens.join(" ");
            match (ItemId::new(item_id), ActionId::new(action_id)) {
                (Ok(item_id), Ok(action_id)) => Command::Exec {
                    query,
                    item_id,
                    action_id,
                },
                // An id over its documented bound cannot name any item this
                // daemon could have delivered.
                _ => Command::Usage,
            }
        }
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

/// Runs `hop query <text>...`: connects to `hopd`, performs the handshake,
/// sends the query, and assembles the streamed results and prints them once
/// `query_done` arrives.
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

/// Everything that can go wrong on a query or exec flow, each variant naming
/// the step it failed at so the stderr line says something more useful than
/// "it broke".
///
/// The three refusal variants mirror the daemon's query-scoped
/// [`ErrorCode`]s of the same names — and also cover the refusals this client
/// can recognize *before* the daemon does (an item or action it resolves
/// against the frame it already holds). `run_query` maps every variant to
/// exit 1; `run_exec` maps the three refusal variants to their dedicated exit
/// codes (see [`run_exec`]) and everything else to 1.
#[derive(Debug)]
enum ClientError {
    RuntimeDirUnset,
    Connect(std::io::Error),
    Io(std::io::Error),
    Frame(FrameError),
    Bound(BoundError),
    Encode(serde_json::Error),
    UnexpectedHandshakeReply(DaemonMsg),
    Daemon(ProtoError),
    /// The daemon refused an execute because it did not deliver the item
    /// (or the query id was stale), or this client's own resolution found no
    /// such item in the last frame it holds.
    UnknownItem(String),
    /// The daemon refused an execute because the item does not offer the
    /// action, or this client's own resolution found no such action on the
    /// item it holds.
    UnknownAction(String),
    /// The daemon reported that the item's provider could not perform the
    /// action.
    ProviderFailed(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::RuntimeDirUnset => write!(f, "XDG_RUNTIME_DIR is not set"),
            ClientError::Connect(err) => write!(f, "failed to connect to hopd: {err}"),
            ClientError::Io(err) => write!(f, "lost the connection to hopd: {err}"),
            ClientError::Frame(err) => write!(f, "{err}"),
            ClientError::Bound(err) => write!(f, "{err}"),
            ClientError::Encode(err) => write!(f, "failed to encode a returned item: {err}"),
            ClientError::UnexpectedHandshakeReply(msg) => {
                write!(f, "hopd did not acknowledge the handshake, got {msg:?}")
            }
            ClientError::Daemon(err) => write!(f, "hopd reported {:?}: {}", err.code, err.message),
            ClientError::UnknownItem(id) => write!(f, "no such item: {id}"),
            ClientError::UnknownAction(id) => write!(f, "no such action: {id}"),
            ClientError::ProviderFailed(what) => write!(f, "provider failed to execute: {what}"),
        }
    }
}

/// Derives `hopd`'s socket path from `XDG_RUNTIME_DIR`, the same convention
/// `hopd::runtime_dir` uses to create it.
fn socket_path() -> Result<PathBuf, ClientError> {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").map_err(|_| ClientError::RuntimeDirUnset)?;
    Ok(PathBuf::from(runtime_dir).join("hop").join("hopd.sock"))
}

fn send(stream: &mut UnixStream, msg: &ClientMsg) -> Result<(), ClientError> {
    let frame = encode_frame(msg).map_err(ClientError::Frame)?;
    stream.write_all(&frame).map_err(ClientError::Io)
}

fn recv(stream: &mut UnixStream) -> Result<DaemonMsg, ClientError> {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream.read_exact(&mut prefix).map_err(ClientError::Io)?;
    let len = payload_len(prefix).map_err(ClientError::Frame)?;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(ClientError::Io)?;
    decode_payload(&payload).map_err(ClientError::Frame)
}

/// Connects, handshakes, sends `text` as one `Query`, and reads until that
/// query's `QueryDone`, returning the live socket (still open for the caller
/// to send an `Execute` on) and the items of the **last** `Results` frame —
/// the same retained-set rule the daemon's `Exchange::delivered` uses, so the
/// caller resolves an exec against exactly what it was shown.
///
/// A query-scoped `Error` naming this client's own [`QUERY_ID`] ends the
/// exchange here (the query failed; there is nothing to resolve against), as
/// does a connection-scoped (`None`) error; any other id is a stale frame and
/// is dropped, exactly as in the query flow.
fn connect_and_query(text: &str) -> Result<(UnixStream, Vec<Item>), ClientError> {
    let socket_path = socket_path()?;
    let mut stream = UnixStream::connect(&socket_path).map_err(ClientError::Connect)?;

    send(
        &mut stream,
        &ClientMsg::Hello {
            api_version: API_VERSION,
        },
    )?;
    match recv(&mut stream)? {
        DaemonMsg::HelloAck { .. } => {}
        DaemonMsg::Error { error, .. } => return Err(ClientError::Daemon(error)),
        other => return Err(ClientError::UnexpectedHandshakeReply(other)),
    }

    let query_text = QueryText::new(text).map_err(ClientError::Bound)?;
    send(
        &mut stream,
        &ClientMsg::Query {
            id: QUERY_ID,
            text: query_text,
        },
    )?;

    let mut assembled: Vec<Item> = Vec::new();
    loop {
        match recv(&mut stream)? {
            DaemonMsg::Results {
                query_id, items, ..
            } if query_id == QUERY_ID => {
                // Replace, not extend — see `DaemonMsg::Results`'s doc
                // comment for the replace rule this implements. There is no
                // exchange-total to cap here either: one frame is one
                // complete list, already refused at the parse by
                // `de_results_items` if it is over-long, so nothing this
                // code does could observe an oversized frame to guard
                // against.
                assembled = items;
            }
            DaemonMsg::QueryDone { query_id } if query_id == QUERY_ID => {
                return Ok((stream, assembled));
            }
            // An `Error`'s `query_id` says what the error is about, and that
            // is what decides whether it ends this query — see
            // `DaemonMsg::Error`'s contract. `None` scopes it to the
            // connection or to a frame that named no query, and this process
            // has nothing to fall back on either way, so it is fatal here.
            DaemonMsg::Error {
                query_id: None,
                error,
            } => return Err(ClientError::Daemon(error)),
            // `Some(id)` is terminal for that exchange alone. Naming this
            // one ends it; naming any other is a stale frame and falls
            // through below. This is also the path #59's refusals take now
            // that hopd sends query-scoped `UnknownItem` / `UnknownAction` /
            // `ProviderFailed` frames.
            DaemonMsg::Error {
                query_id: Some(id),
                error,
            } if id == QUERY_ID => return Err(ClientError::Daemon(error)),
            // Any other id is a stale frame — a `results`, `query_done` or
            // `error` for a query this process is no longer (or was never)
            // waiting on. This CLI only ever has one query in flight and
            // never sends `Cancel`, so it should never see one in practice.
            // Dropped unrendered here, that is the client half of the
            // lifecycle contract (#55), not a permissive default.
            _ => continue,
        }
    }
}

fn try_run_query(text: &str) -> Result<(), ClientError> {
    let (_stream, assembled) = connect_and_query(text)?;
    for item in &assembled {
        let line = serde_json::to_string(item).map_err(ClientError::Encode)?;
        println!("{line}");
    }
    Ok(())
}

/// Maps a daemon [`ProtoError`] to a [`ClientError`], translating the three
/// execute refusals into their typed variants so `run_exec` can pick the
/// right exit code off them. Anything else is an ordinary daemon error.
fn map_daemon_error(error: ProtoError) -> ClientError {
    match error.code {
        ErrorCode::UnknownItem => ClientError::UnknownItem(error.message),
        ErrorCode::UnknownAction => ClientError::UnknownAction(error.message),
        ErrorCode::ProviderFailed => ClientError::ProviderFailed(error.message),
        _ => ClientError::Daemon(error),
    }
}

/// Runs `hop exec <query> <item-id> <action-id>` end to end: connect and
/// handshake, run the query, resolve the item and action against the last
/// results frame (issue #59's live-result-set binding — the item must be one
/// the daemon actually delivered under this query id), send the `Execute`
/// frame, and surface the daemon's reply.
///
/// Local resolution mirrors what the daemon does, so a refusal is recognized
/// even before the `Execute` frame is sent: an item id holding no delivered
/// item is [`ClientError::UnknownItem`], and an action the item does not
/// offer is [`ClientError::UnknownAction`]. The `Execute` reply then carries
/// the same refusals back (this is the authoritative source once the frame is
/// dispatched) and the daemon's [`ErrorCode::ProviderFailed`] as the third
/// refusal. All three are query-scoped and non-terminal to the connection;
/// the sole exchange ends with the daemon's `Executed` or a matching error.
fn try_run_exec(
    query: &str,
    item_id: ItemId,
    action_id: ActionId,
) -> Result<ExecOutcome, ClientError> {
    let (mut stream, assembled) = connect_and_query(query)?;

    let Some(item) = assembled.iter().find(|i| i.id == item_id) else {
        return Err(ClientError::UnknownItem(item_id.as_str().to_string()));
    };
    if !item.actions.iter().any(|a| a.id == action_id) {
        return Err(ClientError::UnknownAction(action_id.as_str().to_string()));
    }

    send(
        &mut stream,
        &ClientMsg::Execute {
            query_id: QUERY_ID,
            item_id,
            action_id,
        },
    )?;

    loop {
        match recv(&mut stream)? {
            DaemonMsg::Executed { query_id, outcome } if query_id == QUERY_ID => {
                return Ok(outcome);
            }
            DaemonMsg::Error {
                query_id: None,
                error,
            } => return Err(ClientError::Daemon(error)),
            DaemonMsg::Error {
                query_id: Some(id),
                error,
            } if id == QUERY_ID => return Err(map_daemon_error(error)),
            // A stale frame for a query this process is not waiting on.
            _ => continue,
        }
    }
}

/// Runs `hop exec <query> <item-id> <action-id>` and maps the outcome to an
/// exit code, printing the failure to stderr first.
///
/// Returns the process's exit code rather than a `Result` — every error this
/// flow can hit is reported to stderr and mapped right here, so there is
/// nothing left for `main` to decide. The numeric mapping is part of the
/// CLI's contract (issue #59, criterion 6):
///
/// | Exit | Meaning |
/// | --- | --- |
/// | 0 | the action executed |
/// | 1 | any other failure — connection, handshake, framing, an unexpected frame |
/// | 10 | unknown item: not delivered under the query id (or stale query id) |
/// | 11 | unknown action: the item does not offer it |
/// | 12 | the item's provider failed to perform the action |
///
/// Codes 10-12 are deliberately above the generic 1 so a script can tell the
/// three refusals apart from a transport failure.
pub fn run_exec(query: &str, item_id: ItemId, action_id: ActionId) -> ExitCode {
    match try_run_exec(query, item_id, action_id) {
        Ok(_) => ExitCode::SUCCESS,
        Err(ClientError::UnknownItem(id)) => {
            eprintln!("hop: no such item: {id}");
            ExitCode::from(10)
        }
        Err(ClientError::UnknownAction(id)) => {
            eprintln!("hop: no such action: {id}");
            ExitCode::from(11)
        }
        Err(ClientError::ProviderFailed(what)) => {
            eprintln!("hop: provider failed to execute: {what}");
            ExitCode::from(12)
        }
        Err(err) => {
            eprintln!("hop: {err}");
            ExitCode::FAILURE
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
    fn query_with_multiple_tokens_joins_them() {
        assert_eq!(
            parse(
                [
                    "query".to_string(),
                    "hello".to_string(),
                    "world".to_string()
                ]
                .into_iter()
            ),
            Command::Query("hello world".to_string())
        );
    }

    #[test]
    fn query_without_text_is_usage() {
        assert_eq!(parse(["query".to_string()].into_iter()), Command::Usage);
    }

    #[test]
    fn exec_with_query_and_ids_parses() {
        assert_eq!(
            parse(
                [
                    "exec".to_string(),
                    "hello".to_string(),
                    "app:1".to_string(),
                    "open".to_string(),
                ]
                .into_iter()
            ),
            Command::Exec {
                query: "hello".to_string(),
                item_id: ItemId::new("app:1").unwrap(),
                action_id: ActionId::new("open").unwrap(),
            }
        );
    }

    #[test]
    fn exec_joins_query_tokens_and_keeps_the_trailing_ids() {
        // Every token before the trailing two is the query, joined like
        // `query` does; only the last two are ids.
        assert_eq!(
            parse(
                [
                    "exec".to_string(),
                    "open".to_string(),
                    "calc".to_string(),
                    "app:1".to_string(),
                    "run".to_string(),
                ]
                .into_iter()
            ),
            Command::Exec {
                query: "open calc".to_string(),
                item_id: ItemId::new("app:1").unwrap(),
                action_id: ActionId::new("run").unwrap(),
            }
        );
    }

    #[test]
    fn exec_with_only_one_token_is_usage() {
        assert_eq!(parse(["exec".to_string()].into_iter()), Command::Usage);
    }

    #[test]
    fn exec_with_only_ids_and_no_query_is_usage() {
        // Two tokens after `exec` name the ids but leave no query text.
        assert_eq!(
            parse(["exec".to_string(), "app:1".to_string(), "open".to_string(),].into_iter()),
            Command::Usage
        );
    }

    #[test]
    fn exec_with_an_out_of_bounds_id_is_usage() {
        // An id over its documented bound cannot name anything the daemon
        // could have delivered, so parse refuses it rather than sending a
        // frame that can only be refused.
        assert_eq!(
            parse(
                [
                    "exec".to_string(),
                    "q".to_string(),
                    "x".repeat(10_000),
                    "open".to_string(),
                ]
                .into_iter()
            ),
            Command::Usage
        );
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
