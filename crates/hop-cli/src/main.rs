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
//! outside world.
//!
//! # Resolved only for the commands that connect
//!
//! Unlike `hopd`'s `main.rs`, which resolves unconditionally because every
//! successful invocation of that binary binds a socket, this function
//! resolves only inside the `Query` and `Exec` arms below. `hop version`
//! never opens a socket — `print_version` reads only `CARGO_PKG_VERSION`
//! and `API_VERSION`, both compile-time constants — so making it depend on
//! a resolvable `$XDG_RUNTIME_DIR` would be a regression `--socket` has no
//! business causing: criterion 6 of issue #180 says omitting the flag is
//! unchanged behavior, and `hop version` working in *any* environment,
//! including a broken one, is exactly the unchanged behavior a user
//! diagnosing that environment reaches for it to get. One consequence is
//! worth naming rather than leaving for a reader to puzzle out: `hop
//! --socket /nonsense version` prints the version and exits 0 rather than
//! refusing. That is deliberate, not a missed check — the flag is inert for
//! a command that never opens the socket it would have named, the same way
//! a `--socket` given to `hopd` would be inert if `hopd` ever grew a
//! subcommand that did not serve.
//!
//! `Command::Usage` resolves nothing either, for the same reason: there is
//! no socket use on that path to resolve one for.

use std::process::ExitCode;

use hop_cli::Command;

fn main() -> ExitCode {
    let args = std::env::args().skip(1);
    let invocation = hop_cli::parse(args);

    match invocation.command {
        Command::Version => {
            hop_cli::print_version();
            ExitCode::SUCCESS
        }
        Command::Toggle => hop_cli::run_toggle(),
        Command::Query(text) => match resolve(invocation.socket.as_deref()) {
            Ok(socket) => hop_cli::run_query(&socket, &text),
            Err(code) => code,
        },
        Command::Exec {
            query,
            item_id,
            action_id,
        } => match resolve(invocation.socket.as_deref()) {
            Ok(socket) => hop_cli::run_exec(&socket, &query, item_id, action_id),
            Err(code) => code,
        },
        Command::Usage => {
            eprintln!("{}", hop_cli::USAGE);
            ExitCode::from(2)
        }
    }
}

/// Resolves `socket` (`None` derives the default, `Some` resolves and
/// constrains the override) for the two command arms that actually connect.
/// A refusal is reported here and mapped to `Command::Usage`'s own exit
/// code (`ExitCode::from(2)`), per criterion 5 — no new error channel.
fn resolve(socket: Option<&std::path::Path>) -> Result<std::path::PathBuf, ExitCode> {
    hop_protocol::socket::socket_path(socket).map_err(|err| {
        eprintln!("hop: {err}");
        ExitCode::from(2)
    })
}
