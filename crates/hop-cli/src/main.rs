//! Entry point. Reads `argv`, hands everything after the program name to
//! [`hop_cli::parse`], and dispatches on the result — see `lib.rs` for why
//! that parse is hand-rolled and why the query flow it can reach is a
//! blocking `std::os::unix::net::UnixStream` rather than an async client.
//!
//! Issue #180 added `--socket <path>`, and with it the one piece of work
//! [`hop_cli::parse`] deliberately does not do: it only recognizes the
//! flag's *shape*, so [`hop_cli::Invocation::socket`] carries an
//! unvalidated path straight off `argv`. Resolving that into a path the
//! query and exec flows can actually connect to — or a refusal, if it does
//! not resolve inside `$XDG_RUNTIME_DIR` — is this file's job, done here
//! rather than inside `parse` (design decision D6 of that issue's plan):
//! `parse` stays pure, with no env read and no filesystem access, and
//! `main` is where an invocation actually starts doing things to the
//! outside world. This mirrors `hopd`'s own `main.rs` exactly, down to
//! resolving the override before anything else runs and sharing the
//! refusal's exit code with the usage arm.

use std::process::ExitCode;

use hop_cli::Command;

fn main() -> ExitCode {
    let args = std::env::args().skip(1);
    let invocation = hop_cli::parse(args);

    // Resolved unconditionally, ahead of dispatching on `command` — `None`
    // derives the default path exactly as before this issue, `Some`
    // resolves and constrains the override. A refusal here is reported and
    // exits through the same code `Command::Usage` below returns, per
    // criterion 5: refusal goes through the existing usage-exit channel
    // rather than a new one.
    let socket = match hop_protocol::socket::socket_path(invocation.socket.as_deref()) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("hop: {err}");
            return ExitCode::from(2);
        }
    };

    match invocation.command {
        Command::Version => {
            hop_cli::print_version();
            ExitCode::SUCCESS
        }
        Command::Query(text) => hop_cli::run_query(&socket, &text),
        Command::Exec {
            query,
            item_id,
            action_id,
        } => hop_cli::run_exec(&socket, &query, item_id, action_id),
        Command::Usage => {
            eprintln!("{}", hop_cli::USAGE);
            ExitCode::from(2)
        }
    }
}
