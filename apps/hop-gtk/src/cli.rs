//! `hop-gtk`'s argument parsing — hand-rolled, in the same spirit as
//! `hop-cli`'s (`crates/hop-cli/src/lib.rs`'s "Why no clap" doc comment).
//!
//! Issue #180 grew this from two flags to three: `--socket <path>` joins
//! `--screenshot` and `--query`, all three taking exactly one value, none
//! recognizing a subcommand, and none needing generated `--help` text this
//! binary wants to own. Three hand-matched flags is still short of what
//! earns a parser generator its dependency weight back — `hop-cli`'s own
//! sibling doc comment draws that line at "multiple flags, subcommand-
//! specific options, and generated `--help` text", none of which this binary
//! has yet — so the `while let` loop below grows one more arm rather than
//! this module reaching for a dependency neither client crate has needed so
//! far. Revisit this call, in place, if a fourth flag or an actual
//! subcommand ever lands.

use std::path::PathBuf;

/// What `hop-gtk`'s argument list resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Args {
    /// Ordinary interactive run: register as the (unique) GApplication
    /// instance, build the pre-built hidden window, and let `activate`
    /// present it — see `app`'s module doc for why a plain re-invocation of
    /// this same binary is what `hop toggle` (§8's control-message path)
    /// resolves to.
    ///
    /// `socket` carries `--socket <path>`'s raw, unvalidated value — `None`
    /// when the flag was not given. See [`Args::Screenshot`]'s own field for
    /// why validation is not this type's job.
    Run { socket: Option<PathBuf> },
    /// `--screenshot <path>`: drive to the state reached by `query` (empty
    /// string, the default, is the empty-query state) and render it to a PNG
    /// at `path`, then exit. Acceptance criterion 7.
    ///
    /// `socket` is the same unvalidated `--socket` override [`Args::Run`]
    /// carries — [`parse`] stays pure (never touches a socket or the
    /// filesystem, see its own doc comment) and cannot itself check the one
    /// rule that matters, that the path resolves inside `$XDG_RUNTIME_DIR`.
    /// `app::run` is what turns a `Some` into a validated path, or a
    /// refusal, before either variant below does anything with it (design
    /// decision D6 of issue #180's plan).
    Screenshot {
        path: PathBuf,
        query: String,
        socket: Option<PathBuf>,
    },
    /// Bad arguments: an unrecognized flag, a flag missing its value,
    /// `--socket` given twice, or `--query` given without `--screenshot`
    /// (there is nothing for it to drive in an interactive run, where the
    /// query comes from the entry widget instead).
    Usage,
}

/// The line `main` prints to stderr for [`Args::Usage`].
pub const USAGE: &str = "usage: hop-gtk [--socket <path>] [--screenshot <path> [--query <text>]]";

/// Parses `args` — the process's arguments with `argv[0]` already stripped.
///
/// Never touches a socket, the filesystem, or `$XDG_RUNTIME_DIR` — it only
/// recognizes each flag's *shape*. `--socket`'s value is carried through
/// unvalidated on both [`Args::Run`] and [`Args::Screenshot`]; `app::run`
/// resolves it, immediately after this function returns, before either
/// variant's own work begins.
pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Args {
    let mut socket: Option<PathBuf> = None;
    let mut screenshot: Option<PathBuf> = None;
    let mut query: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => match (args.next(), &socket) {
                (Some(path), None) => socket = Some(PathBuf::from(path)),
                // Either no value followed `--socket`, or a second
                // `--socket` was given — both refused rather than guessing
                // which override the caller meant, the same rule
                // `hop_cli::parse` and `hopd::parse` apply to a repeated
                // flag.
                _ => return Args::Usage,
            },
            "--screenshot" => match args.next() {
                Some(path) => screenshot = Some(PathBuf::from(path)),
                None => return Args::Usage,
            },
            "--query" => match args.next() {
                Some(text) => query = Some(text),
                None => return Args::Usage,
            },
            _ => return Args::Usage,
        }
    }

    match (screenshot, query) {
        (Some(path), query) => Args::Screenshot {
            path,
            query: query.unwrap_or_default(),
            socket,
        },
        (None, None) => Args::Run { socket },
        (None, Some(_)) => Args::Usage,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn no_args_is_run() {
        assert_eq!(
            parse(std::iter::empty::<String>()),
            Args::Run { socket: None }
        );
    }

    #[test]
    fn screenshot_with_path_parses() {
        assert_eq!(
            parse(["--screenshot".to_string(), "/tmp/out.png".to_string()].into_iter()),
            Args::Screenshot {
                path: PathBuf::from("/tmp/out.png"),
                query: String::new(),
                socket: None,
            }
        );
    }

    #[test]
    fn screenshot_with_query_parses() {
        assert_eq!(
            parse(
                [
                    "--screenshot".to_string(),
                    "/tmp/out.png".to_string(),
                    "--query".to_string(),
                    "2+2".to_string(),
                ]
                .into_iter()
            ),
            Args::Screenshot {
                path: PathBuf::from("/tmp/out.png"),
                query: "2+2".to_string(),
                socket: None,
            }
        );
    }

    #[test]
    fn query_flag_order_does_not_matter() {
        assert_eq!(
            parse(
                [
                    "--query".to_string(),
                    "2+2".to_string(),
                    "--screenshot".to_string(),
                    "/tmp/out.png".to_string(),
                ]
                .into_iter()
            ),
            Args::Screenshot {
                path: PathBuf::from("/tmp/out.png"),
                query: "2+2".to_string(),
                socket: None,
            }
        );
    }

    #[test]
    fn screenshot_without_a_path_is_usage() {
        assert_eq!(parse(["--screenshot".to_string()].into_iter()), Args::Usage);
    }

    #[test]
    fn query_without_screenshot_is_usage() {
        assert_eq!(
            parse(["--query".to_string(), "2+2".to_string()].into_iter()),
            Args::Usage
        );
    }

    #[test]
    fn unknown_flag_is_usage() {
        assert_eq!(parse(["--frobnicate".to_string()].into_iter()), Args::Usage);
    }

    #[test]
    fn a_socket_flag_with_a_path_carries_it_on_run() {
        assert_eq!(
            parse(
                [
                    "--socket".to_string(),
                    "/run/user/1000/hop-dev/hopd.sock".to_string(),
                ]
                .into_iter()
            ),
            Args::Run {
                socket: Some(PathBuf::from("/run/user/1000/hop-dev/hopd.sock"))
            }
        );
    }

    #[test]
    fn a_socket_flag_carries_through_to_screenshot_regardless_of_order() {
        assert_eq!(
            parse(
                [
                    "--screenshot".to_string(),
                    "/tmp/out.png".to_string(),
                    "--socket".to_string(),
                    "/run/user/1000/hop-dev/hopd.sock".to_string(),
                ]
                .into_iter()
            ),
            Args::Screenshot {
                path: PathBuf::from("/tmp/out.png"),
                query: String::new(),
                socket: Some(PathBuf::from("/run/user/1000/hop-dev/hopd.sock")),
            }
        );
    }

    #[test]
    fn a_socket_flag_with_no_value_is_usage() {
        assert_eq!(parse(["--socket".to_string()].into_iter()), Args::Usage);
    }

    #[test]
    fn a_repeated_socket_flag_is_usage() {
        assert_eq!(
            parse(
                [
                    "--socket".to_string(),
                    "/run/user/1000/a.sock".to_string(),
                    "--socket".to_string(),
                    "/run/user/1000/b.sock".to_string(),
                ]
                .into_iter()
            ),
            Args::Usage
        );
    }
}
