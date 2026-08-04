//! hopd — the hop launcher daemon.
//!
//! A real Unix socket inside a 0700 runtime directory, a mandatory version
//! handshake ahead of anything else, and the query lifecycle behind it: every
//! frame of an exchange carries its query id, results stream back as partial
//! `results` frames terminated by `query_done`, a new query cancels the one
//! it supersedes server-side, an explicit `cancel` does the same and is
//! acknowledged, and what one query id delivers is retained under a hard cap
//! so a chatty client cannot grow this process without bound. All of that
//! lives in the crate-private `connection` module, one driver per accepted
//! connection; the seam it pulls items through is [`source`].
//!
//! What it is not yet: a daemon with real providers — the query router and the
//! provider host are wired ([`source`]), but the only provider registered is
//! the walking skeleton's, until issue #57 lands apps and #58 the calculator —
//! a result *assembly* step (ranking, boosts and the pinned tail are
//! `hop-core`'s [`pipeline`](hop_core::pipeline), still uncalled here), or
//! anything with a lifecycle beyond "runs until killed". Each of those gaps is
//! named where it applies, in [`runtime_dir`], [`server`] and [`source`].

pub(crate) mod connection;
pub mod runtime_dir;
pub mod server;
pub mod source;

use std::process::ExitCode;

/// Resolves the runtime directory, binds the socket inside it, and serves
/// connections until an unrecoverable error occurs or the process is
/// killed.
///
/// `main.rs` calls this and nothing else — it parses no arguments, because
/// this slice has none to parse — so every behavior described here is the
/// whole of what running the `hopd` binary does.
///
/// # The runtime is built here, not on `main`
///
/// `#[tokio::main(flavor = "multi_thread")]` on `main` would do the same
/// thing with less code, but it would also mean `main.rs` imports tokio,
/// which the brief's file layout treats as this crate's business, not the
/// entry point's. `Builder::new_multi_thread` here is the acceptance
/// criterion — a multi-threaded runtime — satisfied without that import
/// leaking upward. A multi-threaded runtime is required rather than
/// `current_thread` because [`server::serve`] spawns one task per accepted
/// connection (see that module's docs on connection caps), and the
/// eventual provider trait this daemon will host is `Send`-bound on the
/// assumption those tasks can actually run in parallel.
///
/// # Shutdown
///
/// None beyond the process being killed. [`server::serve`]'s accept loop has
/// no exit beyond an unrecoverable startup error, so under normal operation
/// this function does not return at all. Signal handling and any orderly
/// shutdown belong to issue #62 (socket activation and lifecycle) — this
/// daemon's only contribution to "restart works" is the stale-socket
/// removal [`server::serve`] documents in place.
pub fn run() -> ExitCode {
    let runtime_dir = match runtime_dir::resolve() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("hopd: {err}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("hopd: failed to start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(server::serve(&runtime_dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hopd: {err}");
            ExitCode::FAILURE
        }
    }
}
