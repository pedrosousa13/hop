//! The apps provider: an in-memory index of installed applications built
//! from `.desktop` files (freedesktop.org's Desktop Entry Specification),
//! maintained by filesystem events rather than rebuilt per query — see
//! [`AppIndex`] for the "no disk read on the query path" half of this
//! module's contract (design spec §3), and [`AppsProvider`] for the
//! [`hop_core::provider::Provider`] this module registers.
//!
//! Salvaged from a previous project's `crates/hopd/src/providers/apps.rs`
//! (parsing and root-enumeration logic only — that source rebuilt its whole
//! index from disk on every query, which is exactly the fatal flaw this
//! module's index exists to avoid) and from that project's
//! `lib/providers/appLaunch.js`, the launch semantics [`focus_or_launch`]
//! ports. See this crate's implementation plan
//! (`docs/superpowers/plans/2026-08-04-issue-57-apps-provider.md`) for the
//! full reasoning behind each divergence from those sources.

use hop_protocol::{
    Action, ActionId, ActionKind, IconName, IconPath, IconSpec, Item, ItemId, Kind,
    limits::MAX_TITLE,
};

/// One `.desktop` file's [Desktop Entry] group, parsed and ready to become
/// an [`AppEntry`] once its filesystem-derived app id is known.
///
/// Kept separate from [`AppEntry`] because parsing (this type) is pure and
/// per-file, while assembling an [`AppEntry`] (`build_entry`) also needs the
/// app id, which comes from the file's *name*, not its contents.
#[derive(Debug, PartialEq, Eq)]
// Scoped per-item, not module-wide: a module-wide `#![expect(dead_code)]` is
// satisfied as long as *any* item in the file is still dead, so it would stay
// silently fulfilled if Task 2 wired up some of these seven items but not all
// (`AppEntry`'s `haystack` field, say, staying unread until Task 3's index).
// One `#[expect]` per symbol means each stops being optional independently:
// the moment *that* item gets a real caller, *that* attribute's expectation
// goes unfulfilled and `-D warnings` fails the build, so no partially-wired
// state can hide behind a sibling that's still unused. Matches the scoping
// (not just the `expect`-over-`allow` choice) of this workspace's other use
// of the pattern, `hop-protocol`'s single-statement `#[expect(unsafe_code)]`
// on its `mkfifo` call.
//
// `cfg_attr(not(test), ...)` rather than a bare `#[expect]`: this crate's
// tests (below) call every one of these seven items directly, so under
// `--cfg test` they are not dead at all and an unconditional `#[expect]`
// would itself go unfulfilled on `cargo test` — the exact same "expectation
// silently wrong" failure mode this attribute exists to avoid, just moved to
// the other build. Restricting the expectation to the non-test build is what
// it is actually describing: "no consumer *outside tests* yet."
pub(crate) struct ParsedEntry {
    pub(crate) title: String,
    /// The `Exec=` value, field codes (`%f`, `%U`, ...) stripped — ready to
    /// split on whitespace into a program and its arguments.
    pub(crate) exec: String,
    /// `None` when `Icon=` was absent or empty. `Some` either way — a bare
    /// theme name or an absolute path — with the arm decided at
    /// [`build_entry`], since only there is the value handed to
    /// [`IconName::new`] or [`IconPath::new`].
    pub(crate) icon: Option<String>,
    /// Lowercased title, `Exec=`, `GenericName=`, `Comment=` and
    /// `Keywords=`, space-joined — what [`AppIndex::query`]'s filter matches
    /// against. Never sent to a client; [`Item`] carries no such field.
    pub(crate) haystack: String,
}

/// Parses one `.desktop` file's contents into a [`ParsedEntry`], or `None`
/// if the file has no usable `[Desktop Entry]` group — no `Name=`, or
/// `Hidden=true`/`NoDisplay=true`.
///
/// Ported from the salvaged `parse_desktop_entry`, adjusted to this crate's
/// types: the salvaged version built a project-local `SearchItem` directly;
/// this one stops at a [`ParsedEntry`] so [`build_entry`] can apply
/// `hop-protocol`'s content rules ([`IconName`], [`IconPath`]) before
/// anything becomes an [`Item`].
pub(crate) fn parse_desktop_entry(content: &str) -> Option<ParsedEntry> {
    let mut name = String::new();
    let mut localized_name = String::new();
    let mut exec = String::new();
    let mut keywords = String::new();
    let mut generic_name = String::new();
    let mut comment = String::new();
    let mut icon = String::new();
    let mut hidden = false;
    let mut no_display = false;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }

        if let Some(value) = line.strip_prefix("Name=") {
            if name.is_empty() {
                name = value.trim().to_string();
            }
        } else if line.starts_with("Name[") {
            if let Some((_, value)) = line.split_once('=')
                && localized_name.is_empty()
            {
                localized_name = value.trim().to_string();
            }
        } else if let Some(value) = line.strip_prefix("Exec=") {
            if exec.is_empty() {
                exec = sanitize_exec(value);
            }
        } else if let Some(value) = line.strip_prefix("Keywords=") {
            if keywords.is_empty() {
                keywords = value.replace(';', " ");
            }
        } else if let Some(value) = line.strip_prefix("GenericName=") {
            if generic_name.is_empty() {
                generic_name = value.trim().to_string();
            }
        } else if let Some(value) = line.strip_prefix("Comment=") {
            if comment.is_empty() {
                comment = value.trim().to_string();
            }
        } else if let Some(value) = line.strip_prefix("Icon=") {
            if icon.is_empty() {
                icon = value.trim().to_string();
            }
        } else if let Some(value) = line.strip_prefix("Hidden=") {
            hidden = value.trim().eq_ignore_ascii_case("true");
        } else if let Some(value) = line.strip_prefix("NoDisplay=") {
            no_display = value.trim().eq_ignore_ascii_case("true");
        }
    }

    if hidden || no_display {
        return None;
    }
    if name.is_empty() {
        name = localized_name;
    }
    if name.is_empty() {
        return None;
    }

    // Truncated to MAX_TITLE at a char boundary rather than left as-is: this
    // provider constructs `Item`s directly rather than through
    // `hop_protocol`'s `Deserialize` gate, so nothing else in this crate
    // enforces the bound `crate::source::ResultSource`'s own docs warn a
    // provider is on its honor for. A `Name=` this long has never been
    // observed in a real desktop entry; the guard exists because the type
    // allows it, not because it is expected to fire.
    let title = truncate_to_byte_boundary(&name, MAX_TITLE);

    let merged_keywords = [
        title.as_str(),
        exec.as_str(),
        keywords.as_str(),
        generic_name.as_str(),
        comment.as_str(),
    ]
    .join(" ");

    Some(ParsedEntry {
        title,
        exec,
        icon: (!icon.is_empty()).then_some(icon),
        haystack: merged_keywords.to_lowercase(),
    })
}

/// Strips `%`-prefixed field codes (`%f`, `%F`, `%u`, `%U`, `%i`, `%c`, `%k`,
/// ...) from an `Exec=` value, ported verbatim from the salvaged parser.
/// These placeholders are filled in by whatever launches the entry with
/// arguments the launcher doesn't have (a file to open, an icon path); with
/// none supplied, dropping the token is the specification's own answer for
/// an application invoked with no arguments.
fn sanitize_exec(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|token| !token.starts_with('%'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncates `s` to at most `max` bytes, never splitting a multi-byte
/// character. Short-circuits when already within bound, so this allocates
/// only when it actually has work to do.
fn truncate_to_byte_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The app id a `.desktop` file's name contributes: the file name with a
/// trailing `.desktop` removed, or `None` if the file does not end in
/// `.desktop` at all (`desktop_entry_files`, Task 2, filters to that
/// extension before this is ever called, so `None` here is defensive, not
/// expected).
///
/// This is the *unqualified* desktop-file id — the freedesktop spec's fuller
/// definition joins subdirectory names with `-` for a nested file, which
/// this function does not do; see this plan's Scope section for why that is
/// out of scope.
pub(crate) fn app_id_from_file_name(file_name: &str) -> Option<String> {
    file_name.strip_suffix(".desktop").map(str::to_string)
}

/// Builds the [`Item`] half of an [`AppEntry`] from a parsed entry and its
/// app id: the id `app:<app_id>`, `kind: Kind::App`, the default `open`
/// action, and an icon built through `hop-protocol`'s content rules (see
/// this plan's Design decision 2).
///
/// An icon value that fails its own rule — over length, or, for a name, a
/// forbidden character — becomes `icon: None` rather than sinking the whole
/// entry: unlike a value arriving off the wire, there is no "refuse the
/// frame" available here, and a working item with no icon is a better
/// outcome than no item at all over one malformed `Icon=` line. If
/// [`ItemId::new`] itself fails — over [`MAX_ITEM_ID`](hop_protocol::MAX_ITEM_ID),
/// which no real filename reaches (see the reasoning at the call site in
/// Task 2) — the whole entry is dropped, since there is no id left to build
/// one under.
pub(crate) fn build_entry(app_id: String, parsed: ParsedEntry) -> Option<AppEntry> {
    let id = ItemId::new(format!("app:{app_id}")).ok()?;

    let icon = parsed.icon.and_then(|value| {
        if value.starts_with('/') {
            IconPath::new(value).ok().map(IconSpec::Path)
        } else {
            IconName::new(value).ok().map(IconSpec::Name)
        }
    });

    let item = Item {
        id,
        kind: Kind::App,
        title: parsed.title,
        subtitle: None,
        icon,
        actions: vec![Action {
            id: ActionId::new("open").expect("within bounds by construction"),
            kind: ActionKind::Open,
            label: "Open".to_string(),
        }],
        default_action: ActionId::new("open").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: hop_core::provider::APPS_PROVIDER_ID.to_string(),
    };

    Some(AppEntry {
        app_id,
        item,
        exec: parsed.exec,
        haystack: parsed.haystack,
    })
}

/// One indexed application: the [`Item`] a query returns, plus the fields
/// that exist only to build a query filter and an `execute` dispatch —
/// never serialized, since [`Item`] has no field for either. See this plan's
/// Design decision 7.
#[derive(Debug, Clone)]
pub struct AppEntry {
    pub(crate) app_id: String,
    pub(crate) item: Item,
    pub(crate) exec: String,
    pub(crate) haystack: String,
}

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The XDG Base Directory roots a `.desktop` file may live under, in the
/// order this crate treats as precedence-first-wins: `data_home` (falling
/// back to `~/.local/share`), then each entry of `data_dirs` (falling back
/// to `/usr/local/share:/usr/share`), each with `/applications` appended.
///
/// Order matters beyond cosmetics: [`scan_apps`] keeps the first entry it
/// sees for a given app id and discards later ones, so a user override in
/// `XDG_DATA_HOME` correctly shadows a system-installed entry with the same
/// filename rather than the two colliding arbitrarily.
///
/// Pure — see this task's note on why the caller supplies these values
/// rather than this function reading `std::env` itself.
pub(crate) fn xdg_application_roots(
    home: Option<&str>,
    data_home: Option<&str>,
    data_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(data_home) = data_home {
        roots.push(Path::new(data_home).join("applications"));
    } else if let Some(home) = home {
        roots.push(Path::new(home).join(".local/share/applications"));
    }

    let data_dirs = data_dirs.unwrap_or("/usr/local/share:/usr/share");
    for dir in data_dirs
        .split(':')
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        roots.push(Path::new(dir).join("applications"));
    }

    roots
}

/// The Flatpak export directories: the per-user one under `$HOME`, then the
/// system-wide one. Flatpak does not currently register its export
/// directories in `XDG_DATA_DIRS`, which is why these are enumerated
/// separately rather than folding into [`xdg_application_roots`] — ported
/// from the salvaged parser's `desktop_entry_files`.
pub(crate) fn flatpak_application_roots(home: Option<&str>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home {
        roots.push(Path::new(home).join(".local/share/flatpak/exports/share/applications"));
    }
    roots.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    roots
}

/// The largest `.desktop` file [`scan_apps`] will read.
///
/// `scan_apps` runs synchronously in `build_host()`, before `serve_with`
/// binds the listening socket (see `server.rs`), and again on the watcher
/// thread on every filesystem event thereafter — so an unbounded
/// `read_to_string` here is a startup (and later, a permanent index-update)
/// denial of service: one oversized file dropped under any watched root,
/// including the ordinary `~/.local/share/applications` a downloaded
/// `.desktop` file lands in, either exhausts memory reading it or blocks the
/// daemon from ever accepting a connection. 256 KiB is a couple orders of
/// magnitude past the largest real `.desktop` file this crate's author has
/// ever seen (a few KB at most, since the format is a flat key-value list) —
/// generous enough that no legitimate file is ever rejected, small enough
/// that even reading it is not itself the DoS.
const MAX_DESKTOP_FILE_BYTES: u64 = 256 * 1024;

/// Scans every directory in `roots`, in order, parsing each `.desktop` file
/// found into an [`AppEntry`]. A root that does not exist or cannot be read
/// is skipped, not an error — an unconfigured `~/.icons`-style directory on
/// a fresh machine is normal, not exceptional.
///
/// The first entry seen for a given app id wins; a later root offering the
/// same filename is discarded. This is what makes `roots`' ordering
/// (user-then-system, from [`xdg_application_roots`]) a real precedence
/// rule rather than a coincidence of iteration order.
///
/// Every candidate is `stat`-ed before it is read: anything that is not a
/// regular file (a symlink resolving to a FIFO or a character device such as
/// `/dev/zero`, which has no EOF and would hang a `read_to_string` forever)
/// or that exceeds [`MAX_DESKTOP_FILE_BYTES`] is skipped exactly like a
/// missing or unreadable file, never read. This check runs after an app id
/// is marked "seen" in `seen_ids`, matching this function's existing
/// seen-before-validated ordering (tracked separately; not this change's
/// concern) — an oversized or special file under a later root still blocks a
/// same-named entry from a later root the way any other invalid file does.
///
/// The only place in this module that performs disk I/O other than the
/// inotify watcher itself (`open_watch`/`spawn_index_watcher`, Task 6) —
/// called once at startup and once per filesystem-change notification
/// thereafter, **never** from [`AppIndex::query`] (Task 3).
pub fn scan_apps(roots: &[PathBuf]) -> Vec<AppEntry> {
    let mut seen_ids = HashSet::new();
    let mut entries = Vec::new();

    for root in roots {
        let Ok(dir_entries) = std::fs::read_dir(root) else {
            continue;
        };
        for dir_entry in dir_entries.flatten() {
            let path = dir_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(app_id) = app_id_from_file_name(file_name) else {
                continue;
            };
            if !seen_ids.insert(app_id.clone()) {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_DESKTOP_FILE_BYTES {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(parsed) = parse_desktop_entry(&content) else {
                continue;
            };
            if let Some(entry) = build_entry(app_id, parsed) {
                entries.push(entry);
            }
        }
    }

    entries
}

#[cfg(test)]
mod scan_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::fs;

    fn write_entry(dir: &Path, file_name: &str, name: &str) {
        fs::write(
            dir.join(file_name),
            format!("[Desktop Entry]\nName={name}\nExec={name}\n"),
        )
        .unwrap();
    }

    #[test]
    fn xdg_roots_prefer_data_home_over_the_local_share_default() {
        let roots = xdg_application_roots(Some("/home/x"), Some("/custom/data"), None);
        assert_eq!(roots[0], PathBuf::from("/custom/data/applications"));
    }

    #[test]
    fn xdg_roots_fall_back_to_local_share_when_data_home_is_unset() {
        let roots = xdg_application_roots(Some("/home/x"), None, None);
        assert_eq!(roots[0], PathBuf::from("/home/x/.local/share/applications"));
    }

    #[test]
    fn xdg_roots_split_data_dirs_on_colons_and_ignore_blank_segments() {
        let roots = xdg_application_roots(None, None, Some("/a:/b::/c"));
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/a/applications"),
                PathBuf::from("/b/applications"),
                PathBuf::from("/c/applications"),
            ]
        );
    }

    #[test]
    fn xdg_roots_default_data_dirs_when_unset() {
        let roots = xdg_application_roots(None, None, None);
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/usr/local/share/applications"),
                PathBuf::from("/usr/share/applications"),
            ]
        );
    }

    #[test]
    fn flatpak_roots_include_the_per_user_and_system_export_dirs() {
        let roots = flatpak_application_roots(Some("/home/x"));
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/home/x/.local/share/flatpak/exports/share/applications"),
                PathBuf::from("/var/lib/flatpak/exports/share/applications"),
            ]
        );
    }

    #[test]
    fn scan_apps_finds_desktop_files_and_ignores_others() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), "firefox.desktop", "Firefox");
        fs::write(dir.path().join("not-an-entry.txt"), "irrelevant").unwrap();

        let entries = scan_apps(&[dir.path().to_path_buf()]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].app_id, "firefox");
    }

    #[test]
    fn scan_apps_skips_a_root_that_does_not_exist() {
        let missing = PathBuf::from("/definitely/not/a/real/path/for/this/test");
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), "x.desktop", "X");

        let entries = scan_apps(&[missing, dir.path().to_path_buf()]);
        assert_eq!(
            entries.len(),
            1,
            "a missing root must not abort the whole scan"
        );
    }

    #[test]
    fn scan_apps_skips_a_file_over_the_size_bound() {
        let dir = tempfile::tempdir().unwrap();
        // Deliberately one byte over `MAX_DESKTOP_FILE_BYTES`, not just
        // "big" — this and the boundary test below straddle the `>`
        // comparison so a regression to `>=` (which would wrongly reject a
        // file landing exactly on the bound) is caught by the other test,
        // not silently passed by this one alone.
        let header = "[Desktop Entry]\nName=Huge\nExec=huge\n";
        let padding = "#".repeat(MAX_DESKTOP_FILE_BYTES as usize + 1 - header.len());
        let content = format!("{header}{padding}");
        assert_eq!(content.len() as u64, MAX_DESKTOP_FILE_BYTES + 1);
        fs::write(dir.path().join("huge.desktop"), content).unwrap();
        write_entry(dir.path(), "normal.desktop", "Normal");

        let entries = scan_apps(&[dir.path().to_path_buf()]);
        let ids: Vec<&str> = entries.iter().map(|e| e.app_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["normal"],
            "a file over the size bound must be skipped while a normal file beside it is still indexed"
        );
    }

    #[test]
    fn a_file_landing_exactly_on_the_size_bound_is_still_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let header = "[Desktop Entry]\nName=Boundary\nExec=boundary\n";
        let padding = "#".repeat(MAX_DESKTOP_FILE_BYTES as usize - header.len());
        let content = format!("{header}{padding}");
        assert_eq!(content.len() as u64, MAX_DESKTOP_FILE_BYTES);
        fs::write(dir.path().join("boundary.desktop"), content).unwrap();

        let entries = scan_apps(&[dir.path().to_path_buf()]);
        assert_eq!(
            entries.len(),
            1,
            "a file landing exactly on the bound must still be read, not skipped"
        );
    }

    #[test]
    fn scan_apps_skips_a_symlink_to_a_special_file_without_reading_it() {
        // `/dev/zero` is an infinite stream of zero bytes reporting
        // `st_size == 0`, so the size check alone would never catch it —
        // what actually skips it is `metadata.is_file()`, which is false for
        // a character device even when reached through a symlink. If
        // `scan_apps` ever read it instead of skipping it,
        // `std::fs::read_to_string` would never return: `/dev/zero` has no
        // EOF. This pins Linux special-file behavior the crate already
        // assumes elsewhere (it depends on `inotify`), so it is skipped
        // rather than failed on a system without `/dev/zero`.
        let special = Path::new("/dev/zero");
        if !special.exists() {
            eprintln!("skipping: /dev/zero not present on this system");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(special, dir.path().join("evil.desktop")).unwrap();
        write_entry(dir.path(), "normal.desktop", "Normal");

        let entries = scan_apps(&[dir.path().to_path_buf()]);
        let ids: Vec<&str> = entries.iter().map(|e| e.app_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["normal"],
            "a symlink to a special file must be skipped without being read"
        );
    }

    #[test]
    fn the_first_root_wins_when_two_roots_offer_the_same_app_id() {
        let user_dir = tempfile::tempdir().unwrap();
        let system_dir = tempfile::tempdir().unwrap();
        write_entry(user_dir.path(), "app.desktop", "User Override");
        write_entry(system_dir.path(), "app.desktop", "System Default");

        let entries = scan_apps(&[
            user_dir.path().to_path_buf(),
            system_dir.path().to_path_buf(),
        ]);
        assert_eq!(
            entries.len(),
            1,
            "the same app id from two roots must not duplicate"
        );
        assert_eq!(
            entries[0].item.title, "User Override",
            "the first root in the list must win — this is what makes root order a real precedence rule"
        );
    }

    #[test]
    fn a_hidden_entry_on_disk_does_not_appear_in_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("hidden.desktop"),
            "[Desktop Entry]\nName=Hidden\nExec=hidden\nHidden=true\n",
        )
        .unwrap();

        assert!(scan_apps(&[dir.path().to_path_buf()]).is_empty());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // --- Ported from the salvaged Rust parser's own test module. ---

    #[test]
    fn parses_a_basic_desktop_entry() {
        let parsed = parse_desktop_entry(
            "[Desktop Entry]\nName=Firefox\nExec=firefox %u\nIcon=firefox\nKeywords=browser;web;\n",
        )
        .expect("desktop entry parses");
        assert_eq!(parsed.title, "Firefox");
        assert_eq!(parsed.exec, "firefox");
        assert_eq!(parsed.icon.as_deref(), Some("firefox"));
        assert!(parsed.haystack.contains("browser"));
    }

    #[test]
    fn hidden_and_no_display_entries_are_skipped() {
        assert!(
            parse_desktop_entry("[Desktop Entry]\nName=Hidden\nExec=hidden\nHidden=true\n")
                .is_none()
        );
        assert!(
            parse_desktop_entry("[Desktop Entry]\nName=NoDisp\nExec=nodisp\nNoDisplay=true\n")
                .is_none()
        );
    }

    #[test]
    fn falls_back_to_a_localized_name_when_the_primary_is_missing() {
        let parsed = parse_desktop_entry(
            "[Desktop Entry]\nName[en_US]=Localized App\nExec=localized-app %U\nType=Application\n",
        )
        .expect("desktop entry parses");
        assert_eq!(parsed.title, "Localized App");
        assert!(parsed.haystack.contains("localized-app"));
        assert!(
            !parsed.haystack.contains('%'),
            "field codes must not survive into the haystack"
        );
    }

    // --- New coverage: rules the salvaged suite didn't exercise. ---

    #[test]
    fn an_entry_with_no_name_at_all_is_skipped() {
        assert!(parse_desktop_entry("[Desktop Entry]\nExec=nothing\n").is_none());
    }

    #[test]
    fn content_outside_the_desktop_entry_group_is_ignored() {
        // A mutation that dropped the `in_desktop_entry` gate would pick up
        // this second group's Name= instead of leaving the file nameless.
        let parsed = parse_desktop_entry(
            "[Desktop Entry]\nExec=real\n[Desktop Action new-window]\nName=New Window\n",
        );
        assert!(
            parsed.is_none(),
            "a Name= outside [Desktop Entry] must not count"
        );
    }

    #[test]
    fn field_codes_are_stripped_from_exec() {
        let parsed =
            parse_desktop_entry("[Desktop Entry]\nName=X\nExec=app %f %U --flag %i\n").unwrap();
        assert_eq!(parsed.exec, "app --flag");
    }

    #[test]
    fn an_overlong_name_is_truncated_at_a_char_boundary() {
        // "字" is three bytes, and MAX_TITLE (1024) is not a multiple of 3
        // (1024 = 3*341 + 1), so a raw `s[..MAX_TITLE]` slice would land one
        // byte inside the 342nd character (bytes 1023..1026) rather than on
        // a boundary — which panics outright, since `str` indexing refuses a
        // non-boundary cut. A two-byte character like the previous "é" was
        // wrong for this: MAX_TITLE is even, so byte 1024 is a boundary for
        // *any* run of 2-byte characters regardless of whether the walk-back
        // loop runs at all, which is exactly the "boundary the test can't
        // reach" bug this project has shipped before. `assert_eq!` on the
        // exact walked-back length, not just `<= MAX_TITLE`, so a walk-back
        // that stops one character early or late is also caught, not only
        // one that panics or splits a character.
        let long_name = "字".repeat(MAX_TITLE);
        let parsed =
            parse_desktop_entry(&format!("[Desktop Entry]\nName={long_name}\nExec=x\n")).unwrap();
        assert_eq!(
            parsed.title.len(),
            1023,
            "must walk back to the boundary just below MAX_TITLE, not truncate mid-character"
        );
        assert!(
            std::str::from_utf8(parsed.title.as_bytes()).is_ok(),
            "truncation must not split a multi-byte character"
        );
    }

    #[test]
    fn truncate_to_byte_boundary_walks_back_from_a_mid_character_cut() {
        // Exercises the walk-back loop directly, with a `max` chosen
        // independently of MAX_TITLE's parity so this doesn't depend on that
        // constant ever staying not-a-multiple-of-3. "€" is three bytes, so
        // boundaries fall only at multiples of 3 (0, 3, 6, 9, 12, 15); a
        // `max` of 7 lands inside the third character (bytes 6..9). A
        // mutation that deleted the walk-back (`s[..max]` directly) would
        // panic here rather than compile-time fail, since byte 7 is not a
        // valid `str` slice point; a mutation that walked back too far (or
        // not far enough) would still slice cleanly but return the wrong
        // number of characters, which the exact-match `assert_eq!` catches.
        let s = "€".repeat(5);
        assert_eq!(truncate_to_byte_boundary(&s, 7), "€€");
    }

    #[test]
    fn app_id_from_file_name_strips_the_desktop_suffix() {
        assert_eq!(
            app_id_from_file_name("firefox.desktop").as_deref(),
            Some("firefox")
        );
        assert_eq!(
            app_id_from_file_name("org.gnome.Terminal.desktop").as_deref(),
            Some("org.gnome.Terminal")
        );
        assert_eq!(app_id_from_file_name("not-a-desktop-file.txt"), None);
    }

    // --- build_entry: the Item construction and icon-arm split. ---

    #[test]
    fn build_entry_sets_the_apps_provider_id_and_the_app_prefixed_item_id() {
        let parsed = parse_desktop_entry("[Desktop Entry]\nName=Firefox\nExec=firefox\n").unwrap();
        let entry = build_entry("firefox".to_string(), parsed).unwrap();
        assert_eq!(entry.item.id.as_str(), "app:firefox");
        assert_eq!(entry.item.provider, hop_core::provider::APPS_PROVIDER_ID);
        assert_eq!(entry.item.kind, Kind::App);
    }

    #[test]
    fn build_entry_carries_app_id_exec_and_haystack_onto_the_app_entry() {
        // `AppEntry`'s own fields exist for the index and launch path Task 2
        // and Task 4 add, not for anything already on `Item` — a mutation
        // that dropped `app_id`, `parsed.exec` or `parsed.haystack` from
        // `build_entry`'s `Some(AppEntry { ... })` (or filled one from the
        // wrong local) would leave `Item` looking correct while these three
        // carried nothing, or the wrong value.
        let parsed =
            parse_desktop_entry("[Desktop Entry]\nName=Firefox\nExec=firefox --new-window\n")
                .unwrap();
        let entry = build_entry("firefox".to_string(), parsed).unwrap();
        assert_eq!(entry.app_id, "firefox");
        assert_eq!(entry.exec, "firefox --new-window");
        assert!(entry.haystack.contains("firefox"));
    }

    #[test]
    fn a_slash_prefixed_icon_becomes_the_path_arm() {
        let parsed =
            parse_desktop_entry("[Desktop Entry]\nName=X\nExec=x\nIcon=/usr/share/pixmaps/x.png\n")
                .unwrap();
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert!(matches!(entry.item.icon, Some(IconSpec::Path(_))));
    }

    #[test]
    fn a_bare_icon_name_becomes_the_name_arm() {
        let parsed =
            parse_desktop_entry("[Desktop Entry]\nName=X\nExec=x\nIcon=utilities-terminal\n")
                .unwrap();
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert!(matches!(entry.item.icon, Some(IconSpec::Name(_))));
    }

    #[test]
    fn a_missing_icon_line_produces_no_icon() {
        let parsed = parse_desktop_entry("[Desktop Entry]\nName=X\nExec=x\n").unwrap();
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert_eq!(entry.item.icon, None);
    }

    #[test]
    fn an_icon_name_that_fails_its_own_rule_falls_back_to_no_icon_rather_than_dropping_the_item() {
        // A name carrying a control character is refused by `IconName::new`
        // (see `hop-protocol::content`). A mutation that instead propagated
        // that failure with `?` would drop the whole entry over one bad
        // line; this test catches that.
        let parsed =
            parse_desktop_entry("[Desktop Entry]\nName=X\nExec=x\nIcon=bad\u{1b}name\n").unwrap();
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert_eq!(entry.item.icon, None);
        assert_eq!(entry.item.title, "X", "the item itself must still be built");
    }

    #[test]
    fn every_entry_carries_exactly_one_open_action_agreeing_with_default_action() {
        let parsed = parse_desktop_entry("[Desktop Entry]\nName=X\nExec=x\n").unwrap();
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert_eq!(entry.item.actions.len(), 1);
        assert_eq!(entry.item.actions[0].id, entry.item.default_action);
    }
}

use std::sync::RwLock;

/// The most items [`AppIndex::query`] returns in one answer. Not a ranking
/// cap and not `hop_protocol::limits::MAX_ITEMS_PER_RESULTS_FRAME` (1 000) —
/// it is a per-provider answer-size bound, deliberately matching the daemon's
/// own display cap `MAX_RESULTS` (50), which issue #103 now applies across
/// the whole assembled set. Emitting at most that many per answer means the
/// apps provider never hands assembly a batch larger than what a user can
/// see, and never floods one query with an oversized unranked batch.
pub(crate) const QUERY_RESULT_CAP: usize = 50;

/// The apps provider's in-memory index: an [`AppEntry`] list a background
/// watcher (Task 6) keeps current, queried with no disk access at all.
///
/// # No disk read on the query path
///
/// [`AppIndex::query`]'s signature takes and returns nothing capable of
/// naming a filesystem path, and its body is a lock acquisition over an
/// already-resident `Vec` followed by `filter`/`take`/`clone` — nothing in
/// its call graph reaches `std::fs`. That guarantee is already complete at
/// the type level: `query` has no field or parameter through which a path
/// could arrive. `index_tests::query_still_answers_after_the_backing_
/// directory_is_deleted` below does not strengthen that proof — nothing in
/// the current body can make its pre- and post-deletion assertions diverge
/// — but it stands as a regression trap: if some future change gave
/// `AppIndex` a stored path or root list and wired `query` to consult it,
/// this is the test that would start failing.
pub struct AppIndex {
    entries: RwLock<Vec<AppEntry>>,
}

impl AppIndex {
    /// Wraps an already-scanned entry list in the lock `query` and `replace`
    /// share. Takes the `Vec` directly rather than the roots it came from —
    /// see the module's "no disk read on the query path" contract above —
    /// so the caller decides when [`scan_apps`] runs (at startup, and again
    /// on every filesystem event) rather than this constructor ever doing
    /// disk I/O itself.
    pub fn new(entries: Vec<AppEntry>) -> Self {
        AppIndex {
            entries: RwLock::new(entries),
        }
    }

    /// Filters the index to entries whose haystack contains `term`
    /// (case-insensitive substring match), capped at [`QUERY_RESULT_CAP`].
    /// An empty (or whitespace-only) `term` matches everything, capped the
    /// same way — the empty-query "browse installed apps" case.
    pub(crate) fn query(&self, term: &str) -> Vec<Item> {
        let term = term.trim().to_lowercase();
        let entries = self
            .entries
            .read()
            .expect("no thread panics while holding this lock");
        entries
            .iter()
            .filter(|e| term.is_empty() || e.haystack.contains(&term))
            .take(QUERY_RESULT_CAP)
            .map(|e| e.item.clone())
            .collect()
    }

    /// The full [`AppEntry`] (with its `exec` command, which [`Item`] does
    /// not carry) for the entry whose item id is `id`, or `None` if no
    /// currently-indexed app has it — including "it did, but was
    /// uninstalled since the query that returned it," which `AppsProvider::
    /// execute` (Task 5) treats as an ordinary failure, not a panic.
    pub(crate) fn find_by_item_id(&self, id: &ItemId) -> Option<AppEntry> {
        self.entries
            .read()
            .expect("no thread panics while holding this lock")
            .iter()
            .find(|e| &e.item.id == id)
            .cloned()
    }

    /// Atomically swaps in a freshly-scanned entry list. The only writer;
    /// called once at startup (via [`AppIndex::new`]) and once per
    /// filesystem-change notification thereafter (Task 6) — never from the
    /// query path.
    pub(crate) fn replace(&self, entries: Vec<AppEntry>) {
        *self
            .entries
            .write()
            .expect("no thread panics while holding this lock") = entries;
    }
}

#[cfg(test)]
mod index_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn entry(app_id: &str, title: &str) -> AppEntry {
        let parsed = parse_desktop_entry(&format!(
            "[Desktop Entry]\nName={title}\nExec={app_id}\nKeywords=browser;\n"
        ))
        .unwrap();
        build_entry(app_id.to_string(), parsed).unwrap()
    }

    #[test]
    fn query_matches_the_title_case_insensitively() {
        let index = AppIndex::new(vec![entry("firefox", "Firefox"), entry("files", "Files")]);
        let items = index.query("FIRE");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Firefox");
    }

    #[test]
    fn query_matches_keywords_not_just_the_title() {
        let index = AppIndex::new(vec![entry("firefox", "Firefox")]);
        assert_eq!(
            index.query("browser").len(),
            1,
            "haystack includes Keywords="
        );
    }

    #[test]
    fn an_empty_term_returns_everything_up_to_the_cap() {
        let index = AppIndex::new(vec![entry("a", "A"), entry("b", "B")]);
        assert_eq!(index.query("").len(), 2);
        assert_eq!(index.query("   ").len(), 2, "whitespace-only is also empty");
    }

    #[test]
    fn a_non_matching_term_returns_nothing() {
        let index = AppIndex::new(vec![entry("firefox", "Firefox")]);
        assert!(index.query("nonexistent-app-xyz").is_empty());
    }

    #[test]
    fn results_are_capped_at_query_result_cap() {
        let entries: Vec<_> = (0..QUERY_RESULT_CAP + 10)
            .map(|n| entry(&format!("app{n}"), &format!("App {n}")))
            .collect();
        let index = AppIndex::new(entries);
        assert_eq!(index.query("").len(), QUERY_RESULT_CAP);
    }

    #[test]
    fn find_by_item_id_locates_the_matching_entry_and_carries_its_exec() {
        let index = AppIndex::new(vec![entry("firefox", "Firefox")]);
        let found = index
            .find_by_item_id(&ItemId::new("app:firefox").unwrap())
            .expect("must find the entry it was built from");
        assert_eq!(found.exec, "firefox");
    }

    #[test]
    fn find_by_item_id_returns_none_for_an_id_not_in_the_index() {
        let index = AppIndex::new(vec![entry("firefox", "Firefox")]);
        assert!(
            index
                .find_by_item_id(&ItemId::new("app:not-installed").unwrap())
                .is_none()
        );
    }

    #[test]
    fn replace_swaps_the_whole_set_and_query_sees_it_immediately() {
        let index = AppIndex::new(vec![entry("old", "Old")]);
        assert_eq!(index.query("").len(), 1);
        index.replace(vec![entry("new-a", "New A"), entry("new-b", "New B")]);
        let items = index.query("");
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.title.starts_with("New")));
    }

    #[test]
    fn query_still_answers_after_the_backing_directory_is_deleted() {
        // The runtime half of "no disk read on the query path": build an
        // index from a real scan, delete the directory it was scanned from
        // entirely, then query. If `query` touched disk anywhere in its
        // path, this would either error or return something different —
        // asserting the answer is unchanged is what a regression that
        // routed `query` back through a fresh `scan_apps` call would fail.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("firefox.desktop"),
            "[Desktop Entry]\nName=Firefox\nExec=firefox\n",
        )
        .unwrap();
        let index = AppIndex::new(scan_apps(&[dir.path().to_path_buf()]));
        assert_eq!(index.query("firefox").len(), 1, "sanity: the scan found it");

        std::fs::remove_dir_all(dir.path()).unwrap();

        let items = index.query("firefox");
        assert_eq!(
            items.len(),
            1,
            "query must still answer from memory once the backing directory is gone"
        );
        assert_eq!(items[0].title, "Firefox");
    }
}

/// One open window, as much as this M2 slice can describe before the M5
/// GNOME shim (design spec §7) supplies real ones from the compositor.
/// Ported from `appLaunch.js`'s window shape, collapsed to the fields that
/// logic actually reads — see this plan's Design decision 4 for the two
/// fields deliberately not here (a focus-stealing-prevention timestamp,
/// and the method-vs-property duck-typing `skip_taskbar` had in JS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowHandle {
    pub(crate) id: String,
    /// Compared against a desktop entry's `app_id`, case-insensitively and
    /// with a trailing `.desktop` ignored on either side — ported from
    /// `appLaunch.js`'s `normalizeToken`. `None` when the compositor could
    /// not associate this window with an application at all.
    pub(crate) app_id: Option<String>,
    pub(crate) skip_taskbar: bool,
    pub(crate) minimized: bool,
    pub(crate) override_redirect: bool,
}

/// The window backend `focus_or_launch` dispatches through. Two list
/// methods, mirroring `appLaunch.js`'s two-tier lookup — see Design
/// decision 4 for why collapsing them to one would break half the ported
/// test suite.
pub trait WindowSource: Send + Sync + 'static {
    /// Windows the app itself is known to own — ported from
    /// `app.get_windows()`. No id-matching is needed for anything this
    /// returns: ownership already establishes it.
    fn windows_for_app(&self, app_id: &str) -> Vec<WindowHandle>;
    /// Every open window in the session — ported from
    /// `global.display.get_tab_list()` — for the fallback heuristic used
    /// only when `windows_for_app` came back empty.
    fn all_windows(&self) -> Vec<WindowHandle>;
    /// Restores a minimized window so it can be activated — ported from
    /// `appLaunch.js`'s `window.unminimize()`. [`focus_or_launch`] calls
    /// this before `activate` when `window.minimized` is set; a compositor
    /// backend that skipped it would leave the window restored-but-hidden
    /// behind whatever activation alone does on that desktop.
    fn unminimize(&self, window: &WindowHandle);
    /// Raises and focuses `window` — ported from `appLaunch.js`'s
    /// `window.activate()`, the terminal step of the "existing window
    /// found" branch of [`focus_or_launch`].
    fn activate(&self, window: &WindowHandle);
}

/// The M2 [`WindowSource`]: no windows exist yet, from either tier. This is
/// what makes [`focus_or_launch`] correctly and unconditionally launch
/// until the M5 GNOME shim replaces this with a real implementation — see
/// Design decision 4.
pub struct EmptyWindowSource;

impl WindowSource for EmptyWindowSource {
    fn windows_for_app(&self, _app_id: &str) -> Vec<WindowHandle> {
        Vec::new()
    }
    fn all_windows(&self) -> Vec<WindowHandle> {
        Vec::new()
    }
    fn unminimize(&self, _window: &WindowHandle) {}
    fn activate(&self, _window: &WindowHandle) {}
}

/// Starts a new process for a desktop entry's `Exec=` command.
pub trait Launcher: Send + Sync + 'static {
    /// Spawns `exec` (already split from the desktop entry's `Exec=` line,
    /// field codes stripped) as a new, detached process. The
    /// [`focus_or_launch`] fallback once no focusable window exists — the
    /// seam that lets tests substitute a fake that records a call instead
    /// of actually starting a GUI application.
    fn launch(&self, exec: &str) -> Result<(), String>;
}

/// The real [`Launcher`]: `exec`'s first whitespace-separated token is the
/// program, the rest are its arguments — `exec` has already had field codes
/// stripped by [`sanitize_exec`] at parse time. Standard streams are
/// discarded and detached from the daemon's own terminal, if it has one; a
/// launched app is not expected to write anything hopd should see.
pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn launch(&self, exec: &str) -> Result<(), String> {
        let mut parts = exec.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| "desktop entry has an empty Exec= command".to_string())?;
        std::process::Command::new(program)
            .args(parts)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_child| ())
            .map_err(|err| format!("could not launch {program}: {err}"))
    }
}

/// A window this app can be focused through: not `skip_taskbar`, not
/// `override_redirect` — ported from `appLaunch.js`'s `canUseWindow`, minus
/// the "has an `activate` method" check, which every [`WindowHandle`]
/// trivially satisfies by having a [`WindowSource`] behind it.
fn is_focusable(window: &WindowHandle) -> bool {
    !window.skip_taskbar && !window.override_redirect
}

/// Trims, lowercases, and drops a trailing `.desktop` — ported from
/// `appLaunch.js`'s `normalizeToken`.
fn normalize_app_token(value: &str) -> String {
    let value = value.trim().to_lowercase();
    value
        .strip_suffix(".desktop")
        .map(str::to_string)
        .unwrap_or(value)
}

/// Whether `window` belongs to the app named `app_id`, normalized — ported
/// from `appLaunch.js`'s `windowMatchesApp`, minus the JS version's
/// alternate-name and alternate-executable comparisons, which existed there
/// because GNOME `Shell.App` exposes several names for the same app; this
/// side's index has exactly one id per app.
fn window_matches_app(window: &WindowHandle, app_id: &str) -> bool {
    let Some(window_app_id) = &window.app_id else {
        return false;
    };
    normalize_app_token(window_app_id) == normalize_app_token(app_id)
}

/// The first focusable window belonging to `app_id`, checking the app's own
/// window list before falling back to a full-session scan matched by id —
/// ported from `appLaunch.js`'s `focusExistingAppWindow`.
fn find_focusable_window(windows: &dyn WindowSource, app_id: &str) -> Option<WindowHandle> {
    if let Some(window) = windows
        .windows_for_app(app_id)
        .into_iter()
        .find(is_focusable)
    {
        return Some(window);
    }
    windows
        .all_windows()
        .into_iter()
        .find(|w| is_focusable(w) && window_matches_app(w, app_id))
}

/// Focuses an existing window for `app_id` if one is focusable, unminimizing
/// it first if needed; otherwise launches `exec` as a new process. Ported
/// from `appLaunch.js`'s `launchOrFocusApp` — the behavioral spec this
/// slice's acceptance criteria name — with the divergences recorded in this
/// plan's Design decision 4.
pub(crate) fn focus_or_launch(
    windows: &dyn WindowSource,
    launcher: &dyn Launcher,
    app_id: &str,
    exec: &str,
) -> Result<(), String> {
    if let Some(window) = find_focusable_window(windows, app_id) {
        if window.minimized {
            windows.unminimize(&window);
        }
        windows.activate(&window);
        return Ok(());
    }

    launcher.launch(exec)
}

#[cfg(test)]
mod focus_or_launch_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::sync::Mutex;

    /// A [`WindowSource`] the tests can script and read back — what
    /// `windows_for_app`/`all_windows` answer, and every `unminimize`/
    /// `activate` call it received, in order.
    #[derive(Default)]
    struct FakeWindows {
        for_app: Vec<WindowHandle>,
        all: Vec<WindowHandle>,
        calls: Mutex<Vec<(&'static str, String)>>,
    }

    impl WindowSource for FakeWindows {
        fn windows_for_app(&self, _app_id: &str) -> Vec<WindowHandle> {
            self.for_app.clone()
        }
        fn all_windows(&self) -> Vec<WindowHandle> {
            self.all.clone()
        }
        fn unminimize(&self, window: &WindowHandle) {
            self.calls
                .lock()
                .unwrap()
                .push(("unminimize", window.id.clone()));
        }
        fn activate(&self, window: &WindowHandle) {
            self.calls
                .lock()
                .unwrap()
                .push(("activate", window.id.clone()));
        }
    }

    /// A [`Launcher`] that records whether it was called, never actually
    /// spawning a process — every test in this module must run without a
    /// real GUI application installed.
    #[derive(Default)]
    struct FakeLauncher {
        launched: Mutex<Vec<String>>,
    }

    impl Launcher for FakeLauncher {
        fn launch(&self, exec: &str) -> Result<(), String> {
            self.launched.lock().unwrap().push(exec.to_string());
            Ok(())
        }
    }

    fn window(id: &str) -> WindowHandle {
        WindowHandle {
            id: id.to_string(),
            app_id: None,
            skip_taskbar: false,
            minimized: false,
            override_redirect: false,
        }
    }

    // --- Ported from appLaunch.js's own test suite. ---

    #[test]
    fn prefers_focusing_an_existing_normal_window() {
        let windows = FakeWindows {
            for_app: vec![window("w1")],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();

        assert!(focus_or_launch(&windows, &launcher, "firefox", "firefox").is_ok());
        assert_eq!(
            *windows.calls.lock().unwrap(),
            vec![("activate", "w1".to_string())]
        );
        assert!(launcher.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn restores_and_focuses_a_minimized_existing_window() {
        let mut w = window("w1");
        w.minimized = true;
        let windows = FakeWindows {
            for_app: vec![w],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();

        assert!(focus_or_launch(&windows, &launcher, "firefox", "firefox").is_ok());
        assert_eq!(
            *windows.calls.lock().unwrap(),
            vec![
                ("unminimize", "w1".to_string()),
                ("activate", "w1".to_string())
            ],
            "unminimize must happen, and it must happen before activate"
        );
    }

    #[test]
    fn a_non_minimized_window_is_never_unminimized() {
        // The mutation this guards: dropping the `if window.minimized`
        // guard and always calling `unminimize` would still pass the two
        // tests above (an extra harmless call on an already-visible
        // window) but is wrong — this is the test that catches it.
        let windows = FakeWindows {
            for_app: vec![window("w1")],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();
        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert_eq!(
            *windows.calls.lock().unwrap(),
            vec![("activate", "w1".to_string())]
        );
    }

    #[test]
    fn falls_back_to_launching_when_no_focusable_window_exists() {
        // Represents the JS suite's three-rung launch fallback
        // (activate/open_new_window/launch/appInfo.launch), collapsed to
        // one Launcher call — see Design decision 4.
        let windows = FakeWindows::default();
        let launcher = FakeLauncher::default();

        assert!(focus_or_launch(&windows, &launcher, "firefox", "firefox --new-window").is_ok());
        assert_eq!(
            *launcher.launched.lock().unwrap(),
            vec!["firefox --new-window".to_string()]
        );
        assert!(windows.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn focuses_a_matching_open_window_when_the_apps_own_window_list_is_empty() {
        let mut w = window("w1");
        w.app_id = Some("brave-browser".to_string());
        let windows = FakeWindows {
            for_app: vec![],
            all: vec![w],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();

        assert!(focus_or_launch(&windows, &launcher, "brave-browser.desktop", "brave").is_ok());
        assert_eq!(
            *windows.calls.lock().unwrap(),
            vec![("activate", "w1".to_string())]
        );
        assert!(
            launcher.launched.lock().unwrap().is_empty(),
            "a matching window must be focused, not launched past"
        );
    }

    // --- New coverage: the branches the JS suite didn't isolate. ---

    #[test]
    fn a_skip_taskbar_window_is_not_focusable_and_falls_through_to_launch() {
        let mut w = window("w1");
        w.skip_taskbar = true;
        let windows = FakeWindows {
            for_app: vec![w],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();

        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert!(
            windows.calls.lock().unwrap().is_empty(),
            "skip_taskbar window must not be used"
        );
        assert_eq!(launcher.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_override_redirect_window_is_not_focusable_and_falls_through_to_launch() {
        let mut w = window("w1");
        w.override_redirect = true;
        let windows = FakeWindows {
            for_app: vec![w],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();

        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert!(windows.calls.lock().unwrap().is_empty());
        assert_eq!(launcher.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn falls_through_to_tier_two_when_tier_one_is_non_empty_but_entirely_unfocusable() {
        // Closes a coverage gap: a plausible-but-wrong `find_focusable_window`
        // that short-circuits on "is `windows_for_app`'s *raw* list
        // non-empty" (rather than "did filtering it for focusability find
        // something") would wrongly stop here and never consult tier 2 at
        // all. Every other test with a non-empty `for_app` also has an
        // empty `all`, so that bug is invisible to them — this fixture
        // makes tier 1 non-empty-but-unfocusable *and* gives tier 2 a
        // genuine focusable match, so the two implementations diverge.
        let mut unfocusable = window("w1");
        unfocusable.skip_taskbar = true;
        let mut matching = window("w2");
        matching.app_id = Some("firefox".to_string());
        let windows = FakeWindows {
            for_app: vec![unfocusable],
            all: vec![matching],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();

        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert_eq!(
            *windows.calls.lock().unwrap(),
            vec![("activate", "w2".to_string())],
            "tier 1 being non-empty-but-unfocusable must not block the tier 2 fallback"
        );
        assert!(launcher.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn tier_one_wins_over_tier_two_when_both_have_a_candidate() {
        // Both windows carry an `app_id` that matches — not just the
        // `for_app` one — so a mutation that checked `all_windows` before
        // `windows_for_app` would find a *legitimate* match there too (not
        // fall through on a missing id) and activate "scanned" instead.
        // Giving only the `for_app` window an id (as `window()`'s default
        // leaves it) would let a full tier-order swap hide behind the tier
        // 2 id check legitimately failing — this fixture closes that gap.
        let mut owned = window("owned");
        owned.app_id = Some("firefox".to_string());
        let mut scanned = window("scanned");
        scanned.app_id = Some("firefox".to_string());
        let windows = FakeWindows {
            for_app: vec![owned],
            all: vec![scanned],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();
        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert_eq!(
            *windows.calls.lock().unwrap(),
            vec![("activate", "owned".to_string())]
        );
    }

    #[test]
    fn app_id_matching_ignores_case_and_a_trailing_dot_desktop_on_either_side() {
        let mut w = window("w1");
        w.app_id = Some("Org.Gnome.Terminal".to_string());
        let windows = FakeWindows {
            for_app: vec![],
            all: vec![w],
            ..Default::default()
        };
        let launcher = FakeLauncher::default();

        focus_or_launch(
            &windows,
            &launcher,
            "org.gnome.terminal.desktop",
            "gnome-terminal",
        )
        .unwrap();
        assert_eq!(
            *windows.calls.lock().unwrap(),
            vec![("activate", "w1".to_string())]
        );
    }

    #[test]
    fn a_tier_two_window_with_no_app_id_never_matches() {
        let windows = FakeWindows {
            for_app: vec![],
            all: vec![window("w1")], // app_id: None
            ..Default::default()
        };
        let launcher = FakeLauncher::default();
        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert!(windows.calls.lock().unwrap().is_empty());
        assert_eq!(launcher.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn system_launcher_reports_an_empty_exec_rather_than_spawning_nothing() {
        // Asserts on the error *message*, not just `is_err()`: on Linux,
        // `Command::new("").spawn()` already fails at the OS level (empty
        // program name), so `is_err()` alone would pass even with the
        // explicit `ok_or_else` guard deleted from `SystemLauncher::launch`
        // — it would just report a generic OS error instead of this
        // domain-specific one. Checking the message is what actually pins
        // the guard's existence.
        let err = SystemLauncher.launch("").unwrap_err();
        assert!(
            err.contains("empty Exec="),
            "must report the empty-Exec= guard, not a generic spawn failure: {err}"
        );
        let err = SystemLauncher.launch("   ").unwrap_err();
        assert!(
            err.contains("empty Exec="),
            "whitespace-only must also trip the guard: {err}"
        );
    }

    #[test]
    fn empty_window_source_answers_nothing_from_either_tier() {
        // Pins the M2 production default's whole contract: until the M5
        // GNOME shim replaces it, focus_or_launch must always launch.
        let source = EmptyWindowSource;
        assert!(source.windows_for_app("anything").is_empty());
        assert!(source.all_windows().is_empty());

        let launcher = FakeLauncher::default();
        focus_or_launch(&source, &launcher, "firefox", "firefox").unwrap();
        assert_eq!(launcher.launched.lock().unwrap().len(), 1);
    }
}

use std::sync::Arc;

use hop_core::provider::{APPS_PROVIDER_ID, Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery};

/// The apps provider: answers a query from [`AppIndex`] with no disk
/// access, and dispatches `execute` through [`focus_or_launch`].
pub struct AppsProvider {
    index: Arc<AppIndex>,
    windows: Arc<dyn WindowSource>,
    launcher: Arc<dyn Launcher>,
}

impl AppsProvider {
    /// Wires an already-built index to the window and launch backends
    /// `execute` dispatches through. Kept separate from any single
    /// constructor that also builds the index (compare
    /// [`build_apps_provider`]) so tests can hand it a fixture `AppIndex`
    /// with no filesystem or watcher involved at all.
    pub fn new(
        index: Arc<AppIndex>,
        windows: Arc<dyn WindowSource>,
        launcher: Arc<dyn Launcher>,
    ) -> Self {
        AppsProvider {
            index,
            windows,
            launcher,
        }
    }
}

impl Provider for AppsProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            // Must be the shared constant, never a hand-written literal —
            // see this plan's Scope section and the issue's own first
            // comment. `hop_core::provider::APPS_PROVIDER_ID`'s own docs
            // spell out the silent failure a drift here would cause: every
            // configured app alias would stop boosting anything, with no
            // test failing.
            id: APPS_PROVIDER_ID,
            kinds: vec![Kind::App],
            // Mode::All so this provider is asked for ordinary, unprefixed
            // search — a provider that omits it is never reached by a plain
            // keystroke (see ProviderManifest::modes's own docs). Mode::Apps
            // is the `a ` exclusive prefix.
            modes: vec![Mode::All, Mode::Apps],
            min_term_len: 0,
            budget: std::time::Duration::from_millis(5),
        }
    }

    async fn query(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(self.index.query(&q.term))
    }

    async fn execute(
        self: Arc<Self>,
        item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<hop_protocol::ExecOutcome, ProviderError> {
        let Some(entry) = self.index.find_by_item_id(&item_id) else {
            return Err(ProviderError::Failed(format!(
                "{} is no longer installed",
                item_id.as_str()
            )));
        };

        focus_or_launch(&*self.windows, &*self.launcher, &entry.app_id, &entry.exec)
            .map(|()| hop_protocol::ExecOutcome::Done)
            .map_err(ProviderError::Failed)
    }
}

#[cfg(test)]
mod provider_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hop_core::pipeline::{CheckedItems, ProviderOutput};
    use hop_core::router::route;
    use std::sync::Mutex;

    fn one_app_provider(title: &str) -> AppsProvider {
        let parsed =
            parse_desktop_entry(&format!("[Desktop Entry]\nName={title}\nExec=x\n")).unwrap();
        let entry = build_entry("x".to_string(), parsed).unwrap();
        AppsProvider::new(
            Arc::new(AppIndex::new(vec![entry])),
            Arc::new(EmptyWindowSource),
            Arc::new(SystemLauncher),
        )
    }

    // --- Manifest correctness — the issue's load-bearing addition. ---

    #[test]
    fn the_manifest_uses_the_shared_apps_provider_id_constant() {
        assert_eq!(one_app_provider("X").manifest().id, APPS_PROVIDER_ID);
    }

    #[test]
    fn the_manifest_declares_mode_all_and_mode_apps() {
        let modes = one_app_provider("X").manifest().modes;
        assert!(
            modes.contains(&Mode::All),
            "omitting Mode::All means never reached by a plain keystroke"
        );
        assert!(
            modes.contains(&Mode::Apps),
            "omitting Mode::Apps means `a <term>` never reaches this provider"
        );
    }

    #[test]
    fn the_manifest_declares_kind_app_and_a_minimum_term_length() {
        let manifest = one_app_provider("X").manifest();
        assert_eq!(manifest.kinds, vec![Kind::App]);
        assert_eq!(
            manifest.min_term_len, 0,
            "0 means \"always run\", including for the empty term"
        );
    }

    #[tokio::test]
    async fn the_providers_own_output_passes_its_own_manifest_checks() {
        // Pinned exactly as the issue's first comment asks: run the
        // provider's own output through CheckedItems::check. A manifest/item
        // mismatch here means every item this provider ever returns is
        // silently dropped at assembly, with no test elsewhere failing.
        let provider = Arc::new(one_app_provider("Firefox"));
        let routed = Arc::new(route("firefox"));
        let ctx = QueryCtx {
            cancel: hop_core::provider::CancellationFlag::default(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(1),
        };
        let items = provider.clone().query(routed, ctx).await.unwrap();
        assert_eq!(items.len(), 1, "the fixture must actually produce an item");

        let checked = CheckedItems::check(vec![ProviderOutput::from_provider(&*provider, items)]);
        assert_eq!(
            checked.rejections(),
            &[],
            "the apps provider's own honest output must survive its own manifest"
        );
        assert_eq!(checked.items().len(), 1);
    }

    #[test]
    fn item_ids_are_app_colon_app_id_matching_what_the_alias_table_synthesizes() {
        // hop_core::aliases synthesizes `app:<appId>` by pure string
        // construction with no way to ask this provider what it would have
        // produced — so the two must already agree.
        let parsed = parse_desktop_entry("[Desktop Entry]\nName=Terminal\nExec=t\n").unwrap();
        let entry = build_entry("org.gnome.Terminal".to_string(), parsed).unwrap();
        assert_eq!(entry.item.id.as_str(), "app:org.gnome.Terminal");
    }

    // --- Registration and scheduling through a real ProviderHost. ---

    #[test]
    fn registered_with_a_real_host_the_provider_is_selected_for_an_ordinary_and_an_a_prefixed_query()
     {
        let mut host = hop_core::host::ProviderHost::with_log(Arc::new(hop_core::host::NoopLog));
        host.register(one_app_provider("Firefox")).unwrap();
        // No public "selected_ids" outside hop-core's own tests, so this
        // observes selection through manifests() plus should_query directly
        // — the same predicate ProviderHost::selected calls.
        let manifest = &host.manifests()[0];
        assert!(hop_core::provider::should_query(
            manifest,
            &route("firefox")
        ));
        assert!(hop_core::provider::should_query(
            manifest,
            &route("a firefox")
        ));
    }

    // --- query(): the pure in-memory lookup. ---

    #[tokio::test]
    async fn query_returns_items_matching_the_routed_term() {
        // Two entries, not one: with a single-entry index, "filtered
        // correctly on `fire`" and "the routed term was dropped and the
        // index was queried with an empty string" both answer with the same
        // one item, so a mutation deleting `&q.term` in favor of `""` would
        // pass undetected. A second entry that does not match "fire" makes
        // the two behaviors diverge (1 item vs. 2), so this actually pins
        // that `query` uses the routed term rather than ignoring it.
        let firefox = build_entry(
            "firefox".to_string(),
            parse_desktop_entry("[Desktop Entry]\nName=Firefox\nExec=firefox\n").unwrap(),
        )
        .unwrap();
        let terminal = build_entry(
            "terminal".to_string(),
            parse_desktop_entry("[Desktop Entry]\nName=Terminal\nExec=terminal\n").unwrap(),
        )
        .unwrap();
        let provider = Arc::new(AppsProvider::new(
            Arc::new(AppIndex::new(vec![firefox, terminal])),
            Arc::new(EmptyWindowSource),
            Arc::new(SystemLauncher),
        ));
        let ctx = QueryCtx {
            cancel: hop_core::provider::CancellationFlag::default(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(1),
        };
        let items = provider
            .clone()
            .query(Arc::new(route("fire")), ctx)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Firefox");
    }

    // --- execute(): dispatch through focus_or_launch. ---

    struct RecordingLauncher {
        calls: Mutex<Vec<String>>,
    }

    impl Launcher for RecordingLauncher {
        fn launch(&self, exec: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(exec.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_launches_the_apps_command_when_no_window_is_focusable() {
        let parsed =
            parse_desktop_entry("[Desktop Entry]\nName=Firefox\nExec=firefox --new\n").unwrap();
        let entry = build_entry("firefox".to_string(), parsed).unwrap();
        let launcher = Arc::new(RecordingLauncher {
            calls: Mutex::new(Vec::new()),
        });
        let provider = Arc::new(AppsProvider::new(
            Arc::new(AppIndex::new(vec![entry])),
            Arc::new(EmptyWindowSource),
            launcher.clone(),
        ));

        let outcome = provider
            .clone()
            .execute(
                ItemId::new("app:firefox").unwrap(),
                ActionId::new("open").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, hop_protocol::ExecOutcome::Done);
        assert_eq!(
            *launcher.calls.lock().unwrap(),
            vec!["firefox --new".to_string()]
        );
    }

    #[tokio::test]
    async fn execute_on_an_id_no_longer_in_the_index_fails_rather_than_panicking() {
        let provider = Arc::new(one_app_provider("Firefox"));
        let result = provider
            .clone()
            .execute(
                ItemId::new("app:uninstalled-since-the-query").unwrap(),
                ActionId::new("open").unwrap(),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Failed(_))));
    }
}

use std::io;

use inotify::{Inotify, WatchMask};

/// The inotify events worth rebuilding the index over: a file created,
/// removed, finished being written, or moved in or out of a watched
/// directory. `WatchMask::CLOSE_WRITE` rather than `WatchMask::MODIFY` is
/// deliberate — it fires once a writer has actually finished, rather than
/// once per buffered write syscall, so a package manager writing a
/// `.desktop` file in several chunks produces one event instead of several
/// and is never seen half-written.
fn watch_mask() -> WatchMask {
    WatchMask::CREATE
        | WatchMask::DELETE
        | WatchMask::CLOSE_WRITE
        | WatchMask::MOVED_FROM
        | WatchMask::MOVED_TO
}

/// Opens an inotify instance and adds a watch on every path in `roots` that
/// exists and is readable. A root that does not exist (a never-created
/// `~/.icons`, say) is skipped rather than failing the whole watcher,
/// mirroring [`scan_apps`]'s own tolerance for missing roots. Fails only if
/// *no* root could be watched at all.
///
/// **Known gap, accepted rather than fixed here:** a root skipped because it
/// does not exist yet stays unwatched even if something later creates it —
/// inotify has no event for "a directory now exists at a path I never had a
/// watch on," only for changes under a watch that already exists. On a
/// fresh machine with no `~/.local/share/applications` yet, that directory's
/// own creation (which the first user-level app install causes) is
/// therefore invisible until the daemon restarts. Closing it means watching
/// each root's *parent* for the child's own `CREATE` and adding a real watch
/// once it appears — materially larger than this function's fix for
/// issue #57's scan-then-watch race (that fix only reordered two calls; it
/// does not synthesize watches for paths that don't exist yet). Tracked as
/// part of issue #106 rather than fixed in this slice.
fn open_watch(roots: &[PathBuf]) -> io::Result<Inotify> {
    let inotify = Inotify::init()?;
    let mask = watch_mask();

    let mut watched_any = false;
    for root in roots {
        if inotify.watches().add(root, mask).is_ok() {
            watched_any = true;
        }
    }

    if !watched_any {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no application directory could be watched",
        ));
    }

    Ok(inotify)
}

/// Spawns the background thread that keeps `index` current from an
/// already-open `inotify`: an initial build is assumed already done by the
/// caller (`AppIndex::new` from a [`scan_apps`] call), and this thread
/// rebuilds it every time a watched directory changes, forever, until the
/// process exits.
///
/// Takes the opened [`Inotify`] rather than opening one itself — unlike an
/// earlier version of this function — so the caller controls exactly when
/// watches are registered relative to the initial scan. See
/// [`build_watched_index`], the only production caller, for why that
/// ordering matters.
///
/// A dedicated OS thread, blocking on [`Inotify::read_events_blocking`],
/// rather than the crate's tokio-integrated `EventStream` — see this plan's
/// Design decision 5 for the two reasons: `EventStream` needs a live Tokio
/// runtime context at construction, which this function's callers do not
/// uniformly have (Task 8's test harness builds the provider before its
/// runtime exists), and this thread's work — a blocking read, then
/// [`scan_apps`]'s own blocking directory walk — has nothing to gain from
/// running on a Tokio worker thread regardless.
///
/// This deliberately does not inspect which events `read_events_blocking`
/// returned: every event `watch_mask` is configured for provokes the
/// identical response — a full rescan — so there is nothing to gain from
/// knowing which file changed, and the crate's own `Events` iterator is
/// simply drained by letting it drop.
pub fn spawn_index_watcher(mut inotify: Inotify, index: Arc<AppIndex>, roots: Vec<PathBuf>) {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match inotify.read_events_blocking(&mut buffer) {
                Ok(_events) => index.replace(scan_apps(&roots)),
                Err(err) => {
                    eprintln!("hopd: apps provider: desktop-entry watcher stopped: {err}");
                    return;
                }
            }
        }
    });
}

/// [`build_watched_index`]'s actual body, with `after_watch` run at the
/// exact point that matters: after [`open_watch`] has registered every
/// watch it can, before [`scan_apps`] runs the initial scan. Production
/// code always passes a no-op; `watcher_tests` below passes a hook that
/// writes a `.desktop` file right there, which is how
/// `build_watched_index_registers_the_watch_before_the_initial_scan_runs`
/// pins the ordering itself rather than a race that is too narrow to land
/// deterministically from a test.
fn build_watched_index_with_hook(roots: Vec<PathBuf>, after_watch: impl FnOnce()) -> Arc<AppIndex> {
    let watch = open_watch(&roots);
    after_watch();

    let index = Arc::new(AppIndex::new(scan_apps(&roots)));

    match watch {
        Ok(inotify) => spawn_index_watcher(inotify, index.clone(), roots),
        Err(err) => {
            eprintln!("hopd: apps provider: could not watch for desktop-entry changes: {err}");
        }
    }

    index
}

/// Builds an [`AppIndex`] over `roots` and starts the background watcher
/// that keeps it current — the watch-then-scan order that closes issue
/// #57's scan-then-watch race (acceptance criterion 2): [`open_watch`] runs
/// *before* [`scan_apps`], so a `.desktop` file created or removed while the
/// scan is in flight still generates an inotify event, queued in the
/// kernel until the watcher thread this function spawns reads it, rather
/// than escaping notice with no rescan until some unrelated later change
/// happens to trigger one. The previous order — scan, then watch — left
/// exactly that window open with no catch-up rescan on the other side of
/// it.
///
/// If no root in `roots` can be watched at all ([`open_watch`] failing —
/// most commonly every root missing on a fresh machine), this logs and
/// returns an index that is accurate as of this call but will never update
/// again for the life of the process, matching this crate's existing
/// per-provider-isolation posture (`build_host`'s own doc comment: "a
/// daemon that refuses to start over one misconfigured provider is worse
/// than one that serves the rest").
pub fn build_watched_index(roots: Vec<PathBuf>) -> Arc<AppIndex> {
    build_watched_index_with_hook(roots, || {})
}

#[cfg(test)]
mod watcher_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::time::{Duration, Instant};

    /// Polls `index` for up to `timeout`, checking `matches` against a fresh
    /// query each time. A regression here hangs for the full timeout rather
    /// than forever, which is what makes a broken watcher a failed
    /// assertion instead of a stuck CI job.
    fn wait_until(index: &AppIndex, timeout: Duration, matches: impl Fn(&[Item]) -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if matches(&index.query("")) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn open_watch_blocks_until_a_file_is_created_then_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut inotify = open_watch(&[dir.path().to_path_buf()]).unwrap();

        let writer_path = dir.path().join("new.desktop");
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            std::fs::write(writer_path, "[Desktop Entry]\nName=New\nExec=new\n").unwrap();
        });

        // read_events_blocking blocks until the writer's create+close-write
        // lands; if it never did, this call would hang and the test would
        // time out at the harness level rather than failing an assertion —
        // acceptable here because a hang is itself the failure signature a
        // broken watcher produces.
        let mut buffer = [0u8; 4096];
        let events: Vec<_> = inotify.read_events_blocking(&mut buffer).unwrap().collect();
        assert!(
            !events.is_empty(),
            "at least one event must be reported for the new file"
        );
        writer.join().unwrap();
    }

    #[test]
    fn open_watch_fails_over_no_existing_root() {
        let missing = PathBuf::from("/definitely/not/a/real/path/for/this/test");
        assert!(open_watch(&[missing]).is_err());
    }

    #[test]
    fn open_watch_succeeds_with_at_least_one_existing_root_among_several_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let missing = PathBuf::from("/definitely/not/a/real/path/for/this/test");
        assert!(open_watch(&[missing, dir.path().to_path_buf()]).is_ok());
    }

    #[test]
    fn installing_a_desktop_entry_is_reflected_in_the_index_without_rebuilding_it_by_hand() {
        // Acceptance criterion 2, at the AppIndex level: the index reflects
        // a filesystem change with nothing but the watcher thread acting on
        // it — no test code calls `index.replace` or `scan_apps` directly
        // after the watcher is spawned.
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let inotify = open_watch(&roots).unwrap();
        let index = Arc::new(AppIndex::new(scan_apps(&roots)));
        assert!(index.query("").is_empty(), "sanity: nothing installed yet");

        spawn_index_watcher(inotify, index.clone(), roots);

        std::fs::write(
            dir.path().join("newapp.desktop"),
            "[Desktop Entry]\nName=New App\nExec=newapp\n",
        )
        .unwrap();

        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items
                .iter()
                .any(|i| i.title == "New App")),
            "the new entry must appear without the daemon restarting"
        );
    }

    #[test]
    fn removing_a_desktop_entry_is_reflected_in_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let entry_path = dir.path().join("goingaway.desktop");
        std::fs::write(&entry_path, "[Desktop Entry]\nName=Going Away\nExec=x\n").unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let inotify = open_watch(&roots).unwrap();
        let index = Arc::new(AppIndex::new(scan_apps(&roots)));
        assert_eq!(
            index.query("").len(),
            1,
            "sanity: it was indexed at startup"
        );

        spawn_index_watcher(inotify, index.clone(), roots);
        std::fs::remove_file(&entry_path).unwrap();

        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items.is_empty()),
            "the removed entry must disappear without the daemon restarting"
        );
    }

    // --- The scan-then-watch race (issue #57 finding 1, acceptance
    // criterion 2): closed by registering the watch before the initial scan
    // runs, so nothing created or removed while the scan is in flight can
    // land in a gap where no event exists to catch it. ---

    #[test]
    fn build_watched_index_registers_the_watch_before_the_initial_scan_runs() {
        // Pins the ordering `build_watched_index` promises in its doc
        // comment, via `build_watched_index_with_hook`'s test seam rather
        // than the real race: the true production window (between
        // `open_watch` returning and `scan_apps` starting) is microseconds
        // wide and cannot be landed on deterministically from a test, so
        // this drives the hook to fire at exactly that point instead of
        // relying on timing.
        //
        // The mutation this catches: swapping `build_watched_index_with_hook`
        // back to the old scan-then-watch order (or reordering
        // `open_watch`/`after_watch`/`scan_apps` any other way that runs
        // `scan_apps` before the hook). Under that reordering the file this
        // hook writes would not exist yet when `scan_apps` runs, so the
        // returned index's initial snapshot would not contain it —
        // `assert_eq!` below would see 0 items instead of 1. Confirmed by
        // temporarily reordering scan_apps ahead of the hook: this test
        // failed with "left: 0 right: 1"; reverted after confirming.
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let dir_path = dir.path().to_path_buf();

        let index = build_watched_index_with_hook(roots, move || {
            std::fs::write(
                dir_path.join("raced.desktop"),
                "[Desktop Entry]\nName=Raced\nExec=raced\n",
            )
            .unwrap();
        });

        assert_eq!(
            index.query("").len(),
            1,
            "a file created right after the watch is registered, before the \
             initial scan runs, must already be in that scan's result — it \
             can only be missing if the scan ran before the watch existed"
        );
    }

    #[test]
    fn build_watched_index_still_reacts_to_later_changes_through_the_watcher() {
        // Complements the ordering test above: proves the watch handed to
        // `spawn_index_watcher` by `build_watched_index` is the live one
        // actually driving the background thread, not a second, unrelated
        // instance — a change well after construction must still reach the
        // index with nothing but the watcher acting on it.
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let index = build_watched_index(roots);
        assert!(index.query("").is_empty(), "sanity: nothing installed yet");

        std::fs::write(
            dir.path().join("later.desktop"),
            "[Desktop Entry]\nName=Later\nExec=later\n",
        )
        .unwrap();

        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items
                .iter()
                .any(|i| i.title == "Later")),
            "a change after construction must still be picked up by the watcher"
        );
    }
}

/// Builds a real, environment-backed [`AppsProvider`]: scans the real
/// XDG/flatpak roots once, starts the inotify watcher over them (via
/// [`build_watched_index`]), and wires [`EmptyWindowSource`]/
/// [`SystemLauncher`] as the M2 backends.
///
/// The **only** place in this module that reads `std::env` — everything
/// upstream of this function ([`xdg_application_roots`],
/// [`flatpak_application_roots`], [`scan_apps`], [`AppIndex`]) takes its
/// inputs as plain values precisely so that only this one call site, run
/// once at daemon startup rather than under test, ever touches process
/// environment state.
pub fn build_apps_provider() -> AppsProvider {
    let home = std::env::var("HOME").ok();
    let data_home = std::env::var("XDG_DATA_HOME").ok();
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok();

    let mut roots =
        xdg_application_roots(home.as_deref(), data_home.as_deref(), data_dirs.as_deref());
    roots.extend(flatpak_application_roots(home.as_deref()));

    let index = build_watched_index(roots);

    AppsProvider::new(index, Arc::new(EmptyWindowSource), Arc::new(SystemLauncher))
}
