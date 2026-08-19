//! Where `hopd`'s socket lives, and the one rule an override on that
//! location must obey.
//!
//! Three binaries need this path: `hopd` binds it, `hop-cli` and `hop-gtk`
//! connect to it. Before issue #180 each client derived it independently —
//! `hop-cli/src/lib.rs`'s `socket_path()` and `apps/hop-gtk/src/app.rs`'s
//! `socket_path()`, two copies of the same four lines. The `hop-gtk` copy
//! carried a doc comment saying exactly when that would stop being
//! acceptable: *"Duplicated rather than shared because `hop-cli` does not
//! expose it as a library function today; were a third caller to need it,
//! the pair would be worth promoting into `hop-protocol` instead of copied a
//! second time."* Issue #180 is that third caller — it gives `hopd` itself a
//! `--socket` override, and gives one to each client besides — so this
//! module is that promotion, in the one crate all three binaries already
//! depend on. The two existing copies are untouched; retiring them belongs
//! to the change that wires this module into each binary, not to the change
//! that creates it.
//!
//! # Why an override needs a rule at all
//!
//! With no override, the path is `derived` from `$XDG_RUNTIME_DIR`, a
//! per-user, 0700, `tmpfs`-backed directory a systemd session sets up before
//! any of these binaries run — the same trust boundary
//! `hopd::runtime_dir`'s doc comment describes for the directory it creates.
//! An override lets a caller name a *different* file, most usefully a
//! second, non-conflicting socket for a `hopd` run alongside the session's
//! own (a `--socket $XDG_RUNTIME_DIR/hop-dev/hopd.sock` for local
//! development is the case D2 below is written around). What an override
//! must not do is point at a file the runtime directory's own access mode
//! does not protect — `/tmp/hopd.sock`, or a path reached by a symlink that
//! leads out of the runtime directory — because the socket's only protection
//! against another user on the same machine connecting to it is exactly the
//! 0700 mode on the directory the daemon's own runtime-dir code creates. An
//! override that could land the socket anywhere would let one flag undo that
//! protection, silently, for whoever runs `hopd` with it. So this module
//! resolves an override the same way it would resolve any other path, then
//! refuses it unless the *resolved* location is still inside
//! `$XDG_RUNTIME_DIR` — checked structurally, by canonicalizing both sides
//! and comparing paths, rather than by inspecting the override's text, which
//! is what actually catches a `..` escape or a symlink out.
//!
//! Every failure mode here refuses rather than falls back to the derived
//! path. An override that silently becomes the default the moment it can't
//! be honored is worse than no override at all — the caller asked for one
//! location, quietly got a different one, and has no way to notice short of
//! checking the socket it actually connected to.
//!
//! # Why `resolve_in` takes the runtime directory as a parameter
//!
//! [`socket_path`] is the one call a binary makes; it reads
//! `$XDG_RUNTIME_DIR` itself and is not unit-testable without touching real
//! process environment. [`resolve_in`] is the pure core underneath it — a
//! function of two paths, nothing else — so every constraint this module
//! enforces (outside the root, a `..` escape, a symlink that leads out, a
//! dangling symlink, the root itself) has a test that builds two `tempfile`
//! directories and calls it directly. The alternative, driving those same
//! cases through `socket_path` by setting `XDG_RUNTIME_DIR` for the test
//! process, needs `std::env::set_var`, which edition 2024 makes `unsafe` and
//! this workspace's `unsafe_code = "deny"` lint refuses — and would be racy
//! across parallel test threads regardless, since the environment is
//! process-global.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// The environment variable the socket path derives from. Named once so its
/// spelling appears in exactly one place — including in every error message
/// that names it — the same reasoning `hopd::runtime_dir` gives for its own,
/// separately declared copy of this constant. The two are not unified: that
/// module reaches into the environment to *create* a directory before this
/// crate's `socket` module exists to ask it to, and giving it a public
/// dependency on `hop-protocol` for one string is a heavier coupling than
/// the duplication it would remove.
pub const XDG_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR";

/// The subdirectory of `$XDG_RUNTIME_DIR` the derived socket lives in.
pub const RUNTIME_SUBDIR: &str = "hop";

/// The socket's file name.
pub const SOCKET_FILE_NAME: &str = "hopd.sock";

/// Every way a socket path can fail to be produced, whether derived or
/// overridden.
///
/// Each variant carries the path(s) involved. That is a deliberate
/// difference from [`crate::content::IconOpenError`], this crate's other
/// filesystem-facing error type, which carries none: an icon path arrives
/// off the wire from a provider, a party this process does not trust, so its
/// errors name only the wire field the value came from and leave formatting
/// the value itself to a caller with a seam for doing that safely. Nothing
/// here is peer-controlled in that sense — `$XDG_RUNTIME_DIR` is this
/// process's own environment and an override is a flag the same user typed
/// on the same command line — so a value in one of these errors is one the
/// person reading the error already gave the process, on the same machine,
/// and naming it back is the whole point of a diagnostic that says why a
/// flag was refused.
#[derive(Debug, Error)]
pub enum SocketPathError {
    /// `$XDG_RUNTIME_DIR` is not set.
    #[error("{XDG_RUNTIME_DIR} is not set")]
    RuntimeDirUnset,

    /// `$XDG_RUNTIME_DIR` is set to the empty string. Kept distinct from
    /// [`SocketPathError::RuntimeDirUnset`] because the two point at
    /// different fixes — one names a variable that needs setting, the other
    /// names one that is set to nothing, most plausibly by a shell that
    /// exported it from an unset value (`export XDG_RUNTIME_DIR` with
    /// nothing after the `=`) rather than one that never exported it — and
    /// collapsing them would cost the reader that distinction for free.
    #[error("{XDG_RUNTIME_DIR} is set but empty")]
    RuntimeDirEmpty,

    /// `$XDG_RUNTIME_DIR` itself does not resolve — most plausibly because
    /// the directory it names does not exist. [`resolve_in`]'s first step is
    /// canonicalizing the runtime directory before it can be used as the
    /// root an override is checked against, so a runtime directory that
    /// cannot be resolved fails here rather than partway through resolving
    /// the override.
    #[error("XDG_RUNTIME_DIR ({}) could not be resolved: {source}", path.display())]
    RuntimeDirUnresolvable {
        /// The `$XDG_RUNTIME_DIR` value that failed to canonicalize.
        path: PathBuf,
        /// What the canonicalization failed with.
        source: io::Error,
    },

    /// The override does not resolve, for a reason other than one of the
    /// more specific variants below — most plausibly a permission error on
    /// one of its ancestor directories, or too many levels of symbolic
    /// links.
    #[error("socket path {} could not be resolved: {source}", path.display())]
    Unresolvable {
        /// The path (or, during resolution, the ancestor of the original
        /// path) that failed to resolve.
        path: PathBuf,
        /// What the resolution failed with.
        source: io::Error,
    },

    /// The override names no file — it ends in a path separator, in `.`, or
    /// in `..`, none of which is a name a socket could be bound at.
    #[error("socket path {} names no file", path.display())]
    NoFileName {
        /// The path that named no file.
        path: PathBuf,
    },

    /// An entry exists at the path but is a symlink whose target does not
    /// exist, so what it would resolve to cannot be checked against the
    /// runtime directory.
    #[error("socket path {} is a symlink to nothing", path.display())]
    DanglingSymlink {
        /// The dangling symlink.
        path: PathBuf,
    },

    /// The override resolved, but not to somewhere inside `$XDG_RUNTIME_DIR`
    /// — either it escaped the root (directly, through `..`, or through a
    /// symlink), or it resolved to the root directory itself, which is a
    /// directory rather than a socket.
    #[error(
        "socket path {} resolves outside {}: a socket path must resolve inside $XDG_RUNTIME_DIR",
        path.display(),
        runtime_dir.display()
    )]
    Outside {
        /// Where the override resolved to.
        path: PathBuf,
        /// The canonicalized runtime directory it needed to resolve inside.
        runtime_dir: PathBuf,
    },
}

/// Reads and validates `$XDG_RUNTIME_DIR`, treating "unset" and "set but
/// empty" as the distinct failures they are (see
/// [`SocketPathError::RuntimeDirUnset`] and
/// [`SocketPathError::RuntimeDirEmpty`]).
///
/// Reads with [`env::var_os`] rather than [`env::var`], so a runtime
/// directory whose bytes are not valid Unicode still comes back as a usable
/// [`PathBuf`] instead of being rejected here on a rule paths themselves do
/// not need to follow — [`env::var`] would force exactly that rejection by
/// requiring valid UTF-8 before this function ever saw the value.
///
/// The returned path is not canonicalized: it may not exist yet, and
/// whether it needs to is a decision this function leaves to its caller —
/// [`derived`] never checks, [`resolve_in`] always does.
pub fn runtime_dir() -> Result<PathBuf, SocketPathError> {
    let value = env::var_os(XDG_RUNTIME_DIR).ok_or(SocketPathError::RuntimeDirUnset)?;
    if value.is_empty() {
        return Err(SocketPathError::RuntimeDirEmpty);
    }
    Ok(PathBuf::from(value))
}

/// `<runtime_dir>/hop/hopd.sock` — the path used when no override is given.
pub fn derived(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(RUNTIME_SUBDIR).join(SOCKET_FILE_NAME)
}

/// Resolves `raw` as far as it exists, following every symlink on the way,
/// and refuses it unless the result is inside `runtime_dir` and is not
/// `runtime_dir` itself.
///
/// # The algorithm
///
/// 1. `runtime_dir` is canonicalized first, becoming the root every check
///    below compares against. If it does not resolve — most plausibly
///    because the directory does not exist — this refuses immediately,
///    before touching `raw` at all: there is no root to check `raw` against
///    yet.
/// 2. `raw` is resolved as far as it exists:
///    - [`Path::canonicalize`] on `raw` directly, first. If it succeeds,
///      every symlink `raw` names or passes through has already been
///      followed, so a symlink that leads outside the root resolves to
///      wherever it actually points — which step 3 then catches, the same
///      as any other escape.
///    - A [`std::io::ErrorKind::NotFound`] from that call is not itself a
///      refusal: it is the ordinary shape of an override that names a
///      socket file `hopd` has not bound yet, since binding is exactly what
///      creates it. Before treating it that way, though,
///      [`std::fs::symlink_metadata`] is checked: if *that* succeeds where
///      `canonicalize` reported `NotFound`, an entry exists at `raw` and is
///      a symlink, and `canonicalize` failed because the symlink's *target*
///      is missing. A dangling symlink is refused rather than treated as a
///      not-yet-existing file, because there is no target left to check
///      against the root — unlike an ordinary missing path, resolving it
///      further is not a matter of walking up to a parent that does exist.
///    - Otherwise, `raw`'s file name is taken and its parent resolved by
///      the same three rules, recursively, then the file name is rejoined
///      onto whatever the parent resolved to. A path with no file name —
///      one ending in a path separator, in `.`, or in `..` — is refused
///      rather than recursed on: none of those names anything to rejoin.
///      [`Path::file_name`] alone recognizes `.` and `..`, since Rust
///      normalizes both a trailing separator and a trailing `.` out of a
///      path's components — `Path::new("a/b/")` and `Path::new("a/b")` are
///      the same path once parsed — so this function additionally checks
///      `raw`'s literal last byte before parsing, catching the trailing
///      separator `file_name` cannot see. A path's parent can be empty (a
///      bare relative name like `hopd.sock` has one), which is resolved as
///      `.`, the current directory, rather than as nothing.
///    - Any other error from `canonicalize` is reported as
///      [`SocketPathError::Unresolvable`], naming whichever path — `raw`
///      itself, or the ancestor recursion had reached — produced it.
///
///    Recursion terminates because each call recurses on a strictly shorter
///    path (one fewer component), and a path has finitely many components.
/// 3. The fully resolved path is accepted only if it both starts with the
///    canonicalized root and is not equal to the root: a result equal to
///    the root is the root directory itself, which is a directory rather
///    than a place a socket could be bound. Anything that fails either half
///    is reported as [`SocketPathError::Outside`], which is also what a `..`
///    escape or a symlink leading outside the root produces, since both are
///    caught here rather than by inspecting `raw`'s text — a resolved path
///    either is under the root or it is not, regardless of how it got
///    there.
pub fn resolve_in(runtime_dir: &Path, raw: &Path) -> Result<PathBuf, SocketPathError> {
    let root =
        runtime_dir
            .canonicalize()
            .map_err(|source| SocketPathError::RuntimeDirUnresolvable {
                path: runtime_dir.to_path_buf(),
                source,
            })?;

    let resolved = resolve_existing(raw)?;

    if resolved.starts_with(&root) && resolved != root {
        Ok(resolved)
    } else {
        Err(SocketPathError::Outside {
            path: resolved,
            runtime_dir: root,
        })
    }
}

/// The recursive core of [`resolve_in`]'s step 2 — see that function's doc
/// comment for the algorithm this implements. Kept separate from
/// [`resolve_in`] so the recursion never re-canonicalizes `runtime_dir`,
/// which step 2 only needs to have happened once, before recursion starts.
fn resolve_existing(raw: &Path) -> Result<PathBuf, SocketPathError> {
    match raw.canonicalize() {
        Ok(resolved) => Ok(resolved),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(raw).is_ok() {
                return Err(SocketPathError::DanglingSymlink {
                    path: raw.to_path_buf(),
                });
            }

            // `Path::file_name` alone does not catch a path ending in a
            // separator: Rust normalizes a trailing separator out of a
            // path's components, so `Path::new("a/b/").file_name()` is
            // `Some("b")`, the same as `Path::new("a/b")`'s — the byte is
            // checked directly here instead. `as_encoded_bytes` guarantees
            // any ASCII byte (`/` among them) is represented as itself, on
            // every platform's encoding, so comparing its last byte to `/`
            // is safe without decoding the rest.
            let ends_in_separator = raw.as_os_str().as_encoded_bytes().last() == Some(&b'/');
            let Some(file_name) = raw.file_name().filter(|_| !ends_in_separator) else {
                return Err(SocketPathError::NoFileName {
                    path: raw.to_path_buf(),
                });
            };

            let parent = match raw.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            let resolved_parent = resolve_existing(parent)?;
            Ok(resolved_parent.join(file_name))
        }
        Err(source) => Err(SocketPathError::Unresolvable {
            path: raw.to_path_buf(),
            source,
        }),
    }
}

/// The one call a binary makes to find `hopd`'s socket: `None` derives it
/// from `$XDG_RUNTIME_DIR`, `Some` resolves and constrains the override
/// against the same root.
///
/// Reading `$XDG_RUNTIME_DIR` happens exactly once here, in either branch,
/// which is what keeps a derived path and a validated override answering
/// the same question about the same environment rather than two snapshots
/// of it taken at different times.
pub fn socket_path(overridden: Option<&Path>) -> Result<PathBuf, SocketPathError> {
    let root = runtime_dir()?;
    match overridden {
        None => Ok(derived(&root)),
        Some(raw) => resolve_in(&root, raw),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_path_directly_inside_the_root_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let raw = root.path().join(SOCKET_FILE_NAME);

        let resolved = resolve_in(root.path(), &raw).unwrap();

        assert_eq!(
            resolved,
            root.path().canonicalize().unwrap().join(SOCKET_FILE_NAME)
        );
    }

    #[test]
    fn a_nested_path_whose_parent_exists_but_whose_socket_does_not_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("hop-dev");
        std::fs::create_dir(&nested).unwrap();
        let raw = nested.join(SOCKET_FILE_NAME);

        let resolved = resolve_in(root.path(), &raw).unwrap();

        assert_eq!(
            resolved,
            root.path()
                .canonicalize()
                .unwrap()
                .join("hop-dev")
                .join(SOCKET_FILE_NAME)
        );
    }

    #[test]
    fn a_path_whose_parent_also_does_not_exist_is_accepted_while_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        let raw = root
            .path()
            .join("hop-dev")
            .join("nested")
            .join(SOCKET_FILE_NAME);

        let resolved = resolve_in(root.path(), &raw).unwrap();

        assert_eq!(
            resolved,
            root.path()
                .canonicalize()
                .unwrap()
                .join("hop-dev")
                .join("nested")
                .join(SOCKET_FILE_NAME)
        );
    }

    #[test]
    fn a_path_outside_the_root_is_refused_as_outside() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let raw = elsewhere.path().join(SOCKET_FILE_NAME);

        let err = resolve_in(root.path(), &raw).unwrap_err();

        assert!(matches!(err, SocketPathError::Outside { .. }), "got: {err}");
    }

    #[test]
    fn a_dot_dot_escape_is_caught_by_canonicalization_not_by_text() {
        let root = tempfile::tempdir().unwrap();
        let hop_subdir = root.path().join("hop");
        std::fs::create_dir(&hop_subdir).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        // Textually this still starts with `root`'s own path — only
        // resolving the `..` components reveals it lands under `elsewhere`
        // instead, which is exactly the distinction this test pins: a
        // `starts_with` check on `raw` itself would have accepted this.
        let raw = hop_subdir
            .join("..")
            .join("..")
            .join(elsewhere.path().file_name().unwrap())
            .join(SOCKET_FILE_NAME);

        let err = resolve_in(root.path(), &raw).unwrap_err();

        assert!(matches!(err, SocketPathError::Outside { .. }), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_root_pointing_outside_it_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join(SOCKET_FILE_NAME);
        std::fs::write(&target, b"").unwrap();
        let link = root.path().join("escape.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = resolve_in(root.path(), &link).unwrap_err();

        assert!(matches!(err, SocketPathError::Outside { .. }), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_root_pointing_at_another_spot_inside_it_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("real.sock");
        std::fs::write(&target, b"").unwrap();
        let link = root.path().join("link.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let resolved = resolve_in(root.path(), &link).unwrap();

        assert_eq!(
            resolved,
            root.path().canonicalize().unwrap().join("real.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_inside_the_root_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join("dangling.sock");
        std::os::unix::fs::symlink(root.path().join("never-created"), &link).unwrap();

        let err = resolve_in(root.path(), &link).unwrap_err();

        assert!(
            matches!(err, SocketPathError::DanglingSymlink { .. }),
            "got: {err}"
        );
    }

    #[test]
    fn the_root_itself_is_refused() {
        let root = tempfile::tempdir().unwrap();

        let err = resolve_in(root.path(), root.path()).unwrap_err();

        assert!(matches!(err, SocketPathError::Outside { .. }), "got: {err}");
    }

    #[test]
    fn a_path_ending_in_dot_dot_or_a_separator_is_refused_as_no_file_name() {
        let root = tempfile::tempdir().unwrap();

        let dot_dot = root.path().join("nonexistent-dir").join("..");
        let err = resolve_in(root.path(), &dot_dot).unwrap_err();
        assert!(
            matches!(err, SocketPathError::NoFileName { .. }),
            "got: {err}"
        );

        let mut trailing_slash = root.path().join("nonexistent-dir").into_os_string();
        trailing_slash.push("/");
        let trailing_slash = PathBuf::from(trailing_slash);
        let err = resolve_in(root.path(), &trailing_slash).unwrap_err();
        assert!(
            matches!(err, SocketPathError::NoFileName { .. }),
            "got: {err}"
        );
    }

    #[test]
    fn derived_composes_root_hop_hopd_sock() {
        let root = Path::new("/run/user/1000");

        assert_eq!(derived(root), Path::new("/run/user/1000/hop/hopd.sock"));
    }
}
