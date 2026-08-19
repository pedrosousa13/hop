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
//! provider host are wired ([`source`]), and the walking skeleton's,
//! [`apps`]'s and [`calculator`]'s providers are all registered now
//! ([`apps`] as of issue #57, [`calculator`] as of this issue, #58) — or
//! anything with a lifecycle beyond "runs until killed". Result *assembly* is
//! no longer one of the gaps: every provider arrival re-runs `hop-core`'s
//! [`pipeline`](hop_core::pipeline) over everything received so far for that
//! query and replaces the client's list with the ranked, boosted, capped
//! result (issue #103; see [`source`] for the accumulator that does it).
//! Each remaining gap is named where it applies, in [`runtime_dir`],
//! [`server`] and [`source`].

pub(crate) mod activation;
pub mod apps;
pub mod calculator;
pub mod config;
pub(crate) mod connection;
pub mod runtime_dir;
pub mod server;
pub mod source;
pub mod state_dir;

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;

use hop_core::learning::Learning;
use hop_core::pipeline::Pipeline;
use hop_core::provider::plaintext_provider_ids;
use hop_core::rank::Weights;
use tokio::sync::Mutex;

use crate::source::HostSource;

/// What `hopd`'s argument list resolved to.
///
/// hopd's contract is "no arguments", and this type exists so that contract
/// is *enforced* rather than merely undocumented. Before issue #122 the
/// binary read `argv` not at all: every argument was discarded in silence
/// and the daemon started and served regardless, so
/// `hopd --socket /some/where` bound the default path and reported success.
/// Under systemd that is the worst shape a misconfiguration can take — the
/// unit is green, the daemon is listening, and it is listening somewhere no
/// client looks, because `hop` resolves only
/// `$XDG_RUNTIME_DIR/hop/hopd.sock` (`hop_cli`'s `socket_path`).
///
/// Kept separate from the code that acts on it — [`parse`] never touches a
/// socket or prints anything — so the tests at the bottom of this module
/// exercise the rule alone, without starting a daemon. This mirrors
/// `hop_cli`'s `parse`/`Command` split deliberately: the client half of this
/// workspace already treats unrecognized input as a usage error rather than
/// a default, and the daemon half is the odd one out until it does too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    /// No arguments: run the daemon. [`run`] is the whole of it.
    Serve,
    /// One or more arguments, whatever they were. hopd accepts none, so
    /// there is nothing to distinguish here — a plausible-looking flag, a
    /// typo and a bare word are the same refusal.
    Usage,
}

/// The line `main` prints to stderr for [`Invocation::Usage`].
///
/// Phrased as a statement rather than a synopsis (`usage: hopd`) because a
/// synopsis listing no arguments reads like a truncated message. An operator
/// who just passed `--socket` needs to be told the flag does not exist, not
/// shown an empty grammar.
pub const USAGE: &str = "hopd takes no arguments";

/// Parses `args` — the process's arguments with `argv[0]` already stripped —
/// into an [`Invocation`].
///
/// Any argument at all is [`Invocation::Usage`]. That is the entire rule, and
/// it is deliberately total: hopd has no flags today, so there is no
/// arm here that could silently accept one. If a real `--socket` override is
/// ever wanted (the standalone-run gap issue #122 names and leaves open —
/// `contrib/systemd/hopd.socket` already covers the activated case via
/// `ListenStream`), it becomes a new arm of this function, and the failure
/// mode it replaces is the silence this function exists to end.
///
/// # `OsString`, not `String`
///
/// Takes `OsString` so `main` can call `std::env::args_os()` rather than
/// `std::env::args()`, which *panics* on an argument that is not valid
/// Unicode. Nothing here inspects an argument's contents — the count is the
/// whole decision — so requiring UTF-8 would buy nothing and add a panic
/// path reachable from `argv`. `hop_cli::parse` does take `String`, because
/// a query's text is its payload; hopd has no payload.
pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Invocation {
    if args.into_iter().next().is_some() {
        Invocation::Usage
    } else {
        Invocation::Serve
    }
}

/// Builds the daemon's [`Pipeline`]: the `Learning` store loaded from
/// `store_path`, and `weights` carrying `config.max_term_chars`.
///
/// Split out of [`run`] so this construction — the one place `Config`'s
/// `max_term_chars` actually lands on the pipeline the ranker reads — is
/// unit-testable. `run` itself binds a socket and blocks, so nothing inside
/// it can be asserted on directly; without this seam, a `max_term_chars`
/// that parses into `Config` but never reaches `Weights` would be a silent
/// regression no test could catch.
///
/// `learning` is loaded fresh here, once, rather than per-connection: every
/// query shares price-of-admission-loaded state rather than each getting
/// [`Pipeline::default`]'s empty one. `Learning::load` degrades to a fresh
/// store on any load problem (its own documented contract — see
/// `hop_core::learning`), so a damaged or absent store never stops the
/// daemon from starting.
///
/// `max_term_chars` rides in on `Weights` rather than through
/// `HostSource::with_config` the way `max_results` does, because the two
/// knobs sit at different layers: `max_results` is a per-call parameter
/// `Pipeline::assemble` takes fresh on every query, so `HostSource` is what
/// has to carry it forward from one call to the next. `max_term_chars` is
/// consumed inside `Ranker::rank` off `self.weights`, a field the `Pipeline`
/// already owns — so setting it once here, at construction, is the whole
/// job; no per-call plumbing needed.
///
/// `host` is why this takes the built [`ProviderHost`] rather than only
/// `config` and `store_path`: issue #72 made a provider's manifest the sole
/// authority for whether its ids persist in the clear, and `Learning` does
/// not hold manifests itself (see `hop_core::learning`'s module docs) — it
/// holds the *answer*, synced in once here from `host.manifests()` via
/// [`plaintext_provider_ids`], right after `Learning::load` and before this
/// `Pipeline` is handed to anything that could look a provider up. `load`
/// itself never restores that answer from the file (it is
/// `#[serde(skip)]`), precisely so a store cannot grant itself plaintext
/// persistence — see [`Learning::sync_plaintext_providers`] for why.
fn pipeline_for(
    config: &crate::config::Config,
    store_path: &std::path::Path,
    host: &hop_core::host::ProviderHost,
) -> Pipeline {
    let mut learning = Learning::load(store_path);
    learning.sync_plaintext_providers(plaintext_provider_ids(&host.manifests()));
    Pipeline {
        learning,
        weights: Weights {
            max_term_chars: config.max_term_chars,
            ..Weights::default()
        },
        ..Pipeline::default()
    }
}

/// Resolves the runtime directory, binds the socket inside it, and serves
/// connections until an unrecoverable error occurs or the process is
/// killed.
///
/// `main.rs` calls this once [`parse`] has confirmed the invocation carried
/// no arguments, so every behavior described here is the whole of what
/// running the `hopd` binary *successfully* does; the one other outcome is
/// the [`USAGE`] refusal.
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
/// None beyond the process being killed, still. [`server::serve_with`]'s
/// accept loop has no exit beyond an unrecoverable startup error, so under
/// normal operation this function does not return at all. Issue #62 added
/// *activation* — [`server::acquire_listener`] accepting a listener systemd
/// already bound, instead of always binding one itself — not lifecycle: no
/// signal handler exists, and nothing tears this process down when its
/// `.socket` unit stops. Orderly shutdown remains unowned by any filed
/// issue as of this writing. This daemon's contribution to "restart works"
/// is still the stale-socket recovery `server::serve_with`'s standalone
/// path documents in place — unreachable, now, on the activated path,
/// which never touches the socket file at all (see
/// [`server::acquire_listener`]). Issue #158 narrowed what "stale" means:
/// the standalone path now refuses to recover a path a live listener still
/// answers on, surfacing [`server::ListenerError::AlreadyListening`]
/// through the `Err(err) => eprintln!("hopd: {err}")` arm just below
/// instead of unlinking it.
pub fn run() -> ExitCode {
    // Installed first, ahead of everything else `run` does: `hop-core` only
    // *builds* the provider-panic hook (issue #104) — a library must not
    // mutate process-global state, such as the panic hook, as a side effect
    // of being constructed — so this is the one call that turns the
    // guarantee on for this process. Nothing before it in `run` can panic on
    // a provider's behalf (there is no provider host yet), so there is no
    // earlier point that would matter, but installing it before even the
    // config load keeps that true by construction rather than by reading
    // the rest of this function to confirm it.
    hop_core::host::install_provider_panic_hook();

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
    //
    // The host is built *before* the pipeline now, not after: `pipeline_for`
    // needs `host.manifests()` to sync the learning store's plaintext-provider
    // set (issue #72), so the manifest registry has to exist first.
    let store_path = state_dir.join(crate::state_dir::STORE_FILE_NAME);
    let host = Arc::new(server::build_host());
    let pipeline = Arc::new(Mutex::new(pipeline_for(&config, &store_path, &host)));
    let source = HostSource::with_config(host, pipeline, config.max_results, Some(store_path));

    match runtime.block_on(server::serve_with(&runtime_dir, source)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hopd: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an argument list the way `main` hands one over: `argv[0]`
    /// already stripped, each remaining argument an `OsString`.
    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments_serves() {
        assert_eq!(parse(args(&[])), Invocation::Serve);
    }

    #[test]
    fn a_single_argument_is_usage() {
        assert_eq!(parse(args(&["serve"])), Invocation::Usage);
    }

    /// The regression issue #122 was actually filed for. This flag does not
    /// exist, and before #122 hopd discarded it and bound its default socket
    /// path anyway — a green systemd unit listening where no client looks.
    #[test]
    fn a_plausible_but_nonexistent_socket_flag_is_usage() {
        assert_eq!(
            parse(args(&["--socket", "/run/user/1000/hopd.sock"])),
            Invocation::Usage
        );
    }

    /// A near-miss of a flag hopd might one day have is still a refusal
    /// today. Nothing in [`parse`] pattern-matches an argument's spelling,
    /// so a typo cannot land in an accepting arm by accident.
    #[test]
    fn a_typo_of_a_future_flag_is_usage() {
        assert_eq!(parse(args(&["--socket-path"])), Invocation::Usage);
        assert_eq!(parse(args(&["-socket"])), Invocation::Usage);
    }

    #[test]
    fn several_arguments_are_usage() {
        assert_eq!(parse(args(&["--one", "--two", "three"])), Invocation::Usage);
    }

    /// `hopd ""` passed one argument, even though it carries no text. The
    /// count is the decision, not the content, so an empty argument is as
    /// much a refusal as any other — and notably not the same as no
    /// argument at all.
    #[test]
    fn an_empty_string_argument_is_usage() {
        assert_eq!(parse(args(&[""])), Invocation::Usage);
    }

    /// `std::env::args()` panics on an argument that is not valid Unicode,
    /// which is why [`parse`] takes `OsString` and `main` calls
    /// `args_os()`. This pins that a non-UTF-8 `argv` entry reaches a plain
    /// refusal rather than a panic.
    #[test]
    fn a_non_utf8_argument_is_usage_not_a_panic() {
        use std::os::unix::ffi::OsStringExt;

        // 0x80 is a continuation byte with no lead byte: never valid UTF-8.
        let invalid = OsString::from_vec(vec![b'-', b'-', 0x80]);
        assert_eq!(parse(vec![invalid]), Invocation::Usage);
    }

    /// The regression `pipeline_for` exists to make impossible: a
    /// `max_term_chars` that parses into `Config` but never reaches the
    /// pipeline's `Weights`, silently dropped on the floor between the
    /// config seam and the ranker. `run()` itself binds a socket and blocks,
    /// so it cannot be unit-tested directly — this exercises the exact
    /// construction it delegates to instead.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn pipeline_for_carries_the_configured_max_term_chars_onto_weights() {
        // `Learning::load` degrades to a fresh store on any load problem
        // (its own documented contract), so a nonexistent path in a fresh
        // temp dir is safe here and touches no real user state.
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("does-not-exist.json");
        // Empty and unregistered: this test is about `max_term_chars`, not
        // about the plaintext-provider sync `pipeline_for` also does, and an
        // empty host is a legitimate registry to sync against — it just
        // means every provider hashes.
        let host = hop_core::host::ProviderHost::with_log(Arc::new(hop_core::host::NoopLog));

        let default_pipeline = pipeline_for(&crate::config::Config::default(), &store_path, &host);
        assert_eq!(
            default_pipeline.weights.max_term_chars,
            hop_core::rank::MAX_TERM_CHARS
        );

        let lowered_config = crate::config::Config {
            max_term_chars: 10,
            ..crate::config::Config::default()
        };
        let lowered_pipeline = pipeline_for(&lowered_config, &store_path, &host);
        assert_eq!(lowered_pipeline.weights.max_term_chars, 10);
    }
}
