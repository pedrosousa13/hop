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

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
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
/// Before issue #122 the binary read `argv` not at all: every argument was
/// discarded in silence and the daemon started and served regardless, so
/// `hopd --socket /some/where` bound the default path and reported success.
/// Under systemd that is the worst shape a misconfiguration can take — the
/// unit is green, the daemon is listening, and it is listening somewhere no
/// client looks, because `hop` resolves only
/// `$XDG_RUNTIME_DIR/hop/hopd.sock` (`hop_cli`'s `socket_path`). #122's fix
/// was to make every argument, without exception, a refusal — hopd had no
/// flags at the time, so "any argument at all" and "an unrecognized
/// argument" were the same rule.
///
/// Issue #180 is the flag #122's own doc comment anticipated: `--socket
/// <path>` binds a caller-chosen path instead of the derived one, most
/// usefully a second, non-conflicting socket for a development `hopd`
/// running alongside a session's own. `Serve`'s field carries the raw,
/// **unvalidated** value straight off `argv` — `None` when the flag was not
/// given, `Some` when it was — because [`parse`] stays pure (see its own doc
/// comment for why) and cannot itself check the one rule that matters, that
/// the path resolves inside `$XDG_RUNTIME_DIR`. `main.rs` is what turns that
/// `Some` into a validated path, or a refusal, before [`run`] ever sees it.
///
/// Kept separate from the code that acts on it — [`parse`] never touches a
/// socket or prints anything — so the tests at the bottom of this module
/// exercise the rule alone, without starting a daemon. This mirrors
/// `hop_cli`'s `parse`/`Command` split deliberately: the client half of this
/// workspace already treats unrecognized input as a usage error rather than
/// a default, and the daemon half is the odd one out until it does too.
///
/// No longer `Copy`: a `PathBuf` is not, and `Serve`'s field is one. `Debug,
/// Clone, PartialEq, Eq` are enough for the tests below to build and compare
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Run the daemon. `socket` is `None` when `--socket` was not given (the
    /// derived path applies) and `Some(raw)` when it was, carrying whatever
    /// bytes followed the flag, unvalidated — see this type's own doc
    /// comment for why validation is not this function's job.
    Serve { socket: Option<PathBuf> },
    /// `--socket` with no value, a repeated `--socket`, or any argument this
    /// function does not recognize — a plausible-looking flag, a typo, and a
    /// bare word are all the same refusal.
    Usage,
}

/// The line `main` prints to stderr for [`Invocation::Usage`].
///
/// A synopsis now, not a statement (`hopd takes no arguments`, its wording
/// before issue #180): hopd has exactly one flag today, so a grammar naming
/// it is the more useful message — an operator who typoed `--socket-path` or
/// left off its value sees the flag hopd actually accepts, not just that
/// something was wrong.
pub const USAGE: &str = "usage: hopd [--socket <path>]";

/// Parses `args` — the process's arguments with `argv[0]` already stripped —
/// into an [`Invocation`].
///
/// No arguments is [`Invocation::Serve`] with `socket: None`. Exactly
/// `--socket` followed by one more argument is `Serve` with that argument as
/// `socket`. Everything else — `--socket` with nothing after it, `--socket`
/// given twice, or any argument this function does not recognize — is
/// [`Invocation::Usage`]. This function does not read `$XDG_RUNTIME_DIR`,
/// touch the filesystem, or validate the path in any way; it only recognizes
/// the flag's *shape*. See [`Invocation`]'s doc comment for why that
/// validation belongs to `main.rs` instead: briefly, doing it here would
/// mean an env read and filesystem access inside a function this module
/// documents as pure, and would force `std::env::set_var` into this
/// function's own unit tests below, which edition 2024 makes `unsafe` and
/// this workspace's `unsafe_code = "deny"` lint refuses.
///
/// # `OsString`, not `String`
///
/// Takes `OsString` so `main` can call `std::env::args_os()` rather than
/// `std::env::args()`, which *panics* on an argument that is not valid
/// Unicode. The flag name itself is compared as `OsStr`, never decoded, so a
/// non-UTF-8 `--socket` lookalike still refuses cleanly rather than
/// panicking — and a non-UTF-8 *value* following a genuine `--socket` is
/// accepted outright, because a path is under no obligation to be valid
/// Unicode either. `hop_cli::parse` does take `String`, because a query's
/// text is its payload; hopd's payload, when it has one, is a path, not
/// text meant to be read.
pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Invocation {
    let mut args = args.into_iter();
    match args.next() {
        None => Invocation::Serve { socket: None },
        Some(flag) if flag == OsStr::new("--socket") => match args.next() {
            Some(value) if args.next().is_none() => Invocation::Serve {
                socket: Some(PathBuf::from(value)),
            },
            // Either no value followed `--socket`, or something followed the
            // value too (most notably a second `--socket`) — both are
            // refused rather than guessing which argument the caller meant.
            _ => Invocation::Usage,
        },
        Some(_) => Invocation::Usage,
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

/// Resolves the socket path, binds it, and serves connections until an
/// unrecoverable error occurs or the process is killed.
///
/// `socket` is the **already-resolved** override, or `None` for the derived
/// path — `main.rs` is what turns `Invocation::Serve`'s raw, unvalidated
/// `Option<PathBuf>` into this one (issue #180, design decision D6): it
/// calls [`hop_protocol::socket::runtime_dir`] and
/// [`hop_protocol::socket::resolve_in`] immediately after [`parse`], before
/// this function or anything else runs, and refuses on stderr with
/// [`Invocation::Usage`]'s own exit code if the override does not resolve
/// inside `$XDG_RUNTIME_DIR`. That split keeps this function's own job
/// simple and total: given `None`, resolve the runtime directory and derive
/// the socket path inside it, exactly as before issue #180; given `Some`,
/// the path is already known-good, so the only work left is creating its
/// parent directory.
///
/// # Why the override branch does not call `runtime_dir::resolve`
///
/// [`runtime_dir::resolve`] creates `<XDG_RUNTIME_DIR>/hop` — a directory an
/// override may not use at all (`$XDG_RUNTIME_DIR/hop-dev/hopd.sock`, one
/// level below `hop`, is the case this issue's plan is written around,
/// design decision D2). Calling it anyway on the override branch would
/// create that directory as an unwanted side effect of a flag that never
/// asked for it. What the override branch needs instead is exactly the
/// piece `resolve` itself is built from: create *this* path's own parent at
/// 0700, born that way with no create-then-`chmod` window, left exactly as
/// found if it already exists. [`runtime_dir::create_at_0700`] is that piece,
/// factored out so both branches share the one `DirBuilder` call rather than
/// duplicating it — its own doc comment carries the full reasoning for why
/// the mode is born rather than set after the fact.
///
/// Config and the state directory still resolve before either branch, in
/// the same order as before this issue: a malformed config must refuse to
/// start before anything binds a socket (issue #60 criterion 2), and that
/// ordering does not depend on which socket path is about to be used.
///
/// `main.rs` calls this once [`parse`] and the override resolution above
/// have both succeeded, so every behavior described here is the whole of
/// what running the `hopd` binary *successfully* does; the other outcome is
/// the [`USAGE`] refusal, owned by `main.rs` for both a malformed flag and a
/// refused override alike.
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
pub fn run(socket: Option<PathBuf>) -> ExitCode {
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

    let socket_overridden = socket.is_some();
    let socket_path = match socket {
        None => match runtime_dir::resolve() {
            Ok(dir) => dir.join(hop_protocol::socket::SOCKET_FILE_NAME),
            Err(err) => {
                eprintln!("hopd: {err}");
                return ExitCode::FAILURE;
            }
        },
        Some(path) => {
            // `main.rs` only ever hands this branch a path
            // `hop_protocol::socket::resolve_in` has already accepted, and
            // that function refuses any path with no file name (a path
            // ending in `/`, `.` or `..`) before it can reach here — so
            // `parent()` always has something to return. The fallback below
            // is defensive, not a path this crate expects to exercise
            // through `main.rs`.
            let parent = path.parent().unwrap_or(std::path::Path::new("."));
            if let Err(err) = runtime_dir::create_at_0700(parent) {
                eprintln!("hopd: {err}");
                return ExitCode::FAILURE;
            }
            path
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

    match runtime.block_on(server::serve_with(&socket_path, socket_overridden, source)) {
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
        assert_eq!(parse(args(&[])), Invocation::Serve { socket: None });
    }

    #[test]
    fn a_single_argument_is_usage() {
        assert_eq!(parse(args(&["serve"])), Invocation::Usage);
    }

    /// Issue #122 established that hopd discarding *any* argument was itself
    /// the bug: `hopd --socket /some/where` used to bind the default socket
    /// path anyway and report success — a green systemd unit listening where
    /// no client looked. At the time hopd had no flags, so #122's fix was to
    /// refuse every argument without exception, this one included.
    ///
    /// Issue #180 gives hopd the real flag #122's fix could only refuse:
    /// this exact argument list — `--socket` followed by one value — is now
    /// the flag's ordinary accepted shape, not a refusal. #122's protection
    /// against *silence* is unchanged: an override that fails to resolve
    /// still refuses (see `main.rs`), it just no longer refuses at `parse`
    /// for having been given at all.
    #[test]
    fn a_socket_flag_with_a_value_parses() {
        assert_eq!(
            parse(args(&["--socket", "/run/user/1000/hopd.sock"])),
            Invocation::Serve {
                socket: Some(PathBuf::from("/run/user/1000/hopd.sock"))
            }
        );
    }

    /// `--socket` with nothing after it names no path to resolve, so it is a
    /// refusal rather than `Serve { socket: None }` — silently falling back
    /// to the derived path would hide a caller's mistyped invocation instead
    /// of reporting it.
    #[test]
    fn a_socket_flag_with_no_value_is_usage() {
        assert_eq!(parse(args(&["--socket"])), Invocation::Usage);
    }

    /// A second `--socket` — whatever follows it — is refused rather than
    /// silently letting the last one win: `parse` does not decide which of
    /// two overrides the caller meant.
    #[test]
    fn a_repeated_socket_flag_is_usage() {
        assert_eq!(
            parse(args(&["--socket", "/run/user/1000/a.sock", "--socket"])),
            Invocation::Usage
        );
        assert_eq!(
            parse(args(&[
                "--socket",
                "/run/user/1000/a.sock",
                "--socket",
                "/run/user/1000/b.sock"
            ])),
            Invocation::Usage
        );
    }

    /// A near-miss of the flag is still a refusal. Nothing in [`parse`]
    /// pattern-matches on any spelling but `--socket` itself, so a typo
    /// cannot land in the accepting arm by accident.
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

    /// The flag *name* above must refuse non-UTF-8 cleanly, but a
    /// non-UTF-8 *value* following a genuine `--socket` is a legitimate
    /// path — nothing about a filesystem path requires valid Unicode — and
    /// `parse` never decodes it, so it is accepted rather than refused.
    #[test]
    fn a_non_utf8_socket_value_is_accepted() {
        use std::os::unix::ffi::OsStringExt;

        let invalid_value = OsString::from_vec(vec![b'/', b'x', 0x80, b'y']);
        assert_eq!(
            parse(vec![OsString::from("--socket"), invalid_value.clone()]),
            Invocation::Serve {
                socket: Some(PathBuf::from(invalid_value))
            }
        );
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
