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

// This task (issue #57, Task 1) lands parsing with no consumer yet: nothing
// outside `#[cfg(test)]` calls `parse_desktop_entry`, `app_id_from_file_name`
// or `build_entry` until Task 2's directory scan and Task 3's index exist, so
// a normal (non-test) build sees every item below as unreachable. `expect`
// rather than `allow`, matching this workspace's one other use of the pattern
// (`hop-protocol`'s `mkfifo` call): once Task 2 adds a real caller in this
// same file, the lint stops firing, the expectation goes unfulfilled, and
// `-D warnings` turns that into a build error — the exception is required to
// delete itself rather than survive by inertia.
#![expect(
    dead_code,
    reason = "no consumer until Task 2 (issue #57) wires this module into a directory scan"
)]

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
pub(crate) struct AppEntry {
    pub(crate) app_id: String,
    pub(crate) item: Item,
    pub(crate) exec: String,
    pub(crate) haystack: String,
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
        // "é" is two bytes; a naive byte-index truncation at MAX_TITLE would
        // risk landing mid-character if MAX_TITLE were ever odd relative to
        // the run. `é` repeated MAX_TITLE times is comfortably past the
        // bound either way, so this also pins that oversized input is
        // shortened rather than rejected.
        let long_name = "é".repeat(MAX_TITLE);
        let parsed =
            parse_desktop_entry(&format!("[Desktop Entry]\nName={long_name}\nExec=x\n")).unwrap();
        assert!(parsed.title.len() <= MAX_TITLE);
        assert!(
            std::str::from_utf8(parsed.title.as_bytes()).is_ok(),
            "truncation must not split a multi-byte character"
        );
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
