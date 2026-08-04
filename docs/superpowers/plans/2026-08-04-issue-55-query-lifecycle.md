# Query Lifecycle (Issue #55) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the walking skeleton's single-frame reply with the real
query lifecycle: query ids on every frame, streamed partial results, a new
query cancelling the previous one server-side, client-side stale-frame drop,
and a bounded per-query retained result set.

**Architecture:** `hopd` grows a `ResultSource` seam (the thing that answers
one query with a stream of item batches) and a per-connection select loop
that multiplexes client frames against source batches. Reading moves to a
dedicated reader task per connection, because `tokio::select!` cancels losing
futures and a cancelled half-read frame would desync the stream. The daemon
retains what it delivered for the current query id, capped at a new
`hop-protocol` constant; at the cap it stops the source and terminates the
query rather than evicting — an item the daemon delivered stays resolvable,
which is the property issue #59's execute binding needs. The CLI assembles
the streamed list, drops frames for stale query ids, and refuses a daemon
that streams past the cap.

**Tech Stack:** Rust, tokio (`net`, `sync`, `macros`, `io-util`,
`rt-multi-thread`), serde/serde_json, `hop-protocol`'s IO-free framing.

## Global Constraints

- Gate commands, all four must pass at every commit: `cargo test --workspace`
  · `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D
  warnings` · `cargo deny check`.
- `clippy::unwrap_used` is denied in production code; test files and modules
  open with `#![allow(clippy::unwrap_used)]` / `#!` at module scope as the
  existing files do.
- No `.unwrap()`/`.expect()` in prod code except `expect` on invariants
  constructed in the same function (existing `hardcoded_item` pattern).
- Path deps must carry `version` alongside `path` (cargo-deny wildcard ban).
- No AI attribution anywhere. Comment style: comments state constraints the
  code can't, in the discursive style the existing modules use.
- The issue brief (#55) is the spec of record; its acceptance criteria are
  listed per task below.

## Design decisions (read before any task)

1. **The cap and its behavior.** New constant
   `hop_protocol::limits::MAX_ITEMS_PER_QUERY: usize = 5_000` — the total
   item count one query id may deliver across all its `results` frames,
   distinct from `MAX_ITEMS_PER_RESULTS_FRAME` (1 000), which bounds one
   frame. At the cap the daemon **refuses further items**: it truncates the
   batch that crossed the line, delivers what fit, sends `QueryDone`, and
   drops the source. It never evicts. Rationale: the retained set exists so
   #59 can resolve an `Execute { query_id, item_id }` against what was
   actually shown; eviction would make a delivered item silently
   unresolvable, refusal cannot.
2. **`partial` is advisory; `QueryDone` is the terminal signal.** Every
   streamed `results` frame the daemon sends carries `partial: true`; the
   exchange ends with `QueryDone`, never with a `partial: false` frame.
   Clients must key on `QueryDone`. (This changes the skeleton's single
   `partial: false` reply — the round-trip test's assertion flips.)
3. **Cancellation semantics.**
   - A new `Query` on a connection cancels the connection's active query:
     the daemon drops the source receiver (the source observes send failure
     and stops), sends **no** further frames for the old id — no `QueryDone`
     for it — and starts the new query. The client has moved on; a
     `QueryDone` for the superseded id would be dropped as stale anyway.
   - `Cancel { id }` naming the active query stops it the same way and
     replies `QueryDone { query_id: id }`, so a client that cancels gets a
     definite end-of-exchange. `Cancel` naming anything else is dropped
     silently — a cancel racing a natural `QueryDone` is ordinary traffic,
     not an error.
4. **Retention.** Per connection, the daemon retains the items delivered for
   the **most recent** query id (`Delivered { query_id, items }`), bounded
   by `MAX_ITEMS_PER_QUERY`. A new query replaces the retained set; `Cancel`
   and `QueryDone` leave it in place (what was delivered stays resolvable
   until the next query). Nothing reads it yet — #59 will — but the cap
   tests pin the bound now.
5. **The reader task.** `handle_connection` splits the stream
   (`into_split`), spawns a reader task that loops `read_frame` and forwards
   `ReadEvent`s over an `mpsc::channel(1)`, and drives a select loop over
   that channel and the active source receiver. `mpsc::Receiver::recv` is
   cancel-safe; a raw `read_exact` in a `select!` arm is not — a losing
   branch would drop a half-read frame and desync the connection. On exit
   the driver aborts the reader so a mute peer doesn't leak the task
   (indefinite-read exposure itself is #98's, unchanged here).
6. **Sources.** `ResultSource::start(&self, text: QueryText) ->
   mpsc::Receiver<Vec<Item>>` — the source hands back a receiver and does
   its work behind it; dropping the receiver is cancellation. The production
   source until #56 is `SkeletonSource`: one batch holding the same
   hardcoded item as today, then done. Integration tests inject scripted
   sources via `serve_with`.

## File Structure

- Modify: `crates/hop-protocol/src/limits.rs` — add `MAX_ITEMS_PER_QUERY`.
- Modify: `crates/hop-protocol/src/wire.rs` — lifecycle doc comments only.
- Create: `crates/hopd/src/source.rs` — `ResultSource`, `SkeletonSource`,
  `hardcoded_item`.
- Create: `crates/hopd/src/connection.rs` — reader task, `ReadEvent`, the
  driver select loop, `HandshakeState`, retention/cap, frame send helpers.
- Modify: `crates/hopd/src/server.rs` — slims to bind/accept; `serve` +
  `serve_with`.
- Modify: `crates/hopd/src/lib.rs` — module wiring + crate doc update.
- Create: `crates/hopd/tests/common/mod.rs` — shared client-side helpers.
- Modify: `crates/hopd/tests/socket.rs` — use common helpers; flip `partial`
  assertion.
- Create: `crates/hopd/tests/lifecycle.rs` — streaming, cancellation, cap
  over a real socket with scripted sources.
- Modify: `crates/hop-cli/src/lib.rs` — assemble-after-done, stale-drop, cap.
- Modify: `crates/hop-cli/tests/e2e.rs` — fake-daemon tests for stale-drop,
  assembly, cap refusal.
- Modify: `README.md` — the "one hardcoded item / single reply" wording.

---

### Task 1: `MAX_ITEMS_PER_QUERY` in hop-protocol

**Files:**
- Modify: `crates/hop-protocol/src/limits.rs`
- Modify: `crates/hop-protocol/src/wire.rs` (doc comments only)

**Interfaces:**
- Produces: `pub const MAX_ITEMS_PER_QUERY: usize = 5_000;` in
  `hop_protocol::limits` (re-exported via the existing `limits` module path;
  check `lib.rs` — if other `MAX_*` constants are re-exported at crate root,
  match that; otherwise `hop_protocol::limits::MAX_ITEMS_PER_QUERY` is the
  path every later task uses).

- [ ] **Step 1: Write the failing test** — in `limits.rs`'s existing `tests` module:

```rust
#[test]
fn the_per_query_cap_admits_at_least_one_full_frame() {
    // A cap below one frame's bound would make a single maximal `results`
    // frame unrepresentable: the daemon could accept the frame's items and
    // immediately have to truncate them. The relation, not either number,
    // is the invariant.
    assert!(MAX_ITEMS_PER_QUERY >= MAX_ITEMS_PER_RESULTS_FRAME);
}
```

- [ ] **Step 2: Run it, verify it fails to compile** (`MAX_ITEMS_PER_QUERY` unresolved):
`cargo test -p hop-protocol the_per_query_cap -- --nocapture` → compile error.

- [ ] **Step 3: Add the constant** in `limits.rs`, after `MAX_ITEMS_PER_RESULTS_FRAME`:

```rust
/// Maximum total items one query id may deliver, summed across every
/// `results` frame of the exchange.
///
/// [`MAX_ITEMS_PER_RESULTS_FRAME`] bounds one frame; nothing bounds how many
/// partial frames a daemon sends for the same `query_id`, so without this
/// cap a merely chatty client drives unbounded retained state in the daemon
/// — the daemon keeps what it delivered per query id so `execute` can
/// resolve against it (issue #59, and the threat model's #25 decision,
/// which holds only while that state is bounded).
///
/// Unlike its neighbours this bound is not enforced at the parse: no single
/// frame breaks it. Transports enforce it at accumulation — the daemon caps
/// what it retains and delivers (truncating the crossing batch and ending
/// the query; never evicting, because a delivered item must stay
/// resolvable), and a client caps what it assembles (refusing a daemon that
/// streams past it). That is the same posture [`MAX_FRAME_BYTES`] takes:
/// declared here, applied by the transport.
///
/// 5 000 is five maximal frames. Honest traffic is two orders of magnitude
/// smaller — a launcher renders tens of items — so this is a memory guard,
/// not a display guard: at the composed per-item worst case (84 160 bytes,
/// see the module docs) it holds retained state per connection under
/// ~421 MB hostile-shaped, ~1 MB honest-shaped.
pub const MAX_ITEMS_PER_QUERY: usize = 5_000;
```

- [ ] **Step 4: Run the test, verify it passes**:
`cargo test -p hop-protocol the_per_query_cap`  → PASS.

- [ ] **Step 5: Lifecycle doc comments in `wire.rs`** (no behavior change):
  - On `ClientMsg::Query`: add — a `Query` on a connection with a query
    already active cancels that query server-side; the daemon sends no
    further frames for the superseded id, not even `QueryDone`.
  - On `ClientMsg::Cancel`: replace any placeholder wording with — cancels
    the active query if `id` names it (the daemon answers
    `QueryDone { query_id: id }`); dropped silently otherwise, because a
    cancel racing a natural `QueryDone` is ordinary traffic.
  - On `DaemonMsg::Results.partial` field doc: `partial` is advisory
    (`true` = more frames may follow); the terminal signal is
    [`DaemonMsg::QueryDone`], never a `partial: false` frame, and clients
    must key on it. Reference `MAX_ITEMS_PER_QUERY` as the exchange-total
    bound alongside the existing per-frame reference.
  - On `DaemonMsg::QueryDone`: the one terminal frame of a query exchange;
    sent when the source finishes, when the exchange hits
    [`MAX_ITEMS_PER_QUERY`](crate::limits::MAX_ITEMS_PER_QUERY), or in
    answer to a matching `Cancel` — but not for a query superseded by a new
    `Query`.

- [ ] **Step 6: Gate and commit**

```bash
cargo test -p hop-protocol && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
git add crates/hop-protocol/src/limits.rs crates/hop-protocol/src/wire.rs
git commit -m "protocol: cap the items one query may deliver, distinct from the per-frame bound"
```

---

### Task 2: `ResultSource` seam and `SkeletonSource`

**Files:**
- Create: `crates/hopd/src/source.rs`
- Modify: `crates/hopd/src/server.rs` (delete `hardcoded_item`, import it)
- Modify: `crates/hopd/src/lib.rs` (add `pub mod source;`)

**Interfaces:**
- Produces:
  - `pub trait ResultSource: Clone + Send + Sync + 'static { fn start(&self, text: QueryText) -> tokio::sync::mpsc::Receiver<Vec<Item>>; }`
  - `#[derive(Clone)] pub struct SkeletonSource;`
  - `pub(crate) fn hardcoded_item() -> Item` (moved verbatim from `server.rs`)
- Consumes: `hop_protocol::{Item, QueryText}`, tokio `sync` (already a
  workspace feature).

- [ ] **Step 1: Write the failing tests** — new file `crates/hopd/src/source.rs`, tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn the_skeleton_source_yields_one_batch_then_finishes() {
        let mut rx = SkeletonSource.start(QueryText::new("anything").unwrap());
        let batch = rx.recv().await.expect("one batch must arrive");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].title, "Hello from hopd");
        assert!(rx.recv().await.is_none(), "the source must finish after its one batch");
    }
}
```

- [ ] **Step 2: Write the module** above the tests in the same file:

```rust
//! The seam between a connection and whatever answers its queries.
//!
//! A [`ResultSource`] answers one query with a stream of item batches behind
//! an `mpsc::Receiver`. The channel is the whole contract: batches arrive on
//! it, the source finishing closes it, and the *caller dropping it is
//! cancellation* — a source notices its next `send` fail and stops working.
//! That makes cancellation a property of the seam rather than a protocol
//! bolted onto it, and it is what issue #55's "a new query cancels the old
//! one server-side" hangs off.
//!
//! Until issue #56 lands a provider host, the one production source is
//! [`SkeletonSource`], which answers every query with the same hardcoded
//! item the walking skeleton always has.

use hop_protocol::{Action, ActionId, ActionKind, Item, ItemId, Kind, QueryText};
use tokio::sync::mpsc;

/// Answers queries with streams of item batches.
///
/// `Clone` because every connection gets its own handle; implementations are
/// expected to be cheap handles over shared state, not the state itself.
pub trait ResultSource: Clone + Send + Sync + 'static {
    /// Starts answering one query. Batches arrive on the returned receiver;
    /// the channel closing means the source is done; dropping the receiver
    /// cancels the work.
    fn start(&self, text: QueryText) -> mpsc::Receiver<Vec<Item>>;
}

/// The walking skeleton's source: one batch, one hardcoded item, done.
#[derive(Clone)]
pub struct SkeletonSource;

impl ResultSource for SkeletonSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        // Capacity 1 makes `try_send` infallible here, and dropping `tx` on
        // return is what closes the channel after the one batch — no task
        // needed for a source with nothing to wait on.
        let (tx, rx) = mpsc::channel(1);
        let _ = tx.try_send(vec![hardcoded_item()]);
        rx
    }
}
```

Then move `hardcoded_item()` from `server.rs` into `source.rs` **verbatim**,
as `pub(crate) fn hardcoded_item() -> Item` (keep its doc comment), delete it
from `server.rs`, and have `server.rs` `use crate::source::hardcoded_item;`.
Add `pub mod source;` to `lib.rs`.

- [ ] **Step 3: Run the tests**:
`cargo test -p hopd` → all pass (existing socket tests still green — server
behavior unchanged).

- [ ] **Step 4: Gate and commit**

```bash
cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
git add crates/hopd/src/source.rs crates/hopd/src/server.rs crates/hopd/src/lib.rs
git commit -m "hopd: a ResultSource seam, with the skeleton item as its first source"
```

---

### Task 3: Shared test helpers (`tests/common`)

**Files:**
- Create: `crates/hopd/tests/common/mod.rs`
- Modify: `crates/hopd/tests/socket.rs`

**Interfaces:**
- Produces (in `common`): `pub fn send(stream: &mut UnixStream, msg: &ClientMsg)`,
  `pub fn recv(stream: &mut UnixStream) -> DaemonMsg`,
  `pub fn hello(stream: &mut UnixStream)` — moved **verbatim** from
  `socket.rs` (doc comments included). `DaemonProcess`/`spawn_daemon` stay in
  `socket.rs`: `lifecycle.rs` runs the daemon in-process, not as a child.

- [ ] **Step 1: Create `crates/hopd/tests/common/mod.rs`** — module doc plus the three helpers cut from `socket.rs`:

```rust
//! Client-side helpers shared by this crate's integration tests: framing one
//! message, reading one frame, and the handshake preamble. Kept as a `common`
//! module rather than duplicated per test file so a wire-contract change
//! shows up as one diff here, not a drift between suites.
#![allow(clippy::unwrap_used)]
```

(then `send`, `recv`, `hello` verbatim, each made `pub`.)

- [ ] **Step 2: Point `socket.rs` at it** — add `mod common;` and
`use common::{hello, recv, send};`, delete the moved definitions. Note:
`tests/common/mod.rs` (directory form), not `tests/common.rs`, so cargo does
not treat it as its own test binary.

- [ ] **Step 3: Run the suite**: `cargo test -p hopd` → identical set of tests, all green.

- [ ] **Step 4: Gate and commit**

```bash
cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
git add crates/hopd/tests/common/mod.rs crates/hopd/tests/socket.rs
git commit -m "hopd tests: share the client-side frame helpers"
```

---

### Task 4: The connection driver — streaming, cancellation, retention cap

This is the core task. It lands in one commit because the old
`handle_connection` shape cannot host streaming halfway: the select loop,
the reader task, and the source wiring replace it as a unit. The pure cap
helper is unit-tested first; the loop is pinned by integration tests over a
real socket in Task 5.

**Files:**
- Create: `crates/hopd/src/connection.rs`
- Modify: `crates/hopd/src/server.rs`
- Modify: `crates/hopd/src/lib.rs`
- Modify: `crates/hopd/tests/socket.rs` (one assertion)

**Interfaces:**
- Consumes: `crate::source::{ResultSource, SkeletonSource}` (Task 2).
- Produces:
  - `pub(crate) async fn handle_connection<S: ResultSource>(stream: UnixStream, source: S) -> io::Result<()>` in `connection.rs`.
  - `pub async fn serve(runtime_dir: &Path) -> io::Result<()>` (unchanged
    signature) and `pub async fn serve_with<S: ResultSource>(runtime_dir: &Path, source: S) -> io::Result<()>` in `server.rs`. `serve` delegates:
    `serve_with(runtime_dir, SkeletonSource).await`.
  - `fn take_within_cap(room: usize, batch: Vec<Item>) -> (Vec<Item>, bool)`
    (private, unit-tested in-module).

- [ ] **Step 1: Write the failing unit tests** for the cap helper, in `connection.rs`'s tests module:

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::source::hardcoded_item;

    fn batch_of(n: usize) -> Vec<Item> {
        std::iter::repeat_with(hardcoded_item).take(n).collect()
    }

    #[test]
    fn a_batch_under_the_room_passes_whole_and_does_not_cap() {
        let (taken, capped) = take_within_cap(10, batch_of(9));
        assert_eq!(taken.len(), 9);
        assert!(!capped);
    }

    #[test]
    fn a_batch_exactly_filling_the_room_caps() {
        // Filling the room exactly leaves nothing for a later batch, so the
        // exchange ends now rather than on the next batch's arrival.
        let (taken, capped) = take_within_cap(10, batch_of(10));
        assert_eq!(taken.len(), 10);
        assert!(capped);
    }

    #[test]
    fn a_batch_over_the_room_is_truncated_never_evicted() {
        let (taken, capped) = take_within_cap(10, batch_of(11));
        assert_eq!(taken.len(), 10, "the crossing batch is truncated to fit");
        assert!(capped);
    }

    #[test]
    fn zero_room_takes_nothing() {
        let (taken, capped) = take_within_cap(0, batch_of(3));
        assert!(taken.is_empty());
        assert!(capped);
    }
}
```

- [ ] **Step 2: Run them, verify failure** (`take_within_cap` unresolved):
`cargo test -p hopd take_within_cap` → compile error.

- [ ] **Step 3: Write `connection.rs`.** Move `HandshakeState`, `ReadOutcome`,
`read_frame`, `send_msg`, `send_error` out of `server.rs` and build the
driver around them. The complete module (doc comments abridged here to the
load-bearing ones; write them in the discursive house style, and carry over
the moved functions' existing doc comments — adjusting `read_frame`'s and the
send helpers' signatures to the split-half types):

```rust
//! One connection's protocol loop: the handshake gate, then the query
//! lifecycle — streamed results, server-side cancellation, and the bounded
//! retained result set.
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

use std::io;

use hop_protocol::framing::{FRAME_PREFIX_LEN, FrameError, decode_payload, encode_frame, payload_len};
use hop_protocol::limits::{MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, ErrorCode, Item, ProtoError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::source::ResultSource;

/// A connection's position in the handshake gate every frame passes through.
/// (move the existing doc comment from server.rs verbatim)
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
    /// The transport failed mid-read. The driver surfaces it to `serve`'s
    /// log seam; there is no peer left worth answering.
    Failed(io::Error),
}

/// The query this connection is currently streaming, if any.
struct ActiveQuery {
    id: u64,
    rx: mpsc::Receiver<Vec<Item>>,
}

/// What this connection delivered for its most recent query id — the state
/// issue #59's execute binding resolves against, and the state
/// [`MAX_ITEMS_PER_QUERY`] bounds. Replaced whole by the next query; kept
/// through `Cancel` and `QueryDone`, because an item already delivered must
/// stay resolvable until the client visibly moves on.
struct Delivered {
    query_id: u64,
    items: Vec<Item>,
}

/// Serves one accepted connection to completion.
pub(crate) async fn handle_connection<S: ResultSource>(
    stream: UnixStream,
    source: S,
) -> io::Result<()> {
    let (read_half, write_half) = stream.into_split();
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
            Ok(Some(outcome)) => match outcome {
                ReadOutcome::Message(msg) => ReadEvent::Message(msg),
                ReadOutcome::Refused { code, message } => ReadEvent::Refused { code, message },
            },
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

/// The driver: selects between the peer's frames and the active source's
/// batches, owning the handshake state, the active query, and the retained
/// result set.
async fn drive<S: ResultSource>(
    mut events: mpsc::Receiver<ReadEvent>,
    mut write_half: OwnedWriteHalf,
    source: S,
) -> io::Result<()> {
    let mut state = HandshakeState::AwaitingHello;
    let mut active: Option<ActiveQuery> = None;
    let mut delivered: Option<Delivered> = None;

    loop {
        tokio::select! {
            event = events.recv() => match event {
                None => return Ok(()), // EOF: the peer closed its end.
                Some(ReadEvent::Failed(err)) => return Err(err),
                Some(ReadEvent::Refused { code, message }) => {
                    send_error(&mut write_half, None, code, message).await?;
                    return Ok(());
                }
                Some(ReadEvent::Message(msg)) => {
                    if handle_message(&mut state, &mut active, &mut delivered, &mut write_half, &source, msg).await? {
                        return Ok(());
                    }
                }
            },
            // The guard keeps this arm out of the select entirely while no
            // query is active; the `expect` is unreachable by construction.
            batch = async { active.as_mut().expect("guarded by is_some").rx.recv().await }, if active.is_some() => {
                forward_batch(&mut active, &mut delivered, &mut write_half, batch).await?;
            }
        }
    }
}
```

`handle_message` — the state machine, returning `Ok(true)` when the
connection should close (version mismatch, handshake violation):

```rust
/// Applies one client frame to the connection's state. `Ok(true)` means the
/// connection is done and the driver should return.
async fn handle_message<S: ResultSource>(
    state: &mut HandshakeState,
    active: &mut Option<ActiveQuery>,
    delivered: &mut Option<Delivered>,
    write_half: &mut OwnedWriteHalf,
    source: &S,
    msg: ClientMsg,
) -> io::Result<bool> {
    match (&*state, msg) {
        (HandshakeState::AwaitingHello, ClientMsg::Hello { api_version })
            if api_version == API_VERSION =>
        {
            send_msg(write_half, &DaemonMsg::HelloAck { api_version: API_VERSION }).await?;
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
            // Dropping the previous query's receiver *is* the server-side
            // cancellation: the source's next send fails and it stops. No
            // frames follow for the superseded id — not even QueryDone; the
            // client that issued this query drops them as stale anyway.
            *active = Some(ActiveQuery { id, rx: source.start(text) });
            *delivered = Some(Delivered { query_id: id, items: Vec::new() });
            Ok(false)
        }
        (HandshakeState::Ready, ClientMsg::Cancel { id }) => {
            match active.take() {
                Some(query) if query.id == id => {
                    // Same mechanism as supersession, but acknowledged: the
                    // canceller is still waiting on this id, so it gets the
                    // exchange's terminal frame. `delivered` stays — what
                    // was shown remains resolvable until the next query.
                    send_msg(write_half, &DaemonMsg::QueryDone { query_id: id }).await?;
                }
                other => *active = other, // A stale cancel is ordinary traffic; drop it.
            }
            Ok(false)
        }
        (HandshakeState::Ready, _other) => {
            // A second `hello`, or `execute` (issue #59's slice): refused
            // per frame, the connection stays open.
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
```

`forward_batch` and the cap helper:

```rust
/// Forwards one source event — a batch, or the source finishing — to the
/// peer, retaining what was delivered and enforcing [`MAX_ITEMS_PER_QUERY`].
async fn forward_batch(
    active: &mut Option<ActiveQuery>,
    delivered: &mut Option<Delivered>,
    write_half: &mut OwnedWriteHalf,
    batch: Option<Vec<Item>>,
) -> io::Result<()> {
    let Some(query) = active.as_ref() else {
        return Ok(()); // Unreachable: the select arm is guarded on is_some.
    };
    let query_id = query.id;

    let Some(batch) = batch else {
        // The source finished: the exchange's terminal frame, then no more
        // arms for this query in the select.
        send_msg(write_half, &DaemonMsg::QueryDone { query_id }).await?;
        *active = None;
        return Ok(());
    };

    let retained = delivered
        .as_mut()
        .filter(|d| d.query_id == query_id)
        .expect("delivered is created with active and replaced with it");
    let room = MAX_ITEMS_PER_QUERY.saturating_sub(retained.items.len());
    let (accepted, capped) = take_within_cap(room, batch);

    retained.items.extend(accepted.iter().cloned());
    // A source batch may exceed what one frame is allowed to carry; the
    // per-frame bound is the wire's, so the split happens here, not in the
    // source's contract.
    for chunk in accepted.chunks(MAX_ITEMS_PER_RESULTS_FRAME) {
        send_msg(
            write_half,
            &DaemonMsg::Results { query_id, partial: true, items: chunk.to_vec() },
        )
        .await?;
    }

    if capped {
        // Refusal, not eviction: everything delivered stays retained and
        // resolvable; what didn't fit was never delivered. Dropping the
        // receiver stops the source.
        send_msg(write_half, &DaemonMsg::QueryDone { query_id }).await?;
        *active = None;
    }
    Ok(())
}

/// How much of `batch` fits in `room` more items, and whether the exchange
/// is now at its cap. Truncates the crossing batch; never touches what was
/// already accepted.
fn take_within_cap(room: usize, mut batch: Vec<Item>) -> (Vec<Item>, bool) {
    let capped = batch.len() >= room;
    batch.truncate(room);
    (batch, capped)
}
```

Move `ReadOutcome` + `read_frame` from `server.rs` verbatim, changing the
parameter type from `&mut UnixStream` to `&mut OwnedReadHalf` (the body is
unchanged — `AsyncReadExt` methods exist on the half). Move
`send_msg`/`send_error` verbatim with `&mut UnixStream` → `&mut
OwnedWriteHalf`.

- [ ] **Step 4: Slim `server.rs`** to bind/accept + `serve`/`serve_with`:

```rust
use crate::connection::handle_connection;
use crate::source::{ResultSource, SkeletonSource};

/// Binds `<runtime_dir>/hopd.sock` and serves connections with the
/// production source. (keep the existing doc comment on the bind/permission
/// behavior, unchanged)
pub async fn serve(runtime_dir: &Path) -> io::Result<()> {
    serve_with(runtime_dir, SkeletonSource).await
}

/// [`serve`], generic over the source — the integration seam: tests inject
/// scripted sources here and drive them over the same real socket
/// production traffic takes.
pub async fn serve_with<S: ResultSource>(runtime_dir: &Path, source: S) -> io::Result<()> {
    // ...existing bind/permissions/accept-loop body, with the spawn line
    // becoming:
    //   let source = source.clone();
    //   tokio::spawn(async move {
    //       if let Err(err) = handle_connection(stream, source).await { ... }
    //   });
}
```

Add `pub(crate) mod connection;` (and keep `pub mod source;`) in `lib.rs`;
update `lib.rs`'s crate doc: the skeleton's "single hardcoded item answering
every query" sentence becomes the query lifecycle summary (ids, streaming,
cancellation, bounded retention; providers still #56's).

- [ ] **Step 5: Flip the round-trip assertion** in `tests/socket.rs`
(`the_round_trip_returns_one_item_end_to_end`): `assert!(!partial)` becomes
`assert!(partial, "streamed results frames are partial; QueryDone is the terminal signal")`.
Everything else in that test stays — same item, then `QueryDone { query_id: 7 }`.

- [ ] **Step 6: Run the full suite**:
`cargo test --workspace` → unit tests from Step 1 pass; socket tests pass
with the flipped assertion; e2e still passes (the CLI already loops to
`QueryDone`).

- [ ] **Step 7: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check
git add crates/hopd/src/connection.rs crates/hopd/src/server.rs crates/hopd/src/lib.rs crates/hopd/tests/socket.rs
git commit -m "hopd: the query lifecycle — streaming, cancellation, and the bounded retained set"
```

---

### Task 5: Lifecycle integration tests over a real socket

**Files:**
- Create: `crates/hopd/tests/lifecycle.rs`

**Interfaces:**
- Consumes: `hopd::server::serve_with`, `hopd::source::ResultSource`,
  `common::{send, recv, hello}` (Task 3),
  `hop_protocol::limits::{MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME}`.

Covers the brief's integration criteria: streaming (several frames before
done), observable cancellation, and the cap. Client-side stale-frame drop is
Task 6's (it is CLI behavior). Each test runs `serve_with` on a
multi-thread tokio runtime owned by the test, with a blocking `UnixStream`
client on the test thread — a real socket, an in-process daemon.

- [ ] **Step 1: Write the harness and the streaming test** (failing only if Task 4 misbehaves — these tests are the brief's acceptance evidence, written to fail loudly on any lifecycle regression):

```rust
//! Integration tests for the query lifecycle of issue #55, driven over a
//! real Unix socket against an in-process daemon whose source is scripted.
//! In-process rather than a spawned binary because cancellation must be
//! *observable*: only a source the test owns can report that its work
//! actually stopped.
#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use common::{hello, recv, send};
use hop_protocol::limits::{MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME};
use hop_protocol::{
    Action, ActionId, ActionKind, ClientMsg, DaemonMsg, Item, ItemId, Kind, QueryText,
};
use hopd::server::serve_with;
use hopd::source::ResultSource;
use tokio::sync::mpsc;

/// An in-process daemon on a scripted source, plus the runtime that hosts
/// it. Dropping this drops the runtime, which tears the server task and its
/// socket down with it.
struct TestDaemon {
    _runtime: tokio::runtime::Runtime,
    socket_path: PathBuf,
    _dir: tempfile::TempDir,
}

fn start_daemon<S: ResultSource>(source: S) -> TestDaemon {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = dir.path().to_path_buf();
    // serve_with expects the runtime dir itself (hopd's runtime_dir::resolve
    // is a binary-startup concern, not serve's); create the 0700 dir the
    // way resolve() would.
    let runtime_dir = root.join("hop");
    std::fs::create_dir(&runtime_dir).unwrap();
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    // needs: use std::os::unix::fs::PermissionsExt; at the top of the file.
    let serve_dir = runtime_dir.clone();
    runtime.spawn(async move {
        let _ = serve_with(&serve_dir, source).await;
    });

    let socket_path = runtime_dir.join("hopd.sock");
    for _ in 0..50 {
        if socket_path.exists() {
            return TestDaemon { _runtime: runtime, socket_path, _dir: dir };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("in-process hopd socket did not appear at {socket_path:?} within 5s");
}

/// A tiny item; `n` differentiates ids so assertions can tell items apart.
fn item(n: usize) -> Item {
    Item {
        id: ItemId::new(format!("test:{n}")).unwrap(),
        kind: Kind::Action,
        title: format!("item {n}"),
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

/// Polls `rx` for up to `deadline` — a regression hangs for seconds, not
/// forever. 10 ms matches the suite's existing poll idiom.
fn recv_event_within<T>(rx: &mut mpsc::UnboundedReceiver<T>, deadline: Duration) -> Option<T> {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if let Ok(event) = rx.try_recv() {
            return Some(event);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// A source that streams each scripted batch when the test releases it, and
/// reports on `events` when it observes cancellation (its send failing).
#[derive(Clone)]
struct ScriptedSource {
    batches: Vec<Vec<Item>>,
    events: mpsc::UnboundedSender<&'static str>,
}

impl ResultSource for ScriptedSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        let (tx, rx) = mpsc::channel(1);
        let batches = self.batches.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            for batch in batches {
                if tx.send(batch).await.is_err() {
                    let _ = events.send("cancelled");
                    return;
                }
            }
            let _ = events.send("finished");
        });
        rx
    }
}

#[test]
fn a_query_streams_several_results_frames_before_its_done_frame() {
    let (events, _events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(ScriptedSource {
        batches: vec![vec![item(1)], vec![item(2)], vec![item(3)]],
        events,
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(&mut stream, &ClientMsg::Query { id: 7, text: QueryText::new("q").unwrap() });

    let mut frames = 0;
    let mut total_items = 0;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id, partial, items } => {
                assert_eq!(query_id, 7, "every frame carries its query id");
                assert!(partial);
                frames += 1;
                total_items += items.len();
            }
            DaemonMsg::QueryDone { query_id } => {
                assert_eq!(query_id, 7);
                break;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert!(frames >= 2, "a single query must be able to produce several results frames, got {frames}");
    assert_eq!(total_items, 3);
}
```

(In `item(n)`: write the literal out — the `todo!` above is a placeholder
for *this plan's* brevity, not for the implementation. Mirror
`hardcoded_item`'s field-by-field shape with `ItemId::new(format!("test:{n}")).unwrap()`
and `title: format!("item {n}")`.)

- [ ] **Step 2: The cancellation test** — a source that would stream forever, provably stopped:

```rust
/// A source that streams batches forever until cancelled — cancellation is
/// the only way its work ever stops, so receiving its "cancelled" event is
/// proof the daemon stopped it rather than letting it run out.
#[derive(Clone)]
struct EndlessSource {
    events: mpsc::UnboundedSender<u64>,
    /// Which query this source is answering, stamped into its event so the
    /// test can tell which query's work stopped.
    query_tag: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl ResultSource for EndlessSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        let (tx, rx) = mpsc::channel(1);
        let events = self.events.clone();
        let tag = self.query_tag.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                n += 1;
                if tx.send(vec![item(n)]).await.is_err() {
                    let _ = events.send(tag);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        rx
    }
}

#[test]
fn a_second_query_cancels_the_first_observably() {
    let (events, mut events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(EndlessSource {
        events,
        query_tag: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(&mut stream, &ClientMsg::Query { id: 1, text: QueryText::new("first").unwrap() });
    // At least one frame of query 1 arrives, proving it was running.
    let DaemonMsg::Results { query_id: 1, .. } = recv(&mut stream) else {
        panic!("query 1 must stream before being cancelled");
    };

    send(&mut stream, &ClientMsg::Query { id: 2, text: QueryText::new("second").unwrap() });

    // The first source (tag 0) must observe cancellation: its work stops
    // rather than completing — it *cannot* complete; it is endless.
    let cancelled_tag = recv_event_within(&mut events_rx, Duration::from_secs(5))
        .expect("a cancellation event must arrive");
    assert_eq!(cancelled_tag, 0, "the first query's source is the one cancelled");

    // Frames still flowing belong to query 2 (any late query-1 frames were
    // written before the cancel landed; drain until a query-2 frame shows).
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 2, .. } => break,
            DaemonMsg::Results { query_id: 1, .. } => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}
```

- [ ] **Step 3: The explicit-cancel test**:

```rust
#[test]
fn a_cancel_frame_stops_the_active_query_and_answers_query_done() {
    let (events, mut events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(EndlessSource {
        events,
        query_tag: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(&mut stream, &ClientMsg::Query { id: 9, text: QueryText::new("q").unwrap() });
    let DaemonMsg::Results { query_id: 9, .. } = recv(&mut stream) else {
        panic!("the query must stream before the cancel");
    };

    send(&mut stream, &ClientMsg::Cancel { id: 9 });

    // The source observes the stop, and the exchange ends with QueryDone —
    // late Results frames for id 9 may precede it (already in flight when
    // the cancel landed); drain them.
    assert_eq!(
        recv_event_within(&mut events_rx, Duration::from_secs(5)),
        Some(0),
        "the cancelled query's source must observe its work stopping"
    );
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 9, .. } => continue,
            DaemonMsg::QueryDone { query_id: 9 } => break,
            other => panic!("expected QueryDone for the cancelled query, got {other:?}"),
        }
    }
}
```

- [ ] **Step 4: The cap test** — a source streaming past `MAX_ITEMS_PER_QUERY`:

```rust
#[test]
fn a_query_streaming_past_the_cap_is_truncated_and_terminated() {
    // Six batches of one full frame each: 6 000 items offered, the cap is
    // 5 000. The daemon must deliver exactly the cap and then QueryDone —
    // refusal of the remainder, never eviction of what was delivered.
    let batch: Vec<Item> = (0..MAX_ITEMS_PER_RESULTS_FRAME).map(item).collect();
    let (events, _events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(ScriptedSource {
        batches: vec![batch; 6],
        events,
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(&mut stream, &ClientMsg::Query { id: 3, text: QueryText::new("q").unwrap() });

    let mut total = 0;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 3, items, .. } => total += items.len(),
            DaemonMsg::QueryDone { query_id: 3 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        total, MAX_ITEMS_PER_QUERY,
        "the exchange must deliver exactly the cap and stop"
    );
}
```

- [ ] **Step 5: Run the suite**: `cargo test -p hopd --test lifecycle` → all
four pass; then `cargo test --workspace` for the rest.

- [ ] **Step 6: Gate and commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
git add crates/hopd/tests/lifecycle.rs
git commit -m "hopd tests: streaming, cancellation and the retention cap over a real socket"
```

---

### Task 6: The CLI — assembled output, stale-frame drop, cap

**Files:**
- Modify: `crates/hop-cli/src/lib.rs`
- Modify: `crates/hop-cli/tests/e2e.rs`

**Interfaces:**
- Consumes: `hop_protocol::limits::MAX_ITEMS_PER_QUERY` (Task 1).
- Produces: no new public surface; `try_run_query`'s loop changes shape.

- [ ] **Step 1: Write the failing e2e tests** — a fake daemon the test owns, because only a scripted daemon can send stale frames or overflow the cap. Add to `e2e.rs`:

```rust
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;

use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::limits::{MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, Item};

/// A scripted daemon: binds the socket where `hop` will look, accepts one
/// connection, answers the handshake, hands the accepted stream to `script`,
/// and keeps listening so the CLI's whole exchange happens against bytes
/// this test chose. Runs on a thread; joined via the returned handle so a
/// panic inside the script fails the test instead of vanishing.
fn fake_daemon(
    runtime_dir: &Path,
    script: impl FnOnce(&mut std::os::unix::net::UnixStream, u64) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let hop_dir = runtime_dir.join("hop");
    std::fs::create_dir_all(&hop_dir).unwrap();
    let listener = UnixListener::bind(hop_dir.join("hopd.sock")).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Handshake: expect Hello, answer HelloAck.
        let hello = read_client_frame(&mut stream);
        assert!(matches!(hello, ClientMsg::Hello { .. }));
        write_daemon_frame(&mut stream, &DaemonMsg::HelloAck { api_version: API_VERSION });
        // Expect the query; its id is what the script frames must reference.
        let ClientMsg::Query { id, .. } = read_client_frame(&mut stream) else {
            panic!("expected the query frame after the handshake");
        };
        script(&mut stream, id);
    })
}

fn read_client_frame(stream: &mut std::os::unix::net::UnixStream) -> ClientMsg {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream.read_exact(&mut prefix).unwrap();
    let len = payload_len(prefix).unwrap();
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    decode_payload(&payload).unwrap()
}

fn write_daemon_frame(stream: &mut std::os::unix::net::UnixStream, msg: &DaemonMsg) {
    stream.write_all(&encode_frame(msg).unwrap()).unwrap();
}

fn tiny_item(n: usize, title: &str) -> Item {
    use hop_protocol::{Action, ActionId, ActionKind, ItemId, Kind};
    Item {
        id: ItemId::new(format!("test:{n}")).unwrap(),
        kind: Kind::Action,
        title: title.to_string(),
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

#[test]
fn the_cli_drops_frames_whose_query_id_is_not_current() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        // A stale frame (wrong id) before, between, and after the real ones:
        // none of the "stale" titles may reach stdout.
        write_daemon_frame(stream, &DaemonMsg::Results { query_id: id + 1, partial: true, items: vec![tiny_item(1, "stale before")] });
        write_daemon_frame(stream, &DaemonMsg::Results { query_id: id, partial: true, items: vec![tiny_item(2, "current one")] });
        write_daemon_frame(stream, &DaemonMsg::Results { query_id: id + 1, partial: true, items: vec![tiny_item(3, "stale between")] });
        write_daemon_frame(stream, &DaemonMsg::Results { query_id: id, partial: true, items: vec![tiny_item(4, "current two")] });
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id + 1 }); // stale done: must NOT end the query
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id });
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query").arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output().unwrap();
    daemon.join().unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("current one") && stdout.contains("current two"));
    assert!(!stdout.contains("stale"), "a stale frame's items must never be rendered, got: {stdout}");
    // Assembled output: both current items, in delivery order.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn the_cli_refuses_a_daemon_that_streams_past_the_per_query_cap() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        // One item over the cap, delivered as six frames — each frame is
        // individually in-bounds; only the exchange total is not.
        let full: Vec<Item> = (0..MAX_ITEMS_PER_RESULTS_FRAME).map(|n| tiny_item(n, "x")).collect();
        for _ in 0..5 {
            write_daemon_frame(stream, &DaemonMsg::Results { query_id: id, partial: true, items: full.clone() });
        }
        write_daemon_frame(stream, &DaemonMsg::Results { query_id: id, partial: true, items: vec![tiny_item(9, "the straw")] });
        // No QueryDone: the CLI must have bailed already; writing more would
        // hit a closed pipe. (Ignore write errors in this closure if the
        // helper unwraps — use a non-panicking write for this last frame.)
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query").arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output().unwrap();
    let _ = daemon.join();

    assert!(!output.status.success(), "an over-cap stream must be refused");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&MAX_ITEMS_PER_QUERY.to_string()),
        "the refusal must name the cap, got: {stderr}");
    assert!(output.stdout.is_empty(),
        "nothing may be printed for a query that was refused mid-assembly");
}
```

(`write_daemon_frame` panicking on a broken pipe in the over-cap test: give
the last write its own `let _ = stream.write_all(...)` inline instead of the
helper. The CLI closing early is the *success* condition there.)

- [ ] **Step 2: Run them, verify failure**:
`cargo test -p hop-cli` — the stale-drop test fails today on line-by-line
printing (items print as frames arrive, and 2 lines vs immediate print
ordering may accidentally pass — the decisive failure is the cap test, which
today exits 0). Verify at least one fails; if the stale test passes by
coincidence, note why (current code already drops non-matching ids) and keep
it as a pin.

- [ ] **Step 3: Rework `try_run_query`'s receive loop** in `hop-cli/src/lib.rs`:

```rust
    let mut assembled: Vec<Item> = Vec::new();
    loop {
        match recv(&mut stream)? {
            DaemonMsg::Results { query_id, items, .. } if query_id == QUERY_ID => {
                // The exchange-total cap, mirrored client-side: a client
                // trusts its daemon no more than the daemon trusts it. Over
                // the cap is refusal, not truncation — printing a silently
                // shortened list would misrepresent what the daemon said.
                if assembled.len() + items.len() > MAX_ITEMS_PER_QUERY {
                    return Err(QueryError::OverCap);
                }
                assembled.extend(items);
            }
            DaemonMsg::QueryDone { query_id } if query_id == QUERY_ID => {
                // The terminal frame: print the assembled list, one item per
                // line, in delivery order.
                for item in &assembled {
                    let line = serde_json::to_string(item).map_err(QueryError::Encode)?;
                    println!("{line}");
                }
                return Ok(());
            }
            DaemonMsg::Error { error, .. } => return Err(QueryError::Daemon(error)),
            // Any other id is a stale frame — a `results` or `query_done`
            // for a query this process is no longer (or was never) waiting
            // on. Dropped unrendered: that is the client half of the
            // lifecycle contract (#55), not a permissive default.
            _ => continue,
        }
    }
```

Add the error variant and import:

```rust
use hop_protocol::limits::MAX_ITEMS_PER_QUERY;
// in QueryError:
    /// The daemon streamed more than [`MAX_ITEMS_PER_QUERY`] items for one
    /// query — a protocol violation, refused rather than truncated.
    OverCap,
// in Display:
    QueryError::OverCap => write!(
        f,
        "hopd sent more than {MAX_ITEMS_PER_QUERY} items for one query; refusing the response"
    ),
```

Also update `Item` import (`use hop_protocol::{.., Item}`) and the crate doc
header's "prints each returned item as one line" wording to "assembles the
streamed results and prints them once `query_done` arrives".

- [ ] **Step 4: Run the tests**: `cargo test -p hop-cli` → new tests pass;
`the_cli_query_round_trips_and_exits_zero` still passes (one item, printed
after done).

- [ ] **Step 5: Gate and commit**

```bash
cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check
git add crates/hop-cli/src/lib.rs crates/hop-cli/tests/e2e.rs
git commit -m "cli: assemble streamed results, drop stale frames, refuse an over-cap stream"
```

---

### Task 7: Documentation alignment and final gate

**Files:**
- Modify: `README.md`
- Modify: `crates/hopd/src/lib.rs`, `crates/hopd/src/server.rs` (only if
  Task 4 left stale skeleton wording)

- [ ] **Step 1: README** — rewrite the two stale claims:
  - Lines 5–9: the repo now contains M2's daemon through the query
    lifecycle: `hopd` serves streamed, cancellable queries over
    `$XDG_RUNTIME_DIR/hop/hopd.sock` (results still come from a placeholder
    source until the provider host lands); `hop` CLI unchanged in role.
  - Lines 26–30: replace "answers every query with the same hardcoded item"
    with: `hopd` streams query results with server-side cancellation and a
    bounded per-query retained set, but does not yet call into `hop-core` —
    its one source is still the skeleton's placeholder item; the provider
    host is a later M2 slice.
- [ ] **Step 2: Sweep for leftovers**: `grep -rn "walking skeleton" crates/ README.md`
  — every remaining mention must be historical ("the skeleton did X") or
  scoped to what is still true (e.g. `SkeletonSource`'s own docs). Fix any
  that now lie.
- [ ] **Step 3: Full gate**:

```bash
cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check
```

- [ ] **Step 4: Commit**

```bash
git add README.md crates/
git commit -m "docs: the daemon streams now; retire the single-reply wording"
```

---

## Acceptance criteria coverage (from issue #55)

| Criterion | Where |
| --- | --- |
| Several results frames before done | Task 4 (loop), Task 5 streaming test |
| Every frame carries its query id | Wire types already carry it; Task 5 asserts per frame |
| Second query cancels the first, observably | Task 4 (drop rx), Task 5 cancellation test (endless source stopped) |
| Client discards non-current query ids | Task 6 loop + stale-drop e2e test |
| CLI waits for done, prints assembled list | Task 6 loop + both e2e tests |
| Documented total cap distinct from per-frame bound | Task 1 (`MAX_ITEMS_PER_QUERY`) |
| Cap behavior documented and tested (refuse vs evict) | Refuse-and-terminate: Task 1 docs, Task 4 unit tests, Task 5 cap test |
| Integration test: streaming, cancellation, stale-frame over a real socket | Task 5 (streaming, cancellation), Task 6 (stale-frame, real socket, fake daemon) |
| Integration test: past-the-cap behavior | Task 5 cap test (daemon side), Task 6 cap test (client side) |

`Cancel { id }` handling (Task 4/5) is adjacent scope the brief's title
("server-side cancellation") covers: the wire variant has existed since M1
with no behavior, the lifecycle machinery makes it a five-line arm, and
leaving it answering "not implemented" while supersession works would ship
an inconsistent lifecycle. Flag in the PR body as a deliberate inclusion.
