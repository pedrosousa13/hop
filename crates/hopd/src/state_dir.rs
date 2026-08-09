//! Locates and creates hopd's state directory: `$XDG_STATE_HOME/hop`,
//! falling back to `$HOME/.local/state/hop`.
//!
//! This is where hopd's persistent state lives — today, the learning store
//! ([`STORE_FILE_NAME`], the file the pipeline's `Learning` persists to).
//! Unlike [`crate::runtime_dir`], whose socket location is a security
//! decision and so deliberately refuses a fallback, the XDG Base Directory
//! spec *defines* `$XDG_STATE_HOME`'s fallback as `$HOME/.local/state` — the
//! same default this repo's own `hop-core::learning` prose already names as
//! `~/.local/state/hop`. The state dir is not the socket boundary, so this
//! module honors the standard fallback instead of refusing it.

use std::env;
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

/// The environment variable that names the state directory root. Named once
/// so the variable's spelling appears in exactly one place — including in
/// every error message that names it.
const XDG_STATE_HOME: &str = "XDG_STATE_HOME";

/// The fallback base for the state directory when `XDG_STATE_HOME` is unset,
/// per the XDG Base Directory spec: `$HOME/.local/state`. Also named once.
const HOME: &str = "HOME";

/// The directory name under the state base that this daemon owns.
const STATE_DIR_NAME: &str = "hop";

/// The name of the file this daemon's persistent state is stored in, inside
/// the state directory this module resolves. Named once here — the module
/// that loads and saves the store joins this name onto [`resolve`]'s result
/// (see Design decision 7's `store_path`), rather than spelling it again.
pub const STORE_FILE_NAME: &str = "learning.json";

/// Reads `XDG_STATE_HOME` (falling back to `$HOME/.local/state` when it is
/// unset) and creates `<base>/hop` at mode 0700 if it does not already
/// exist, returning that directory's path.
///
/// # Why this reads the env in a thin wrapper
///
/// The path computation that follows is a pure function of the two variables
/// — see [`resolve_from_env`] — precisely so the unit tests below can pin the
/// XDG fallback and error behaviors without touching the process
/// environment. Reading the environment *is* this function's whole job;
/// everything load-bearing about the path lives one level down and is
/// testable with explicit values.
///
/// # Why the directory is born at 0700
///
/// The learning store this directory holds records which program launches
/// came through the socket — a private, per-user log. `DirBuilder::mode`
/// passes the mode straight to `mkdir(2)`, so the directory exists at 0700
/// from the instant it exists at all — there is no create-then-`chmod`
/// window for a wider mode to be observed or raced. `mkdir`'s mode is masked
/// by the umask (`mode & ~umask`), which can only *clear* bits, and 0700 has
/// no group or other bits to begin with, so no umask can widen this call's
/// result.
///
/// A directory that already exists is left exactly as found, whatever its
/// mode, rather than being narrowed to match — the same asymmetry
/// [`crate::runtime_dir`] documents for its own directory: this module can
/// reason about a directory it created itself, and cannot reason safely
/// about one the environment already supplied.
///
/// # Why this call is not recursive
///
/// This function is responsible for the one `hop` directory it names, not
/// for inventing everything above it. `$XDG_STATE_HOME` (or `.local/state`
/// under a user's home) is the environment's to provide — the same posture
/// [`crate::runtime_dir`] takes — so a missing parent is a plain error, not
/// something this module silently creates.
///
/// # Errors
///
/// `Err` if neither `XDG_STATE_HOME` nor `HOME` is set (refusing to start,
/// the same posture as [`crate::runtime_dir`]), or if creating the directory
/// fails for any other reason. Every error's `Display` names what went
/// wrong.
pub fn resolve() -> io::Result<PathBuf> {
    let xdg = env::var(XDG_STATE_HOME).ok().filter(|v| !v.is_empty());
    let home = env::var(HOME).ok().filter(|v| !v.is_empty());
    resolve_from_env(xdg, home)
}

/// The pure core of [`resolve`]: given the *values* of `XDG_STATE_HOME` and
/// `HOME`, computes the state directory path and creates it at 0700.
///
/// This is the function the unit tests exercise, because it takes the
/// environment as explicit parameters rather than reading it — the workspace
/// denies `unsafe_code` (and Rust 2024 makes `env::set_var` `unsafe`), so
/// tests cannot safely mutate process env.
fn resolve_from_env(xdg_state_home: Option<String>, home: Option<String>) -> io::Result<PathBuf> {
    // Treat an empty-but-set variable as unset here too, so this pure core
    // is robust regardless of whether its caller already filtered.
    let xdg_state_home = xdg_state_home.filter(|v| !v.is_empty());
    let home = home.filter(|v| !v.is_empty());
    let base_dir = match xdg_state_home {
        Some(dir) => PathBuf::from(dir),
        None => match home {
            Some(home_dir) => PathBuf::from(home_dir).join(".local").join("state"),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("neither {XDG_STATE_HOME} nor {HOME} is set"),
                ));
            }
        },
    };

    let hop_dir = base_dir.join(STATE_DIR_NAME);
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match builder.create(&hop_dir) {
        Ok(()) => Ok(hop_dir),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(hop_dir),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Asserts the group and other permission bits are all clear — robust to
    /// any umask, since a umask can only clear bits and there are none to
    /// clear below 0700.
    fn assert_dir_is_0700(path: &PathBuf) {
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "expected mode 0700, got {mode:#o}");
    }

    #[test]
    fn xdg_state_home_is_honored() {
        // XDG_STATE_HOME set: the resolved dir must be `<it>/hop`, created
        // there at 0700.
        let base = tempfile::tempdir().unwrap();
        let base_str = base.path().to_string_lossy().into_owned();
        let hop = resolve_from_env(Some(base_str), None).unwrap();
        assert_eq!(hop, base.path().join(STATE_DIR_NAME));
        assert!(hop.is_dir());
        assert_dir_is_0700(&hop);
    }

    #[test]
    fn falls_back_to_home_local_state() {
        // XDG_STATE_HOME unset, HOME set: the XDG spec's fallback,
        // `$HOME/.local/state/hop`.
        let home = tempfile::tempdir().unwrap();
        // The module is not recursive: `$HOME/.local/state` is the environment's
        // to provide. Create it here to simulate a real user home, then resolve.
        let fallback_base = home.path().join(".local").join("state");
        std::fs::create_dir_all(&fallback_base).unwrap();
        let home_str = home.path().to_string_lossy().into_owned();
        let hop = resolve_from_env(None, Some(home_str)).unwrap();
        let expected = home
            .path()
            .join(".local")
            .join("state")
            .join(STATE_DIR_NAME);
        assert_eq!(hop, expected);
        assert!(hop.is_dir());
        assert_dir_is_0700(&hop);
    }

    #[test]
    fn creates_dir_at_0700() {
        // The directory did not exist, so this function creates it — and it
        // must be born at 0700 with the group/other bits clear.
        let base = tempfile::tempdir().unwrap();
        let base_str = base.path().to_string_lossy().into_owned();
        let hop = resolve_from_env(Some(base_str), None).unwrap();
        assert_dir_is_0700(&hop);
    }

    #[test]
    fn preexisting_dir_is_left_as_found() {
        // A directory that already exists must be left exactly as found, not
        // narrowed to 0700 — the same asymmetry runtime_dir documents.
        let base = tempfile::tempdir().unwrap();
        let hop = base.path().join(STATE_DIR_NAME);
        std::fs::create_dir(&hop).unwrap();
        std::fs::set_permissions(&hop, std::fs::Permissions::from_mode(0o750)).unwrap();

        let base_str = base.path().to_string_lossy().into_owned();
        let resolved = resolve_from_env(Some(base_str), None).unwrap();
        assert_eq!(resolved, hop);
        let mode = std::fs::metadata(&hop).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o750, "pre-existing dir was altered");
    }

    #[test]
    fn missing_both_envs_is_an_error() {
        // Neither XDG_STATE_HOME nor HOME set: there is no base to derive a
        // state dir from, and inventing one would be a silent fallback. Must
        // be an explicit error, the same posture runtime_dir takes.
        let err = resolve_from_env(None, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let msg = err.to_string();
        assert!(
            msg.contains(XDG_STATE_HOME),
            "error must name the var: {msg}"
        );
        assert!(msg.contains(HOME), "error must name the var: {msg}");
    }

    #[test]
    fn empty_xdg_state_home_falls_back_to_home() {
        // An empty-but-set XDG_STATE_HOME is treated as unset, per the XDG
        // spec and this module's docs — it must not become an empty path.
        let home = tempfile::tempdir().unwrap();
        // See `falls_back_to_home_local_state`: the module is not recursive.
        let fallback_base = home.path().join(".local").join("state");
        std::fs::create_dir_all(&fallback_base).unwrap();
        let home_str = home.path().to_string_lossy().into_owned();
        let hop = resolve_from_env(Some(String::new()), Some(home_str)).unwrap();
        let expected = home
            .path()
            .join(".local")
            .join("state")
            .join(STATE_DIR_NAME);
        assert_eq!(hop, expected);
    }
}
