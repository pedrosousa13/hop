//! Entry point. Parses no arguments — this slice has none — and delegates
//! everything to [`hopd::run`].

use std::process::ExitCode;

fn main() -> ExitCode {
    hopd::run()
}
