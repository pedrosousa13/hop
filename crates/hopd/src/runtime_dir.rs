//! Locates and creates hopd's runtime directory: `$XDG_RUNTIME_DIR/hop`.
//!
//! This is the directory [`crate::server::serve_with`] binds `hopd.sock` inside,
//! and its mode is the access control the socket itself relies on during the
//! brief window between `bind` and the explicit `chmod` to 0600 — see that
//! module's docs. Getting this directory's mode right at creation, with no
//! window at a wider mode, is therefore load-bearing for the whole boundary,
//! not a cosmetic detail.
//!
//! [`create_at_0700`] is `pub(crate)` rather than private: issue #180 gave
//! `hopd` a `--socket` override whose own parent directory needs the exact
//! same mode-at-birth guarantee this module already built for `hop` itself,
//! so `crate::run`'s override branch calls it directly instead of this
//! module growing a second copy of the same `DirBuilder` call.

use std::env;
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

/// The environment variable this module reads. Named once so the variable's
/// spelling appears in exactly one place — including in every error message
/// that names it.
const XDG_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR";

/// Creates `dir` at mode 0700 if it does not already exist, leaving an
/// existing directory exactly as found.
///
/// Shared by [`resolve`], which creates `<XDG_RUNTIME_DIR>/hop`, and
/// `crate::run`'s `--socket` override branch (issue #180), which creates an
/// overridden socket path's own parent instead — the override's constraint
/// root is `$XDG_RUNTIME_DIR` itself, not `$XDG_RUNTIME_DIR/hop`, so a
/// caller-chosen path can sit anywhere under the root, at a parent
/// `runtime_dir::resolve` never creates and would be wrong to. Both callers
/// need the identical mode-at-birth, leave-as-found behavior [`resolve`]'s
/// own doc comment used to argue for in place before this function existed;
/// factoring it out here is what stops that argument, and the one
/// `DirBuilder::mode(0o700)` call it justifies, from being written twice.
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
/// Neither caller asks this to be recursive, and it is not: it is
/// responsible for the one directory it is given, not for inventing
/// everything above it. [`resolve`] relies on `$XDG_RUNTIME_DIR` already
/// existing; the override branch relies on [`hop_protocol::socket::resolve_in`]
/// having already proven the override's resolved parent sits inside that
/// same, already-existing root.
pub(crate) fn create_at_0700(dir: &std::path::Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match builder.create(dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

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
/// The directory itself is created by [`create_at_0700`] — see that
/// function's doc comment for why it is born at 0700 with no create-then-
/// `chmod` window, and why a pre-existing directory is left exactly as
/// found rather than narrowed to match.
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
    create_at_0700(&hop_dir)?;
    Ok(hop_dir)
}
