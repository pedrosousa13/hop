//! Entry point. Reads `argv`, hands everything after the program name to
//! [`hopd::parse`], and dispatches on the result: serve, or refuse.
//!
//! Since issue #122, an unrecognized argument says so instead of being
//! discarded — see [`hopd::parse`] for why silence was the dangerous answer
//! under systemd, and why this reads `args_os` rather than `args`. Issue
//! #180 added a real flag, `--socket <path>`, and with it the one piece of
//! work `parse` deliberately does not do: [`hopd::parse`] only recognizes
//! the flag's *shape*, so a `Some(raw)` socket carries an unvalidated path
//! straight off `argv`. Turning that into a path [`hopd::run`] can actually
//! bind — or a refusal, if it does not resolve inside `$XDG_RUNTIME_DIR` —
//! is this file's job, done here rather than inside `parse` (design decision
//! D6 of this issue's plan): `parse` stays pure, with no env read and no
//! filesystem access, and `main` is where an invocation actually starts
//! doing things to the outside world. The shape here otherwise still matches
//! `hop-cli`'s `main`: read argv, parse it with a pure function, dispatch,
//! and let the usage arm own the stderr line and the exit code — a refused
//! override now shares that same line and that same code, criterion 5.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hop_protocol::socket::SocketPathError;
use hopd::Invocation;

/// Resolves a raw `--socket` value against `$XDG_RUNTIME_DIR`, the one
/// question [`hopd::parse`] does not answer.
///
/// Reads the runtime directory itself only here, at the one place this
/// binary needs it before dispatching to [`hopd::run`] — [`hopd::run`]'s own
/// `None` branch reads it a second time, independently, through
/// [`hopd::runtime_dir::resolve`], because that path also has to *create*
/// `<XDG_RUNTIME_DIR>/hop`, a step this function has no reason to take for
/// an override that may live somewhere else entirely.
fn resolve_override(raw: &Path) -> Result<PathBuf, SocketPathError> {
    let runtime_dir = hop_protocol::socket::runtime_dir()?;
    hop_protocol::socket::resolve_in(&runtime_dir, raw)
}

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1);
    match hopd::parse(args) {
        Invocation::Serve { socket } => {
            let resolved = match socket {
                None => None,
                Some(raw) => match resolve_override(&raw) {
                    Ok(resolved) => Some(resolved),
                    Err(err) => {
                        eprintln!("hopd: {err}");
                        return ExitCode::from(2);
                    }
                },
            };
            hopd::run(resolved)
        }
        Invocation::Usage => {
            eprintln!("hopd: {}", hopd::USAGE);
            ExitCode::from(2)
        }
    }
}
