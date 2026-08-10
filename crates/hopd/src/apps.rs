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
    /// The `Exec=` value, parsed exactly once by [`parse_exec`]: quoting
    /// resolved per the freedesktop Desktop Entry Specification and field
    /// codes (`%f`, `%U`, ...) stripped. `exec[0]` is the program, `exec[1..]`
    /// its arguments — nothing downstream splits this again.
    pub(crate) exec: Vec<String>,
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

/// The three distinct outcomes parsing a `.desktop` file's contents can
/// produce — named for what each means to [`scan_apps`], the only caller,
/// not for the syntax that produced them.
///
/// Before issue #108, [`parse_desktop_entry`] returned `Option<ParsedEntry>`,
/// and `None` covered two unrelated situations: the file was malformed, and
/// the file was valid but deliberately hidden (`Hidden=true`/
/// `NoDisplay=true`). Because those collapsed into the same value,
/// [`scan_apps`] could not tell them apart at the point it had to decide
/// whether to claim the app id — so it claimed early, before validation,
/// which is what let a corrupt higher-precedence file erase a working
/// lower-precedence one with no trace. This type exists to stop that
/// collapse.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DesktopEntryOutcome {
    /// A usable parsed entry. [`scan_apps`] claims the app id and
    /// contributes an item.
    Valid(ParsedEntry),
    /// Parsed correctly, but deliberately hidden per the freedesktop Desktop
    /// Entry Specification (`Hidden=true` or `NoDisplay=true`). [`scan_apps`]
    /// claims the app id and contributes no item — a lower-precedence entry
    /// with the same id stays suppressed, which is the whole point of the
    /// convention this outcome exists to honor.
    Occluded,
    /// Could not be understood: no usable `Name=`, or a malformed value in a
    /// key this parser validates (currently only `Exec=`, whose
    /// unterminated-quote rejection — see [`parse_exec`] — reaches this
    /// variant, never [`DesktopEntryOutcome::Occluded`]). [`scan_apps`]
    /// claims *nothing* for this outcome: a same-named entry in a
    /// lower-precedence root is considered normally. Carries a
    /// human-readable reason so the caller can log the path and the cause
    /// together.
    Malformed(String),
}

/// Parses one `.desktop` file's contents into a [`DesktopEntryOutcome`].
///
/// Ported from the salvaged `parse_desktop_entry`, adjusted to this crate's
/// types: the salvaged version built a project-local `SearchItem` directly
/// and returned `Option`; this one stops at a [`ParsedEntry`] wrapped in
/// [`DesktopEntryOutcome::Valid`] so [`build_entry`] can apply
/// `hop-protocol`'s content rules ([`IconName`], [`IconPath`]) before
/// anything becomes an [`Item`] — and so that a malformed file and a
/// deliberately hidden one, which used to collapse into the same `None`, are
/// values a caller can tell apart (issue #108).
pub(crate) fn parse_desktop_entry(content: &str) -> DesktopEntryOutcome {
    let mut name = String::new();
    let mut localized_name = String::new();
    let mut exec: Option<Vec<String>> = None;
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
            if exec.is_none() {
                match parse_exec(value) {
                    Some(argv) => exec = Some(argv),
                    None => {
                        // A malformed Exec= (most importantly, an
                        // unterminated quote) rejects the whole entry rather
                        // than guessing at a split — see `parse_exec`'s own
                        // doc comment. This is `DesktopEntryOutcome::
                        // Malformed`, not `Occluded`: a broken Exec= must
                        // not suppress a valid lower-precedence entry with
                        // the same app id (issue #108) the way a deliberate
                        // `Hidden=true` does. The reason travels as the
                        // return value itself now, so `scan_apps` is the one
                        // that logs it — path and reason together, via
                        // `malformed_log_line` — once this value reaches it;
                        // that is also what makes the reason a plain value a
                        // test can assert on directly, rather than requiring
                        // the stderr capture #109 deliberately left this
                        // untested over (a new dependency or `unsafe` fd
                        // redirection, both forbidden here).
                        return DesktopEntryOutcome::Malformed(format!(
                            "malformed Exec= value (unterminated quote): {value:?}"
                        ));
                    }
                }
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
        return DesktopEntryOutcome::Occluded;
    }
    if name.is_empty() {
        name = localized_name;
    }
    if name.is_empty() {
        return DesktopEntryOutcome::Malformed(
            "no Name= (or Name[locale]=) present in the [Desktop Entry] group".to_string(),
        );
    }

    // Truncated to MAX_TITLE at a char boundary rather than left as-is: this
    // provider constructs `Item`s directly rather than through
    // `hop_protocol`'s `Deserialize` gate, so nothing else in this crate
    // enforces the bound `crate::source::ResultSource`'s own docs warn a
    // provider is on its honor for. A `Name=` this long has never been
    // observed in a real desktop entry; the guard exists because the type
    // allows it, not because it is expected to fire.
    let title = truncate_to_byte_boundary(&name, MAX_TITLE);

    // The exec vector as a space-joined string exists only for the
    // haystack — build_entry/AppEntry carry the real Vec<String> onward for
    // launching, but AppIndex::query's substring filter still wants one
    // string to search, exactly as it did when `exec` itself was a String.
    let exec = exec.unwrap_or_default();
    let exec_haystack = exec.join(" ");

    let merged_keywords = [
        title.as_str(),
        exec_haystack.as_str(),
        keywords.as_str(),
        generic_name.as_str(),
        comment.as_str(),
    ]
    .join(" ");

    DesktopEntryOutcome::Valid(ParsedEntry {
        title,
        exec,
        icon: (!icon.is_empty()).then_some(icon),
        haystack: merged_keywords.to_lowercase(),
    })
}

/// The field codes the Desktop Entry Specification defines for `Exec=`
/// (`%f`, `%F`, `%u`, `%U`, `%d`, `%D`, `%n`, `%N`, `%i`, `%c`, `%k`, `%v`,
/// `%m`). [`parse_exec`] drops one of these only when it is a whole,
/// *unquoted* argument — see that function's doc comment for why quoting and
/// this list interact the way they do.
const FIELD_CODES: [&str; 13] = [
    "%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v", "%m",
];

/// Undoes the Desktop Entry Specification's general string escaping — the
/// `\\`, `\s`, `\n`, `\t`, `\r` sequences every string-typed value (not just
/// `Exec=`) is defined in terms of — before [`parse_exec`]'s own quoting
/// rule ever runs. This is what makes the *double*-escaping rule in that
/// function's doc comment fall out for free: a raw `\\` collapses to one `\`
/// here, and if quoting then requires that `\` to itself be escaped, the
/// author had to write two of them, i.e. four backslashes in the file.
///
/// A backslash followed by anything else (`\"`, `` \` ``, `\$`, or a
/// character this escape does not define) is left untouched, backslash and
/// all — those are not this layer's escapes, they belong to the quoting
/// layer downstream (`\"`, `` \` `` and `\$` are exactly the three, beyond
/// `\\` itself, [`parse_exec`] resolves inside a quoted argument), and
/// leaving them alone here is what lets that layer see them at all.
///
/// **Deliberately wired into `Exec=` only.** The escaping described above is
/// defined for every string-typed key, but [`parse_desktop_entry`] still
/// hands `Name=`, `GenericName=`, `Comment=` and `Keywords=` through
/// unescaped, so a `\s` in one of those reaches the client as the literal
/// two characters. Fixing that changes displayed text rather than launch
/// behavior, which is a different blast radius from this function's own
/// reason for existing, and #109's brief scoped it out. Tracked separately;
/// do not read this function's presence as evidence the other keys are
/// handled.
fn unescape_general_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('\\') => {
                out.push('\\');
                chars.next();
            }
            Some('s') => {
                out.push(' ');
                chars.next();
            }
            Some('n') => {
                out.push('\n');
                chars.next();
            }
            Some('t') => {
                out.push('\t');
                chars.next();
            }
            Some('r') => {
                out.push('\r');
                chars.next();
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// Parses an `Exec=` value into an ordered argument vector per the
/// freedesktop Desktop Entry Specification's quoting rules — the *only*
/// place in this module that splits an `Exec=` line into a program and its
/// arguments. (The pre-#109 shape of this module split twice: once here,
/// dropping anything starting with `%` by a raw-token guess with no notion
/// of quoting, and again in `SystemLauncher::launch`, splitting on
/// whitespace a second time — which is exactly what broke on a quoted
/// `Exec=` value, since neither site ever looked for a `"`.)
///
/// Passes, each over the whole value in order:
///
/// 1. [`unescape_general_string`] — the desktop-entry-wide backslash
///    escapes, which the specification applies before any key-specific
///    parsing (here, this function's own quoting rule) runs.
/// 2. Tokenizing on whitespace, where a double-quoted run is one argument
///    (its whitespace preserved) with its own backslash escapes — `\"`,
///    `` \` ``, `\$`, `\\` — resolved, backslash dropped. Quoted and
///    unquoted runs may concatenate into a single argument (`--app="value"`
///    is one token, not two) with no separating whitespace between them. An
///    opening `"` with no matching close before the value ends is
///    malformed: this function returns `None` rather than guessing at a
///    split, and the caller ([`parse_desktop_entry`]) rejects the whole
///    entry over it rather than launching a guess.
/// 3. Per resulting argument: dropped entirely if it is a whole *unquoted*
///    field code (see [`FIELD_CODES`]) — a quoted argument that happens to
///    read `"%f"` survives verbatim, since quoting it is how an author says
///    "this is not a field code" — otherwise `%%` within it resolves to a
///    literal `%`. Deliberately in that order, not the reverse: an author
///    writing `%%f` to mean the literal text "%f" would otherwise have that
///    intent erased if `%%` were collapsed to `%` *before* the field-code
///    check ran, since the collapsed result would then read as the exact
///    field code `%f` and get dropped.
///
/// An empty or whitespace-only value is not malformed: it parses to
/// `Some(vec![])`, matching this function's pre-#109 behavior of producing
/// an empty exec in that case and leaving "no program to launch" to
/// whichever [`Launcher`] the empty vector eventually reaches.
fn parse_exec(raw: &str) -> Option<Vec<String>> {
    let unescaped = unescape_general_string(raw);
    let mut chars = unescaped.chars().peekable();

    // (argument text, whether any part of it came from inside quotes) —
    // the second field is what lets step 3 tell a quoted "%f" apart from an
    // unquoted %f without re-scanning the original value.
    let mut args: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut current_quoted = false;
    let mut current_present = false;

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            if current_present {
                args.push((std::mem::take(&mut current), current_quoted));
                current_present = false;
                current_quoted = false;
            }
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            current_present = true;
            current_quoted = true;
            loop {
                match chars.next() {
                    None => return None, // unterminated quote: malformed
                    Some('"') => break,
                    Some('\\') => match chars.peek() {
                        Some(&next) if matches!(next, '"' | '`' | '$' | '\\') => {
                            current.push(next);
                            chars.next();
                        }
                        _ => current.push('\\'),
                    },
                    Some(other) => current.push(other),
                }
            }
            continue;
        }
        current.push(c);
        current_present = true;
        chars.next();
    }
    if current_present {
        args.push((current, current_quoted));
    }

    Some(
        args.into_iter()
            .filter(|(token, quoted)| *quoted || !FIELD_CODES.contains(&token.as_str()))
            .map(|(token, _)| token.replace("%%", "%"))
            .collect(),
    )
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
    pub(crate) exec: Vec<String>,
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

/// What [`scan_apps`] does with one candidate file once its content has
/// already been read: whether it claims the file's app id, and if so,
/// whether it also contributes an item.
///
/// Kept distinct from [`DesktopEntryOutcome`] — rather than reusing it
/// directly — because this enum additionally covers a case that outcome
/// cannot express on its own: [`build_entry`] failing on an otherwise
/// [`DesktopEntryOutcome::Valid`] parse. From `scan_apps`'s point of view
/// that failure is exactly as malformed as a parse failure — an entry whose
/// id could not be built must not claim the id it failed to build.
#[derive(Debug)]
enum ScanDecision {
    /// Claims the app id; contributes this item. Boxed only to keep this
    /// enum's size close to its other variants' — `clippy::large_enum_
    /// variant` flags `AppEntry` inline here as ~10x the next-largest
    /// variant's size, and every `ScanDecision` this module builds is
    /// short-lived (matched once, immediately, in `scan_apps`'s loop body),
    /// so there is no repeated allocation cost this indirection is trading
    /// away.
    Entry(Box<AppEntry>),
    /// Claims the app id; contributes no item. Reached only for
    /// [`DesktopEntryOutcome::Occluded`] — a deliberately hidden entry.
    ClaimOnly,
    /// Claims nothing. Carries the reason [`scan_apps`] logs alongside the
    /// file's path.
    Malformed(String),
}

/// Decides [`ScanDecision`] for one candidate whose content has already been
/// read, given the app id its file name contributed.
///
/// Pulled out of `scan_apps`'s loop body so this exact decision is
/// unit-testable without touching disk — which matters most for the
/// [`ScanDecision::Malformed`] arm reached when [`build_entry`] fails: that
/// only happens when `app_id` is long enough to push `app:<app_id>` over
/// [`hop_protocol::limits::MAX_ITEM_ID`] (4 096 bytes), and every common
/// Linux filesystem enforces a 255-byte `NAME_MAX` on the file name alone —
/// so there is no real `.desktop` file this crate could ever write to disk
/// with a name long enough to drive that path through `scan_apps` end to
/// end.
fn evaluate_candidate(app_id: String, content: &str) -> ScanDecision {
    match parse_desktop_entry(content) {
        DesktopEntryOutcome::Valid(parsed) => match build_entry(app_id, parsed) {
            Some(entry) => ScanDecision::Entry(Box::new(entry)),
            None => ScanDecision::Malformed(
                "parsed successfully but its app id could not be built into an item id \
                 (over the item id length bound)"
                    .to_string(),
            ),
        },
        DesktopEntryOutcome::Occluded => ScanDecision::ClaimOnly,
        DesktopEntryOutcome::Malformed(reason) => ScanDecision::Malformed(reason),
    }
}

/// Builds the one line [`scan_apps`] logs for a file skipped as malformed —
/// path and reason together, so "an app vanished from search with no trace"
/// (the exact defect issue #108 fixes) is never silently true again. Never
/// called for a [`ScanDecision::ClaimOnly`] (an occluded entry): a
/// deliberately hidden entry is ordinary freedesktop behavior, not something
/// to flag as broken.
///
/// Extracted as a pure function — mirroring `source.rs`'s
/// `rejection_summary_line`, which exists for the identical reason — because
/// capturing stderr in a unit test needs either a new dependency or `unsafe`
/// fd redirection, and this workspace forbids both. Asserting on this
/// function's return value is as close as a test can get to pinning what
/// `scan_apps` actually sends to stderr.
fn malformed_log_line(path: &Path, reason: &str) -> String {
    format!("hopd: apps provider: skipping {}: {reason}", path.display())
}

/// [`scan_apps`]'s actual body, with every malformed-file log line routed
/// through `log` instead of going straight to stderr. Production code
/// always passes a sink that `eprintln!`s the line; `scan_tests` below
/// passes one that records lines into a `Vec` instead, which is how the
/// tests there pin the criterion `malformed_log_line`'s own doc comment
/// could only assert in isolation: that a file skipped for a malformed
/// reason actually reaches the log, and a hidden or valid file produces no
/// line at all.
///
/// The mutation this catches: deleting one of the five `eprintln!`-via-`log`
/// call sites below (or the `malformed_log_line` call feeding it) while
/// leaving its `continue` in place. That mutation is invisible to every
/// test that only inspects `scan_apps`'s returned `Vec<AppEntry>` — the id
/// still goes unclaimed and the entry still goes missing either way — so
/// nothing but a test that reads what was logged can tell "skipped and
/// logged" apart from "skipped in silence," which is the exact regression
/// issue #108 exists to prevent.
fn scan_apps_with_log(roots: &[PathBuf], log: &mut dyn FnMut(&str)) -> Vec<AppEntry> {
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
            if seen_ids.contains(&app_id) {
                continue;
            }

            let Ok(metadata) = std::fs::metadata(&path) else {
                log(&malformed_log_line(&path, "could not stat the file"));
                continue;
            };
            if !metadata.is_file() {
                log(&malformed_log_line(&path, "not a regular file"));
                continue;
            }
            if metadata.len() > MAX_DESKTOP_FILE_BYTES {
                log(&malformed_log_line(&path, "over the size bound"));
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(err) => {
                    log(&malformed_log_line(
                        &path,
                        &format!("could not be read: {err}"),
                    ));
                    continue;
                }
            };

            match evaluate_candidate(app_id.clone(), &content) {
                ScanDecision::Entry(entry) => {
                    seen_ids.insert(app_id);
                    entries.push(*entry);
                }
                ScanDecision::ClaimOnly => {
                    seen_ids.insert(app_id);
                }
                ScanDecision::Malformed(reason) => {
                    log(&malformed_log_line(&path, &reason));
                }
            }
        }
    }

    entries
}

/// Scans every directory in `roots`, in order, turning each `.desktop` file
/// found into an [`AppEntry`]. A root that does not exist or cannot be read
/// is skipped, not an error — an unconfigured `~/.icons`-style directory on
/// a fresh machine is normal, not exceptional.
///
/// A file name ending in `.desktop` is only a *candidate* app id, not yet an
/// *understood* one. The id is claimed in `seen_ids` only once the candidate
/// has been stat-ed, read, parsed, and classified — never at candidate time.
/// [`ScanDecision::Entry`] and [`ScanDecision::ClaimOnly`] claim the id;
/// [`ScanDecision::Malformed`] claims nothing, whatever produced it: an
/// unreadable file, a file that is not a regular file, a file over
/// [`MAX_DESKTOP_FILE_BYTES`], a file [`parse_desktop_entry`] could not
/// understand, or a parsed file [`build_entry`] could not turn into an item.
/// Every one of those leaves the id free for a lower-precedence root to
/// supply a working entry, and every one is logged — path and reason,
/// via [`malformed_log_line`] — because silence is the specific defect
/// issue #108 fixes: an app vanishing from search with no diagnostic. The
/// one case that both claims the id *and* contributes nothing is
/// `ScanDecision::ClaimOnly`, reached only for a file that parsed correctly
/// and deliberately opted out via `Hidden=true`/`NoDisplay=true` — the
/// freedesktop convention for occluding a lower-precedence entry with the
/// same id on purpose, which is ordinary and is not logged.
///
/// This is what makes `roots`' ordering (user-then-system, from
/// [`xdg_application_roots`]) a real precedence rule: the first *understood*
/// file for a given id wins, not the first *candidate filename* seen — a
/// corrupt user-level override no longer erases a working system-level entry
/// beneath it.
///
/// Every candidate is `stat`-ed before it is read: anything that is not a
/// regular file (a symlink resolving to a FIFO or a character device such as
/// `/dev/zero`, which has no EOF and would hang a `read_to_string` forever)
/// or that exceeds [`MAX_DESKTOP_FILE_BYTES`] is skipped exactly like a
/// missing or unreadable file, never read.
///
/// The only place in this module that performs disk I/O other than the
/// inotify watcher itself (`open_watch`/`spawn_index_watcher`, Task 6) —
/// called once at startup and once per filesystem-change notification
/// thereafter, **never** from [`AppIndex::query`] (Task 3).
pub fn scan_apps(roots: &[PathBuf]) -> Vec<AppEntry> {
    scan_apps_with_log(roots, &mut |line| eprintln!("{line}"))
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

    // --- New coverage: issue #108, a broken higher-precedence entry must
    // not silently occlude a valid lower-precedence one. ---

    #[test]
    fn a_corrupt_higher_precedence_file_does_not_hide_a_valid_lower_precedence_entry() {
        // The triage repro from issue #108: a garbage `firefox.desktop` (no
        // [Desktop Entry] header at all) in the higher-precedence root used
        // to claim the "firefox" id and then produce nothing, erasing the
        // valid entry beneath it — Firefox vanished from the index
        // entirely, alongside an unrelated control entry that had nothing
        // to do with the bug.
        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        fs::write(
            higher.path().join("firefox.desktop"),
            "this is not a desktop entry\njust garbage\n",
        )
        .unwrap();
        write_entry(lower.path(), "firefox.desktop", "Firefox");
        write_entry(lower.path(), "gimp.desktop", "GIMP");

        let entries = scan_apps(&[higher.path().to_path_buf(), lower.path().to_path_buf()]);
        let ids: Vec<&str> = entries.iter().map(|e| e.app_id.as_str()).collect();
        assert!(
            ids.contains(&"firefox"),
            "the valid lower-precedence entry must still be indexed: {ids:?}"
        );
        assert!(
            ids.contains(&"gimp"),
            "an unrelated entry must be unaffected: {ids:?}"
        );
    }

    #[test]
    fn an_unreadable_higher_precedence_file_does_not_hide_a_valid_lower_precedence_entry() {
        use std::os::unix::fs::PermissionsExt;

        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        let higher_file = higher.path().join("app.desktop");
        write_entry(higher.path(), "app.desktop", "Unreadable");
        fs::set_permissions(&higher_file, fs::Permissions::from_mode(0o000)).unwrap();

        if fs::read_to_string(&higher_file).is_ok() {
            // Running as root (or under some container setups) ignores
            // permission bits entirely, which would make this test's
            // premise false rather than its assertion — same escape hatch
            // as the /dev/zero test above uses for a different
            // environment-dependent precondition.
            eprintln!("skipping: this process can read a 0o000 file (likely running as root)");
            fs::set_permissions(&higher_file, fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        write_entry(lower.path(), "app.desktop", "Valid");

        let entries = scan_apps(&[higher.path().to_path_buf(), lower.path().to_path_buf()]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].item.title, "Valid");

        // Restore permissions so `TempDir`'s own cleanup on drop can remove
        // the file without needing directory-only write access to do it.
        fs::set_permissions(&higher_file, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn a_higher_precedence_file_over_the_size_bound_does_not_hide_a_valid_lower_precedence_entry() {
        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        let header = "[Desktop Entry]\nName=Huge\nExec=huge\n";
        let padding = "#".repeat(MAX_DESKTOP_FILE_BYTES as usize + 1 - header.len());
        fs::write(
            higher.path().join("app.desktop"),
            format!("{header}{padding}"),
        )
        .unwrap();
        write_entry(lower.path(), "app.desktop", "Valid");

        let entries = scan_apps(&[higher.path().to_path_buf(), lower.path().to_path_buf()]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].item.title, "Valid");
    }

    #[test]
    fn a_higher_precedence_symlink_to_a_special_file_does_not_hide_a_valid_lower_precedence_entry()
    {
        let special = Path::new("/dev/zero");
        if !special.exists() {
            eprintln!("skipping: /dev/zero not present on this system");
            return;
        }

        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(special, higher.path().join("app.desktop")).unwrap();
        write_entry(lower.path(), "app.desktop", "Valid");

        let entries = scan_apps(&[higher.path().to_path_buf(), lower.path().to_path_buf()]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].item.title, "Valid");
    }

    #[test]
    fn a_malformed_exec_in_a_higher_precedence_entry_does_not_hide_a_valid_lower_precedence_entry()
    {
        // The interaction with #109 the brief calls out: a broken Exec=
        // used to make `parse_desktop_entry` return `None`, exactly like
        // `Hidden=true` did, so it occluded a valid entry beneath it by
        // accident. It must map to `DesktopEntryOutcome::Malformed`, never
        // `Occluded`.
        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        fs::write(
            higher.path().join("app.desktop"),
            "[Desktop Entry]\nName=Broken\nExec=app \"unterminated\n",
        )
        .unwrap();
        write_entry(lower.path(), "app.desktop", "Valid");

        let entries = scan_apps(&[higher.path().to_path_buf(), lower.path().to_path_buf()]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].item.title, "Valid");
    }

    #[test]
    fn evaluate_candidate_is_malformed_when_build_entry_fails_to_construct_the_id() {
        // `build_entry` only fails when `app_id` is long enough to push
        // `app:<app_id>` over `MAX_ITEM_ID` (4 096 bytes) — and every common
        // Linux filesystem enforces a 255-byte `NAME_MAX` on a file name
        // alone, so there is no real `.desktop` file this crate could ever
        // write to disk with a name that long (verified empirically: even
        // ~4 KiB is refused with ENAMETOOLONG well before the bound). This
        // exercises `scan_apps`'s per-candidate decision directly instead;
        // the "does this claim the id" half of the behavior — that a
        // `ScanDecision::Malformed` never inserts into `seen_ids` — is
        // exercised by the same match arm the tests above already drive
        // through corrupt/unreadable/oversized/symlink/malformed-Exec
        // candidates.
        let over_long_app_id = "a".repeat(hop_protocol::limits::MAX_ITEM_ID);
        let decision = evaluate_candidate(over_long_app_id, "[Desktop Entry]\nName=X\nExec=x\n");
        assert!(
            matches!(decision, ScanDecision::Malformed(_)),
            "an id build_entry could not construct must not claim the id it failed to build"
        );
    }

    #[test]
    fn a_hidden_higher_precedence_entry_still_occludes_a_valid_lower_precedence_entry() {
        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        fs::write(
            higher.path().join("app.desktop"),
            "[Desktop Entry]\nName=Hidden\nExec=hidden\nHidden=true\n",
        )
        .unwrap();
        write_entry(lower.path(), "app.desktop", "Valid");

        let entries = scan_apps(&[higher.path().to_path_buf(), lower.path().to_path_buf()]);
        assert!(
            entries.is_empty(),
            "Hidden=true must keep occluding the lower-precedence entry, \
             and contribute no item of its own: {entries:?}"
        );
    }

    #[test]
    fn a_no_display_higher_precedence_entry_still_occludes_a_valid_lower_precedence_entry() {
        let higher = tempfile::tempdir().unwrap();
        let lower = tempfile::tempdir().unwrap();
        fs::write(
            higher.path().join("app.desktop"),
            "[Desktop Entry]\nName=NoDisp\nExec=nodisp\nNoDisplay=true\n",
        )
        .unwrap();
        write_entry(lower.path(), "app.desktop", "Valid");

        let entries = scan_apps(&[higher.path().to_path_buf(), lower.path().to_path_buf()]);
        assert!(
            entries.is_empty(),
            "NoDisplay=true must keep occluding the lower-precedence entry, \
             and contribute no item of its own: {entries:?}"
        );
    }

    #[test]
    fn malformed_log_line_names_the_path_and_the_reason() {
        // A focused unit test of the pure line-building function, isolated
        // from `scan_apps`'s own file-classification logic — the
        // `scan_apps_with_log`-driven tests below are what actually pin
        // that `scan_apps` calls this function (and only this function) at
        // each of the five malformed sites, and never for an occluded or
        // valid file.
        let line = malformed_log_line(Path::new("/tmp/x/firefox.desktop"), "not a regular file");
        assert!(line.contains("/tmp/x/firefox.desktop"), "{line:?}");
        assert!(line.contains("not a regular file"), "{line:?}");
    }

    // --- New coverage: issue #108's review follow-up — a file skipped for
    // a malformed reason must actually be observed to log something naming
    // the path and the reason, not just be inspected for correctness by
    // reading the source. `scan_apps_with_log` is the seam that makes the
    // log itself, not just its would-be content, part of what a test can
    // assert on. ---

    /// Runs `scan_apps_with_log` over `roots`, returning the entries found
    /// and every line it logged, in call order.
    fn scan_and_capture_log(roots: &[PathBuf]) -> (Vec<AppEntry>, Vec<String>) {
        let mut lines = Vec::new();
        let entries = scan_apps_with_log(roots, &mut |line| lines.push(line.to_string()));
        (entries, lines)
    }

    #[test]
    fn scan_apps_with_log_reports_a_stat_failure() {
        // A symlink whose target does not exist: `std::fs::metadata`
        // follows it and fails with `ENOENT`, which is a different failure
        // point from every other test here (none of which ever reach the
        // `std::fs::metadata` error arm at all).
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("does-not-exist"),
            dir.path().join("broken.desktop"),
        )
        .unwrap();

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert!(entries.is_empty());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("broken.desktop"), "{lines:?}");
        assert!(lines[0].contains("could not stat the file"), "{lines:?}");
    }

    #[test]
    fn scan_apps_with_log_reports_a_non_regular_file() {
        let special = Path::new("/dev/zero");
        if !special.exists() {
            eprintln!("skipping: /dev/zero not present on this system");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(special, dir.path().join("evil.desktop")).unwrap();

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert!(entries.is_empty());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("evil.desktop"), "{lines:?}");
        assert!(lines[0].contains("not a regular file"), "{lines:?}");
    }

    #[test]
    fn scan_apps_with_log_reports_a_file_over_the_size_bound() {
        let dir = tempfile::tempdir().unwrap();
        let header = "[Desktop Entry]\nName=Huge\nExec=huge\n";
        let padding = "#".repeat(MAX_DESKTOP_FILE_BYTES as usize + 1 - header.len());
        fs::write(
            dir.path().join("huge.desktop"),
            format!("{header}{padding}"),
        )
        .unwrap();

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert!(entries.is_empty());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("huge.desktop"), "{lines:?}");
        assert!(lines[0].contains("over the size bound"), "{lines:?}");
    }

    #[test]
    fn scan_apps_with_log_reports_an_unreadable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.desktop");
        write_entry(dir.path(), "app.desktop", "Unreadable");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();

        if fs::read_to_string(&file).is_ok() {
            // Same environment-dependent escape hatch as the other
            // permission-bit tests in this module: root (or some container
            // setups) ignores the mode bits entirely.
            eprintln!("skipping: this process can read a 0o000 file (likely running as root)");
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert!(entries.is_empty());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("app.desktop"), "{lines:?}");
        assert!(lines[0].contains("could not be read"), "{lines:?}");

        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn scan_apps_with_log_reports_a_parse_failure() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("garbage.desktop"),
            "this is not a desktop entry\njust garbage\n",
        )
        .unwrap();

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert!(entries.is_empty());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("garbage.desktop"), "{lines:?}");
        assert!(lines[0].contains("Name="), "{lines:?}");
    }

    #[test]
    fn scan_apps_with_log_emits_nothing_for_a_hidden_entry() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("hidden.desktop"),
            "[Desktop Entry]\nName=Hidden\nExec=hidden\nHidden=true\n",
        )
        .unwrap();

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert!(entries.is_empty());
        assert!(
            lines.is_empty(),
            "a deliberately hidden entry is ordinary, not something to log: {lines:?}"
        );
    }

    #[test]
    fn scan_apps_with_log_emits_nothing_for_a_no_display_entry() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("nodisplay.desktop"),
            "[Desktop Entry]\nName=NoDisp\nExec=nodisp\nNoDisplay=true\n",
        )
        .unwrap();

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert!(entries.is_empty());
        assert!(
            lines.is_empty(),
            "a deliberately hidden entry is ordinary, not something to log: {lines:?}"
        );
    }

    #[test]
    fn scan_apps_with_log_emits_nothing_for_a_valid_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_entry(dir.path(), "firefox.desktop", "Firefox");

        let (entries, lines) = scan_and_capture_log(&[dir.path().to_path_buf()]);
        assert_eq!(entries.len(), 1);
        assert!(
            lines.is_empty(),
            "a successfully indexed entry has nothing to log: {lines:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Unwraps a [`DesktopEntryOutcome::Valid`], panicking with the actual
    /// variant otherwise. Every test in this module below that only cares
    /// about a successfully parsed entry reaches for this instead of
    /// matching `DesktopEntryOutcome` out by hand each time — a plain,
    /// local helper rather than a `#[cfg(test)]` inherent method on
    /// `DesktopEntryOutcome` itself, so the return-type change this helper
    /// papers over stays visible at every call site's diff instead of
    /// disappearing behind a same-named method.
    fn parsed(content: &str) -> ParsedEntry {
        match parse_desktop_entry(content) {
            DesktopEntryOutcome::Valid(parsed) => parsed,
            other => panic!("expected DesktopEntryOutcome::Valid, got {other:?}"),
        }
    }

    // --- Ported from the salvaged Rust parser's own test module. ---

    #[test]
    fn parses_a_basic_desktop_entry() {
        let parsed = parsed(
            "[Desktop Entry]\nName=Firefox\nExec=firefox %u\nIcon=firefox\nKeywords=browser;web;\n",
        );
        assert_eq!(parsed.title, "Firefox");
        assert_eq!(parsed.exec, vec!["firefox".to_string()]);
        assert_eq!(parsed.icon.as_deref(), Some("firefox"));
        assert!(parsed.haystack.contains("browser"));
    }

    #[test]
    fn hidden_and_no_display_entries_are_skipped() {
        assert!(matches!(
            parse_desktop_entry("[Desktop Entry]\nName=Hidden\nExec=hidden\nHidden=true\n"),
            DesktopEntryOutcome::Occluded
        ));
        assert!(matches!(
            parse_desktop_entry("[Desktop Entry]\nName=NoDisp\nExec=nodisp\nNoDisplay=true\n"),
            DesktopEntryOutcome::Occluded
        ));
    }

    #[test]
    fn falls_back_to_a_localized_name_when_the_primary_is_missing() {
        let parsed = parsed(
            "[Desktop Entry]\nName[en_US]=Localized App\nExec=localized-app %U\nType=Application\n",
        );
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
        assert!(matches!(
            parse_desktop_entry("[Desktop Entry]\nExec=nothing\n"),
            DesktopEntryOutcome::Malformed(_)
        ));
    }

    #[test]
    fn content_outside_the_desktop_entry_group_is_ignored() {
        // A mutation that dropped the `in_desktop_entry` gate would pick up
        // this second group's Name= instead of leaving the file nameless.
        let outcome = parse_desktop_entry(
            "[Desktop Entry]\nExec=real\n[Desktop Action new-window]\nName=New Window\n",
        );
        assert!(
            matches!(outcome, DesktopEntryOutcome::Malformed(_)),
            "a Name= outside [Desktop Entry] must not count"
        );
    }

    #[test]
    fn field_codes_are_stripped_from_exec() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=app %f %U --flag %i\n");
        assert_eq!(parsed.exec, vec!["app".to_string(), "--flag".to_string()]);
    }

    // --- New coverage: issue #109, `Exec=` quoting per the freedesktop
    // Desktop Entry Specification, parsed exactly once into a Vec<String>. ---

    #[test]
    fn a_quoted_program_path_with_spaces_becomes_one_argument_with_no_quote_chars() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=\"/opt/My App/bin/app\" --flag\n");
        assert_eq!(
            parsed.exec,
            vec!["/opt/My App/bin/app".to_string(), "--flag".to_string()]
        );
    }

    #[test]
    fn a_quoted_program_path_without_spaces_has_its_quote_chars_stripped() {
        // The live failure on this machine: a quoted `Exec=` with no
        // internal whitespace was still emitting a program token with a
        // literal leading `"` attached, because the old parser never
        // understood quoting at all.
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=\"/home/pedro/.local/bin/unity\"\n");
        assert_eq!(
            parsed.exec,
            vec!["/home/pedro/.local/bin/unity".to_string()]
        );
    }

    #[test]
    fn a_quoted_argument_with_spaces_reaches_the_result_as_exactly_one_argument() {
        let parsed = parsed(
            "[Desktop Entry]\nName=X\nExec=ibus-daemon --daemon-args \"--xim --panel disable\"\n",
        );
        assert_eq!(
            parsed.exec,
            vec![
                "ibus-daemon".to_string(),
                "--daemon-args".to_string(),
                "--xim --panel disable".to_string(),
            ]
        );
    }

    #[test]
    fn backslash_escapes_inside_a_quoted_argument_are_resolved_and_the_backslash_dropped() {
        // The trailing `\\\\` (four backslashes) is deliberate, not a typo:
        // representing one literal backslash needs four backslashes in the
        // file — see the double-escaping test below for why. The other
        // three escapes (`\"`, `` \` ``, `\$`) need only the single
        // backslash the quoting rule itself defines, since the general
        // desktop-entry string escape (applied first, by
        // `unescape_general_string`) does not recognize any of the three
        // and passes them through untouched for the quoting layer to
        // resolve.
        let parsed = parse_exec(r#"app "quote:\" backtick:\` dollar:\$ slash:\\\\""#).unwrap();
        assert_eq!(
            parsed,
            vec![
                "app".to_string(),
                "quote:\" backtick:` dollar:$ slash:\\".to_string()
            ]
        );
    }

    #[test]
    fn a_literal_backslash_written_per_the_double_escaping_rule_yields_exactly_one_backslash() {
        // The general desktop-entry string escape (`\\` -> `\`) runs before
        // the quoting rule's own backslash escape (`\\` -> `\`), so a
        // *literal* backslash inside a quoted argument must be written as
        // four backslashes in the file: two collapse to one under the
        // general string escape, then that one pair collapses to one
        // backslash under the quoting rule.
        let parsed = parse_exec(r#"app "one\\\\two""#).unwrap();
        assert_eq!(parsed, vec!["app".to_string(), r"one\two".to_string()]);
    }

    #[test]
    fn a_field_code_is_stripped_only_as_a_whole_unquoted_argument() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=app %f \"%f\"\n");
        assert_eq!(
            parsed.exec,
            vec!["app".to_string(), "%f".to_string()],
            "the unquoted %f must be dropped; the quoted \"%f\" must survive verbatim"
        );
    }

    #[test]
    fn double_percent_resolves_to_a_literal_percent() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=app --progress=%%\n");
        assert_eq!(
            parsed.exec,
            vec!["app".to_string(), "--progress=%".to_string()]
        );
    }

    #[test]
    fn an_unterminated_quote_rejects_the_whole_entry() {
        assert_eq!(parse_exec("app \"unterminated"), None);
        assert!(
            matches!(
                parse_desktop_entry("[Desktop Entry]\nName=X\nExec=app \"unterminated\n"),
                DesktopEntryOutcome::Malformed(_)
            ),
            "a malformed Exec= must reject the whole entry, not guess at a split"
        );
    }

    #[test]
    fn a_malformed_exec_value_is_reported_as_malformed_with_a_reason_naming_the_cause() {
        // The deferred assertion from #109: that PR left this rejection
        // untested because capturing stderr from a unit test needs either a
        // new dependency or `unsafe` fd redirection, both forbidden here,
        // and said the fix was to make the rejection a *value* once #108
        // split `None` into distinct occluded and malformed outcomes. This
        // is that assertion — and it also pins that an unterminated quote
        // maps to Malformed, never Occluded, so it cannot suppress a valid
        // lower-precedence entry with the same app id.
        let outcome = parse_desktop_entry("[Desktop Entry]\nName=X\nExec=app \"unterminated\n");
        let DesktopEntryOutcome::Malformed(reason) = outcome else {
            panic!("expected DesktopEntryOutcome::Malformed, got {outcome:?}");
        };
        assert!(
            reason.contains("Exec="),
            "reason must name what was wrong: {reason:?}"
        );
        assert!(
            reason.contains("unterminated"),
            "reason must describe why: {reason:?}"
        );
    }

    #[test]
    fn an_empty_exec_value_parses_to_an_empty_argument_vector_not_a_panic() {
        assert_eq!(parse_exec(""), Some(Vec::new()));
        assert_eq!(parse_exec("   "), Some(Vec::new()));
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=\n");
        assert_eq!(parsed.exec, Vec::<String>::new());
    }

    #[test]
    fn unquoted_exec_still_splits_on_whitespace_exactly_as_before() {
        let parsed = parse_exec("firefox --new-window https://example.com").unwrap();
        assert_eq!(
            parsed,
            vec![
                "firefox".to_string(),
                "--new-window".to_string(),
                "https://example.com".to_string(),
            ]
        );
    }

    // --- Regression coverage: the real-world shapes that motivated #109. ---

    #[test]
    fn regression_google_chrome_with_a_quoted_app_flag() {
        let parsed =
            parse_exec(r#"google-chrome --app="https://app.hey.com/" --name=HEY"#).unwrap();
        assert_eq!(
            parsed,
            vec![
                "google-chrome".to_string(),
                "--app=https://app.hey.com/".to_string(),
                "--name=HEY".to_string(),
            ]
        );
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
        let parsed = parsed(&format!("[Desktop Entry]\nName={long_name}\nExec=x\n"));
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
        let parsed = parsed("[Desktop Entry]\nName=Firefox\nExec=firefox\n");
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
        let parsed = parsed("[Desktop Entry]\nName=Firefox\nExec=firefox --new-window\n");
        let entry = build_entry("firefox".to_string(), parsed).unwrap();
        assert_eq!(entry.app_id, "firefox");
        assert_eq!(
            entry.exec,
            vec!["firefox".to_string(), "--new-window".to_string()]
        );
        assert!(entry.haystack.contains("firefox"));
    }

    #[test]
    fn a_slash_prefixed_icon_becomes_the_path_arm() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=x\nIcon=/usr/share/pixmaps/x.png\n");
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert!(matches!(entry.item.icon, Some(IconSpec::Path(_))));
    }

    #[test]
    fn a_bare_icon_name_becomes_the_name_arm() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=x\nIcon=utilities-terminal\n");
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert!(matches!(entry.item.icon, Some(IconSpec::Name(_))));
    }

    #[test]
    fn a_missing_icon_line_produces_no_icon() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=x\n");
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert_eq!(entry.item.icon, None);
    }

    #[test]
    fn an_icon_name_that_fails_its_own_rule_falls_back_to_no_icon_rather_than_dropping_the_item() {
        // A name carrying a control character is refused by `IconName::new`
        // (see `hop-protocol::content`). A mutation that instead propagated
        // that failure with `?` would drop the whole entry over one bad
        // line; this test catches that.
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=x\nIcon=bad\u{1b}name\n");
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert_eq!(entry.item.icon, None);
        assert_eq!(entry.item.title, "X", "the item itself must still be built");
    }

    #[test]
    fn every_entry_carries_exactly_one_open_action_agreeing_with_default_action() {
        let parsed = parsed("[Desktop Entry]\nName=X\nExec=x\n");
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

    /// Unwraps a [`DesktopEntryOutcome::Valid`], panicking with the actual
    /// variant otherwise — see `tests::parsed`'s doc comment for why this is
    /// a plain local helper rather than a method on `DesktopEntryOutcome`
    /// itself.
    fn parsed(content: &str) -> ParsedEntry {
        match parse_desktop_entry(content) {
            DesktopEntryOutcome::Valid(parsed) => parsed,
            other => panic!("expected DesktopEntryOutcome::Valid, got {other:?}"),
        }
    }

    fn entry(app_id: &str, title: &str) -> AppEntry {
        let parsed = parsed(&format!(
            "[Desktop Entry]\nName={title}\nExec={app_id}\nKeywords=browser;\n"
        ));
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
        assert_eq!(found.exec, vec!["firefox".to_string()]);
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
    /// Spawns `argv` — already parsed from the desktop entry's `Exec=` line
    /// by [`parse_exec`] at index time, quoting resolved and field codes
    /// stripped — as a new, detached process. `argv[0]` is the program,
    /// `argv[1..]` its arguments; this trait does no splitting of its own
    /// (contrast the pre-#109 contract, where `exec` was one whitespace-
    /// joined string and the first token found by splitting on whitespace
    /// again was taken as the program — the very split that broke on any
    /// quoted `Exec=` value). The [`focus_or_launch`] fallback once no
    /// focusable window exists — the seam that lets tests substitute a fake
    /// that records a call instead of actually starting a GUI application.
    fn launch(&self, argv: &[String]) -> Result<(), String>;
}

/// The real [`Launcher`]: `argv[0]` is the program, `argv[1..]` its
/// arguments — no splitting here at all, since [`parse_exec`] has already
/// resolved quoting and stripped field codes at parse time. Standard streams
/// are discarded and detached from the daemon's own terminal, if it has one;
/// a launched app is not expected to write anything hopd should see.
pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn launch(&self, argv: &[String]) -> Result<(), String> {
        let [program, args @ ..] = argv else {
            return Err("desktop entry has an empty Exec= command".to_string());
        };
        std::process::Command::new(program)
            .args(args)
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
    exec: &[String],
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
    /// real GUI application installed. Records each call's argv space-joined
    /// back into one string, matching this fake's pre-#109 recording shape
    /// (when `Launcher::launch` itself took one already-joined string) so
    /// this module's existing assertions read unchanged.
    #[derive(Default)]
    struct FakeLauncher {
        launched: Mutex<Vec<String>>,
    }

    impl Launcher for FakeLauncher {
        fn launch(&self, argv: &[String]) -> Result<(), String> {
            self.launched.lock().unwrap().push(argv.join(" "));
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

        assert!(focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).is_ok());
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

        assert!(focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).is_ok());
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
        focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).unwrap();
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

        assert!(
            focus_or_launch(
                &windows,
                &launcher,
                "firefox",
                &["firefox".to_string(), "--new-window".to_string()]
            )
            .is_ok()
        );
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

        assert!(
            focus_or_launch(
                &windows,
                &launcher,
                "brave-browser.desktop",
                &["brave".to_string()]
            )
            .is_ok()
        );
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

        focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).unwrap();
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

        focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).unwrap();
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

        focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).unwrap();
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
        focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).unwrap();
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
            &["gnome-terminal".to_string()],
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
        focus_or_launch(&windows, &launcher, "firefox", &["firefox".to_string()]).unwrap();
        assert!(windows.calls.lock().unwrap().is_empty());
        assert_eq!(launcher.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn system_launcher_reports_an_empty_exec_rather_than_spawning_nothing() {
        // Asserts on the error *message*, not just `is_err()`: on Linux,
        // `Command::new("").spawn()` already fails at the OS level (empty
        // program name), so `is_err()` alone would pass even with the
        // explicit slice-pattern guard deleted from `SystemLauncher::launch`
        // — it would just report a generic OS error instead of this
        // domain-specific one. Checking the message is what actually pins
        // the guard's existence.
        //
        // An empty `argv` is the only shape this layer sees for "no
        // program": [`parse_exec`] already collapses both an empty and a
        // whitespace-only `Exec=` value to `vec![]` upstream, at parse time.
        let err = SystemLauncher.launch(&[]).unwrap_err();
        assert!(
            err.contains("empty Exec="),
            "must report the empty-Exec= guard, not a generic spawn failure: {err}"
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
        focus_or_launch(&source, &launcher, "firefox", &["firefox".to_string()]).unwrap();
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
            // Opts in (issue #72): every id this provider mints is
            // `app:<desktop-entry-id>` (`AppIndex`'s own construction), where
            // `<desktop-entry-id>` names which installed `.desktop` file
            // matched — enumerable from what is installed on the system, and
            // never anything the user typed. Contrast `CalculatorProvider`,
            // which embeds the raw query text in its ids and does not opt in.
            ids_are_safe_to_persist_in_the_clear: true,
        }
    }

    async fn query(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(self.index.query(q.term.as_str()))
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

    /// Unwraps a [`DesktopEntryOutcome::Valid`], panicking with the actual
    /// variant otherwise — see `tests::parsed`'s doc comment for why this is
    /// a plain local helper rather than a method on `DesktopEntryOutcome`
    /// itself.
    fn parsed(content: &str) -> ParsedEntry {
        match parse_desktop_entry(content) {
            DesktopEntryOutcome::Valid(parsed) => parsed,
            other => panic!("expected DesktopEntryOutcome::Valid, got {other:?}"),
        }
    }

    fn one_app_provider(title: &str) -> AppsProvider {
        let parsed = parsed(&format!("[Desktop Entry]\nName={title}\nExec=x\n"));
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
        let parsed = parsed("[Desktop Entry]\nName=Terminal\nExec=t\n");
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
            parsed("[Desktop Entry]\nName=Firefox\nExec=firefox\n"),
        )
        .unwrap();
        let terminal = build_entry(
            "terminal".to_string(),
            parsed("[Desktop Entry]\nName=Terminal\nExec=terminal\n"),
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
        fn launch(&self, argv: &[String]) -> Result<(), String> {
            self.calls.lock().unwrap().push(argv.join(" "));
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_launches_the_apps_command_when_no_window_is_focusable() {
        let parsed = parsed("[Desktop Entry]\nName=Firefox\nExec=firefox --new\n");
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
use std::time::Duration;

use inotify::{Inotify, WatchMask};

/// The size, in bytes, of the buffer [`run_watcher_loop`] reads inotify
/// events into.
///
/// Issue #106 gap 4 speculated that a large-enough burst of events could
/// overflow the previous 4096-byte buffer and surface as a read error.
/// That premise does not hold: a single event can never come close to
/// filling even 4096 bytes. The kernel's `struct inotify_event` is 16
/// bytes (four `u32`-sized fields — `wd`, `mask`, `cookie`, `len`), and
/// `inotify(7)` gives `sizeof(struct inotify_event) + NAME_MAX + 1` as the
/// minimum buffer a caller needs to be guaranteed to fit *one* event —
/// `NAME_MAX` is 255, so that ceiling is 272 bytes. What the read side
/// actually errors over is only a buffer too small to hold *that next
/// single event*: `Inotify::read_events`'s own docs promise
/// `ErrorKind::InvalidInput` (`UnexpectedEof` on very old kernels) for
/// exactly and only that case — they say nothing about what happens to a
/// batch that does not fit as a whole. That part is not the crate's
/// documented behavior but the kernel's: `inotify(7)` describes `read(2)`
/// on an inotify fd as returning as many complete events as currently fit
/// in the caller's buffer, with whatever is left simply staying queued
/// for the next call rather than being lost or erroring — which is what
/// actually rules out the issue's overflow scenario for a burst larger
/// than one buffer's worth. 4096 was over fifteen times the largest event
/// that can exist; it was never a correctness risk, and this buffer is
/// not being resized to fix one. It is resized anyway, to sixteen times
/// its old size, purely for throughput: a burst of many small events (a
/// package manager unpacking a few dozen `.desktop` files in one
/// transaction) is drained in fewer `read` syscalls.
const WATCH_BUFFER_LEN: usize = 64 * 1024;

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
/// **Known gap, narrowed but not closed (issue #106 gap 2, and gap 1 in
/// the same circumstance):** a root skipped here because it does not
/// exist yet is picked up without a daemon restart in the common case —
/// [`run_watcher_loop`] re-adds a watch on every root, including this
/// one, before every rescan, so the next event from *any other* watched
/// root retries this one and succeeds once something has created it. The
/// identical residual applies to a root deleted after being watched
/// (gap 1): if it is still missing at the moment the loop's next re-arm
/// runs, that `Watches::add` call fails the exact same way this one does,
/// and from that point on there is nothing distinguishing "never existed"
/// from "existed, then vanished." What neither case's re-arm covers is a
/// system where no other watched root ever produces an event at all: with
/// only this one root configured, or every other root equally quiet,
/// nothing wakes the loop to retry the missing or deleted one, and it
/// stays unwatched until the daemon restarts — the same as before this
/// fix, just for a narrower set of machines (multi-root setups where at
/// least one other root is ever touched are covered; single-root setups,
/// or setups where every other root is as dormant as this one, are not).
/// Closing that residual fully means watching each root's *parent* for
/// the child's own `CREATE` and adding a real watch once it appears —
/// deliberately declined: it means either a full rescan on every
/// unrelated change under a busy parent like `~/.local/share`, or
/// teaching the loop to inspect event names when it currently and
/// deliberately ignores them entirely (see [`run_watcher_loop`]'s doc
/// comment). Tracked as part of issue #106; this slice narrows both gaps
/// rather than closing either.
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
/// process exits. Unlike an earlier version of this function, that is now
/// literally true rather than "until a read happens to fail" — the loop
/// body, [`run_watcher_loop`], retries a read error rather than returning
/// from the thread; see its doc comment for that and for the re-arming
/// that narrows issue #106 gaps 1 and 2.
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
pub fn spawn_index_watcher(inotify: Inotify, index: Arc<AppIndex>, roots: Vec<PathBuf>) {
    std::thread::spawn(move || {
        run_watcher_loop(
            inotify,
            index,
            roots,
            BackoffSchedule::PRODUCTION,
            |inotify, buffer| inotify.read_events_blocking(buffer).map(|_events| ()),
        );
    });
}

/// How long [`run_watcher_loop`] waits before retrying a failed read, and
/// how that wait grows on repeated failures: doubles from `base` on each
/// consecutive failure, capped at `max`, and reset back to `base` the
/// moment a watch is re-established. A struct rather than two bare
/// [`Duration`] parameters purely so [`spawn_index_watcher`]'s production
/// call site and `watcher_tests`' zero-delay call site both read as
/// passing *a policy*, not two easily-transposed numbers.
///
/// A zero `base` disables backoff entirely: doubling zero is still zero,
/// so `delay` never grows no matter how many consecutive failures occur.
/// `watcher_tests` relies on exactly that to make its retry test run in
/// milliseconds instead of sleeping in real wall-clock time, and it is
/// safe there only because that test's injected failure is bounded — it
/// fails exactly once, then succeeds forever after. A zero-`base`
/// schedule used anywhere failures can repeat without bound would instead
/// spin hot, retrying with no delay between attempts at all. `PRODUCTION`
/// must never be this, and no other schedule should be either unless its
/// caller can make the same bounded-failure guarantee a test can.
struct BackoffSchedule {
    base: Duration,
    max: Duration,
}

impl BackoffSchedule {
    /// 200ms doubling to a 30s ceiling. A watcher thread dying used to be
    /// silent and total — nothing else in the process noticed — so the
    /// base is short enough that a transient error (an `EMFILE` from a fd
    /// leak elsewhere, say) barely delays the next rescan, while the 30s
    /// cap keeps a watch that stays broken for a while (the filesystem
    /// backing a root gone until the next mount, say) from retrying at a
    /// sub-second cadence indefinitely.
    const PRODUCTION: BackoffSchedule = BackoffSchedule {
        base: Duration::from_millis(200),
        max: Duration::from_secs(30),
    };
}

/// [`spawn_index_watcher`]'s actual loop body, factored out so a test can
/// drive it with an injected `read` and a zero-delay `schedule` instead of
/// a real inotify fd and real wall-clock backoff. See
/// `a_read_error_does_not_kill_the_watcher_thread_and_indexing_resumes_after_recovery`
/// in `watcher_tests` for why an injected `read` is the only practical way
/// to exercise the retry path at all: there is no portable way to force a
/// real inotify file descriptor into an error state from a test.
///
/// **Retry, forever, on error (issue #106 gap 3).** The previous version
/// of this loop logged a read error and returned, silently freezing the
/// index for the rest of the process's life — the bug this issue exists
/// to close. This version instead logs, backs off per `schedule`, and
/// calls [`open_watch`] again, looping forever rather than giving up
/// after some number of attempts. When that call succeeds, its new
/// [`Inotify`] replaces the broken one and the backoff resets to `base`;
/// when it fails too, there is no better `Inotify` to hold, so the loop
/// keeps the one it has and tries reading from it again next iteration,
/// backed off further, until some later `open_watch` call finally
/// succeeds. "Retry forever" rather than "retry a few times, then alert"
/// is deliberate: this crate has no alerting channel and no `tracing` (or
/// any other structured logging) seam to page anyone through —
/// `eprintln!("hopd: …")` to stderr is the entire logging convention
/// here, confirmed directly against every other call site in this crate
/// rather than assumed. An `eprintln!` that nobody is watching, followed
/// by a thread that gives up, is strictly worse than one that keeps
/// trying: retrying costs nothing but a little CPU once backed off to the
/// cap, and a root that starts working again (the disk that held it
/// remounting, say) recovers the index without a daemon restart instead
/// of requiring one.
///
/// **Re-arm every root before every rescan (issue #106 gaps 1 and 2).**
/// After each successful read, every root in `roots` gets `Watches::add`ed
/// again, unconditionally, before [`scan_apps`] runs — ignoring individual
/// failures. `Watches::add` on a path that already has a watch updates it
/// and returns the same descriptor rather than erroring (confirmed
/// against the crate's own docs for `Watches::add`, and pinned by
/// `watches_add_on_an_already_watched_root_updates_the_watch_rather_than_erroring`
/// in `watcher_tests`), so this needs no bookkeeping of which roots are
/// currently watched — a root that's still missing just fails this call
/// and is skipped, same tolerance [`open_watch`] itself has. This is one
/// mechanism that narrows both gap 1 (a root deleted out from under its
/// watch) and gap 2 (a root that did not exist at startup): either is
/// picked back up the next time *any other* watched root's event wakes
/// this loop, since every root gets re-armed on every successful read,
/// not just the one whose event fired. Neither gap is *closed*, and for
/// the same reason in both cases: if no other root ever produces an
/// event — a single-root configuration, or every other root equally
/// quiet — nothing wakes the loop to retry the missing or deleted one,
/// and it stays unwatched until the daemon restarts. See [`open_watch`]'s
/// doc comment for that residual spelled out concretely. This
/// deliberately does not inspect which events were read or which root
/// they came from: re-arming every root on every successful read is
/// cheap enough (a handful of `inotify_add_watch` calls) that there is
/// nothing to gain from tracking which one changed.
fn run_watcher_loop(
    mut inotify: Inotify,
    index: Arc<AppIndex>,
    roots: Vec<PathBuf>,
    schedule: BackoffSchedule,
    mut read: impl FnMut(&mut Inotify, &mut [u8]) -> io::Result<()>,
) -> ! {
    let mut buffer = vec![0u8; WATCH_BUFFER_LEN];
    let mut delay = schedule.base;
    loop {
        match read(&mut inotify, &mut buffer) {
            Ok(()) => {
                let mut watches = inotify.watches();
                for root in &roots {
                    let _ = watches.add(root, watch_mask());
                }
                index.replace(scan_apps(&roots));
            }
            Err(err) => {
                eprintln!(
                    "hopd: apps provider: desktop-entry watcher read error, retrying in {delay:?}: {err}"
                );
                std::thread::sleep(delay);
                delay = (delay * 2).min(schedule.max);
                match open_watch(&roots) {
                    Ok(new_inotify) => {
                        inotify = new_inotify;
                        delay = schedule.base;
                    }
                    Err(err) => {
                        eprintln!(
                            "hopd: apps provider: could not re-establish the desktop-entry watch, will keep retrying: {err}"
                        );
                    }
                }
            }
        }
    }
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

    // --- Issue #106: watcher robustness. Gaps 1 and 2 (a deleted or
    // late-created root never gets a watch back) share one fix —
    // `run_watcher_loop` re-arming every root on every successful read —
    // and gap 3 (the thread dying silently on a read error) is that same
    // loop's retry-forever behavior. ---

    #[test]
    fn watches_add_on_an_already_watched_root_updates_the_watch_rather_than_erroring() {
        // Verifies the assumption `run_watcher_loop`'s re-arm step relies
        // on: calling `Watches::add` again on a path that already has a
        // watch is documented to update the existing watch and return the
        // same descriptor, not fail. That is what makes it safe to re-add
        // a watch on every root on every loop iteration unconditionally,
        // with no bookkeeping of which roots are already watched.
        let dir = tempfile::tempdir().unwrap();
        let inotify = open_watch(&[dir.path().to_path_buf()]).unwrap();
        let mut watches = inotify.watches();
        let first = watches.add(dir.path(), watch_mask()).unwrap();
        let second = watches.add(dir.path(), watch_mask()).unwrap();
        assert_eq!(
            first, second,
            "re-adding a watch on the same path must update the existing \
             watch and return the same descriptor, not create a second one"
        );
    }

    #[test]
    fn a_deleted_and_recreated_root_is_rewatched_without_a_daemon_restart() {
        // Issue #106 gap 1: inotify drops a watch silently when its
        // directory is removed, and nothing re-established it. `dir_a` is
        // removed and recreated here, and does end up watched again — but
        // this deliberately does not pin down *which* event actually woke
        // the loop for the re-arm that picked it back up. Removing a
        // watched directory always emits `IN_IGNORED`, delivered by the
        // kernel regardless of the requested watch mask, so `dir_a`'s own
        // removal is itself a plausible wake source here: `create_dir`
        // below runs synchronously right after `remove_dir`, so by the
        // time the watcher thread gets around to processing `IN_IGNORED`
        // the directory may already exist again, making that alone
        // sufficient without any help from `dir_b`. Proving the wake came
        // from `dir_b` specifically would mean suppressing or
        // intercepting `IN_IGNORED`, which the watch mask cannot do and
        // which this test does not attempt. What it does prove is the
        // outcome that matters: a root deleted and recreated ends up
        // watched again with no daemon restart, through some combination
        // of these two roots' events driving the re-arm.
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let roots = vec![dir_a.path().to_path_buf(), dir_b.path().to_path_buf()];
        let inotify = open_watch(&roots).unwrap();
        let index = Arc::new(AppIndex::new(scan_apps(&roots)));

        spawn_index_watcher(inotify, index.clone(), roots);

        std::fs::remove_dir(dir_a.path()).unwrap();
        std::fs::create_dir(dir_a.path()).unwrap();

        // dir_b's event guarantees at least one more read/re-arm pass
        // runs, whether or not dir_a's own IN_IGNORED already did the job.
        std::fs::write(
            dir_b.path().join("wake.desktop"),
            "[Desktop Entry]\nName=Wake\nExec=wake\n",
        )
        .unwrap();
        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items
                .iter()
                .any(|i| i.title == "Wake")),
            "sanity: dir_b's event must be processed before dir_a's rewatch can be checked"
        );

        // dir_a should be watched again now — a new file inside it must be
        // seen with no further help from dir_b.
        std::fs::write(
            dir_a.path().join("recovered.desktop"),
            "[Desktop Entry]\nName=Recovered\nExec=recovered\n",
        )
        .unwrap();
        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items
                .iter()
                .any(|i| i.title == "Recovered")),
            "a root re-created after being deleted must be re-watched, not left dark until a restart"
        );
    }

    #[test]
    fn a_root_missing_at_startup_is_watched_once_it_exists_and_another_roots_event_wakes_the_loop()
    {
        // Issue #106 gap 2's residual: a root that does not exist yet when
        // `open_watch` runs is skipped, per that function's contract, but
        // the same re-arm-every-root step that also narrows gap 1 means
        // the next event from *any other* watched root retries the watch
        // on it too — so once something creates the missing root, one
        // unrelated event elsewhere is enough to pick it up. (What is
        // still not covered — a missing root that never gets this nudge
        // because no other root ever fires an event — is exactly the
        // residual `open_watch`'s doc comment now documents, shared with
        // gap 1's identical circumstance.)
        let missing = tempfile::tempdir().unwrap();
        let missing_path = missing.path().to_path_buf();
        // Delete it immediately, so `open_watch` below sees a root that
        // does not exist yet — the way a never-created
        // `~/.local/share/applications` would on a fresh machine.
        std::fs::remove_dir(&missing_path).unwrap();

        let dir_b = tempfile::tempdir().unwrap();
        let roots = vec![missing_path.clone(), dir_b.path().to_path_buf()];
        let inotify = open_watch(&roots).unwrap();
        let index = Arc::new(AppIndex::new(scan_apps(&roots)));

        spawn_index_watcher(inotify, index.clone(), roots);

        std::fs::create_dir(&missing_path).unwrap();

        std::fs::write(
            dir_b.path().join("wake.desktop"),
            "[Desktop Entry]\nName=Wake\nExec=wake\n",
        )
        .unwrap();
        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items
                .iter()
                .any(|i| i.title == "Wake")),
            "sanity: dir_b's event must be processed before the missing root's rewatch can be checked"
        );

        std::fs::write(
            missing_path.join("arrived.desktop"),
            "[Desktop Entry]\nName=Arrived\nExec=arrived\n",
        )
        .unwrap();
        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items
                .iter()
                .any(|i| i.title == "Arrived")),
            "a root that appeared after startup must be watched once another root's event nudges the loop"
        );
    }

    #[test]
    fn a_read_error_does_not_kill_the_watcher_thread_and_indexing_resumes_after_recovery() {
        // Issue #106 gap 3, the core of this issue: `read_events_blocking`
        // returning `Err` used to log it and return from the thread,
        // freezing the index for the rest of the process's life.
        // `run_watcher_loop` must instead retry: back off, re-establish
        // the watch via `open_watch`, and keep going.
        //
        // The injected `read` fails exactly once, then defers to the real
        // `read_events_blocking` — there is no portable way to force a
        // real inotify fd into an error state from a test, which is
        // exactly why `run_watcher_loop` was factored out to take `read`
        // as a parameter. A channel signals the instant the loop is about
        // to perform its first post-recovery read — which is also the
        // instant `open_watch` has already re-armed the watch — so writing
        // the trigger file after that signal cannot race the watch being
        // established.
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let inotify = open_watch(&roots).unwrap();
        let index = Arc::new(AppIndex::new(scan_apps(&roots)));

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let mut has_failed = false;
        let read = move |inotify: &mut Inotify, buffer: &mut [u8]| -> io::Result<()> {
            if !has_failed {
                has_failed = true;
                return Err(io::Error::other("synthetic read error for the test"));
            }
            let _ = ready_tx.send(());
            inotify.read_events_blocking(buffer).map(|_events| ())
        };

        let index_for_loop = index.clone();
        std::thread::spawn(move || {
            run_watcher_loop(
                inotify,
                index_for_loop,
                roots,
                BackoffSchedule {
                    base: Duration::ZERO,
                    max: Duration::ZERO,
                },
                read,
            );
        });

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the watcher must retry and reach a post-recovery read rather than exiting");

        std::fs::write(
            dir.path().join("recovered.desktop"),
            "[Desktop Entry]\nName=Recovered\nExec=recovered\n",
        )
        .unwrap();

        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items
                .iter()
                .any(|i| i.title == "Recovered")),
            "indexing must resume after the watcher recovers from a read error"
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
