//! `hop-gtk`'s argument parsing — hand-rolled, in the same spirit as
//! `hop-cli`'s (`crates/hop-cli/src/lib.rs`'s "Why no clap" doc comment):
//! two flags, no subcommands, nothing a parser generator would earn its
//! dependency weight back on.

use std::path::PathBuf;

/// What `hop-gtk`'s argument list resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Args {
    /// Ordinary interactive run: register as the (unique) GApplication
    /// instance, build the pre-built hidden window, and let `activate`
    /// present it — see `app`'s module doc for why a plain re-invocation of
    /// this same binary is what `hop toggle` (§8's control-message path)
    /// resolves to.
    Run,
    /// `--screenshot <path>`: drive to the state reached by `query` (empty
    /// string, the default, is the empty-query state) and render it to a PNG
    /// at `path`, then exit. Acceptance criterion 7.
    Screenshot { path: PathBuf, query: String },
    /// Bad arguments: an unrecognized flag, a flag missing its value, or
    /// `--query` given without `--screenshot` (there is nothing for it to
    /// drive in an interactive run, where the query comes from the entry
    /// widget instead).
    Usage,
}

/// The line `main` prints to stderr for [`Args::Usage`].
pub const USAGE: &str = "usage: hop-gtk [--screenshot <path> [--query <text>]]";

/// Parses `args` — the process's arguments with `argv[0]` already stripped.
pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Args {
    let mut screenshot: Option<PathBuf> = None;
    let mut query: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
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
        },
        (None, None) => Args::Run,
        (None, Some(_)) => Args::Usage,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn no_args_is_run() {
        assert_eq!(parse(std::iter::empty::<String>()), Args::Run);
    }

    #[test]
    fn screenshot_with_path_parses() {
        assert_eq!(
            parse(["--screenshot".to_string(), "/tmp/out.png".to_string()].into_iter()),
            Args::Screenshot {
                path: PathBuf::from("/tmp/out.png"),
                query: String::new(),
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
}
