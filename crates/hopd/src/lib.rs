//! hopd — the hop launcher daemon.
//!
//! This crate is today's walking skeleton for issue #54: a real Unix socket
//! inside a 0700 runtime directory, a mandatory version handshake ahead of
//! anything else, and a single hardcoded item answering every query. What it
//! is not yet: a query router (`hop-core` exists and is unused here), a
//! provider host, or anything with a lifecycle beyond "runs until killed".
//! Each of those gaps is named where it applies, in [`runtime_dir`] and
//! [`server`].

pub mod runtime_dir;
pub mod server;

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
/// walking skeleton's only contribution to "restart works" is the
/// stale-socket removal [`server::serve`] documents in place.
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
