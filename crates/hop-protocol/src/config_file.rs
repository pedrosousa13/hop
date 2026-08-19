//! A bounded, hazard-aware read of a config file at a caller-given path:
//! `O_NONBLOCK` on the open, a check that what was actually opened is a
//! regular file, and a read capped at a caller-supplied maximum.
//!
//! Every one of those three protections, and the reasoning behind each, is
//! carried over from `hopd::config::Config::from_path`, which grew all
//! three under issue #160 — read that function's own doc comment history
//! for the incident each one closes. This module is that logic promoted
//! out of `hopd`, because issue #182 gives `hop-gtk` a second reader of the
//! exact same attacker-influencable path
//! (`$XDG_CONFIG_HOME/hop/config.toml`), and copying forty lines of
//! security-relevant code into a second crate is how the two copies drift
//! apart rather than staying in lock step.
//!
//! This is the same rule [`crate::socket`] establishes, and that this
//! crate's own root doc comment now states for the crate as a whole:
//! nothing in this module crosses the wire, but this is the one crate all
//! three binaries (`hopd`, `hop-cli`, `hop-gtk`) already depend on, so a
//! function genuinely needed by more than one of them lives here rather
//! than being copied. Read that module's doc comment for the fuller case;
//! this one is its sibling, promoted for a different hazard.
//!
//! # What this module does *not* do
//!
//! It has no opinion on what the bytes mean. `hopd`'s config has two
//! scalar keys; `hop-gtk`'s keymap (issue #182) is a `[keymap]` table of
//! roughly nine bindings; neither schema, nor a byte-budget constant, nor
//! an error type describing *why a parsed value was refused* belongs here
//! — those stay with each binary, because the byte cap is a policy choice
//! each caller makes for itself (see `hopd::config`'s `MAX_CONFIG_BYTES`
//! for the reasoning behind its own choice) and the schema is obviously
//! not shared at all between a daemon's tuning knobs and a launcher's
//! keymap. This module returns bytes, or a refusal that is about the
//! *file*, never about what is inside it.
//!
//! # Absent file
//!
//! [`read`] returns `Ok(None)` when nothing exists at `path` — not an
//! error. An absent config is the documented default for every caller of
//! this function today (`hopd::config::Config::load`'s own doc comment:
//! "An absent config file is `Ok`, by contract, never an error"), so
//! folding "there was nothing to read" into the same refusal vocabulary as
//! "there was something there and it was not safe to read" would force
//! every caller to re-derive that distinction from an IO error kind after
//! the fact. What `None` *means* — fall back to defaults, in both
//! `hopd`'s and `hop-gtk`'s case — is exactly the kind of schema-level
//! policy this module leaves to its caller, for the same reason the byte
//! cap and the schema are left to it.
//!
//! # Why refusal messages name the path with [`Path::display`] rather than
//! `escape_path`
//!
//! `hopd::config::ConfigError` runs every path it displays through
//! `hop_core::sanitize::escape_path` (issue #159): the path is derived
//! from `XDG_CONFIG_HOME` or a symlink target it follows, both environment
//! an attacker can influence, and an unescaped control character or
//! bidi-override byte in it would otherwise reach a terminal raw. That
//! hazard applies just as much to the path this module's own errors name —
//! it is the identical path, resolved the identical way — but this crate
//! cannot reach for the fix `hopd` uses: `escape_path` lives in
//! `hop-core`, and `hop-core` depends on `hop-protocol`, not the other way
//! around (see `hop-core/Cargo.toml`), so calling it from here would be a
//! cyclic dependency. Moving `escape_path` itself into this crate was
//! considered and rejected too: it would strand every other caller of
//! `hop_core::sanitize::escape_path` on a promotion this task was never
//! asked to make, for a module whose own brief is explicit that the
//! schema and everything schema-adjacent stays where it is. And it would
//! buy `hop-gtk`, this module's other caller, nothing today: that crate
//! does not depend on `hop-core` either, so it has no escaping discipline
//! of its own to preserve yet — see `apps/hop-gtk/Cargo.toml`.
//!
//! The discipline is not dropped, only moved to where it is affordable.
//! This module's own [`ConfigFileError`] formats `path` with
//! [`Path::display`], unescaped — the plain, lossy `Display` every other
//! path in this crate uses — and every existing caller that needs the safe
//! string keeps producing it exactly as before, one layer up, from a path
//! it already holds. `hopd::config::ConfigError` already ran its own
//! `path` field through `escape_path` in every variant before this module
//! existed, using a `path` it already had rather than one read back out of
//! a refusal from here, and that is unchanged by this move:
//! `hopd::config::Config::from_path` maps a [`ConfigFileError`] into its
//! own `ConfigError` variant, carrying the same `path` it passed into
//! [`read`], and that variant's `Display` still escapes it. A future
//! `hop-gtk` caller that wants the same protection for its own
//! user-visible messages needs the same fix `hop-gtk` would need for any
//! other path-bearing message today, regardless of this module — this
//! module is not the reason that gap exists, and closing it here would not
//! close it there.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Every way [`read`] can refuse a file it found at `path`. Absence is not
/// among them — see [`read`]'s own doc comment, "Absent file", for why a
/// missing file is `Ok(None)` rather than a variant here.
///
/// Every variant's `path` is formatted with plain [`Path::display`] rather
/// than an escaping routine — see this module's own doc comment, "Why
/// refusal messages name the path with `Path::display` rather than
/// `escape_path`", for why that discipline could not travel down into this
/// crate, and where it is preserved instead.
#[derive(Debug, Error)]
pub enum ConfigFileError {
    /// The open, or a later read off the descriptor it returned, failed —
    /// most plausibly a permission error, a descriptor that opened but
    /// could not be `fstat`ed, or (issue #160's fourth case, carried over
    /// unchanged) a Unix domain socket at the path: `open()` on a socket
    /// fails immediately with `ENXIO`, before there is a descriptor for
    /// `fstat` to classify, so it lands here rather than in
    /// [`ConfigFileError::NotARegularFile`] the way a FIFO or a device
    /// does.
    #[error("could not read config file {}: {source}", path.display())]
    Read {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying IO error.
        #[source]
        source: io::Error,
    },

    /// What the path resolved to, once opened, is something other than a
    /// regular file — a directory, a FIFO, or a device. Reported of the
    /// *descriptor* [`read`] already has open (`fstat`, via
    /// [`std::fs::File::metadata`]), never of the path a second time — see
    /// [`read`]'s doc comment for why that distinction is the whole point
    /// of this check.
    #[error("config file {} is not a regular file", path.display())]
    NotARegularFile {
        /// The path that did not resolve to a regular file.
        path: PathBuf,
    },

    /// The file is larger than the `max_bytes` its caller passed to
    /// [`read`]. The bytes past the cap are never read off the descriptor,
    /// so whether they would have been valid content is never known — this
    /// file is refused for its size alone, exactly as `hopd::config`'s own
    /// `ConfigError::TooLarge` was before this move.
    #[error(
        "config file {} is larger than the {max_bytes}-byte limit",
        path.display()
    )]
    TooLarge {
        /// The path whose contents exceeded `max_bytes`.
        path: PathBuf,
        /// The cap its caller passed to [`read`].
        max_bytes: u64,
    },
}

/// Reads `path` as a config file: bytes back if it is safe to treat as one,
/// `Ok(None)` if nothing is there, or a typed refusal naming what was
/// wrong. The read is bounded at `max_bytes`, a cap the caller chooses —
/// this module has no opinion on what value is right for any particular
/// schema.
///
/// # Open, classify, read — in that order (issue #160)
///
/// This is not `fs::read_to_string(path)`: that has no bound and no check
/// that the thing it opens is even a regular file, which is what let a
/// FIFO at `hopd`'s config path stall the whole daemon before it ever
/// bound a socket (issue #160's original incident) with no diagnostic
/// anywhere a user could see it.
///
/// **The open carries `O_NONBLOCK`.** Opening a FIFO for reading otherwise
/// blocks until a writer appears — forever, for a FIFO nobody is writing
/// to. `O_NONBLOCK` on a read-only open of a FIFO returns a descriptor
/// immediately instead of waiting, which is what lets the next step
/// classify and refuse it rather than hang on it. On a regular file the
/// flag has no effect at all, so an ordinary config opens and reads
/// exactly as it would without it. This is the same flag
/// [`crate::content::IconPath::open_regular_file`] carries for the
/// identical hazard on the icon-open path (issue #131), the precedent
/// `hopd::config::Config::from_path` followed when it first grew this
/// flag and that this promotion keeps following.
///
/// **What is classified is the descriptor, not the path.**
/// [`std::fs::File::metadata`] is `fstat` on the file this call already
/// has open — it reports the object the open actually returned, not
/// whatever the path names by the time this line runs. Stat-ing the path
/// instead would inspect a second, possibly different, object (see
/// "Replacement between open and read" below); classifying the descriptor
/// is what guarantees the check and the read that follows act on the same
/// file. A directory, device, socket, or FIFO fails `metadata.is_file()`
/// and is never read from — reported as [`ConfigFileError::NotARegularFile`]
/// (a Unix domain socket instead fails the open itself with `ENXIO`,
/// before there is ever a descriptor to classify — see
/// [`ConfigFileError::Read`]'s doc comment).
///
/// **The read is bounded at `max_bytes`**, via [`std::io::Read::take`]:
/// `take(max_bytes + 1)` rather than `take(max_bytes)` is what tells a file
/// sitting exactly on the cap (accepted) apart from one a single byte over
/// it (refused) without the read — or the `Vec<u8>` it fills — ever
/// growing past `max_bytes + 1` bytes to find out. This is
/// `hop-core::learning`'s bounded-read precedent (issue #37), followed
/// here rather than reinvented, the same precedent `hopd::config` named
/// when it first bounded this read.
///
/// # Errors
///
/// A `NotFound` on the open is not an error — see this module's doc
/// comment, "Absent file". Any other open or classification failure is
/// [`ConfigFileError::Read`]. Something other than a regular file is
/// [`ConfigFileError::NotARegularFile`]. A file over `max_bytes` is
/// [`ConfigFileError::TooLarge`]. A file that is both a regular file and
/// within the cap comes back as `Ok(Some(bytes))`, undecoded — this module
/// does not know whether the bytes are valid UTF-8, TOML, or anything
/// else; that is each caller's own schema's job.
///
/// # Symlinks
///
/// The open follows a symlink at `path`, exactly as `fs::read_to_string`
/// would: pointing a config path at a config that lives elsewhere is
/// ordinary and must keep working, the same reasoning
/// [`crate::content::IconPath::open_regular_file`]'s docs give for not
/// adding `O_NOFOLLOW`. What is classified is what the link *resolves to*
/// — a symlink to a regular file loads, a symlink to `/dev/zero` does not,
/// because the `fstat` sees the character device on the other end of the
/// link rather than the link itself.
///
/// A dangling symlink — one whose target was moved or removed — fails the
/// open with `ENOENT`, the identical error a path that names nothing at
/// all fails with, so it is handled the same way: `Ok(None)`, as though no
/// config had been placed there. A link that resolves to nothing is
/// indistinguishable, from this function's point of view, from nothing
/// having been placed at the path at all.
///
/// # Replacement between open and read
///
/// The open and every read that follows act on one file descriptor, not on
/// the path a second time, so once the open succeeds this function reads
/// the object it opened even if the path is unlinked, replaced, or
/// repointed at something else immediately afterward — the classic TOCTOU
/// window is closed by construction here, because there is only ever one
/// path lookup. What remains is the window *before* that one lookup:
/// nothing stops a concurrent write from finishing, or a replacement from
/// landing, in the instant before the open runs, and the open simply reads
/// whatever was there then. Nothing after the open can change which object
/// gets classified or read.
pub fn read(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, ConfigFileError> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigFileError::Read {
                path: path.to_owned(),
                source: err,
            });
        }
    };

    // `File::metadata` is `fstat` on this descriptor: it reports the
    // object that was actually opened, never the path.
    let metadata = file.metadata().map_err(|err| ConfigFileError::Read {
        path: path.to_owned(),
        source: err,
    })?;
    if !metadata.is_file() {
        return Err(ConfigFileError::NotARegularFile {
            path: path.to_owned(),
        });
    }

    let mut data = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut data)
        .map_err(|err| ConfigFileError::Read {
            path: path.to_owned(),
            source: err,
        })?;
    if data.len() as u64 > max_bytes {
        return Err(ConfigFileError::TooLarge {
            path: path.to_owned(),
            max_bytes,
        });
    }

    Ok(Some(data))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    const CAP: u64 = 1024;

    #[test]
    fn an_absent_file_is_ok_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        assert!(read(&path, CAP).unwrap().is_none());
    }

    #[test]
    fn an_ordinary_regular_file_within_the_cap_is_read_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, b"max_results = 10\n").unwrap();

        let data = read(&path, CAP).unwrap().unwrap();
        assert_eq!(data, b"max_results = 10\n");
    }

    #[test]
    fn a_directory_at_the_path_is_not_a_regular_file() {
        // A directory opens read-only without complaint on Linux — only the
        // fstat catches it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::create_dir(&path).unwrap();

        let err = read(&path, CAP).unwrap_err();
        assert!(
            matches!(err, ConfigFileError::NotARegularFile { .. }),
            "expected NotARegularFile, got {err:?}"
        );
    }

    #[test]
    fn a_file_over_the_cap_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "x".repeat(CAP as usize + 1)).unwrap();

        let err = read(&path, CAP).unwrap_err();
        match &err {
            ConfigFileError::TooLarge { path: p, max_bytes } => {
                assert_eq!(*p, path);
                assert_eq!(*max_bytes, CAP);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn a_file_exactly_at_the_cap_is_accepted() {
        // Pins the `take(max_bytes + 1)` boundary from the accepting side —
        // a file sitting exactly on the ceiling must still load.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "x".repeat(CAP as usize);
        fs::write(&path, &text).unwrap();

        let data = read(&path, CAP).unwrap().unwrap();
        assert_eq!(data.len() as u64, CAP);
    }

    #[test]
    fn a_fifo_at_the_path_is_not_opened_and_does_not_block_the_read() {
        // The hazard here happens *before* any check could run: opening a
        // FIFO for reading blocks until a writer appears, so a regression
        // here would hang this test rather than fail it cleanly.
        // `O_NONBLOCK` is what makes the open return instead of waiting.
        //
        // Modeled on `hopd::config`'s identical test
        // (`a_fifo_at_the_config_path_is_not_opened_and_does_not_block_load`)
        // and `hop-protocol::content`'s (`a_fifo_is_not_opened`): the read
        // runs on a worker thread and the result is awaited with a timeout,
        // so a regression here fails rather than hangs the suite.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a live NUL-terminated string for the duration
        // of the call, which is all `mkfifo(3)` requires of it.
        //
        // Test-only, on the same footing as this workspace's other
        // test-only `unsafe` for this identical hazard
        // (`hop-protocol::content`'s and `hopd::config`'s own `libc::mkfifo`
        // calls, issue #131 and issue #160): none of them is production
        // code, and each needs its own narrow `#[expect(unsafe_code)]` to
        // build at all under this workspace's `unsafe_code = "deny"` lint.
        // `expect` rather than `allow` so that if `mkfifo` ever grows a
        // safe wrapper, the unfulfilled expectation becomes a warning and
        // CI's `-D warnings` turns that into a build failure — the
        // exception deletes itself instead of outliving its reason.
        #[expect(
            unsafe_code,
            reason = "mkfifo(3) has no safe wrapper in libc; test-only, and production code \
                      has none, matching hop-protocol::content's and hopd::config's precedent \
                      for this exact hazard"
        )]
        let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(
            made,
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read(&path, CAP).map(|_| ()));
        });

        let result = rx.recv_timeout(Duration::from_secs(10)).expect(
            "reading a FIFO config must return rather than wait for a writer; timing out \
             here means the open blocked",
        );
        let err = result.expect_err("a FIFO is not a regular file");
        assert!(
            matches!(err, ConfigFileError::NotARegularFile { .. }),
            "expected NotARegularFile, got {err:?}"
        );
    }
}
