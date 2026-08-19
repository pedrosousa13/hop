//! Everything that talks to `hopd`'s socket, and the one seam that keeps it
//! off the GTK main thread.
//!
//! # No socket IO on the main thread — how this is structural, not a promise
//!
//! §8 of the design spec names the predecessor branch's frontend flaw:
//! blocking the UI thread on the socket, including during connect and during
//! a slow provider's stream. This module is built so that flaw cannot come
//! back by accident, not just so that it does not exist today:
//!
//! - [`spawn`] is the *only* public entry point. It returns a
//!   [`CommandSender`] and an [`EventReceiver`] — two [`async_channel`]
//!   endpoints over the [`IpcCommand`] and [`IpcEvent`] enums below — and
//!   nothing else. Nothing in this module's public API exposes a
//!   `UnixStream`, a `tokio::net` type, or anything [`hop_protocol::framing`]
//!   decodes frames into or out of; [`client::run`], the function that
//!   actually owns the socket, is private to this module (`mod client;` with
//!   no `pub use`), so no other module in this crate can even *name* the
//!   type that touches the wire, let alone call a blocking or awaiting read
//!   on it from the GTK thread.
//! - [`spawn`] moves the socket entirely onto a dedicated OS thread carrying
//!   its own single-threaded tokio runtime (`client::run`'s doc comment
//!   explains why current-thread is enough). The GTK thread that calls
//!   [`spawn`] never touches that runtime; it only holds the two channel
//!   halves, which are `Send` because [`IpcCommand`] and [`IpcEvent`] are
//!   plain data — see the `messages_cross_the_channel_because_they_are_send`
//!   test below, which is what makes that claim checkable rather than
//!   asserted here in prose. A `gtk::Widget` or any other GObject wrapper
//!   accidentally added to either enum would stop this crate compiling,
//!   because those types are `!Send` — the compiler is the enforcement, this
//!   test is what pins the enforcement down as a named, run test rather than
//!   a fact nobody re-checks.
//! - The UI side only ever calls [`glib::spawn_future_local`] over
//!   [`EventReceiver::recv`], an `async fn` that suspends the *future*
//!   without blocking the *thread* — the glib main loop keeps pumping input,
//!   redraw and timer events while it waits. See `ui::window`'s use of this
//!   module for the call site.
//!
//! [`tests/ipc_off_main_thread.rs`] is the other half of "checkable rather
//! than asserted": it drives a real fake daemon over a real `UnixStream` from
//! [`spawn`]'s background thread, and asserts from the calling ("main-thread
//! stand-in") thread that the frames were read on a *different* OS thread
//! than the one that called [`spawn`] — a runtime proof that the read
//! actually happened elsewhere, not just a structural one.

mod client;

use std::path::PathBuf;

use hop_protocol::{ActionId, ExecOutcome, Item, ItemId, Mode};

/// A request the UI sends to the IPC thread. Carries no wire-protocol id —
/// [`client::run`] assigns and tracks the `Query`/`Execute` frame's `id`
/// itself (see its module doc for why), so the UI never has to reconcile a
/// counter against what it is currently displaying.
#[derive(Debug, Clone)]
pub enum IpcCommand {
    /// Replace the in-flight query (if any) with a new one for `text`. Every
    /// keystroke sends one of these; the daemon's own supersede rule
    /// (`ClientMsg::Query`'s doc comment in `hop_protocol::wire`) means an
    /// in-flight query for stale text is abandoned server-side rather than
    /// raced client-side.
    Query(String),
    /// Run `action_id` on `item_id`, against whatever query is currently
    /// active. Refused locally (an [`IpcEvent::Error`] with no daemon round
    /// trip) if no query is active — see [`client::run`].
    Execute {
        item_id: ItemId,
        action_id: ActionId,
    },
}

/// Something the IPC thread reports back to the UI. Every variant is plain,
/// `'static`, owned data — no borrow, no socket, no GTK object — which is
/// what lets this cross the channel back to the main thread's future; see
/// this module's doc comment.
#[derive(Debug, Clone)]
pub enum IpcEvent {
    /// The handshake with `hopd` completed.
    Connected,
    /// `hopd`'s socket could not be reached, or the handshake failed. Human
    /// readable rather than typed: the UI has one thing to do with this
    /// (show it, and rely on the automatic reconnect described on
    /// [`client::run`]), not a set of cases to branch on.
    ConnectFailed(String),
    /// How the daemon routed the active query — see
    /// `DaemonMsg::QueryRouted`'s contract for what `exclusive` means.
    Routed { mode: Mode, exclusive: bool },
    /// The complete current result list for the active query, replacing
    /// whatever the UI is holding — the same replace rule
    /// `DaemonMsg::Results` documents.
    Results(Vec<Item>),
    /// The active query finished; nothing more will arrive for it.
    QueryDone,
    /// The active query's `Execute` completed.
    Executed(ExecOutcome),
    /// A query-scoped or connection-scoped error, already turned into a
    /// message a status row can show as-is.
    Error(String),
    /// The connection was lost. `client::run` is already retrying; this is
    /// purely so the UI can show an offline state in the meantime.
    Disconnected,
}

/// The UI-held half of the command channel.
#[derive(Clone)]
pub struct CommandSender(async_channel::Sender<IpcCommand>);

impl CommandSender {
    /// Queues `cmd` for the IPC thread. Never blocks and never touches the
    /// socket — it is a bounded-free, in-memory channel send; see this
    /// module's doc comment for why that is exactly the property that
    /// matters here.
    pub fn send(&self, cmd: IpcCommand) {
        // The channel is unbounded and the receiver lives for the process's
        // lifetime (`client::run` only returns when this sender — and thus
        // every clone — has been dropped), so the one failure mode,
        // `Closed`, means the IPC thread has already exited during process
        // shutdown. Nothing left for the UI to do with that at this call
        // site; a queued keystroke or execute request racing shutdown is
        // dropped, same as it would be if the process had already exited a
        // moment later.
        let _ = self.0.try_send(cmd);
    }
}

/// The UI-held half of the event channel.
pub struct EventReceiver(async_channel::Receiver<IpcEvent>);

impl EventReceiver {
    /// Awaits the next [`IpcEvent`]. `None` once the IPC thread has exited
    /// and every event it might still have sent has been drained — normal
    /// only during shutdown.
    pub async fn recv(&self) -> Option<IpcEvent> {
        self.0.recv().await.ok()
    }
}

/// Starts the IPC thread and returns the two channel halves the UI drives it
/// with. `socket_path` is resolved once, by the caller: `app::run` calls
/// `hop_protocol::socket::socket_path` right after `cli::parse` returns and
/// passes the result in here already resolved — the same function `hop-cli`'s
/// and `hopd`'s own entry points call, not a copy mirroring theirs (issue
/// #180 promoted what used to be three separate derivations into that one
/// shared `hop-protocol` function; see `app::run`'s own doc comment for the
/// history).
///
/// Spawning is fire-and-forget from the caller's point of view: the returned
/// [`CommandSender`] is usable immediately, before the background thread has
/// so much as attempted to connect — commands sent before `Connected` arrives
/// simply queue, and `client::run`'s connect-then-serve loop drains them once
/// it is ready. There is nothing to await here precisely because there is
/// nothing this function does that touches the socket; see this module's doc
/// comment.
pub fn spawn(socket_path: PathBuf) -> (CommandSender, EventReceiver) {
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<IpcCommand>();
    let (evt_tx, evt_rx) = async_channel::unbounded::<IpcEvent>();

    std::thread::Builder::new()
        .name("hop-gtk-ipc".to_string())
        .spawn(move || {
            // `new_current_thread` rather than a multi-thread runtime: this
            // is one connection, serialized exactly like `hop-cli`'s (see
            // that crate's "Why no tokio" doc comment) except long-lived —
            // there is nothing here for a second worker thread to run
            // concurrently.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to start the hop-gtk IPC runtime");
            runtime.block_on(client::run(socket_path, cmd_rx, evt_tx));
        })
        .expect("failed to spawn the hop-gtk IPC thread");

    (CommandSender(cmd_tx), EventReceiver(evt_rx))
}

/// Test-only escape hatch: a [`CommandSender`] paired with a plain, directly
/// readable receiver of the [`IpcCommand`]s sent through it — bypassing
/// [`spawn`]'s background thread and real socket entirely.
/// [`CommandSender`]'s only *production* constructor is [`spawn`],
/// deliberately (this module's own doc comment: nothing outside this module
/// can even name the type that touches the wire), but `ui::window`'s own
/// `#[cfg(test)]` dispatch tests need to observe which [`IpcCommand`] a key
/// press or a mouse click on a row produced, and have no interest in a real
/// `hopd` connection to get there. `pub(crate)` rather than `pub`: reachable
/// from anywhere in this crate's own test code, never from outside it — an
/// integration test under `tests/` cannot see it (and does not need to,
/// since none of issue #182's GTK-dependent tests live there; see
/// `ui::window`'s test module for why they live beside the code they test
/// instead).
#[cfg(test)]
pub(crate) fn test_channel() -> (CommandSender, async_channel::Receiver<IpcCommand>) {
    let (tx, rx) = async_channel::unbounded();
    (CommandSender(tx), rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compiles only if `T` is `Send` — the same check `spawn`'s background
    /// thread closure needs to pass to be spawnable at all, pulled out as a
    /// standalone assertion so it is checked for these specific types even
    /// if nothing else in this module's tests happens to move one across a
    /// thread boundary.
    fn assert_send<T: Send>() {}

    #[test]
    fn messages_cross_the_channel_because_they_are_send() {
        // If either enum ever grows a field holding a GTK/GObject wrapper —
        // every one of which is `!Send`, since GObject reference counting is
        // not atomic — this stops compiling right here, which is the module
        // doc comment's "checkable rather than asserted" claim for the
        // no-main-thread-socket-IO guarantee: the only way data reaches the
        // UI from the IPC thread is through `IpcEvent`, and the only way a
        // command reaches the IPC thread is through `IpcCommand`, so pinning
        // both to `Send` pins the whole channel boundary.
        assert_send::<IpcCommand>();
        assert_send::<IpcEvent>();
    }
}
