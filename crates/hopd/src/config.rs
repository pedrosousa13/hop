//! Loads hopd's configuration from `$XDG_CONFIG_HOME/hop/config.toml`.
//!
//! This is the daemon's one read of the user's real filesystem at startup,
//! and it is deliberately read-only: nothing in this module ever writes a
//! file. The config carves out of a larger system (a launcher renders as
//! many rows as a user can look at, and matches a query term against them)
//! two knobs today — `max_results` and `max_term_chars` — under the XDG
//! Base Directory path `hop/config.toml`, because that is where the spec
//! (§9) says a launcher's config lives. A config that is absent means the
//! documented defaults; a config that exists but does not parse, or parses
//! to a value that breaks the results-frame contract (`max_results`) or
//! exceeds the ranker's absolute term-length ceiling (`max_term_chars`), is
//! an explicit error rather than a silently-invented fallback or a silent
//! clamp — the same posture [`crate::runtime_dir`] takes toward a missing
//! `XDG_RUNTIME_DIR`, and `Aliases::from_json` toward invalid JSON.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use hop_protocol::MAX_ITEMS_PER_RESULTS_FRAME;

/// The environment variable that names the config directory root. Named once
/// so the variable's spelling appears in exactly one place — including in
/// every error message that names it.
const XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";

/// The fallback base for the config directory when `XDG_CONFIG_HOME` is
/// unset, per the XDG Base Directory spec: `$HOME/.config`. Also named once.
const HOME: &str = "HOME";

/// The directory name under the config base that this daemon owns.
const CONFIG_DIR_NAME: &str = "hop";

/// The file name of the config inside that directory.
const CONFIG_FILE_NAME: &str = "config.toml";

/// A single config key's generous line budget, in bytes: the longer of the
/// two key names this file holds today (`max_term_chars`, 15 bytes; see
/// [`Config`]) plus `" = "`, the widest value either key can carry
/// (`max_results` tops out at [`MAX_ITEMS_PER_RESULTS_FRAME`], 4 digits;
/// `max_term_chars` tops out at [`hop_core::rank::MAX_TERM_CHARS`], 3
/// digits), room for a trailing `# ...` comment about as long again as the
/// assignment itself, and the newline. Rounded well past what that adds up
/// to, so a key with a slightly longer name or a decimal-looking value still
/// fits without this constant needing to move.
const CONFIG_KEY_LINE_BYTES: u64 = 128;

/// Headroom, in key-lines, for scalar keys this file does not have yet — not
/// a count of the two it has today. `max_term_chars` itself arrived after
/// `max_results` (issue #46), and the point of budgeting ahead is that the
/// next knob like it should not force [`MAX_CONFIG_BYTES`] to move along
/// with it. Eight times today's key count is comfortably more than a config
/// holding "a handful of scalar keys" — the shape issue #160 itself
/// describes this file as having — will hold before this constant is worth
/// revisiting on its own merits.
const MAX_CONFIG_KEYS: u64 = 16;

/// Budget, in bytes, for what actually dominates a hand-written config in
/// this repo's own commenting style: prose. A config file documented the way
/// this module documents itself — a paragraph of `#`-prefixed explanation
/// above each key — runs to kilobytes without a single extra key. Eight KiB
/// covers that comfortably while staying nowhere near "unbounded".
const CONFIG_COMMENT_BUDGET_BYTES: u64 = 8 * 1024;

/// The byte ceiling on a config file's contents, enforced in
/// [`Config::from_path`] via [`std::io::Read::take`] before the bytes ever
/// reach the TOML parser — the same shape `hop-core::learning`'s
/// `MAX_STORE_BYTES` uses for the learning store (issue #37: a `take(cap +
/// 1)` read, so an over-cap file is detected without ever allocating it),
/// sized down for how much smaller this file's job is.
///
/// `CONFIG_KEY_LINE_BYTES * MAX_CONFIG_KEYS` prices a config built from
/// several times today's key count; `CONFIG_COMMENT_BUDGET_BYTES` prices the
/// prose around them, which is what a config in this repo's style would
/// actually spend most of its bytes on. The total — a little over ten
/// KiB — is nowhere near enough to trouble memory even fully buffered, and
/// nowhere near small enough to reject an ordinarily-commented config that
/// only sets these two knobs.
const MAX_CONFIG_BYTES: u64 = CONFIG_KEY_LINE_BYTES * MAX_CONFIG_KEYS + CONFIG_COMMENT_BUDGET_BYTES;

/// hopd's configuration, loaded once at startup and never written.
///
/// Two fields today: how many results the daemon assembles for a query, and
/// how many characters of a query term the ranker matches against. The
/// struct is deliberately not `#[non_exhaustive]` — future keys arrive here
/// with the slices that read them, rather than being anticipated ahead of
/// time (see Design decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The `max_results` passed to the pipeline on every assembly.
    pub max_results: usize,
    /// The `max_term_chars` set on the pipeline's `Weights`, capping how
    /// many characters of a query term reach `Pattern::new` — see
    /// [`hop_core::rank::Weights::max_term_chars`].
    pub max_term_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        // The single source of truth for `max_results`'s default, shared
        // with the daemon's compile-time frame-bound assertion in
        // `source.rs`; `max_term_chars`'s default and ceiling are likewise
        // one source of truth, shared with the ranker itself.
        Self {
            max_results: crate::source::MAX_RESULTS,
            max_term_chars: hop_core::rank::MAX_TERM_CHARS,
        }
    }
}

/// Errors loading a config. Anything that means "there was a config to read,
/// or a config that could not be parsed safely" is a distinct, explicit error
/// rather than a silent fallback to defaults.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Neither `XDG_CONFIG_HOME` nor `HOME` is set, so no config path can be
    /// derived at all.
    #[error("neither {XDG_CONFIG_HOME} nor {HOME} is set; cannot locate a config directory")]
    MissingHome,

    /// A config file exists but could not be read — most plausibly a
    /// permission error, the same class as an unreadable file anywhere else
    /// in this daemon, which is also never silently skipped. Also covers a
    /// descriptor that opened but could not be inspected (`fstat` failing),
    /// a regular file whose bytes are not valid UTF-8, and a Unix domain
    /// socket at the config path: `open()` on a socket fails immediately
    /// with `ENXIO`, before there is a descriptor for `fstat` to classify,
    /// so it never reaches [`ConfigError::NotARegularFile`] the way a FIFO
    /// or a device does — it lands here instead, alongside every other way
    /// the open itself can fail. Four different failures, but all of them
    /// "this file could not be turned into text", which is this variant's
    /// whole scope; what the text says once it exists is
    /// [`ConfigError::Parse`]'s.
    #[error("could not read config file {}: {err}", .path.display())]
    Read {
        /// The path that could not be read.
        path: PathBuf,
        #[source]
        /// The underlying IO error.
        err: io::Error,
    },

    /// The config path resolves to something other than a regular file — a
    /// directory, a FIFO, a device, a socket. Reported of the *descriptor*
    /// returned by the open (`fstat`, via [`std::fs::File::metadata`]), not
    /// of the path — see [`Config::from_path`]'s doc comment for why that
    /// distinction is the whole point of this check (issue #160). Distinct
    /// from [`ConfigError::Read`]: the open itself succeeded here, and it is
    /// the object's type, not an I/O failure, that disqualifies it.
    #[error("config file {} is not a regular file", .path.display())]
    NotARegularFile {
        /// The path that did not resolve to a regular file.
        path: PathBuf,
    },

    /// The config file is larger than [`MAX_CONFIG_BYTES`] (issue #160; see
    /// that constant's doc comment for the cap's reasoning). Distinct from
    /// [`ConfigError::Read`] and [`ConfigError::Parse`]: the bytes past the
    /// cap are never read off the descriptor, so whether they would have
    /// been valid, readable TOML is never known — this file is refused for
    /// its size alone.
    #[error("config file {} is larger than the {MAX_CONFIG_BYTES}-byte limit", .path.display())]
    TooLarge {
        /// The path whose contents exceeded [`MAX_CONFIG_BYTES`].
        path: PathBuf,
    },

    /// A config file exists but is not valid TOML. The error names the path
    /// so a user can open the offending file.
    #[error("config file {} is not valid TOML: {err}", .path.display())]
    Parse {
        /// The path that did not parse.
        path: PathBuf,
        #[source]
        /// The TOML parse error.
        err: toml::de::Error,
    },

    /// `max_results` is present but is not a whole number.
    #[error("config `max_results` in {} is not a whole number", .path.display())]
    MaxResultsNotInteger {
        /// The config path that carried the bad value.
        path: PathBuf,
    },

    /// `max_results` is an integer below the valid range of `1..=MAX_ITEMS_PER_RESULTS_FRAME`.
    #[error(
        "config `max_results` in {} is {value}, but at least 1 is required",
        .path.display()
    )]
    MaxResultsOutOfRange {
        /// The config path that carried the bad value.
        path: PathBuf,
        /// The offending value, kept as the signed integer the TOML carried.
        value: i64,
    },

    /// `max_results` exceeds the maximum items one results frame may carry, so
    /// honoring it would let a config break the replace-frame invariant the
    /// daemon's own `source.rs` guards at compile time (`assert!(MAX_RESULTS
    /// <= MAX_ITEMS_PER_RESULTS_FRAME)`). Refused at load time instead of
    /// clamping, exactly as the assertion refuses a raised constant.
    #[error(
        "config `max_results` in {} is {value}, which exceeds the maximum of {MAX_ITEMS_PER_RESULTS_FRAME} items per results frame",
        .path.display()
    )]
    MaxResultsOverFrame {
        /// The config path that carried the bad value.
        path: PathBuf,
        /// The offending value.
        value: usize,
    },

    /// `max_term_chars` is present but is not a whole number.
    #[error("config `max_term_chars` in {} is not a whole number", .path.display())]
    MaxTermCharsNotInteger {
        /// The config path that carried the bad value.
        path: PathBuf,
    },

    /// `max_term_chars` is an integer below the valid range of
    /// `1..=MAX_TERM_CHARS`.
    #[error(
        "config `max_term_chars` in {} is {value}, but at least 1 is required",
        .path.display()
    )]
    MaxTermCharsOutOfRange {
        /// The config path that carried the bad value.
        path: PathBuf,
        /// The offending value, kept as the signed integer the TOML carried.
        value: i64,
    },

    /// `max_term_chars` exceeds [`hop_core::rank::MAX_TERM_CHARS`], the
    /// ranker's absolute ceiling on the knob — refused at load time instead
    /// of clamping, exactly as `MaxResultsOverFrame` refuses a `max_results`
    /// that would break the frame contract rather than truncating it down.
    #[error(
        "config `max_term_chars` in {} is {value}, which exceeds the maximum of {} characters",
        .path.display(),
        hop_core::rank::MAX_TERM_CHARS
    )]
    MaxTermCharsOverCeiling {
        /// The config path that carried the bad value.
        path: PathBuf,
        /// The offending value.
        value: usize,
    },
}

impl Config {
    /// Loads the config from the real environment, reading `XDG_CONFIG_HOME`
    /// (falling back to `$HOME/.config` when it is unset) plus `hop/config.toml`.
    ///
    /// # Why the env is read here, in a thin wrapper
    ///
    /// The path computation that follows is a pure function of the two
    /// variables — see [`Config::load_from_env`] — precisely so the unit
    /// tests below can pin the fallback and error behaviors without touching
    /// the process environment. Reading the environment *is* this function's
    /// whole job; everything load-bearing about the path lives one level down
    /// and is testable with explicit values.
    ///
    /// # Errors
    ///
    /// [`ConfigError::MissingHome`] if neither variable is set, and a read or
    /// parse error if the config exists but is unusable. Beyond that, each of
    /// the two knobs has its own error family, refused rather than clamped:
    /// [`ConfigError::MaxResultsOutOfRange`] or
    /// [`ConfigError::MaxResultsNotInteger`] if `max_results` isn't a usable
    /// positive integer, and [`ConfigError::MaxResultsOverFrame`] if it would
    /// break the frame contract; [`ConfigError::MaxTermCharsOutOfRange`] or
    /// [`ConfigError::MaxTermCharsNotInteger`] if `max_term_chars` isn't a
    /// usable positive integer, and [`ConfigError::MaxTermCharsOverCeiling`]
    /// if it would exceed the ranker's term-length ceiling. An absent config
    /// file is [`Ok`], by contract, never an error.
    pub fn load() -> Result<Config, ConfigError> {
        let xdg = env::var(XDG_CONFIG_HOME).ok().filter(|v| !v.is_empty());
        let home = env::var(HOME).ok().filter(|v| !v.is_empty());
        Self::load_from_env(xdg, home)
    }

    /// The pure core of [`Config::load`]: given the *values* of
    /// `XDG_CONFIG_HOME` and `HOME`, resolves the config path and loads it.
    ///
    /// This is the function the unit tests exercise, because it takes the
    /// environment as explicit parameters rather than reading it — the
    /// workspace denies `unsafe_code` (and Rust 2024 makes `env::set_var`
    /// `unsafe`), so tests cannot safely mutate process env.
    fn load_from_env(
        xdg_config_home: Option<String>,
        home: Option<String>,
    ) -> Result<Config, ConfigError> {
        let xdg_config_home = xdg_config_home.filter(|v| !v.is_empty());
        let home = home.filter(|v| !v.is_empty());
        let base_dir = match xdg_config_home {
            Some(dir) => PathBuf::from(dir),
            None => match home {
                Some(home_dir) => PathBuf::from(home_dir).join(".config"),
                None => return Err(ConfigError::MissingHome),
            },
        };
        Self::from_path(&base_dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
    }

    /// Loads from a concrete config file path.
    ///
    /// # Open, classify, read — in that order (issue #160)
    ///
    /// Before this issue this was `fs::read_to_string(path)`: unbounded, and
    /// with no check that the thing it opened was even a regular file.
    /// `run()` in `lib.rs` calls the config loader before it creates the
    /// runtime directory or binds the socket, so anything that stalled here
    /// stalled the whole daemon with no socket and no diagnostic anywhere a
    /// user could see it.
    ///
    /// **The open carries `O_NONBLOCK`.** Opening a FIFO for reading
    /// otherwise blocks until a writer appears — forever, for a FIFO nobody
    /// is writing to. `O_NONBLOCK` on a read-only open of a FIFO returns a
    /// descriptor immediately instead of waiting, which is what lets the
    /// next step classify and refuse it rather than hang on it. On a regular
    /// file the flag has no effect at all, so an ordinary config opens and
    /// reads exactly as before. This is the same flag
    /// `hop-protocol::content::IconPath::open_regular_file` carries for the
    /// identical hazard on the icon-open path (issue #131), the closest
    /// precedent in this workspace for "open under a policy that cannot
    /// block acquiring a special-file descriptor."
    ///
    /// **What is classified is the descriptor, not the path.**
    /// [`std::fs::File::metadata`] is `fstat` on the file this call already
    /// has open — it reports the object the open actually returned, not
    /// whatever the path names by the time this line runs. `fs::metadata`
    /// on the path instead would stat a second, possibly different, object
    /// (see "Replacement", below); classifying the descriptor is what
    /// guarantees the check and the read that follows name the same file.
    /// A directory, device, socket or FIFO fails
    /// `metadata.is_file()` and is never read from — reported as
    /// [`ConfigError::NotARegularFile`].
    ///
    /// **The read is bounded through [`MAX_CONFIG_BYTES`]**, via
    /// [`std::io::Read::take`] — see that constant's doc comment for why the
    /// number is what it is. This is `hop-core::learning`'s bounded-read
    /// precedent (issue #37), followed rather than reinvented:
    /// `take(MAX_CONFIG_BYTES + 1)` rather than `take(MAX_CONFIG_BYTES)` is
    /// what tells a file sitting exactly on the cap (accepted) apart from
    /// one a single byte over it (refused) without the read — or the
    /// `Vec<u8>` it fills — ever growing past `MAX_CONFIG_BYTES + 1` bytes to
    /// find out. Measured before decoding, for the same reason
    /// `learning.rs` measures before decoding: `take` cuts at a byte offset
    /// and can land inside a multibyte character, so decoding first would
    /// report an unrelated UTF-8 failure for a file that was really refused
    /// for its size.
    ///
    /// # Symlinks
    ///
    /// The open follows a symlink at the config path, exactly as
    /// `fs::read_to_string` did before this change: pointing
    /// `~/.config/hop/config.toml` at a config that lives elsewhere is
    /// ordinary and must keep working, the same reasoning
    /// `IconPath::open_regular_file`'s docs give for not adding
    /// `O_NOFOLLOW`. What is classified is what the link *resolves to* — a
    /// symlink to a regular file loads, a symlink to `/dev/zero` does not,
    /// because the `fstat` sees the character device on the other end of the
    /// link rather than the link itself.
    ///
    /// A symlink whose target does not exist — dangling, because the file it
    /// once pointed at was moved or removed — fails the open with `ENOENT`,
    /// the identical error a config path that names nothing at all fails
    /// with. It is therefore handled by the same `NotFound` arm below and
    /// reads as an absent config: deliberately, since a link that resolves
    /// to nothing is indistinguishable, from the daemon's point of view,
    /// from no config having been placed there.
    ///
    /// # Replacement between open and read
    ///
    /// The open and every read that follows act on one file descriptor, not
    /// on the path a second time, so once the open succeeds this function
    /// reads the object it opened even if the path is unlinked, replaced, or
    /// repointed at something else immediately afterward — the classic
    /// TOCTOU window is closed by construction here, because there is only
    /// ever one path lookup. What remains is the window *before* that one
    /// lookup: nothing stops a concurrent write from finishing, or a
    /// replacement from landing, in the instant before the open runs, and
    /// the open simply reads whatever was there then. Nothing after the open
    /// can change which object gets classified or read.
    ///
    /// # Errors
    ///
    /// A `NotFound` on the open is *not* an error: an absent config is the
    /// documented default, so a missing file maps to `Ok(Config::default())`.
    /// Any other open or inspection failure, or bytes that are not valid
    /// UTF-8, is [`ConfigError::Read`]. Something other than a regular file
    /// is [`ConfigError::NotARegularFile`]. A file over [`MAX_CONFIG_BYTES`]
    /// is [`ConfigError::TooLarge`]. A file that is both a regular file and
    /// within the cap goes through the TOML parse.
    fn from_path(path: &Path) -> Result<Config, ConfigError> {
        let file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(err) => {
                return Err(ConfigError::Read {
                    path: path.to_owned(),
                    err,
                });
            }
        };

        // `File::metadata` is `fstat` on this descriptor: it reports the
        // object that was actually opened, never the path.
        let metadata = file.metadata().map_err(|err| ConfigError::Read {
            path: path.to_owned(),
            err,
        })?;
        if !metadata.is_file() {
            return Err(ConfigError::NotARegularFile {
                path: path.to_owned(),
            });
        }

        let mut data = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|err| ConfigError::Read {
                path: path.to_owned(),
                err,
            })?;
        if data.len() as u64 > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge {
                path: path.to_owned(),
            });
        }

        let text = String::from_utf8(data).map_err(|err| ConfigError::Read {
            path: path.to_owned(),
            err: io::Error::new(io::ErrorKind::InvalidData, err.utf8_error()),
        })?;

        parse(path, &text)
    }
}

/// Parses a config file's text into a [`Config`], refusing anything that
/// cannot be honored safely.
///
/// Parsed via [`toml::Value`] rather than a serde `Deserialize` derive so
/// that this crate needs no serde dependency of its own — the toml crate's
/// generic value tree already does the parse, and each field is read out of
/// it directly. A key absent from the file falls back to that field's
/// default; a value that is not a usable positive integer within its valid
/// range (`1..=MAX_ITEMS_PER_RESULTS_FRAME` for `max_results`,
/// `1..=hop_core::rank::MAX_TERM_CHARS` for `max_term_chars`) is an explicit
/// error, never a clamp.
fn parse(path: &Path, text: &str) -> Result<Config, ConfigError> {
    let value: toml::Value = toml::from_str(text).map_err(|err| ConfigError::Parse {
        path: path.to_owned(),
        err,
    })?;

    let max_results = match value.get("max_results") {
        None => Config::default().max_results,
        Some(v) => {
            let n = match v.as_integer() {
                Some(n) => n,
                None => {
                    return Err(ConfigError::MaxResultsNotInteger {
                        path: path.to_owned(),
                    });
                }
            };
            validate_max_results(path, n)?
        }
    };

    let max_term_chars = match value.get("max_term_chars") {
        None => Config::default().max_term_chars,
        Some(v) => {
            let n = match v.as_integer() {
                Some(n) => n,
                None => {
                    return Err(ConfigError::MaxTermCharsNotInteger {
                        path: path.to_owned(),
                    });
                }
            };
            validate_max_term_chars(path, n)?
        }
    };

    Ok(Config {
        max_results,
        max_term_chars,
    })
}

/// Validates a parsed `max_results` integer against the valid range.
fn validate_max_results(path: &Path, n: i64) -> Result<usize, ConfigError> {
    if n < 1 {
        return Err(ConfigError::MaxResultsOutOfRange {
            path: path.to_owned(),
            value: n,
        });
    }
    let n = n as usize;
    if n > MAX_ITEMS_PER_RESULTS_FRAME {
        return Err(ConfigError::MaxResultsOverFrame {
            path: path.to_owned(),
            value: n,
        });
    }
    Ok(n)
}

/// Validates a parsed `max_term_chars` integer against the valid range.
fn validate_max_term_chars(path: &Path, n: i64) -> Result<usize, ConfigError> {
    if n < 1 {
        return Err(ConfigError::MaxTermCharsOutOfRange {
            path: path.to_owned(),
            value: n,
        });
    }
    let n = n as usize;
    if n > hop_core::rank::MAX_TERM_CHARS {
        return Err(ConfigError::MaxTermCharsOverCeiling {
            path: path.to_owned(),
            value: n,
        });
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Writes `text` into a fresh temp dir and returns `(config, dir)`.
    /// The dir is kept alive so the file persists for the read.
    fn config_from_text(text: &str) -> (Config, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        let config =
            Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        (config, dir)
    }

    #[test]
    fn absent_file_uses_defaults() {
        // An empty temp dir holds no config file, so the load must not error
        // and must yield the documented default — never a silent parse of
        // nothing.
        let dir = tempfile::tempdir().unwrap();
        let config =
            Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.max_results, crate::source::MAX_RESULTS);
    }

    #[test]
    fn malformed_toml_is_an_error_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "max_results = [not valid").unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match err {
            ConfigError::Parse { path: p, .. } => {
                assert_eq!(p, path, "parse error must name the config path");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn valid_flat_toml_parses_max_results() {
        let (config, _dir) = config_from_text("max_results = 12");
        assert_eq!(config.max_results, 12);
    }

    #[test]
    fn absent_max_results_key_uses_the_default() {
        let (config, _dir) = config_from_text("some_future_key = 1");
        assert_eq!(config.max_results, Config::default().max_results);
    }

    #[test]
    fn over_frame_max_results_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("max_results = {}", MAX_ITEMS_PER_RESULTS_FRAME + 1),
        )
        .unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match &err {
            ConfigError::MaxResultsOverFrame { path: p, value } => {
                assert_eq!(*p, path);
                assert_eq!(*value, MAX_ITEMS_PER_RESULTS_FRAME + 1);
            }
            other => panic!("expected MaxResultsOverFrame, got {other:?}"),
        }
        assert!(
            err.to_string().contains(&path.display().to_string()),
            "error message must name the config path: {err}"
        );
    }

    #[test]
    fn frame_bound_max_results_is_accepted() {
        let (config, _dir) =
            config_from_text(&format!("max_results = {MAX_ITEMS_PER_RESULTS_FRAME}"));
        assert_eq!(config.max_results, MAX_ITEMS_PER_RESULTS_FRAME);
    }

    #[test]
    fn zero_max_results_is_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "max_results = 0").unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, ConfigError::MaxResultsOutOfRange { value: 0, .. }),
            "expected MaxResultsOutOfRange, got {err:?}"
        );
    }

    #[test]
    fn missing_both_envs_is_an_explicit_error() {
        // Neither XDG_CONFIG_HOME nor HOME set: there is no way to derive a
        // config path, and inventing one would be a silent fallback. Must be
        // an explicit error, the same posture runtime_dir takes.
        let err = Config::load_from_env(None, None).unwrap_err();
        assert!(matches!(err, ConfigError::MissingHome), "got {err:?}");
    }

    #[test]
    fn home_fallback_is_honored() {
        // XDG_CONFIG_HOME unset, HOME set: the path must be
        // `$HOME/.config/hop/config.toml`. This pins the XDG spec's fallback
        // at the pure-function level, without touching process env.
        let home = tempfile::tempdir().unwrap();
        let config_path = home
            .path()
            .join(".config")
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME);
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, "max_results = 7").unwrap();

        let home_str = home.path().to_string_lossy().into_owned();
        let config = Config::load_from_env(None, Some(home_str)).unwrap();
        assert_eq!(config.max_results, 7);
    }

    // --- `max_term_chars` (issue #46's remaining acceptance criterion): a
    // second, independent knob, mirroring `max_results`'s shape exactly.

    #[test]
    fn absent_max_term_chars_key_uses_the_default() {
        let (config, _dir) = config_from_text("some_future_key = 1");
        assert_eq!(config.max_term_chars, Config::default().max_term_chars);
        assert_eq!(config.max_term_chars, hop_core::rank::MAX_TERM_CHARS);
    }

    #[test]
    fn valid_flat_toml_parses_max_term_chars() {
        let (config, _dir) = config_from_text("max_term_chars = 64");
        assert_eq!(config.max_term_chars, 64);
    }

    #[test]
    fn zero_max_term_chars_is_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "max_term_chars = 0").unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, ConfigError::MaxTermCharsOutOfRange { value: 0, .. }),
            "expected MaxTermCharsOutOfRange, got {err:?}"
        );
    }

    #[test]
    fn over_ceiling_max_term_chars_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let over = hop_core::rank::MAX_TERM_CHARS + 1;
        fs::write(&path, format!("max_term_chars = {over}")).unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match &err {
            ConfigError::MaxTermCharsOverCeiling { path: p, value } => {
                assert_eq!(*p, path);
                assert_eq!(*value, over);
            }
            other => panic!("expected MaxTermCharsOverCeiling, got {other:?}"),
        }
        assert!(
            err.to_string().contains(&path.display().to_string()),
            "error message must name the config path: {err}"
        );
    }

    #[test]
    fn ceiling_bound_max_term_chars_is_accepted() {
        let (config, _dir) = config_from_text(&format!(
            "max_term_chars = {}",
            hop_core::rank::MAX_TERM_CHARS
        ));
        assert_eq!(config.max_term_chars, hop_core::rank::MAX_TERM_CHARS);
    }

    #[test]
    fn non_integer_max_term_chars_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "max_term_chars = \"lots\"").unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, ConfigError::MaxTermCharsNotInteger { .. }),
            "expected MaxTermCharsNotInteger, got {err:?}"
        );
    }

    #[test]
    fn empty_xdg_config_home_falls_back_to_home() {
        // An empty-but-set XDG_CONFIG_HOME is treated as unset, per the XDG
        // spec and this module's docs — it must not become an empty path.
        let home = tempfile::tempdir().unwrap();
        let config_path = home
            .path()
            .join(".config")
            .join(CONFIG_DIR_NAME)
            .join(CONFIG_FILE_NAME);
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(&config_path, "max_results = 3").unwrap();

        let home_str = home.path().to_string_lossy().into_owned();
        let config = Config::load_from_env(Some(String::new()), Some(home_str)).unwrap();
        assert_eq!(config.max_results, 3);
    }

    // --- File classification and the byte cap (issue #160) -----------------
    //
    // Before this issue, `Config::from_path` was `fs::read_to_string(path)`:
    // no check that the thing at the path was a regular file, and no bound
    // on how many bytes it would buffer. Every case below is a way that used
    // to go wrong — a FIFO blocked the daemon before it ever bound a socket,
    // a symlink to `/dev/zero` grew the buffer forever, and a large regular
    // file was fully read before TOML ever saw a byte of it — mirroring
    // `hop-protocol::content`'s `IconOpenError` tests (issue #131) for the
    // open-then-fstat shape, and `hop-core::learning`'s `MAX_STORE_BYTES`
    // tests (issue #37) for the `take(cap + 1)` shape.

    #[test]
    fn a_directory_at_the_config_path_is_not_a_regular_file() {
        // A directory opens read-only without complaint on Linux — only the
        // fstat catches it. `create_dir_all` on the config path itself makes
        // the path a directory rather than a file underneath it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(&path).unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match &err {
            ConfigError::NotARegularFile { path: p } => assert_eq!(*p, path),
            other => panic!("expected NotARegularFile, got {other:?}"),
        }
    }

    #[test]
    fn a_fifo_at_the_config_path_is_not_opened_and_does_not_block_load() {
        // The hazard here happens *before* any check could run: opening a
        // FIFO for reading blocks until a writer appears, so a regression
        // here would stall `hopd::run` before it ever creates the runtime
        // directory or binds the socket (see `lib.rs::run`). `O_NONBLOCK` is
        // what makes the open return instead of waiting.
        //
        // This test is written so that losing that flag makes it *fail*
        // rather than hang the suite: the load runs on a worker thread and
        // the result is awaited with a timeout. A test whose only defense
        // against hanging is the guard it is testing is not a test of that
        // guard.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a live NUL-terminated string for the duration
        // of the call, which is all `mkfifo(3)` requires of it.
        //
        // Test-only, on the same footing as this workspace's other test-only
        // `unsafe` (`hop-protocol::content`'s `libc::mkfifo` call for the
        // identical FIFO-open hazard, issue #131; `hopd::tests::activation`'s
        // `pre_exec`): none of the three is production code, and each needs
        // its own narrow `#[expect(unsafe_code)]` to build at all under this
        // workspace's `unsafe_code = "deny"` lint. `expect` rather than
        // `allow` so that if `mkfifo` ever grows a safe wrapper, the
        // unfulfilled expectation becomes a warning and CI's `-D warnings`
        // turns that into a build failure — the exception deletes itself
        // instead of outliving its reason.
        #[expect(
            unsafe_code,
            reason = "mkfifo(3) has no safe wrapper in libc; test-only, and production code \
                      has none, matching hop-protocol::content's precedent for this exact hazard"
        )]
        let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(
            made,
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );

        let dir_str = dir.path().to_string_lossy().into_owned();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(Config::load_from_env(Some(dir_str), None).map(|_| ()));
        });

        let result = rx.recv_timeout(Duration::from_secs(10)).expect(
            "loading a FIFO config must return rather than wait for a writer; timing out \
             here means the open blocked",
        );
        let err = result.expect_err("a FIFO is not a regular file");
        assert!(
            matches!(err, ConfigError::NotARegularFile { .. }),
            "expected NotARegularFile, got {err:?}"
        );
    }

    #[test]
    fn a_unix_socket_at_the_config_path_is_a_bounded_error() {
        // A fourth way the config path can name something other than a
        // regular file, beyond the directory/FIFO/device trio above: a Unix
        // domain socket. `open()` on a socket fails immediately with
        // `ENXIO` — before there is a descriptor for `fstat` to classify —
        // so this lands in `ConfigError::Read` rather than
        // `NotARegularFile` (see that variant's doc comment for why). Which
        // variant it lands in matters less than that it lands in *some*
        // bounded, diagnostic error: unlike opening a FIFO, opening a
        // socket never blocks, so this test needs no timeout guard the way
        // the FIFO test above does.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _listener = UnixListener::bind(&path).unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, ConfigError::Read { .. }),
            "expected Read, got {err:?}"
        );
    }

    #[test]
    fn a_symlink_to_a_character_device_at_the_config_path_is_not_a_regular_file() {
        // The endless-read case the issue names by name: `/dev/zero` never
        // ends, so a decoder reading it to completion never returns. The
        // fstat sees the character device the symlink resolves to, not the
        // symlink itself, so this is refused exactly like `/dev/zero`
        // directly at the config path would be.
        let dir = tempfile::tempdir().unwrap();
        let hop_dir = dir.path().join(CONFIG_DIR_NAME);
        fs::create_dir_all(&hop_dir).unwrap();
        let path = hop_dir.join(CONFIG_FILE_NAME);
        symlink("/dev/zero", &path).unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        assert!(
            matches!(err, ConfigError::NotARegularFile { .. }),
            "expected NotARegularFile, got {err:?}"
        );
    }

    #[test]
    fn a_symlink_to_a_regular_config_file_is_followed_and_loads() {
        // The other half of the pair above, and the reason the open does not
        // refuse symlinks outright: pointing `~/.config/hop/config.toml` at a
        // config that lives elsewhere is an ordinary thing to do (the same
        // reasoning `hop-protocol::content::IconPath::open_regular_file`
        // documents for icon themes) and must keep working. What is
        // classified is what the link *resolves to* — here, a regular file —
        // not the link itself.
        let dir = tempfile::tempdir().unwrap();
        let hop_dir = dir.path().join(CONFIG_DIR_NAME);
        fs::create_dir_all(&hop_dir).unwrap();
        let real = dir.path().join("real-config.toml");
        fs::write(&real, "max_results = 9").unwrap();
        let path = hop_dir.join(CONFIG_FILE_NAME);
        symlink(&real, &path).unwrap();

        let config =
            Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        assert_eq!(config.max_results, 9);
    }

    #[test]
    fn a_dangling_symlink_at_the_config_path_reads_as_absent() {
        // The other case the "Symlinks" doc section covers, beyond
        // symlink-to-regular and symlink-to-device above: a symlink whose
        // target has been moved or removed. `open()` fails with `ENOENT`,
        // the same error a config path that names nothing at all produces,
        // so this falls into the `NotFound` arm and yields the defaults —
        // deliberately, since a link resolving to nothing looks, from here,
        // exactly like no config having been placed at all.
        let dir = tempfile::tempdir().unwrap();
        let hop_dir = dir.path().join(CONFIG_DIR_NAME);
        fs::create_dir_all(&hop_dir).unwrap();
        let path = hop_dir.join(CONFIG_FILE_NAME);
        symlink(hop_dir.join("gone-config.toml"), &path).unwrap();

        let config =
            Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn a_config_file_over_the_byte_cap_is_refused() {
        // The bytes are never parsed, so their content does not matter here
        // — only their length disqualifies this file. One byte past
        // `MAX_CONFIG_BYTES` is the smallest input that must be refused,
        // pinning the `take(cap + 1)` boundary from the other side of the
        // one below.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = "x".repeat(MAX_CONFIG_BYTES as usize + 1);
        fs::write(&path, &text).unwrap();

        let err = Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None)
            .unwrap_err();
        match &err {
            ConfigError::TooLarge { path: p } => assert_eq!(*p, path),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn a_realistically_commented_config_file_parses_well_under_the_cap() {
        // `MAX_CONFIG_BYTES`'s doc comment justifies
        // `CONFIG_COMMENT_BUDGET_BYTES` by claiming 8 KiB prices "a
        // heavily-commented file in this repo's own prose-per-key doc
        // style" (issue #160) — a claim nothing tested until now. The test
        // above pins the `take(cap + 1)` boundary with a fabricated
        // `x`-repeat pad, which proves the boundary is exactly where the
        // constant says it is, but proves nothing about what a config
        // actually written the way this module writes its own doc comments
        // would cost. This test builds that file instead: every key
        // `Config` supports, each preceded by a paragraph in this file's
        // own commenting style, mirroring `hop-core::learning`'s
        // `the_largest_store_save_can_write_still_reloads_intact` (issue
        // #37) — close the loop with the actual maximal legitimate value,
        // not a synthetic stand-in.
        let text = "\
# `max_results` controls how many rows the daemon assembles for a single
# query — the count that flows end to end from the ranker's output through
# to what the launcher UI renders. Must be a whole number from 1 up to the
# maximum number of items one results frame may carry; anything outside
# that range is refused at load time rather than silently clamped, so a
# typo here is a startup error instead of a launcher that quietly behaves
# differently. Unset, this defaults to the daemon's own compiled-in default.
max_results = 10

# `max_term_chars` caps how many characters of a query term reach the
# ranker's pattern match — the same absolute ceiling
# `hop_core::rank::MAX_TERM_CHARS` enforces everywhere else a term is
# scored. Must be a whole number from 1 up to that ceiling; anything
# outside that range is refused at load time, the same posture
# `max_results` takes toward its own range. Unset, this defaults to the
# ranker's own compiled-in ceiling.
max_term_chars = 64
";

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();

        let config =
            Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        assert_eq!(config.max_results, 10);
        assert_eq!(config.max_term_chars, 64);
        assert!(
            (text.len() as u64) < MAX_CONFIG_BYTES,
            "a realistically-commented config carrying every key came to {} bytes, against a \
             {MAX_CONFIG_BYTES}-byte cap — a future key that pushes a real config past this \
             cap must fail here, with a legible byte count, before it fails on somebody's \
             actual config file",
            text.len()
        );
    }

    #[test]
    fn a_config_file_exactly_at_the_byte_cap_is_accepted() {
        // The largest file the cap still admits: valid TOML padded with a
        // trailing comment out to exactly `MAX_CONFIG_BYTES`. Pins the `+ 1`
        // in `take(MAX_CONFIG_BYTES + 1)` from the accepting side — a file
        // sitting exactly on the ceiling must still load.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let base = "max_results = 5\n# ";
        let pad_len = MAX_CONFIG_BYTES as usize - base.len();
        let text = format!("{base}{}", "x".repeat(pad_len));
        assert_eq!(
            text.len() as u64,
            MAX_CONFIG_BYTES,
            "test fixture must sit exactly on the cap"
        );
        fs::write(&path, &text).unwrap();

        let config =
            Config::load_from_env(Some(dir.path().to_string_lossy().into_owned()), None).unwrap();
        assert_eq!(config.max_results, 5);
    }
}
