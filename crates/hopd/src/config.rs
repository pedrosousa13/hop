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
use std::io;
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
    /// in this daemon, which is also never silently skipped.
    #[error("could not read config file {}: {err}", .path.display())]
    Read {
        /// The path that could not be read.
        path: PathBuf,
        #[source]
        /// The underlying IO error.
        err: io::Error,
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
    /// A `NotFound` is *not* an error: an absent config is the documented
    /// default, so a missing file maps to `Ok(Config::default())`. Any other
    /// read failure is [`ConfigError::Read`], and a file that *is* there goes
    /// through the TOML parse.
    fn from_path(path: &Path) -> Result<Config, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => parse(path, &text),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(ConfigError::Read {
                path: path.to_owned(),
                err,
            }),
        }
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

    use std::fs;

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
}
