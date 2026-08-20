//! The allow-list issue #93 closes the gap `hop_protocol::IconPath` leaves
//! open: which filesystem locations an `IconSpec::Path` is allowed to
//! resolve to, computed once from this process's own environment.
//!
//! # Why this exists here and not in `hop-protocol` or `hopd`
//!
//! `IconPath`'s own doc comment (`crates/hop-protocol/src/content.rs`,
//! "Where an icon is expected to live" and "What documenting the roots
//! instead of enforcing them costs") explains why the roots are documented
//! on the wire type rather than enforced by it: they depend on
//! `$XDG_DATA_DIRS` and `$XDG_DATA_HOME`, which are a property of whichever
//! process resolves the path, not of the frame itself — a frame valid on
//! one machine would be refused on another if the check lived in the
//! contract. Issue #93's triage settled the second half of the same
//! question: `hopd` cannot compute this check either, because it runs as a
//! systemd `--user` socket-activated service and is not guaranteed to
//! inherit the interactive session's `XDG_DATA_DIRS` (Flatpak/Snap entries
//! included) — a daemon-side check could refuse a value the client would
//! accept, or the reverse, which is worse than no check. So this lives in
//! `hop-gtk`, the one process whose own environment is authoritative for
//! what its roots are, at the one place that opens an icon file:
//! `ui::row::load_path_texture`.
//!
//! # The roots, and where the list comes from
//!
//! [`icon_roots`] computes exactly the list `IconPath`'s own "Where an icon
//! is expected to live" section documents:
//!
//! - `$XDG_DATA_DIRS/icons` (`/usr/local/share/icons` and
//!   `/usr/share/icons` by default) — the freedesktop icon theme
//!   specification's search path.
//! - `/usr/share/pixmaps` — the legacy flat fallback the same
//!   specification keeps, included unconditionally since it is not derived
//!   from any environment variable.
//! - `~/.icons` and `$XDG_DATA_HOME/icons` (falling back to
//!   `~/.local/share/icons`) — the per-user themes.
//!
//! It is a pure function of three `Option<&str>` values, modelled on the
//! *shape* of `hopd::apps::xdg_application_roots` (not called from here —
//! that function answers a different question, where `.desktop` files
//! live, for a different process) so the computation is unit-testable
//! without mutating this process's real environment. [`icon_roots_from_env`]
//! is the thin wrapper that actually reads `std::env`.
//!
//! # Computed once, not per icon: [`ALLOWED_ICON_ROOTS`]
//!
//! The roots are read from the environment once and reused for the life of
//! the process, the same discipline `tokens.rs` already applies to values
//! derived from `assets/tokens.css` (`tokens::ROW_HEIGHT_PX` and its
//! siblings): a [`std::sync::LazyLock`] computes [`AllowedIconRoots`] on
//! first access and every access after that is a plain read. `app.rs`'s
//! `run` forces that computation explicitly, near the top, before either
//! run mode builds a window — see that function's own comment for why one
//! call site there covers both `run_interactive` and `run_screenshot`
//! without needing a second one in either `connect_startup` handler.
//!
//! A value threaded as a parameter through `ui::view::bind`/`unbind` and
//! `ui::row::bind` was the other option the issue's brief named. It was
//! rejected here because the roots are genuinely process-wide, invariant
//! state — closer in kind to `tokens::ICON_SIZE_PX` than to anything that
//! varies per call — and threading it through every dispatch layer between
//! `ui::view::bind` and `load_path_texture` would touch signatures four
//! layers deep for a value that never differs between two calls in the same
//! process.
//!
//! # The enforcement mechanism: resolve the opened descriptor, not the path
//!
//! See [`AllowedIconRoots::permits`] for the check itself and the argument
//! for why it reads `/proc/self/fd/<n>` on the already-opened file rather
//! than canonicalizing the path a second time.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// The freedesktop icon theme search roots, computed from three optional
/// environment values rather than read from `std::env` directly — see this
/// module's doc comment for why, and for the list this reproduces from
/// `hop_protocol::content::IconPath`'s own documentation.
///
/// `data_dirs` falls back to `/usr/local/share:/usr/share`, the same
/// default the freedesktop base directory specification gives
/// `XDG_DATA_DIRS` and the same literal `hopd::apps::xdg_application_roots`
/// falls back to for the same reason. `data_home` falls back to deriving
/// `~/.local/share/icons` from `home` when unset, matching the base
/// directory spec's own fallback for `XDG_DATA_HOME`.
pub(crate) fn icon_roots(
    home: Option<&str>,
    data_home: Option<&str>,
    data_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    let data_dirs = data_dirs.unwrap_or("/usr/local/share:/usr/share");
    for dir in data_dirs
        .split(':')
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
    {
        roots.push(Path::new(dir).join("icons"));
    }

    // Not derived from any environment variable — the freedesktop icon
    // theme specification names this exact path as a fixed fallback
    // location, so it is as much a documented root as the ones built from
    // `data_dirs` below, not a hardcoded stand-in for them.
    roots.push(PathBuf::from("/usr/share/pixmaps"));

    if let Some(home) = home {
        roots.push(Path::new(home).join(".icons"));
    }

    if let Some(data_home) = data_home {
        roots.push(Path::new(data_home).join("icons"));
    } else if let Some(home) = home {
        roots.push(Path::new(home).join(".local/share/icons"));
    }

    roots
}

/// [`icon_roots`], reading `$HOME`, `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`
/// from this process's real environment. The only caller of `icon_roots`
/// that touches `std::env` — every test exercises the pure function above
/// instead.
fn icon_roots_from_env() -> Vec<PathBuf> {
    icon_roots(
        std::env::var("HOME").ok().as_deref(),
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("XDG_DATA_DIRS").ok().as_deref(),
    )
}

/// The startup-computed allow-list: [`icon_roots_from_env`]'s output, each
/// entry canonicalized once — resolved to the real, symlink-free absolute
/// path it names, including when a root itself is a symlink (`~/.icons` is
/// one on some distros, usually pointing at
/// `~/.local/share/icons`) — so [`permits`](Self::permits) compares two
/// already-resolved paths against each other rather than a resolved path
/// against a root that might still itself be one hop away from where its
/// files actually live.
///
/// A root that does not exist on this machine (no `~/.icons` directory is
/// entirely ordinary) fails to canonicalize and is silently dropped: it
/// contributes nothing to permit, and nothing to refuse, since no file can
/// ever resolve under a location nothing occupies.
#[derive(Debug)]
pub(crate) struct AllowedIconRoots {
    canonical: Vec<PathBuf>,
}

impl AllowedIconRoots {
    /// Builds the allow-list from an explicit root list — the entry point
    /// every test in this module uses, so none of them has to go through
    /// [`from_env`](Self::from_env) or mutate `std::env`.
    pub(crate) fn new(roots: Vec<PathBuf>) -> Self {
        let canonical = roots
            .into_iter()
            .filter_map(|root| std::fs::canonicalize(&root).ok())
            .collect();
        Self { canonical }
    }

    /// [`Self::new`] over [`icon_roots_from_env`] — the constructor
    /// [`ALLOWED_ICON_ROOTS`] uses.
    fn from_env() -> Self {
        Self::new(icon_roots_from_env())
    }

    /// True if `resolved` — an already fully-resolved absolute path, with
    /// every symlink component followed — sits under one of these roots.
    ///
    /// [`Path::starts_with`] compares whole path components, not a
    /// substring of the text, which is what keeps a root named
    /// `/usr/share/icons` from matching a resolved path under
    /// `/usr/share/icons-evil`: the latter's first component after
    /// `/usr/share/` is `icons-evil`, a different component from `icons`,
    /// so the two paths share no `starts_with` relationship even though one
    /// string is a textual prefix of the other.
    fn contains(&self, resolved: &Path) -> bool {
        self.canonical.iter().any(|root| resolved.starts_with(root))
    }

    /// True if `file` — already opened by
    /// [`hop_protocol::IconPath::open_regular_file`] — resolves to a
    /// location under one of these roots. This is the enforcement point
    /// issue #93 asks for: [`ui::row::load_path_texture`](crate::ui::row)
    /// calls this on the file `open_regular_file` already returned, before
    /// reading a single byte from it.
    ///
    /// # Mechanism: resolve the open descriptor, not the path
    ///
    /// This reads `/proc/self/fd/<n>` for the descriptor `file` already
    /// holds, rather than canonicalizing `path.as_path()` a second time.
    /// The difference matters for the same reason
    /// `IconPath::open_regular_file`'s own doc comment gives for
    /// open-then-fstat over stat-then-open: "the descriptor *is* the
    /// file". Canonicalizing the path string and opening it are two
    /// separate operations on two separate looks at the filesystem — the
    /// path can be repointed (by replacing a symlink, or by unlinking and
    /// recreating a component) between them, so a check against the
    /// re-resolved path proves nothing about what the earlier open actually
    /// returned. `/proc/self/fd/<n>` has no such gap: it is a "magic
    /// symlink" the kernel computes from the open file description itself
    /// — the real, already-resolved absolute path of the exact inode that
    /// call opened, with every symlink already followed, including any
    /// among the parent directories (which a check against only the final
    /// path component, or a plain `O_NOFOLLOW` on the open, would both miss
    /// — see `IconPath::open_regular_file`'s own "Why not `O_NOFOLLOW`"
    /// section for the parallel argument there). Reading it after the file
    /// is already open, off the descriptor `open_regular_file` handed back,
    /// is what keeps the check and the open referring to the same file with
    /// no window between them: there is no second `open` anywhere in this
    /// crate, only a read of what the *existing* descriptor already
    /// resolved to.
    ///
    /// # What this means when an allowed root is itself a symlink
    ///
    /// [`AllowedIconRoots::new`] canonicalizes every root once, at
    /// construction, which resolves a symlinked root (again, `~/.icons` on
    /// some distros) to the real directory it points at. The path this
    /// method reads out of `/proc/self/fd` is, independently, also fully
    /// resolved. So both sides of the [`contains`](Self::contains)
    /// comparison are canonical real paths, and a file that physically
    /// lives under a symlinked root's real target is permitted exactly as
    /// if the root had been a plain directory all along — proven by
    /// `a_symlinked_allowed_root_still_permits_files_under_its_real_target`
    /// below.
    ///
    /// # Why not `openat2`
    ///
    /// `openat2` with `RESOLVE_*` flags can refuse to leave a root as part
    /// of the open itself, which was the other mechanism issue #93's brief
    /// named. It was not used here because it would be a *second* opener:
    /// `IconPath::open_regular_file` already ran the open this method
    /// inspects, and `hop_protocol`'s own docs are explicit that it is the
    /// only place `hop-protocol` makes a syscall opening an icon path, and
    /// `ui::row`'s doc comment is equally explicit that nothing in this
    /// crate opens one any other way. A second `openat2` call here would
    /// duplicate that open (and its own `O_NONBLOCK`/regular-file handling)
    /// under a different name, for no benefit `/proc/self/fd` does not
    /// already give for free, with no `unsafe` and no new dependency.
    ///
    /// # No `unsafe`
    ///
    /// Everything this method does — `AsRawFd::as_raw_fd`,
    /// `std::fs::read_link`, string and `Path` comparison — is a safe,
    /// ordinary standard-library call. `openat2` would have needed a raw
    /// syscall (`libc`, not currently a `hop-gtk` dependency, behind a
    /// narrow `#[expect(unsafe_code)]`); this mechanism needs none, which
    /// is itself part of why it was preferred once both were shown to
    /// satisfy the brief's actual requirement equally well.
    ///
    /// # Assumes Linux
    ///
    /// `/proc/self/fd` is a Linux-specific mechanism (present on some other
    /// Unixes under compatibility layers, not guaranteed). This crate
    /// already assumes a Linux desktop session throughout — GNOME's own
    /// D-Bus activation and `systemd --user` socket activation
    /// (`hopd`'s own contrib units), `gtk4-layer-shell` — so this adds no
    /// new platform assumption beyond what `hop-gtk` already carries.
    pub(crate) fn permits(&self, file: &std::fs::File) -> bool {
        use std::os::unix::io::AsRawFd;

        let link = format!("/proc/self/fd/{}", file.as_raw_fd());
        let Ok(resolved) = std::fs::read_link(&link) else {
            return false;
        };

        // A descriptor whose file has been unlinked since it was opened
        // reads back with " (deleted)" appended to the target string by
        // the kernel. Refused rather than stripped: this method's contract
        // is "resolves under an allowed root", and an unlinked inode no
        // longer resolves to any path at all, under a root or otherwise —
        // treating the suffixed string as if it still named a location
        // under the root it used to occupy would be answering a question
        // that no longer has the meaning this check exists to give it.
        if resolved.to_string_lossy().ends_with(" (deleted)") {
            return false;
        }

        self.contains(&resolved)
    }
}

/// The allow-list this process enforces every `IconSpec::Path` against —
/// computed once, from this process's own environment, the first time
/// anything reads it (which `app.rs`'s `run` forces to happen at startup;
/// see this module's doc comment). `ui::row::load_path_texture` is the sole
/// reader.
pub(crate) static ALLOWED_ICON_ROOTS: LazyLock<AllowedIconRoots> =
    LazyLock::new(AllowedIconRoots::from_env);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_roots_computes_the_documented_list_from_explicit_values() {
        let roots = icon_roots(Some("/home/u"), Some("/home/u/.data"), Some("/a:/b"));
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/a/icons"),
                PathBuf::from("/b/icons"),
                PathBuf::from("/usr/share/pixmaps"),
                PathBuf::from("/home/u/.icons"),
                PathBuf::from("/home/u/.data/icons"),
            ]
        );
    }

    #[test]
    fn icon_roots_derives_data_home_from_home_when_data_home_is_unset() {
        let roots = icon_roots(Some("/home/u"), None, Some("/a"));
        assert!(roots.contains(&PathBuf::from("/home/u/.local/share/icons")));
        // and does not also include a bare $XDG_DATA_HOME/icons entry, since
        // there is no data_home value to build one from
        assert_eq!(roots.iter().filter(|p| p.ends_with("icons")).count(), 2);
    }

    #[test]
    fn icon_roots_falls_back_to_the_documented_default_data_dirs_when_unset() {
        let roots = icon_roots(None, None, None);
        assert!(roots.contains(&PathBuf::from("/usr/local/share/icons")));
        assert!(roots.contains(&PathBuf::from("/usr/share/icons")));
    }

    #[test]
    fn icon_roots_always_includes_the_legacy_pixmaps_fallback() {
        let roots = icon_roots(None, None, None);
        assert!(roots.contains(&PathBuf::from("/usr/share/pixmaps")));
    }

    #[test]
    fn a_file_under_an_allowed_root_is_permitted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("icon.png");
        std::fs::write(&file_path, b"fake-icon-bytes").expect("write");
        let roots = AllowedIconRoots::new(vec![dir.path().to_path_buf()]);
        let file = std::fs::File::open(&file_path).expect("open");
        assert!(roots.permits(&file));
    }

    #[test]
    fn a_file_outside_every_allowed_root_is_not_permitted() {
        let allowed = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let file_path = outside.path().join("icon.png");
        std::fs::write(&file_path, b"fake-icon-bytes").expect("write");
        let roots = AllowedIconRoots::new(vec![allowed.path().to_path_buf()]);
        let file = std::fs::File::open(&file_path).expect("open");
        assert!(!roots.permits(&file));
    }

    #[test]
    fn a_symlink_under_an_allowed_root_that_leads_outside_it_is_not_permitted() {
        let allowed = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let target = outside.path().join("secret.png");
        std::fs::write(&target, b"secret").expect("write");
        let link = allowed.path().join("escapes.png");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let roots = AllowedIconRoots::new(vec![allowed.path().to_path_buf()]);
        let file = std::fs::File::open(&link).expect("open");
        assert!(!roots.permits(&file));
    }

    #[test]
    fn an_ordinary_symlink_within_an_allowed_root_is_permitted() {
        // The case that makes `O_NOFOLLOW` the wrong tool: icon themes are
        // built out of symlinks that stay inside the theme's own root
        // (`/usr/share/icons/hicolor` is largely links between sizes and
        // themes), and refusing to follow one would leave those ordinary
        // icons unopened.
        let allowed = tempfile::tempdir().expect("tempdir");
        let target = allowed.path().join("real.png");
        std::fs::write(&target, b"real-icon-bytes").expect("write");
        let link = allowed.path().join("themed-link.png");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let roots = AllowedIconRoots::new(vec![allowed.path().to_path_buf()]);
        let file = std::fs::File::open(&link).expect("open");
        assert!(roots.permits(&file));
    }

    #[test]
    fn a_symlinked_allowed_root_still_permits_files_under_its_real_target() {
        // Models `~/.icons` being a symlink to `~/.local/share/icons`, as
        // it is on some distros: the *root itself*, not a file under it, is
        // the symlink.
        let real_root = tempfile::tempdir().expect("tempdir");
        let link_parent = tempfile::tempdir().expect("tempdir");
        let root_link = link_parent.path().join("icons");
        std::os::unix::fs::symlink(real_root.path(), &root_link).expect("symlink");
        let file_path = real_root.path().join("icon.png");
        std::fs::write(&file_path, b"real-icon-bytes").expect("write");

        let roots = AllowedIconRoots::new(vec![root_link]);
        let file = std::fs::File::open(&file_path).expect("open");
        assert!(roots.permits(&file));
    }

    #[test]
    fn proc_self_mem_is_not_permitted_by_this_process_real_environment() {
        // Named literally, per issue #93's acceptance criteria: this is the
        // regular file the issue exists to close a path to, and it must
        // never resolve under any icon root regardless of what roots this
        // machine happens to have.
        let roots = AllowedIconRoots::from_env();
        let file = std::fs::File::open("/proc/self/mem").expect("procfs must exist on Linux");
        assert!(!roots.permits(&file));
    }
}
