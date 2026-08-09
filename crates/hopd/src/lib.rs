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
//! What it is not yet: a daemon with every provider — the query router and the
//! provider host are wired ([`source`]), and the walking skeleton's and
//! [`apps`]'s providers are both registered as of this issue (#57), but #58's
//! calculator is still a gap — or anything with a lifecycle beyond "runs
//! until killed". Result *assembly* is no longer one of the gaps: every
//! provider arrival re-runs `hop-core`'s [`pipeline`](hop_core::pipeline) over
//! everything received so far for that query and replaces the client's list
//! with the ranked, boosted, capped result (issue #103; see [`source`] for
//! the accumulator that does it). Each remaining gap is named where it
//! applies, in [`runtime_dir`], [`server`] and [`source`].

pub mod apps;
pub mod calculator;
pub mod config;
pub(crate) mod connection;
pub mod runtime_dir;
pub mod server;
pub mod source;
pub mod state_dir;

use std::process::ExitCode;
use std::sync::Arc;

use hop_core::learning::Learning;
use hop_core::pipeline::Pipeline;
use tokio::sync::Mutex;

use crate::source::HostSource;

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
/// `current_thread` because [`server::serve_with`] spawns one task per
/// accepted connection (see that module's docs on connection caps), and the
/// eventual provider trait this daemon will host is `Send`-bound on the
/// assumption those tasks can actually run in parallel.
///
/// # Shutdown
///
/// None beyond the process being killed. [`server::serve_with`]'s accept
/// loop has no exit beyond an unrecoverable startup error, so under normal
/// operation this function does not return at all. Signal handling and any
/// orderly shutdown belong to issue #62 (socket activation and lifecycle) —
/// this daemon's only contribution to "restart works" is the stale-socket
/// removal [`server::serve_with`] documents in place.
pub fn run() -> ExitCode {
    // Config is resolved first, ahead of even the runtime dir: a malformed
    // config must refuse to start the daemon before anything binds a socket
    // (issue #60 criterion 2). The error's `Display` names the config path
    // and what about it did not parse, so a user can find and fix the file.
    let config = match crate::config::Config::load() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("hopd: {err}");
            return ExitCode::FAILURE;
        }
    };

    let state_dir = match crate::state_dir::resolve() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("hopd: {err}");
            return ExitCode::FAILURE;
        }
    };

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

    // The daemon's pipeline is built once here rather than per-connection:
    // it carries the `Learning` store loaded from the state dir, so every
    // query shares price-of-admission-loaded state rather than each getting
    // [`Pipeline::default`]'s empty one. `Learning::load` degrades to a
    // fresh store on any load problem (its own documented contract — see
    // `hop_core::learning`), so a damaged or absent store never stops the
    // daemon from starting; the store *path* (`Some`) rides into the source
    // so a later slice can persist recorded launches back to the same file.
    let store_path = state_dir.join(crate::state_dir::STORE_FILE_NAME);
    let pipeline = Arc::new(Mutex::new(Pipeline {
        learning: Learning::load(&store_path),
        ..Pipeline::default()
    }));
    let source = HostSource::with_config(
        Arc::new(server::build_host()),
        pipeline,
        config.max_results,
        Some(store_path),
    );

    match runtime.block_on(server::serve_with(&runtime_dir, source)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hopd: {err}");
            ExitCode::FAILURE
        }
    }
}
