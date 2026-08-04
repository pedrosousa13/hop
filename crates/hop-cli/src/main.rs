//! Entry point. Reads `argv`, hands everything after the program name to
//! [`hop_cli::parse`], and dispatches on the result — see `lib.rs` for why
//! that parse is hand-rolled and why the query flow it can reach is a
//! blocking `std::os::unix::net::UnixStream` rather than an async client.

use std::process::ExitCode;

use hop_cli::Command;

fn main() -> ExitCode {
    let args = std::env::args().skip(1);
    match hop_cli::parse(args) {
        Command::Version => {
            hop_cli::print_version();
            ExitCode::SUCCESS
        }
        Command::Query(text) => hop_cli::run_query(&text),
        Command::Usage => {
            eprintln!("{}", hop_cli::USAGE);
            ExitCode::from(2)
        }
    }
}
