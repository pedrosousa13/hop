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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "AppIndex (Task 3) reads these fields, but AppIndex itself has no non-test \
                  consumer until Task 5 (issue #57) wires it into AppsProvider"
    )
)]
pub(crate) struct AppEntry {
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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no consumer until Task 7 (issue #57) wires this into startup"
    )
)]
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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no consumer until Task 7 (issue #57) wires this into startup"
    )
)]
pub(crate) fn flatpak_application_roots(home: Option<&str>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home {
        roots.push(Path::new(home).join(".local/share/flatpak/exports/share/applications"));
    }
    roots.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    roots
}

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
/// The only place in this module that performs disk I/O other than the
/// inotify watcher itself (`open_watch`/`spawn_index_watcher`, Task 6) —
/// called once at startup and once per filesystem-change notification
/// thereafter, **never** from [`AppIndex::query`] (Task 3).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no consumer until Task 7 (issue #57) wires this into startup"
    )
)]
pub(crate) fn scan_apps(roots: &[PathBuf]) -> Vec<AppEntry> {
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
/// this is smaller and exists only to keep one provider's unranked batch a
/// sane size while issue #103 (wiring `Pipeline::assemble`, and with it a
/// real cap) remains unlanded. See this plan's Scope section.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no consumer until Task 5 (issue #57) wires AppIndex into AppsProvider"
    )
)]
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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no consumer until Task 7 (issue #57) wires AppIndex into startup"
    )
)]
pub(crate) struct AppIndex {
    entries: RwLock<Vec<AppEntry>>,
}

impl AppIndex {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no consumer until Task 7 (issue #57) wires AppIndex into startup"
        )
    )]
    pub(crate) fn new(entries: Vec<AppEntry>) -> Self {
        AppIndex {
            entries: RwLock::new(entries),
        }
    }

    /// Filters the index to entries whose haystack contains `term`
    /// (case-insensitive substring match), capped at [`QUERY_RESULT_CAP`].
    /// An empty (or whitespace-only) `term` matches everything, capped the
    /// same way — the empty-query "browse installed apps" case.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no consumer until Task 5 (issue #57) wires AppIndex into AppsProvider"
        )
    )]
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no consumer until Task 5 (issue #57) wires AppIndex into AppsProvider"
        )
    )]
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Task 6's spawn_index_watcher calls this (index.replace(scan_apps(&roots))), \
                      but that closure has no non-test caller until Task 7 (issue #57) wires \
                      spawn_index_watcher into startup"
        )
    )]
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
