//! Entry point. Reads `argv`, hands everything after the program name to
//! [`hopd::parse`], and dispatches on the result: serve, or refuse.
//!
//! hopd takes no arguments, and since issue #122 it says so instead of
//! discarding them — see [`hopd::parse`] for why silence was the dangerous
//! answer under systemd, and why this reads `args_os` rather than `args`.
//! The shape here deliberately matches `hop-cli`'s `main`: read argv, parse
//! it with a pure function, dispatch, and let the usage arm own the stderr
//! line and the exit code.

use std::process::ExitCode;

use hopd::Invocation;

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1);
    match hopd::parse(args) {
        Invocation::Serve => hopd::run(),
        Invocation::Usage => {
            eprintln!("hopd: {}", hopd::USAGE);
            ExitCode::from(2)
        }
    }
}
