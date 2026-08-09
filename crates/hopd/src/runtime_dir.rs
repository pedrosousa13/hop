//! Locates and creates hopd's runtime directory: `$XDG_RUNTIME_DIR/hop`.
//!
//! This is the directory [`crate::server::serve_with`] binds `hopd.sock` inside,
//! and its mode is the access control the socket itself relies on during the
//! brief window between `bind` and the explicit `chmod` to 0600 — see that
//! module's docs. Getting this directory's mode right at creation, with no
//! window at a wider mode, is therefore load-bearing for the whole boundary,
//! not a cosmetic detail.

use std::env;
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

/// The environment variable this module reads. Named once so the variable's
/// spelling appears in exactly one place — including in every error message
/// that names it.
const XDG_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR";

/// Reads `XDG_RUNTIME_DIR` and creates `<that>/hop` at mode 0700 if it does
/// not already exist, returning that directory's path.
///
/// # Why there is no fallback for a missing or empty variable
///
/// The v1 spec's socket layout assumes a systemd user session, which sets
/// `XDG_RUNTIME_DIR` to a per-user, 0700, tmpfs-backed directory before hopd
/// ever runs. But the variable is still environment the user controls, not a
/// guarantee this process can make for itself — the threat model
/// (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "The
/// boundary") states the same reasoning `learning.rs` already applies to
/// `XDG_STATE_HOME`: a path derived from user-controlled environment is not
/// one the process can reason about unaided. Inventing a fallback (`/tmp`, a
/// path under `$HOME`) would be a security decision — *which* directory
/// hopd's socket, and everything reachable through it, lives under — made
/// silently in this slice rather than deliberately in a later one. Refusing
/// to start is the only choice this function makes; the caller decides what
/// "refusing to start" looks like on stderr and as an exit code.
///
/// # Why the directory is born at 0700
///
/// `DirBuilder::mode` passes the mode straight to `mkdir(2)`, so the
/// directory exists at 0700 from the instant it exists at all — there is no
/// create-then-`chmod` window for a wider mode to be observed or raced.
/// `mkdir`'s mode is masked by the umask (`mode & ~umask`), which can only
/// *clear* bits, and 0700 has no group or other bits to begin with, so no
/// umask can widen this call's result.
///
/// A directory that already exists is left exactly as found, whatever its
/// mode, rather than being narrowed to match. `DirBuilder::create` reports
/// that case as [`io::ErrorKind::AlreadyExists`], which this function treats
/// as success without touching the directory's mode at all. This is the same
/// asymmetry `hop-core`'s `learning.rs::persist_atomically` documents in
/// place for `XDG_STATE_HOME`, cited by the threat model as the precedent to
/// follow here: this function can reason about a directory it created
/// itself, and cannot reason safely about one the environment already
/// supplied — chmodding a pre-existing directory could narrow (or, with a
/// symlink involved, redirect) something outside hopd's own state.
///
/// # Errors
///
/// `Err` if `XDG_RUNTIME_DIR` is unset or empty, or if creating the
/// directory fails for any other reason — most notably, if
/// `$XDG_RUNTIME_DIR` itself does not exist, since this call is not
/// recursive: it is responsible for the one directory named above, not for
/// inventing everything above it. Every error's `Display` names what went
/// wrong; turning that into a stderr line and a non-zero exit is
/// [`crate::run`]'s job, not this function's.
pub fn resolve() -> io::Result<PathBuf> {
    let value = env::var(XDG_RUNTIME_DIR).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{XDG_RUNTIME_DIR} is not set"),
        )
    })?;
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{XDG_RUNTIME_DIR} is set but empty"),
        ));
    }

    let hop_dir = PathBuf::from(value).join("hop");
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match builder.create(&hop_dir) {
        Ok(()) => Ok(hop_dir),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(hop_dir),
        Err(err) => Err(err),
    }
}
