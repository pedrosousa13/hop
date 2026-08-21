//! The `[hotkey]` section of `config.toml`: what it looks like, how it is
//! read, and why every way it can go wrong degrades to a logged no-op
//! rather than a crash (issue #234's acceptance criterion 2).
//!
//! # The schema
//!
//! ```toml
//! [hotkey]
//! toggle = "ctrl+alt+space"          # one binding, or:
//! # toggle = ["ctrl+alt+space", "super+space"]  # several
//! ```
//!
//! One key today, `toggle`, naming the binding(s) that run the universal
//! toggle — design spec §3's "DE-configured shortcut runs `hop toggle`",
//! except this daemon *is* the shortcut on X11. The value is a flat string
//! or an array of flat strings, spelled exactly like `[keymap]`'s values
//! (`binding.rs` carries the notation and its reasoning). An array was
//! worth the second accepted shape here where it was not in `keymap`: the
//! issue asks for "the configured key(s)", and a user whose keyboard makes
//! one of two candidate keys awkward genuinely needs two grabs, not a
//! schema that forces them to pick.
//!
//! # Absent or malformed: logged no-op, never a crash
//!
//! Criterion 2 fixes the degradation posture explicitly, and it is a
//! different posture from the sibling config readers on purpose:
//!
//! - `hopd::config` *refuses to start* on a malformed value, because a
//!   daemon serving queries under silently-wrong tuning knobs is worse than
//!   no daemon.
//! - `hop-gtk::keymap` likewise refuses to start: a rebound key that does
//!   nothing is indistinguishable from a typo until the moment it bites.
//! - `hop-hotkeyd` **logs and runs as a no-op** instead. This daemon is an
//!   *optional enhancement* (spec §2/§3: "not required on any platform") —
//!   refusing to start would turn "no hotkey configured" into a red unit in
//!   every systemd status listing, and crashing on a typo'd section would
//!   take down an agent whose absence is already a fully-supported state of
//!   the world. The failure mode criterion 2 rules out is the crash; the
//!   logged line is what keeps the no-op from being silent.
//!
//! Both halves are `main`'s call, not this module's: [`load`] returns
//! `Ok(None)` for absent-or-empty and a typed error for malformed, and
//! `main.rs` maps both onto the same exit-0-with-a-log-line outcome.
//!
//! # Path resolution and the hazard-aware read
//!
//! Same derivation as every other reader of this file —
//! `$XDG_CONFIG_HOME/hop/config.toml`, falling back to `$HOME/.config` —
//! duplicated rather than shared for the reason `hop-gtk::keymap`'s own doc
//! comment records under D1 of issue #182's plan: each binary parses the
//! sections it cares about, and the path derivation is inseparable from
//! that per-binary schema. What *is* shared is
//! [`hop_protocol::config_file::read`] — the `O_NONBLOCK` open, the
//! descriptor classification, the bounded read — because that is the part
//! hazardous to get wrong twice, and this crate is now the third caller
//! that function exists for.

use std::env;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::binding::Binding;

/// The environment variable naming the config directory root — named once,
/// the same single-spelling rule `hopd::config` and `hop-gtk::keymap` each
/// apply to their own private copy of these constants.
const XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";

/// The fallback base for the config directory when `XDG_CONFIG_HOME` is
/// unset, per the XDG Base Directory spec: `$HOME/.config`.
const HOME: &str = "HOME";

/// The directory name under the config base every binary that reads this
/// file agrees on.
const CONFIG_DIR_NAME: &str = "hop";

/// The file name inside that directory.
const CONFIG_FILE_NAME: &str = "config.toml";

/// A single hotkey entry's generous line budget, in bytes — the same shape
/// of arithmetic `hop-gtk::keymap`'s `KEYMAP_KEY_LINE_BYTES` runs: the key
/// name (`toggle`, 6 bytes), `" = "`, a modifier-qualified spelling
/// (`"ctrl+shift+Page_Up"` fits in 32), room for a trailing comment as long
/// again, and the newline. Rounded well past the sum.
const HOTKEY_KEY_LINE_BYTES: u64 = 128;

/// Headroom, in key-lines, for the whole file this module reads but does
/// not own — `hopd`'s two scalars and `hop-gtk`'s ~10-entry `[keymap]`
/// table sit in the same bytes this read must pass before finding
/// `[hotkey]`. Priced at eight times the documented-key count (2 + 10 + 1),
/// the same multiplier `hopd::config::MAX_CONFIG_KEYS` uses, so an invited,
/// fully-written config never trips the cap.
const MAX_HOTKEY_KEYS: u64 = 96;

/// Budget, in bytes, for the prose a hand-written config in this repo's
/// commenting style carries — see `hopd::config::CONFIG_COMMENT_BUDGET_BYTES`
/// (same value, same reasoning).
const HOTKEY_COMMENT_BUDGET_BYTES: u64 = 8 * 1024;

/// The byte ceiling passed to [`hop_protocol::config_file::read`]:
/// `128 * 96 + 8192` = 20 KiB, nowhere near enough to trouble memory even
/// fully buffered, nowhere near small enough to reject an ordinarily
/// documented config.
const MAX_HOTKEY_BYTES: u64 = HOTKEY_KEY_LINE_BYTES * MAX_HOTKEY_KEYS + HOTKEY_COMMENT_BUDGET_BYTES;

/// One `[hotkey]` entry as the grab loop consumes it: the parsed
/// [`Binding`] plus the spelling it was written with, kept so every log
/// line names what the user actually wrote (`ctrl+alt+Space`) rather than a
/// canonicalized reconstruction of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleEntry {
    /// The spelling from `config.toml`, verbatim.
    pub spelling: String,
    /// The parsed binding that spelling resolved to.
    pub binding: Binding,
}

/// The parsed `[hotkey]` bindings: what the grab loop should hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkeys {
    /// Bindings that trigger the universal toggle, in config order.
    pub toggle: Vec<ToggleEntry>,
}

/// Every way reading the `[hotkey]` section can fail. None of these is
/// "the file was absent" — that is `Ok(None)` from [`load`], the documented
/// default, not an error. Every variant names the path so the log line a
/// degraded startup prints says which file was wrong.
#[derive(Debug, Error)]
pub enum HotkeyConfigError {
    /// The file could not be safely read at all (a FIFO, a directory, over
    /// the byte cap) — [`hop_protocol::config_file::ConfigFileError`]'s
    /// refusals, carried verbatim.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The config path that was refused.
        path: PathBuf,
        /// The underlying refusal, naming what kind of thing the path was.
        #[source]
        source: hop_protocol::config_file::ConfigFileError,
    },
    /// The file exists but is not valid TOML.
    #[error("{path}: config is not valid TOML: {message}")]
    Parse {
        /// The config path that failed to parse.
        path: PathBuf,
        /// The TOML parser's own message.
        message: String,
    },
    /// The `[hotkey]` section exists but is not shaped as this module
    /// documents — not a table, `toggle` wrongly typed, a spelling
    /// [`crate::binding::Binding::parse`] refuses, or an unknown key inside
    /// the section (refused rather than ignored for the same
    /// typo-protection reason `keymap` refuses unknown actions: a silently
    /// ignored misspelling of `toggle` would look exactly like a working
    /// config while grabbing nothing).
    #[error("{path}: bad [hotkey] section: {reason}")]
    Malformed {
        /// The config path carrying the bad section.
        path: PathBuf,
        /// What specifically was wrong with it.
        reason: String,
    },
}

/// Resolves `$XDG_CONFIG_HOME/hop/config.toml` (falling back to
/// `$HOME/.config/hop/config.toml`) — the identical derivation every other
/// reader of this file performs privately; see this module's doc comment.
pub fn config_path() -> Option<PathBuf> {
    let base = match env::var_os(XDG_CONFIG_HOME) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env::var_os(HOME)?).join(".config"),
    };
    Some(base.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
}

/// Loads the `[hotkey]` bindings from `path`.
///
/// Returns `Ok(None)` when there is nothing to do — the file is absent, or
/// parses but carries no `[hotkey]` section, or carries an empty one — and
/// the typed [`HotkeyConfigError`] when something is actually wrong. See
/// this module's doc comment for why the error is a logged no-op at the
/// `main` level rather than a refusal to start.
pub fn load(path: &Path) -> Result<Option<Hotkeys>, HotkeyConfigError> {
    let bytes = hop_protocol::config_file::read(path, MAX_HOTKEY_BYTES).map_err(|source| {
        HotkeyConfigError::Read {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes);
    let value: toml::Value = toml::from_str(&text).map_err(|err| HotkeyConfigError::Parse {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;

    let Some(hotkey) = value.get("hotkey") else {
        return Ok(None);
    };
    let Some(table) = hotkey.as_table() else {
        return Err(malformed(
            path,
            "[hotkey] must be a table (`[hotkey]` on its own line)",
        ));
    };

    let mut toggle = Vec::new();
    for (key, entry) in table {
        if key != "toggle" {
            return Err(malformed(
                path,
                &format!("unknown key `{key}` (only `toggle` is defined)"),
            ));
        }
        let spellings: Vec<&str> = match entry {
            toml::Value::String(spelling) => vec![spelling],
            toml::Value::Array(items) => {
                let mut spellings = Vec::with_capacity(items.len());
                for item in items {
                    let Some(spelling) = item.as_str() else {
                        return Err(malformed(path, "`toggle` array entries must be strings"));
                    };
                    spellings.push(spelling);
                }
                spellings
            }
            other => {
                return Err(malformed(
                    path,
                    &format!(
                        "`toggle` must be a string or array of strings, got {}",
                        type_name(other)
                    ),
                ));
            }
        };
        if spellings.is_empty() {
            return Err(malformed(path, "`toggle` names no keys"));
        }
        for spelling in spellings {
            let binding = Binding::parse(spelling)
                .map_err(|err| malformed(path, &format!("`{spelling}`: {err}")))?;
            toggle.push(ToggleEntry {
                spelling: spelling.to_string(),
                binding,
            });
        }
    }

    if toggle.is_empty() {
        // `[hotkey]` present but carrying nothing bindable — the same
        // "nothing to do" answer an absent section gets.
        return Ok(None);
    }
    Ok(Some(Hotkeys { toggle }))
}

fn malformed(path: &Path, reason: &str) -> HotkeyConfigError {
    HotkeyConfigError::Malformed {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

/// Names a [`toml::Value`]'s type the way the error message should say it —
/// `as_table`/`as_str` failures otherwise produce messages with no noun in
/// them.
fn type_name(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "an integer",
        toml::Value::Float(_) => "a float",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "a datetime",
        toml::Value::Table(_) => "a table",
        toml::Value::Array(_) => "an array",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_config(dir: &Path, contents: &str) -> PathBuf {
        let config_dir = dir.join("hop");
        fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join(CONFIG_FILE_NAME);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn an_absent_file_is_ok_none_not_an_error() {
        let dir = tempdir().unwrap();
        assert!(matches!(
            load(&dir.path().join("hop").join(CONFIG_FILE_NAME)),
            Ok(None)
        ));
    }

    #[test]
    fn a_file_without_a_hotkey_section_is_ok_none() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            "max_results = 20\n\n[keymap]\nnavigate_down = \"j\"\n",
        );
        assert!(matches!(load(&path), Ok(None)));
    }

    #[test]
    fn a_single_toggle_binding_parses() {
        let dir = tempdir().unwrap();
        let path = write_config(dir.path(), "[hotkey]\ntoggle = \"ctrl+alt+space\"\n");
        let hotkeys = load(&path).unwrap().unwrap();
        assert_eq!(hotkeys.toggle.len(), 1);
        assert_eq!(hotkeys.toggle[0].binding.keysym, 0x0020);
        assert_eq!(hotkeys.toggle[0].binding.modifiers, 0x0c);
        assert_eq!(hotkeys.toggle[0].spelling, "ctrl+alt+space");
    }

    #[test]
    fn an_array_of_toggle_bindings_preserves_order() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            "[hotkey]\ntoggle = [\"ctrl+alt+space\", \"super+p\"]\n",
        );
        let hotkeys = load(&path).unwrap().unwrap();
        assert_eq!(hotkeys.toggle.len(), 2);
        assert_eq!(hotkeys.toggle[1].binding.keysym, b'p' as u32);
    }

    #[test]
    fn invalid_toml_is_a_parse_error_naming_the_path() {
        let dir = tempdir().unwrap();
        let path = write_config(dir.path(), "[hotkey\ntoggle =");
        match load(&path) {
            Err(HotkeyConfigError::Parse { path: p, .. }) => assert_eq!(p, path),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_section_is_a_typed_error_not_a_crash() {
        let dir = tempdir().unwrap();
        // A typo'd spelling inside an otherwise-valid file.
        let path = write_config(dir.path(), "[hotkey]\ntoggle = \"ctrl+notakey\"\n");
        match load(&path) {
            Err(HotkeyConfigError::Malformed { reason, .. }) => {
                assert!(reason.contains("notakey"), "{reason}");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn a_wrongly_typed_section_and_entry_are_malformed() {
        let dir = tempdir().unwrap();
        let scalar = write_config(dir.path(), "hotkey = \"ctrl+alt+space\"\n");
        assert!(matches!(
            load(&scalar),
            Err(HotkeyConfigError::Malformed { .. })
        ));

        let wrong_type = write_config(dir.path(), "[hotkey]\ntoggle = 7\n");
        assert!(matches!(
            load(&wrong_type),
            Err(HotkeyConfigError::Malformed { .. })
        ));
    }

    #[test]
    fn an_unknown_key_inside_the_section_is_refused_not_ignored() {
        let dir = tempdir().unwrap();
        let path = write_config(dir.path(), "[hotkey]\ntoggl = \"ctrl+alt+space\"\n");
        match load(&path) {
            Err(HotkeyConfigError::Malformed { reason, .. }) => {
                assert!(reason.contains("toggl"), "{reason}");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_section_is_ok_none() {
        let dir = tempdir().unwrap();
        let empty_section = write_config(dir.path(), "[hotkey]\n");
        assert!(matches!(load(&empty_section), Ok(None)));
    }
}
