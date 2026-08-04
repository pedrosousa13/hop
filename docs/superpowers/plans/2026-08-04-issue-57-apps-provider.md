# Apps Provider (Issue #57) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the apps provider — the first real source of items, and the first provider whose query path is a pure in-memory lookup against an index that filesystem events keep current, per design spec §3's latency contract and issue #57's acceptance criteria.

**Architecture:** A new module, `crates/hopd/src/apps.rs`, holds an in-memory `AppIndex` built by a directory scan at startup and rebuilt by a background thread that blocks on Linux's inotify facility (via the `inotify` crate); a `Provider` implementation (`AppsProvider`) that answers `query()` from that index with no disk I/O and answers `execute()` with a focus-existing-window-else-launch dispatch ported from the previous extension's `appLaunch.js`, driven through two small injected traits (`WindowSource`, `Launcher`) so the M2 slice runs today against no-op window data and lights up unmodified once the M5 GNOME shim supplies real windows. `hopd::server::build_host` registers it alongside `SkeletonProvider`.

**Tech Stack:** Rust 2024, the `inotify` crate (ISC — new to this workspace, see Design decision 5) for filesystem-event watching, `tempfile` (already a dev-dependency) for the filesystem-event tests.

## Global Constraints

- **One new third-party dependency, deliberate and licensed.** `inotify` (ISC), and its own dependency `inotify-sys` (also ISC), are added to watch application directories for changes — see Design decision 5 for why a safe crate was chosen over hand-rolled `libc` FFI, and Task 6 for the exact `deny.toml` edit this requires (`cargo deny check` fails without it). Every other dependency this slice touches (`tempfile`) is already in the tree.
- **Gate commands, all four required:** `cargo test --workspace` (430 tests today, all green — verified by running the suite before writing this plan) · `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny check`.
- **No `.unwrap()` in production code** (`clippy::unwrap_used` + `-D warnings`). Test files open with `#![allow(clippy::unwrap_used)]`.
- **`unsafe_code = "deny"` needs no exception from this slice.** `inotify`'s public API this plan uses — `Inotify::init`, `Watches::add`, `Inotify::read_events_blocking` — is entirely safe Rust; the `unsafe` FFI calls inotify needs live inside that crate, not in `hopd`. This workspace's sole existing `unsafe` block remains in `hop-protocol`'s test-only code, unchanged by this plan.
- **The latency contract (spec §3):** keystroke → results < 10 ms on the query path; no disk reads, subprocess spawns or HTTP inside `Provider::query`. `Provider::execute` is not the query path — spawning a process there is exactly what a launcher does — and this plan does not hold it to the same bound.
- **No AI attribution** in commits or the PR.

## Scope: what this slice is and is not

**In scope**, the seven acceptance criteria on issue #57:

1. A query returns real installed applications from the index.
2. The index is maintained by filesystem events; installing or removing a desktop entry is reflected without restarting the daemon.
3. No disk read occurs on the query path.
4. Focus-existing-window-else-launch semantics match the ported app-launch test suite.
5. The provider declares a manifest with its kinds and a minimum term length, and the host honors it.
6. Icons resolve through icon-theme lookup.
7. An integration test drives the provider through the daemon over a real socket.

Plus the load-bearing addition from the issue's own first comment: the manifest's `id` must be `hop_core::provider::APPS_PROVIDER_ID`, not a hand-written literal, and item ids must be `app:<app_id>` — both pinned with a test that runs the provider's own output through `CheckedItems::check`, per that comment's explicit ask.

**Not in scope, deliberately:**

- **Dispatching `execute` through the daemon.** `ErasedProvider` in `crates/hop-core/src/host.rs` does not erase `execute` — only `query` — because issue #59 (`hop exec`) is the slice that wires action dispatch through the socket, and it is blocked by this issue and has not landed. So `AppsProvider::execute` is implemented and tested **directly against the provider** (`Arc<AppsProvider>::execute(...).await`), exactly as `hop-core`'s own `FakeProvider` and `hopd`'s `SkeletonProvider` are tested today. `SkeletonProvider::execute` itself still answers `Err(ProviderError::Failed("action dispatch is not implemented yet"))` for the same reason, unchanged by this slice.
- **Icon-theme resolution inside hopd.** See Design decision 2. A themed `Icon=` name becomes `IconSpec::Name`; hopd does not look up which theme is installed or resolve the name to a file.
- **Result ranking or capping.** Issue #103 (wire `Pipeline::assemble` into the daemon) is triaged P1 and blocked by this issue; it lands immediately after. Until it lands, the daemon streams each provider's manifest-checked items as its own batch, unranked, in the order providers answer — exactly as it does today for `SkeletonProvider`. The apps provider's `query()` applies a simple, self-imposed result cap (Task 3) purely to keep one batch's byte size sane; that cap is not ranking, and nothing in this slice orders items by relevance.
- **Real window data.** The M5 GNOME shim (spec §7) is what will someday supply `WindowSource::windows_for_app`/`all_windows` with live compositor data. This slice's registered implementation, `EmptyWindowSource`, always answers `vec![]`, so `AppsProvider::execute` correctly and unconditionally launches rather than focuses — see Design decision 4.
- **Desktop-file-id subdirectory nesting.** The freedesktop desktop-entry spec derives a multi-directory app's id by joining subdirectory names with `-`. No standard installation nests `.desktop` files under `applications/`, so this plan (like the salvaged parser) treats the filename alone, minus its `.desktop` suffix, as the app id.

## Design decisions (read before any task)

**1. The code lives in `crates/hopd/src/apps.rs`, a flat module — not a `providers/` subdirectory, and not a new crate.** The salvaged Rust source lived at `crates/hopd/src/providers/apps.rs` in a different project (`hop-launcher`) whose `hopd` crate already had several providers and a `providers/` directory to hold them. This workspace's `hopd/src/` is flat today — `connection.rs`, `runtime_dir.rs`, `server.rs`, `source.rs`, no subdirectories — and `SkeletonProvider` lives inline in `source.rs` rather than under a `providers/` module. Introducing a directory for exactly one module would be structure built ahead of the need it serves; if issue #58 (calculator) or a later provider make a shared parent module worth having, that is a decision for whichever issue adds the second one, not this one. A new crate was also considered and rejected: the provider needs no API surface outside `hopd` (it is registered once, in `build_host`, and never referenced by `hop-cli` or `hop-protocol`), so a crate boundary would buy only `Cargo.toml` overhead.

**2. Icon handling stops at `IconSpec` construction; hopd does not resolve themed names to files.** `IconSpec` (`crates/hop-protocol/src/item.rs`) has two arms, `Name(IconName)` and `Path(IconPath)`. `IconName`'s own docs (`crates/hop-protocol/src/content.rs:560-566`) state the reasoning directly: a name naming nothing in the installed theme "is accepted here and answered by whatever does the lookup, which falls back to a generic icon... which names a theme carries is a property of the machine, and this crate holds a contract, not an inventory." So a `.desktop` `Icon=` value that is a bare themed name (no leading `/`) becomes `IconSpec::Name(IconName::new(value)?)`; one that is an absolute path becomes `IconSpec::Path(IconPath::new(value)?)`. Resolving a name to an actual file is the client's job — doing it in hopd would mean a disk read on every result (violating the icon field's own "no I/O until a client opens it" design) and would bake one desktop's icon theme into a value every client receives, when `IconPath`'s own docs make the same point about roots: "which names a theme carries is a property of the machine." This slice satisfies acceptance criterion 6 ("icons resolve through icon-theme lookup") by producing the right *shape* — `Name` for a theme key, `Path` for a file — and leaving the lookup itself to whoever renders the item.

**3. `execute` is implemented and tested directly against `AppsProvider`, never through the daemon's socket.** Restated from Scope for visibility: `ErasedProvider` erases `query` but not `execute` (verified by reading `crates/hop-core/src/host.rs`'s `ErasedProvider` trait — it declares only `query_erased` and `output`, no `execute` method at all), so there is no path from a `ClientMsg` to `Provider::execute` yet. Task 5's tests call `Arc::new(provider).execute(item_id, action_id).await` directly, exactly as `hop-core/src/provider.rs`'s own `provider_trait_is_implementable_and_runnable_on_an_executor` test does for its `FakeProvider`.

**4. The launch path is built against two small injected traits, `WindowSource` and `Launcher`, so M2's no-op window data falls through to launching with no future redesign.** `appLaunch.js`'s `launchOrFocusApp` — the "behavioral spec for this slice" the issue names — has two tiers: first, windows the app object itself already knows about (`app.get_windows()`); second, a fallback scan of every open window matched by id heuristics (`global.display.get_tab_list()` + `windowMatchesApp`), used only when the first tier is empty. Both tiers matter to the ported test suite: `launchOrFocusApp prefers focusing an existing normal window` exercises the first tier with no id-matching at all (a real GNOME `Shell.App.get_windows()` only ever returns that app's own windows, so nothing needs to check whose windows they are), while `launchOrFocusApp focuses matching open window when app.get_windows is empty` exercises the second tier, where id-matching is exactly the point. Collapsing these to one tier would either break the first test (require an id match that test's fixture never sets) or the second (skip the id check the test is about) — so `WindowSource` keeps both:
   ```rust
   pub trait WindowSource: Send + Sync + 'static {
       fn windows_for_app(&self, app_id: &str) -> Vec<WindowHandle>;
       fn all_windows(&self) -> Vec<WindowHandle>;
       fn unminimize(&self, window: &WindowHandle);
       fn activate(&self, window: &WindowHandle);
   }
   ```
   M2's registered implementation, `EmptyWindowSource`, answers `vec![]` from both list methods and does nothing from `unminimize`/`activate` — so `focus_or_launch` (Task 4) always falls through to `Launcher::launch`, correctly, until the M5 GNOME shim ships a real `WindowSource` that populates both tiers from the compositor. No line in `focus_or_launch` itself changes when that happens.

   **Divergence 1 — the Clutter timestamp is dropped.** `launchOrFocusApp(app, nowProvider)` threads a GNOME Mutter/Clutter event timestamp through `activate`/`unminimize`, which is a focus-stealing-prevention detail owned by the compositor. Nothing on this side of the M5 shim has such a timestamp to supply, so `WindowSource::activate`/`unminimize` take none; whoever implements the real `WindowSource` in M5 owns sourcing one from the compositor at that point.

   **Divergence 2 — the three-rung launch fallback (`app.activate` → `app.open_new_window` → `app.launch` → `appInfo.launch`) collapses to one `Launcher::launch` call.** Those four JS branches exist because GNOME Shell's `Shell.App`/`Gio.DesktopAppInfo` objects expose several historically-redundant ways to start an app; hop's own `Launcher` starts a process directly from a desktop entry's `Exec=` line, so there is exactly one way to do it. `launchOrFocusApp falls back to open_new_window when activate is unavailable`, `... uses appInfo.launch as last fallback` and `... uses app.launch when app is a DesktopAppInfo-like object` are three tests of that same JS-specific ladder; they are not ported individually, and are represented in Task 4 by one test, `falls_back_to_launching_when_no_focusable_window_exists`, that exercises the one fallback this side actually has.

   **Not ported at all — `launchOrFocusApp does not throw when window flag methods require bound this`, and the "`skip_taskbar` as a method vs. a plain boolean" half of the other tests.** Both are artifacts of GNOME Shell exposing some window properties as plain values and others as unbound-`this`-sensitive methods, which is a hazard specific to duck-typed JS objects pulled from a dynamic API. `WindowHandle` (Task 4) is a plain Rust struct with a `bool` field — there is no method-binding hazard for a typed struct to reproduce, so there is nothing here to port; noted so a reviewer does not go looking for it.

**5. Filesystem watching uses the `inotify` crate (ISC); hand-rolled `libc` FFI was considered and declined.** An earlier draft of this plan built the watcher directly on raw `libc::inotify_init1`/`libc::inotify_add_watch` calls specifically to avoid a `deny.toml` edit, reasoning that `libc` was already a workspace dependency and a new license entry was avoidable. That reasoning is not what shipped, on reconsideration: reading inotify's event buffer by hand is not "two syscalls and done" — the kernel hands back a sequence of variable-length C structs whose filename length lives inside the struct itself, which has to be walked by pointer arithmetic that reinterprets raw bytes as `inotify_event` values, squarely inside alignment-and-aliasing territory even when a hand-written parser is careful. Every `unsafe` block in this workspace today is confined to test code, and the codebase's whole posture — bounded newtypes, enforced content rules, no `.unwrap()` in production — argues against hand-rolled FFI for what is, in the end, an off-hot-path bookkeeping task: rebuilding an index when a directory changes, not a place any measured requirement needs raw syscall access.

   The `inotify` crate (0.11.4, verified against its actual published source before writing this plan rather than assumed) is ISC-licensed, and its own dependency `inotify-sys` (0.1.8) is ISC too — one allow-list entry covers both. ISC is a short, maximally permissive license, functionally equivalent to the two-clause BSD license with no copyleft obligation, and both the FSF and the OSI classify it as GPL-compatible; a GPL-3.0-only workspace absorbs it cleanly. `inotify`'s remaining required dependency, `bitflags` (2.x, `MIT OR Apache-2.0`), needs no new entry at all — the existing `MIT` line already covers it, the same way it already covers `serde`, `regex`, `thiserror` and the rest of that dual-licensed family. `deny.toml`'s own comments already describe the process for taking on a new license: an allow-list entry plus a sentence naming the crate, the license, and why it is acceptable — exactly the shape every existing entry (`MIT`, `MPL-2.0`, `Unicode-3.0`) already takes, and exactly what Task 6's Step 2 adds, in the same house style, as a change a reviewer reads and can reverse by deleting one line — unlike an `unsafe` precedent, which is neither reviewable at that granularity nor cleanly reversible once other code leans on it.

   **Which of the crate's two integrations this slice uses, and why.** `inotify` offers two ways to read events: a synchronous `Inotify::read_events_blocking`, and an async `EventStream` from `Inotify::into_event_stream`, built on `tokio::io::unix::AsyncFd` and gated behind the crate's `stream` feature (which pulls in `futures-util` and tokio's `net` feature, and is the crate's *default* feature — this plan explicitly disables it). Task 6 uses the synchronous API, blocked on inside a dedicated `std::thread`, not the async stream. Two reasons: first, `AsyncFd` needs a live Tokio `Handle` at construction, which ties the watcher's start-up to running inside `tokio::runtime::Runtime::block_on`'s dynamic scope — true at `hopd::server::build_host`'s real call site, but not at `crates/hopd/tests/apps.rs`'s test harness, which builds `AppsProvider` (and its watcher) *before* `tests/common::start_daemon` creates its runtime; threading a `Handle` through would mean restructuring the shared harness `host.rs` and `lifecycle.rs` also depend on, for a component nowhere near the latency-sensitive path. Second, and more simply, this watcher's whole job — a blocking read, then a blocking directory scan (`scan_apps`) — has nothing to gain from an async integration regardless: neither operation belongs on a Tokio worker thread, so putting them on one and immediately blocking it would be the wrong trade. A dedicated OS thread blocking on `read_events_blocking` costs one thread for the daemon's lifetime and needs no runtime context at all, which is what keeps `spawn_index_watcher`'s signature — and its call sites in `build_apps_provider` (Task 7) and the test harness (Task 8) — plain, synchronous functions requiring nothing of their caller. `deny.toml` therefore gains no allow-list entry for `futures-util`, and `hopd`'s `tokio` dependency gains no new feature: this plan sets `default-features = false` on the `inotify` dependency with no `features` list at all.

**6. The query path's "no disk read" guarantee is a fact about `AppIndex::query`'s signature and body, proven both by construction and by a runtime test.** `AppIndex::query(&self, term: &str) -> Vec<Item>` takes and returns nothing capable of naming a filesystem path — no `Path`, no `File`, no `PathBuf` — and its body is a `RwLock::read()` over a `Vec<Item>` already resident in memory, then a `filter`/`take`/`clone`. That is inspectable directly: nothing in the function's dependency graph reaches `std::fs`. Task 3 also pins this with a stronger, runtime test: build an `AppIndex` from a real directory, then delete that directory from disk entirely (`std::fs::remove_dir_all`), then query — if `query` touched disk in any way, the query would either error or return nothing; asserting it still returns the original entries is a test a regression (someone routing `query` back through a fresh scan "for freshness") would fail loudly rather than silently.

**7. `AppEntry` carries the `Item` the client sees plus two fields the client never does: a lowercased match haystack, and the sanitized `Exec=` command.** `Item` has no keywords field and no way to carry a launch command — those exist only to make `query()`'s filter and `execute()`'s dispatch possible, and travel in `AppEntry`, the index's own row type, never in anything serialized to a client.

## File Structure

**Created:**
- `crates/hopd/src/apps.rs` — desktop-entry parsing, XDG/flatpak root enumeration, the directory scan, `AppIndex`, the `WindowSource`/`Launcher` traits and their M2 no-op/real implementations, `focus_or_launch`, the inotify watcher, and `AppsProvider` itself.
- `crates/hopd/tests/apps.rs` — the integration test driving `AppsProvider` through the daemon over a real socket (acceptance criterion 7), including a filesystem-event-to-live-query test that exercises criteria 1, 2 and 7 together.

**Modified:**
- `crates/hopd/src/lib.rs` — declare `pub mod apps;`; retire the module-doc sentence that says apps is not registered yet.
- `crates/hopd/src/server.rs` — `build_host` registers `AppsProvider`, built from the real environment, and starts its watcher.
- `Cargo.toml` (workspace root) — add `inotify = { version = "0.11", default-features = false }` to `[workspace.dependencies]`.
- `crates/hopd/Cargo.toml` — add `inotify.workspace = true` to `[dependencies]`.
- `deny.toml` — add `"ISC"` to `[licenses] allow`, with the justification paragraph Task 6 spells out.
- `CONTEXT.md` — a short glossary addition: **Desktop entry** and **App id**, cross-referenced against the existing **Provider host** section's **Registration**.

---

### Task 1: Desktop-entry parsing — pure, no I/O

**Files:**
- Create: `crates/hopd/src/apps.rs`
- Modify: `crates/hopd/src/lib.rs` (add `pub mod apps;` only — leave the module doc fix to Task 7, so this task's diff stays about parsing)

**Interfaces:**
- Produces, for Task 2:
  ```rust
  pub(crate) struct ParsedEntry {
      pub(crate) title: String,
      pub(crate) exec: String,
      pub(crate) icon: Option<String>,
      pub(crate) haystack: String,
  }
  pub(crate) fn parse_desktop_entry(content: &str) -> Option<ParsedEntry>;
  pub(crate) fn app_id_from_file_name(file_name: &str) -> Option<String>;
  pub(crate) struct AppEntry {
      pub(crate) app_id: String,
      pub(crate) item: hop_protocol::Item,
      pub(crate) exec: String,
      pub(crate) haystack: String,
  }
  pub(crate) fn build_entry(app_id: String, parsed: ParsedEntry) -> AppEntry;
  ```

This task is pure: no `std::fs`, no `std::env`. Everything is a function from a `&str` (or a `&str` file name) to a value.

- [ ] **Step 1: Write the failing tests**

Create `crates/hopd/src/apps.rs`:

```rust
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
        assert!(!parsed.haystack.contains('%'), "field codes must not survive into the haystack");
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
        assert!(parsed.is_none(), "a Name= outside [Desktop Entry] must not count");
    }

    #[test]
    fn field_codes_are_stripped_from_exec() {
        let parsed = parse_desktop_entry("[Desktop Entry]\nName=X\nExec=app %f %U --flag %i\n")
            .unwrap();
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
        let parsed = parse_desktop_entry(
            "[Desktop Entry]\nName=X\nExec=x\nIcon=/usr/share/pixmaps/x.png\n",
        )
        .unwrap();
        let entry = build_entry("x".to_string(), parsed).unwrap();
        assert!(matches!(entry.item.icon, Some(IconSpec::Path(_))));
    }

    #[test]
    fn a_bare_icon_name_becomes_the_name_arm() {
        let parsed = parse_desktop_entry("[Desktop Entry]\nName=X\nExec=x\nIcon=utilities-terminal\n")
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd apps::`
Expected: FAIL — the module does not exist until `pub mod apps;` is added to `lib.rs` (next step), and every test name above must then compile and pass.

- [ ] **Step 3: Add the module declaration**

In `crates/hopd/src/lib.rs`, add `pub mod apps;` to the existing module list (alongside `connection`, `runtime_dir`, `server`, `source` — keep whatever ordering convention the file already uses). Do not touch the module-doc paragraph that mentions issue #57 yet; Task 7 does that.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p hopd apps::`
Expected: PASS, every test above.

- [ ] **Step 5: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/hopd/src/apps.rs crates/hopd/src/lib.rs
git commit -m "hopd: parse desktop entries into indexable app entries"
```

---

### Task 2: XDG/flatpak root enumeration and the directory scan

**Files:**
- Modify: `crates/hopd/src/apps.rs`

**Interfaces:**
- Consumes: Task 1's `parse_desktop_entry`, `app_id_from_file_name`, `build_entry`, `AppEntry`.
- Produces, for Task 3 and Task 6:
  ```rust
  pub(crate) fn xdg_application_roots(
      home: Option<&str>,
      data_home: Option<&str>,
      data_dirs: Option<&str>,
  ) -> Vec<std::path::PathBuf>;
  pub(crate) fn flatpak_application_roots(home: Option<&str>) -> Vec<std::path::PathBuf>;
  pub(crate) fn scan_apps(roots: &[std::path::PathBuf]) -> Vec<AppEntry>;
  ```

This is the one task in this plan whose functions genuinely touch disk — `scan_apps` is the "read everything" half that only ever runs at startup and from the watcher thread (Task 6), never from the query path (Task 3's `AppIndex::query`).

**A deliberate design choice, stated up front:** `xdg_application_roots` and `flatpak_application_roots` take `Option<&str>` parameters instead of reading `std::env::var` internally. Rust 2024 made `std::env::set_var` `unsafe` precisely because mutating process-wide environment state races with any other thread reading it, and `cargo test` runs a crate's tests concurrently by default — so a test suite that wants to exercise "what happens when `XDG_DATA_HOME` is set" by mutating the real environment around a call is either flaky or requires serializing every test in the module. Passing the values in sidesteps the whole hazard: these two functions become pure and are tested as such, and only Task 7's single startup wiring function ever calls `std::env::var` for real.

- [ ] **Step 1: Write the failing tests**

Append to `crates/hopd/src/apps.rs`, above the existing `#[cfg(test)] mod tests` block (move the `use` additions into that block's existing `use super::*;`):

```rust
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
    for dir in data_dirs.split(':').map(str::trim).filter(|d| !d.is_empty()) {
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
        assert_eq!(entries.len(), 1, "a missing root must not abort the whole scan");
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
        assert_eq!(entries.len(), 1, "the same app id from two roots must not duplicate");
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd apps::`
Expected: FAIL to compile — `xdg_application_roots`, `flatpak_application_roots` and `scan_apps` are undefined.

- [ ] **Step 3: Run the tests to verify they pass**

The implementation is already written into Step 1's diff (this task, unlike the reference plan's style, writes production code and tests together per function since each is small and the two cannot usefully be separated — the "write the failing test" step is satisfied by the fact that `scan_tests` cannot compile before the three functions above it exist).

Run: `cargo test -p hopd apps::`
Expected: PASS, every test in both `tests` and `scan_tests`.

- [ ] **Step 4: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/src/apps.rs
git commit -m "hopd: enumerate XDG/flatpak application roots and scan them into entries"
```

---

### Task 3: `AppIndex` — the pure, in-memory query path

**Files:**
- Modify: `crates/hopd/src/apps.rs`

**Interfaces:**
- Consumes: Task 2's `AppEntry`, `scan_apps`.
- Produces, for Task 5 and Task 6:
  ```rust
  pub(crate) const QUERY_RESULT_CAP: usize = 50;
  pub(crate) struct AppIndex { /* private */ }
  impl AppIndex {
      pub(crate) fn new(entries: Vec<AppEntry>) -> Self;
      pub(crate) fn query(&self, term: &str) -> Vec<hop_protocol::Item>;
      pub(crate) fn find_by_item_id(&self, id: &hop_protocol::ItemId) -> Option<AppEntry>;
      pub(crate) fn replace(&self, entries: Vec<AppEntry>);
  }
  ```

- [ ] **Step 1: Write the failing tests**

Append to `crates/hopd/src/apps.rs`:

```rust
use std::sync::RwLock;

/// The most items [`AppIndex::query`] returns in one answer. Not a ranking
/// cap and not `hop_protocol::limits::MAX_ITEMS_PER_RESULTS_FRAME` (1 000) —
/// this is smaller and exists only to keep one provider's unranked batch a
/// sane size while issue #103 (wiring `Pipeline::assemble`, and with it a
/// real cap) remains unlanded. See this plan's Scope section.
pub(crate) const QUERY_RESULT_CAP: usize = 50;

/// The apps provider's in-memory index: an [`AppEntry`] list a background
/// watcher (Task 6) keeps current, queried with no disk access at all.
///
/// # No disk read on the query path
///
/// [`AppIndex::query`]'s signature takes and returns nothing capable of
/// naming a filesystem path, and its body is a lock acquisition over an
/// already-resident `Vec` followed by `filter`/`take`/`clone` — nothing in
/// its call graph reaches `std::fs`. `tests::query_still_answers_after_the_
/// backing_directory_is_deleted` below is the stronger, runtime version of
/// that claim: it proves the answer does not change when the disk it was
/// built from is no longer there to be read.
pub(crate) struct AppIndex {
    entries: RwLock<Vec<AppEntry>>,
}

impl AppIndex {
    pub(crate) fn new(entries: Vec<AppEntry>) -> Self {
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
        assert_eq!(index.query("browser").len(), 1, "haystack includes Keywords=");
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
        assert!(index.find_by_item_id(&ItemId::new("app:not-installed").unwrap()).is_none());
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd apps::`
Expected: FAIL to compile — `AppIndex`, `QUERY_RESULT_CAP` undefined.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p hopd apps::`
Expected: PASS, every test in `index_tests`.

- [ ] **Step 4: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/src/apps.rs
git commit -m "hopd: an in-memory app index queried with no disk access"
```

---

### Task 4: Focus-existing-window-else-launch — ported from `appLaunch.js`

**Files:**
- Modify: `crates/hopd/src/apps.rs`

**Interfaces:**
- Produces, for Task 5:
  ```rust
  pub(crate) struct WindowHandle {
      pub(crate) id: String,
      pub(crate) app_id: Option<String>,
      pub(crate) skip_taskbar: bool,
      pub(crate) minimized: bool,
      pub(crate) override_redirect: bool,
  }
  pub(crate) trait WindowSource: Send + Sync + 'static {
      fn windows_for_app(&self, app_id: &str) -> Vec<WindowHandle>;
      fn all_windows(&self) -> Vec<WindowHandle>;
      fn unminimize(&self, window: &WindowHandle);
      fn activate(&self, window: &WindowHandle);
  }
  pub(crate) struct EmptyWindowSource;
  pub(crate) trait Launcher: Send + Sync + 'static {
      fn launch(&self, exec: &str) -> Result<(), String>;
  }
  pub(crate) struct SystemLauncher;
  pub(crate) fn focus_or_launch(
      windows: &dyn WindowSource,
      launcher: &dyn Launcher,
      app_id: &str,
      exec: &str,
  ) -> Result<(), String>;
  ```

This task is independent of Tasks 1–3: `focus_or_launch` and everything it calls take plain strings and trait objects, not `AppEntry` or `AppIndex`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/hopd/src/apps.rs`:

```rust
/// One open window, as much as this M2 slice can describe before the M5
/// GNOME shim (design spec §7) supplies real ones from the compositor.
/// Ported from `appLaunch.js`'s window shape, collapsed to the fields that
/// logic actually reads — see this plan's Design decision 4 for the two
/// fields deliberately not here (a focus-stealing-prevention timestamp,
/// and the method-vs-property duck-typing `skip_taskbar` had in JS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowHandle {
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
pub(crate) trait WindowSource: Send + Sync + 'static {
    /// Windows the app itself is known to own — ported from
    /// `app.get_windows()`. No id-matching is needed for anything this
    /// returns: ownership already establishes it.
    fn windows_for_app(&self, app_id: &str) -> Vec<WindowHandle>;
    /// Every open window in the session — ported from
    /// `global.display.get_tab_list()` — for the fallback heuristic used
    /// only when `windows_for_app` came back empty.
    fn all_windows(&self) -> Vec<WindowHandle>;
    fn unminimize(&self, window: &WindowHandle);
    fn activate(&self, window: &WindowHandle);
}

/// The M2 [`WindowSource`]: no windows exist yet, from either tier. This is
/// what makes [`focus_or_launch`] correctly and unconditionally launch
/// until the M5 GNOME shim replaces this with a real implementation — see
/// Design decision 4.
pub(crate) struct EmptyWindowSource;

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
pub(crate) trait Launcher: Send + Sync + 'static {
    fn launch(&self, exec: &str) -> Result<(), String>;
}

/// The real [`Launcher`]: `exec`'s first whitespace-separated token is the
/// program, the rest are its arguments — `exec` has already had field codes
/// stripped by [`sanitize_exec`] at parse time. Standard streams are
/// discarded and detached from the daemon's own terminal, if it has one; a
/// launched app is not expected to write anything hopd should see.
pub(crate) struct SystemLauncher;

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
    value.strip_suffix(".desktop").map(str::to_string).unwrap_or(value)
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
            self.calls.lock().unwrap().push(("unminimize", window.id.clone()));
        }
        fn activate(&self, window: &WindowHandle) {
            self.calls.lock().unwrap().push(("activate", window.id.clone()));
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
        assert_eq!(*windows.calls.lock().unwrap(), vec![("activate", "w1".to_string())]);
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
            vec![("unminimize", "w1".to_string()), ("activate", "w1".to_string())],
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
        assert_eq!(*windows.calls.lock().unwrap(), vec![("activate", "w1".to_string())]);
    }

    #[test]
    fn falls_back_to_launching_when_no_focusable_window_exists() {
        // Represents the JS suite's three-rung launch fallback
        // (activate/open_new_window/launch/appInfo.launch), collapsed to
        // one Launcher call — see Design decision 4.
        let windows = FakeWindows::default();
        let launcher = FakeLauncher::default();

        assert!(focus_or_launch(&windows, &launcher, "firefox", "firefox --new-window").is_ok());
        assert_eq!(*launcher.launched.lock().unwrap(), vec!["firefox --new-window".to_string()]);
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
            focus_or_launch(&windows, &launcher, "brave-browser.desktop", "brave").is_ok()
        );
        assert_eq!(*windows.calls.lock().unwrap(), vec![("activate", "w1".to_string())]);
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
        assert!(windows.calls.lock().unwrap().is_empty(), "skip_taskbar window must not be used");
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
    fn tier_one_wins_over_tier_two_when_both_have_a_candidate() {
        // A mutation that checked `all_windows` first (or unconditionally,
        // ignoring `windows_for_app`) would still find *a* window in most
        // fixtures — this test uses two different ids to make the tier
        // that answered observable.
        let windows = FakeWindows {
            for_app: vec![window("owned")],
            all: vec![window("scanned")],
        };
        let launcher = FakeLauncher::default();
        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert_eq!(*windows.calls.lock().unwrap(), vec![("activate", "owned".to_string())]);
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

        focus_or_launch(&windows, &launcher, "org.gnome.terminal.desktop", "gnome-terminal")
            .unwrap();
        assert_eq!(*windows.calls.lock().unwrap(), vec![("activate", "w1".to_string())]);
    }

    #[test]
    fn a_tier_two_window_with_no_app_id_never_matches() {
        let windows = FakeWindows {
            for_app: vec![],
            all: vec![window("w1")], // app_id: None
        };
        let launcher = FakeLauncher::default();
        focus_or_launch(&windows, &launcher, "firefox", "firefox").unwrap();
        assert!(windows.calls.lock().unwrap().is_empty());
        assert_eq!(launcher.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn system_launcher_reports_an_empty_exec_rather_than_spawning_nothing() {
        assert!(SystemLauncher.launch("").is_err());
        assert!(SystemLauncher.launch("   ").is_err());
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd apps::`
Expected: FAIL to compile — `WindowHandle`, `WindowSource`, `Launcher`, `focus_or_launch` and friends are undefined.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p hopd apps::`
Expected: PASS, every test in `focus_or_launch_tests`.

- [ ] **Step 4: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/src/apps.rs
git commit -m "hopd: focus-existing-window-else-launch, ported from appLaunch.js"
```

---

### Task 5: `AppsProvider` — the `Provider` implementation

**Files:**
- Modify: `crates/hopd/src/apps.rs`

**Interfaces:**
- Consumes: Task 3's `AppIndex`; Task 4's `WindowSource`, `Launcher`, `focus_or_launch`, `EmptyWindowSource`, `SystemLauncher`.
- Produces, for Task 6 and Task 7:
  ```rust
  pub struct AppsProvider {
      /* private: index: Arc<AppIndex>, windows: Arc<dyn WindowSource>, launcher: Arc<dyn Launcher> */
  }
  impl AppsProvider {
      pub(crate) fn new(
          index: Arc<AppIndex>,
          windows: Arc<dyn WindowSource>,
          launcher: Arc<dyn Launcher>,
      ) -> Self;
  }
  impl hop_core::provider::Provider for AppsProvider { ... }
  ```

- [ ] **Step 1: Write the failing tests**

Append to `crates/hopd/src/apps.rs`:

```rust
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
    pub(crate) fn new(
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
            // comment. `crate::aliases::APPS_PROVIDER_ID`'s docs spell out
            // the silent failure a drift here would cause: every configured
            // app alias would stop boosting anything, with no test failing.
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
        assert!(modes.contains(&Mode::All), "omitting Mode::All means never reached by a plain keystroke");
        assert!(modes.contains(&Mode::Apps), "omitting Mode::Apps means `a <term>` never reaches this provider");
    }

    #[test]
    fn the_manifest_declares_kind_app_and_a_minimum_term_length() {
        let manifest = one_app_provider("X").manifest();
        assert_eq!(manifest.kinds, vec![Kind::App]);
        assert_eq!(manifest.min_term_len, 0, "0 means \"always run\", including for the empty term");
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
    fn registered_with_a_real_host_the_provider_is_selected_for_an_ordinary_and_an_a_prefixed_query() {
        let mut host = hop_core::host::ProviderHost::with_log(Arc::new(hop_core::host::NoopLog));
        host.register(one_app_provider("Firefox")).unwrap();
        // No public "selected_ids" outside hop-core's own tests, so this
        // observes selection through manifests() plus should_query directly
        // — the same predicate ProviderHost::selected calls.
        let manifest = &host.manifests()[0];
        assert!(hop_core::provider::should_query(manifest, &route("firefox")));
        assert!(hop_core::provider::should_query(manifest, &route("a firefox")));
    }

    // --- query(): the pure in-memory lookup. ---

    #[tokio::test]
    async fn query_returns_items_matching_the_routed_term() {
        let provider = Arc::new(one_app_provider("Firefox"));
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
        let parsed = parse_desktop_entry("[Desktop Entry]\nName=Firefox\nExec=firefox --new\n")
            .unwrap();
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
            .execute(ItemId::new("app:firefox").unwrap(), ActionId::new("open").unwrap())
            .await
            .unwrap();
        assert_eq!(outcome, hop_protocol::ExecOutcome::Done);
        assert_eq!(*launcher.calls.lock().unwrap(), vec!["firefox --new".to_string()]);
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd apps::`
Expected: FAIL to compile — `AppsProvider` is undefined.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p hopd apps::`
Expected: PASS, every test in `provider_tests`.

- [ ] **Step 4: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/src/apps.rs
git commit -m "hopd: AppsProvider — the Provider implementation over AppIndex and focus_or_launch"
```

---

### Task 6: The inotify watcher — filesystem events keep the index current

**Files:**
- Modify: `Cargo.toml` (workspace root) — add `inotify` to `[workspace.dependencies]`
- Modify: `deny.toml` — allow the `ISC` license
- Modify: `crates/hopd/Cargo.toml` — depend on `inotify`
- Modify: `crates/hopd/src/apps.rs`

**Interfaces:**
- Consumes: Task 2's `scan_apps`; Task 3's `AppIndex`.
- Produces, for Task 7:
  ```rust
  pub(crate) fn spawn_index_watcher(index: Arc<AppIndex>, roots: Vec<std::path::PathBuf>);
  ```

Read this plan's Design decision 5 before writing this task: it argues for the `inotify` crate over hand-rolled `libc` FFI, and explains why this task uses the crate's synchronous, blocking API on a dedicated thread rather than its tokio-integrated `EventStream`.

- [ ] **Step 1: Add the dependency to the workspace**

In the workspace root `Cargo.toml`'s `[workspace.dependencies]`, add a line (placed next to `libc`, both being low-level OS-facing dependencies):

```toml
inotify = { version = "0.11", default-features = false }
```

`default-features = false` matters: the crate's only feature, `stream`, is enabled by default and pulls in `futures-util` plus tokio's `net` feature for its async `EventStream` API, which this task does not use — see Design decision 5. Disabling it keeps the dependency to exactly `inotify`, `inotify-sys` and `bitflags`, the last already covered by the existing `MIT` allow-list entry.

- [ ] **Step 2: Allow the `ISC` license in `deny.toml`**

In `deny.toml`'s `[licenses]` section, add `"ISC"` to the `allow` array, in alphabetical order (matching the array's existing order: `GPL-3.0-only`, `MIT`, `MPL-2.0`, `Unicode-3.0`):

```toml
allow = [
    "GPL-3.0-only",
    "ISC",
    "MIT",
    "MPL-2.0",
    "Unicode-3.0",
]
```

And extend the comment block directly above that array — which currently documents `MIT`, `MPL-2.0`, `Unicode-3.0` and `GPL-3.0-only` in that order, one paragraph each, aligned to the same label column every existing entry uses — with a new paragraph for `ISC`, inserted before the `MIT` paragraph (matching the array's alphabetical order) and matching the existing paragraphs' indentation exactly:

```
#   ISC           inotify and inotify-sys (crates/hopd's apps-provider
#                 filesystem watcher, issue #57 — installing or removing a
#                 desktop entry must be reflected without restarting the
#                 daemon, which needs a real filesystem-event source).
#                 ISC is a short, maximally permissive license — the same
#                 grant as the two-clause BSD license, with no copyleft
#                 obligation — and both the FSF and the OSI classify it as
#                 GPL-compatible. Both crates offer ISC as their only
#                 license, so, like MPL-2.0 below, this entry exists for one
#                 dependency pair rather than one arm of a choice MIT
#                 already covers.
#   MIT           Permissive and GPL-compatible, and what most of this tree
                   ...
```

(The `MIT` paragraph and everything below it is unchanged — only reproduced above to show where the new paragraph is inserted. Read the actual file before editing; do not retype the existing paragraphs from memory.)

Run `cargo deny check` once, expecting it to still fail (or pass vacuously) until Step 4 actually adds the dependency to `Cargo.lock` — this step alone only makes the allow-list ready for it.

- [ ] **Step 3: Add the dependency to `hopd`**

In `crates/hopd/Cargo.toml`'s `[dependencies]`, add:

```toml
inotify.workspace = true
```

Run `cargo build -p hopd` once to confirm `Cargo.lock` picks up `inotify`, `inotify-sys` and `bitflags` and nothing else (no `futures-util`, no new `tokio` feature — confirming Step 1's `default-features = false` took effect). Then run `cargo deny check` and confirm it is green: this is the check that would have failed had Step 2 been skipped or gotten the license wrong.

- [ ] **Step 4: Write the failing tests**

Append to `crates/hopd/src/apps.rs`:

```rust
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
    WatchMask::CREATE | WatchMask::DELETE | WatchMask::CLOSE_WRITE | WatchMask::MOVED_FROM
        | WatchMask::MOVED_TO
}

/// Opens an inotify instance and adds a watch on every path in `roots` that
/// exists and is readable. A root that does not exist (a never-created
/// `~/.icons`, say) is skipped rather than failing the whole watcher,
/// mirroring [`scan_apps`]'s own tolerance for missing roots. Fails only if
/// *no* root could be watched at all.
fn open_watch(roots: &[PathBuf]) -> io::Result<Inotify> {
    let mut inotify = Inotify::init()?;
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

/// Spawns the background thread that keeps `index` current: an initial
/// build is assumed already done by the caller (`AppIndex::new` from a
/// [`scan_apps`] call), and this thread rebuilds it every time a watched
/// directory changes, forever, until the process exits.
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
///
/// If no root could be watched at all (`open_watch` failing), this logs and
/// returns without spawning anything — `index` then stays at its startup
/// snapshot for the life of the process rather than the daemon refusing to
/// start over a watch failure, matching this crate's existing
/// per-provider-isolation posture (`build_host`'s own doc comment: "a
/// daemon that refuses to start over one misconfigured provider is worse
/// than one that serves the rest").
pub(crate) fn spawn_index_watcher(index: Arc<AppIndex>, roots: Vec<PathBuf>) {
    let mut inotify = match open_watch(&roots) {
        Ok(i) => i,
        Err(err) => {
            eprintln!("hopd: apps provider: could not watch for desktop-entry changes: {err}");
            return;
        }
    };
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match inotify.read_events_blocking(&mut buffer) {
                Ok(_events) => index.replace(scan_apps(&roots)),
                Err(_err) => return,
            }
        }
    });
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
        assert!(!events.is_empty(), "at least one event must be reported for the new file");
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
        let index = Arc::new(AppIndex::new(scan_apps(&roots)));
        assert!(index.query("").is_empty(), "sanity: nothing installed yet");

        spawn_index_watcher(index.clone(), roots);

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
        let index = Arc::new(AppIndex::new(scan_apps(&roots)));
        assert_eq!(index.query("").len(), 1, "sanity: it was indexed at startup");

        spawn_index_watcher(index.clone(), roots);
        std::fs::remove_file(&entry_path).unwrap();

        assert!(
            wait_until(&index, Duration::from_secs(5), |items| items.is_empty()),
            "the removed entry must disappear without the daemon restarting"
        );
    }
}
```

- [ ] **Step 5: Run the tests to verify they fail**

Run: `cargo test -p hopd apps::`
Expected: FAIL to compile — `open_watch`, `spawn_index_watcher`, `Inotify`, `WatchMask` undefined until Steps 1–3 above are applied (this task's steps are ordered dependency-then-code, unlike other tasks' test-then-code order, because nothing here compiles at all without `inotify` in the graph).

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p hopd apps::`
Expected: PASS, every test in `watcher_tests`. These tests use a real inotify instance (via the crate) and real files under a `tempfile::tempdir()` — no mocking of the kernel facility itself, which is what makes them prove acceptance criterion 2 rather than a fake standing in for it.

- [ ] **Step 7: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
`cargo deny check` is the check this task exists to satisfy correctly: it fails without Step 2's `deny.toml` edit, and Step 3 already ran it once — run it again here as part of the full gate, not as a substitute for that earlier check.

Expected: all four green.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml deny.toml crates/hopd/Cargo.toml crates/hopd/src/apps.rs Cargo.lock
git commit -m "hopd: watch application directories with inotify and rebuild the index on change"
```

---

### Task 7: Wire `AppsProvider` into `build_host`

**Files:**
- Modify: `crates/hopd/src/server.rs`
- Modify: `crates/hopd/src/lib.rs`
- Modify: `CONTEXT.md`

**Interfaces:** none new — this task calls what Tasks 1–6 produced from one real-environment entry point.

- [ ] **Step 1: Write the failing test**

Add to `crates/hopd/src/server.rs`'s existing `#[cfg(test)] mod tests` (create one if none exists yet — check the file first; if `server.rs` currently has no test module, add one):

```rust
#[cfg(test)]
mod build_host_tests {
    use super::*;

    #[test]
    fn build_host_registers_both_the_skeleton_and_apps_providers() {
        // Not a behavior test of AppsProvider itself (Task 5 already covers
        // that) — this pins that `build_host` actually calls the wiring
        // function this task adds, so a future edit that adds the function
        // but forgets to call it fails here rather than silently shipping a
        // daemon with no apps provider registered.
        let host = build_host();
        let ids: Vec<_> = host.manifests().iter().map(|m| m.id).collect();
        assert!(ids.contains(&"skeleton"));
        assert!(ids.contains(&hop_core::provider::APPS_PROVIDER_ID));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hopd build_host_registers_both_the_skeleton_and_apps_providers`
Expected: FAIL — only `"skeleton"` is registered today.

- [ ] **Step 3: Add the real-environment wiring function**

In `crates/hopd/src/apps.rs`, append:

```rust
/// Builds a real, environment-backed [`AppsProvider`]: scans the real
/// XDG/flatpak roots once, starts the inotify watcher over them, and wires
/// [`EmptyWindowSource`]/[`SystemLauncher`] as the M2 backends.
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

    let mut roots = xdg_application_roots(home.as_deref(), data_home.as_deref(), data_dirs.as_deref());
    roots.extend(flatpak_application_roots(home.as_deref()));

    let index = Arc::new(AppIndex::new(scan_apps(&roots)));
    spawn_index_watcher(index.clone(), roots);

    AppsProvider::new(index, Arc::new(EmptyWindowSource), Arc::new(SystemLauncher))
}
```

In `crates/hopd/src/server.rs`, modify `build_host`:

```rust
fn build_host() -> ProviderHost {
    let mut host = ProviderHost::with_log(Arc::new(StderrLog));
    if let Err(err) = host.register(SkeletonProvider) {
        eprintln!("hopd: could not register the skeleton provider: {err}");
    }
    if let Err(err) = host.register(crate::apps::build_apps_provider()) {
        eprintln!("hopd: could not register the apps provider: {err}");
    }
    host
}
```

- [ ] **Step 4: Retire the stale module doc**

In `crates/hopd/src/lib.rs`'s module doc, the sentence reading roughly "the only provider registered is the walking skeleton's, until issue #57 lands apps and #58 the calculator" is now half false. Rewrite it to state that the apps provider is registered as of this issue, and that the calculator (#58) is the remaining gap — keep the sentence about `Pipeline::assemble`/issue #103 unchanged, since that gap is still real.

- [ ] **Step 5: Extend `CONTEXT.md`'s glossary**

Add to the existing `## Provider host` section (after the **Log seam** entry, before `## Frames`), matching the file's established style:

```markdown
**Desktop entry** — a `.desktop` file under an XDG application directory
(freedesktop.org's Desktop Entry Specification), the source the apps
provider indexes. **App id** — the desktop entry's file name with its
trailing `.desktop` removed (`firefox.desktop` → `firefox`); the apps
provider's items carry it as `app:<app id>`, which is what `hop-core`'s
alias table also synthesizes for an `app` alias boost — see
`APPS_PROVIDER_ID`'s own docs for why the two must agree.
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p hopd build_host_registers_both_the_skeleton_and_apps_providers`
Expected: PASS.

- [ ] **Step 7: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: all four green.

- [ ] **Step 8: Commit**

```bash
git add crates/hopd/src/apps.rs crates/hopd/src/server.rs crates/hopd/src/lib.rs CONTEXT.md
git commit -m "hopd: register the apps provider in build_host"
```

---

### Task 8: Integration tests over a real socket

**Files:**
- Create: `crates/hopd/tests/apps.rs`

**Interfaces:**
- Consumes: `crates/hopd/tests/common/mod.rs`'s `hello`, `recv`, `send`, `start_daemon`; `hopd::source::HostSource`; `hop_core::host::{HostPolicy, ProviderHost}`; `hopd::apps::{AppsProvider, AppIndex, EmptyWindowSource, SystemLauncher, scan_apps}` — note `apps` must be `pub` (it already is per Task 1's `pub mod apps;`) and `AppsProvider`, `AppIndex`, `EmptyWindowSource`, `SystemLauncher`, `scan_apps` must be `pub` rather than `pub(crate)` for this external test binary to reach them. **Before writing this task's code**, grep `crates/hopd/src/apps.rs` for every `pub(crate)` item this file's `use` lines need and widen each to `pub` — do this as the first sub-step, not as a fix-up after a compile error.

This is the acceptance-criterion-7 test: "an integration test drives the provider through the daemon over a real socket." It follows `crates/hopd/tests/host.rs`'s established shape exactly — plain `#[test]` functions over a blocking `std::os::unix::net::UnixStream`, an in-process daemon from `start_daemon`, no second harness invented.

- [ ] **Step 1: Write the tests**

Create `crates/hopd/tests/apps.rs`:

```rust
//! The apps provider through the daemon, over a real socket: acceptance
//! criterion 7 on issue #57. `apps.rs`'s own unit and `watcher_tests`
//! modules cover the provider's units and the watcher directly; this file
//! covers what a client actually receives.
//!
//! Plain `#[test]` functions driving a blocking
//! `std::os::unix::net::UnixStream` client, matching `lifecycle.rs`'s and
//! `host.rs`'s shape — no `#[tokio::test]` client in this crate's suites,
//! and inventing one here would be a second harness where `tests/common`
//! exists to prevent exactly that.

#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use common::{hello, recv, send, start_daemon};
use hop_core::host::{HostPolicy, ProviderHost};
use hop_protocol::{ClientMsg, DaemonMsg, QueryText};
use hopd::apps::{AppIndex, AppsProvider, EmptyWindowSource, SystemLauncher, scan_apps};
use hopd::source::HostSource;

/// Writes one `.desktop` file into `dir`.
fn write_entry(dir: &std::path::Path, file_name: &str, name: &str) {
    std::fs::write(
        dir.join(file_name),
        format!("[Desktop Entry]\nName={name}\nExec={name}\n"),
    )
    .unwrap();
}

/// A daemon serving a `ProviderHost` with one `AppsProvider` registered over
/// `roots`, built the same way `hopd::apps::build_apps_provider` builds the
/// real one — minus the environment read, since the roots are the test's
/// own tempdir rather than the process's real XDG state.
fn daemon_over(roots: Vec<std::path::PathBuf>) -> common::TestDaemon {
    let index = Arc::new(AppIndex::new(scan_apps(&roots)));
    hopd::apps::spawn_index_watcher_for_test(index.clone(), roots);
    let provider = AppsProvider::new(index, Arc::new(EmptyWindowSource), Arc::new(SystemLauncher));

    let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(hop_core::host::NoopLog));
    host.register(provider).unwrap();
    start_daemon(HostSource::new(Arc::new(host)))
}

fn connect(daemon: &common::TestDaemon) -> UnixStream {
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    hello(&mut stream);
    stream
}

#[test]
fn a_query_over_the_socket_returns_a_real_installed_application() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "firefox.desktop", "Firefox");
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("fire").unwrap(),
        },
    );

    let mut items = Vec::new();
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 1, items: batch, .. } => items.extend(batch),
            DaemonMsg::QueryDone { query_id: 1 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Firefox");
    assert_eq!(items[0].provider, hop_core::provider::APPS_PROVIDER_ID);
}

#[test]
fn the_a_prefix_reaches_the_apps_provider_exclusively() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "firefox.desktop", "Firefox");
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 2,
            text: QueryText::new("a fire").unwrap(),
        },
    );

    let mut items = Vec::new();
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 2, items: batch, .. } => items.extend(batch),
            DaemonMsg::QueryDone { query_id: 2 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(items.len(), 1, "the `a ` prefix must still reach the apps provider");
    assert_eq!(items[0].title, "Firefox");
}

#[test]
fn a_query_that_matches_nothing_still_reaches_a_clean_query_done() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "firefox.desktop", "Firefox");
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 3,
            text: QueryText::new("nonexistent-application-xyz").unwrap(),
        },
    );

    let frame = recv(&mut stream);
    assert_eq!(frame, DaemonMsg::QueryDone { query_id: 3 });
}

#[test]
fn installing_an_app_while_the_daemon_is_running_is_reflected_in_the_next_query() {
    // The strongest available proof, combining acceptance criteria 1, 2 and
    // 7 in one test: a filesystem change, observed through a live daemon,
    // over the real socket, with no restart anywhere in the test.
    let dir = tempfile::tempdir().unwrap();
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);

    write_entry(dir.path(), "newapp.desktop", "Brand New App");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut found = false;
    let mut next_id = 10u64;
    while std::time::Instant::now() < deadline && !found {
        let mut stream = connect(&daemon);
        send(
            &mut stream,
            &ClientMsg::Query {
                id: next_id,
                text: QueryText::new("Brand New").unwrap(),
            },
        );
        loop {
            match recv(&mut stream) {
                DaemonMsg::Results { items, .. } => {
                    found = items.iter().any(|i| i.title == "Brand New App");
                }
                DaemonMsg::QueryDone { .. } => break,
                other => panic!("unexpected frame: {other:?}"),
            }
        }
        next_id += 1;
        if !found {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(found, "an app installed after the daemon started must be found without a restart");
}
```

**One naming loose end to resolve while implementing this step:** the helper above calls `hopd::apps::spawn_index_watcher_for_test`, but Task 6 made `spawn_index_watcher` take ownership of `roots` and return nothing — which is fine for this file too (`spawn_index_watcher(index.clone(), roots)` works as-is, no separate test-only name needed). Delete the `_for_test` line above and call `hopd::apps::spawn_index_watcher(index.clone(), roots)` directly; it is already `pub` from the visibility sweep this task's Step 0 performs. This note exists because a fresh implementer copying the sketch verbatim would otherwise hit an unresolved-name error for a function this plan does not actually ask for — fix it during Step 1, not after Step 2's compile failure.

- [ ] **Step 2: Widen visibility from `pub(crate)` to `pub`**

In `crates/hopd/src/apps.rs`, change `AppsProvider` (if not already `pub` from Task 5 — it was specified `pub` there), `AppIndex`, `AppEntry`, `EmptyWindowSource`, `SystemLauncher`, `WindowSource`, `Launcher`, `scan_apps`, `spawn_index_watcher`, and `build_apps_provider` (already `pub`) to `pub` wherever they are currently `pub(crate)`. Leave genuinely internal helpers (`parse_desktop_entry`, `sanitize_exec`, `truncate_to_byte_boundary`, `app_id_from_file_name`, `build_entry`, `xdg_application_roots`, `flatpak_application_roots`, `WindowHandle`, `is_focusable`, `normalize_app_token`, `window_matches_app`, `find_focusable_window`, `focus_or_launch`, `open_watch`, `watch_mask`, `QUERY_RESULT_CAP`) at `pub(crate)` — this test file does not need them, and the visibility sweep should be no wider than what `tests/apps.rs`'s own `use` lines actually require.

- [ ] **Step 3: Run the tests to verify they fail, then pass**

Run: `cargo test -p hopd --test apps`
Expected: FAIL first on the visibility of whichever item Step 2 has not yet widened (compile errors naming the private item), then PASS once Step 2 is complete and the loose end from Step 1 is resolved.

- [ ] **Step 4: Run the gate**

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```
Expected: all four green — this is the Landing gate for the whole issue.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/src/apps.rs crates/hopd/tests/apps.rs
git commit -m "hopd: integration tests driving the apps provider through the daemon over a real socket"
```

---

## Acceptance criteria coverage (from issue #57)

| Criterion | Where |
| --- | --- |
| A query returns real installed applications from the index | Task 5 — `query_returns_items_matching_the_routed_term`; Task 8 — `a_query_over_the_socket_returns_a_real_installed_application` |
| The index is maintained by filesystem events; installing/removing an entry is reflected without restarting | Task 6 — `installing_a_desktop_entry_is_reflected_in_the_index_without_rebuilding_it_by_hand`, `removing_a_desktop_entry_is_reflected_in_the_index`; Task 8 — `installing_an_app_while_the_daemon_is_running_is_reflected_in_the_next_query` |
| No disk read occurs on the query path | Design decision 6 (the structural argument); Task 3 — `query_still_answers_after_the_backing_directory_is_deleted` (the runtime proof) |
| Focus-existing-window-else-launch semantics match the ported app-launch test suite | Task 4 — `focus_or_launch_tests` (every ported case, plus the divergences recorded in Design decision 4) |
| The provider declares a manifest with its kinds and a minimum term length, and the host honors it | Task 5 — `the_manifest_declares_kind_app_and_a_minimum_term_length`, `registered_with_a_real_host_the_provider_is_selected_for_an_ordinary_and_an_a_prefixed_query` |
| Icons resolve through icon-theme lookup | Design decision 2; Task 1 — `a_slash_prefixed_icon_becomes_the_path_arm`, `a_bare_icon_name_becomes_the_name_arm` |
| An integration test drives the provider through the daemon over a real socket | Task 8 (all four tests) |
| (Issue comment) manifest `id` is `APPS_PROVIDER_ID`, never a literal | Task 5 — `the_manifest_uses_the_shared_apps_provider_id_constant` |
| (Issue comment) item ids match what the alias table synthesizes, pinned via `CheckedItems::check` | Task 5 — `item_ids_are_app_colon_app_id_matching_what_the_alias_table_synthesizes`, `the_providers_own_output_passes_its_own_manifest_checks` |

## Self-review notes

- **Spec coverage.** §3's latency contract shapes Design decision 6 and Task 3's whole test suite; §7 (M5 GNOME shim) is Design decision 4's justification for the `WindowSource` seam; §9's per-provider isolation is inherited unchanged from issue #56's host and needs nothing new here, since `AppsProvider` is an ordinary registered `Provider` like `SkeletonProvider`.
- **Deliberate omissions**, each argued in Scope: dispatching `execute` through the daemon (blocked on #59), icon-theme resolution inside hopd (Design decision 2), ranking (blocked on #103), desktop-file-id subdirectory nesting.
- **The one new dependency and license in this plan:** `inotify`/`inotify-sys` (ISC), added and justified in Design decision 5, with the exact `deny.toml` diff in Task 6 Step 2. Chosen deliberately over a hand-rolled `libc` FFI alternative an earlier draft of this plan carried — see Design decision 5 for why that alternative was declined. This slice introduces no `unsafe` in `hopd`'s own source; the workspace's sole existing `unsafe` block (test-only, in `hop-protocol`) is untouched.
- **Type consistency.** `AppEntry::{app_id, item, exec, haystack}`, `AppIndex::{new, query, find_by_item_id, replace}`, `WindowSource::{windows_for_app, all_windows, unminimize, activate}`, `Launcher::launch`, `focus_or_launch`, `AppsProvider::new`, `spawn_index_watcher` and `build_apps_provider` are used under exactly these names from the task that introduces each through every later task that consumes it — cross-checked while writing this plan, not assumed. `spawn_index_watcher`'s signature (`Arc<AppIndex>, Vec<PathBuf>) -> ()`, a plain synchronous function that spawns its own thread) is unchanged by this revision, which is why Tasks 7 and 8 needed no edits when Task 6's internals moved from raw `libc` to the `inotify` crate.
- **Verified against the actual files, not assumed from the issue text:** `Provider`'s exact signature (`self: Arc<Self>`, owned `Arc<RoutedQuery>` and `QueryCtx`) and `ProviderManifest`'s fields, read from `crates/hop-core/src/provider.rs`; `ProviderHost::register`'s signature and the augmentation rule in `ProviderHost::selected` (why `Mode::All` is necessary and `Mode::Apps` is additionally correct), read from the current `crates/hop-core/src/host.rs` — which has grown past what issue #56's own plan describes (an augmentation rule and `DelayedWideningProvider` test exist in the tree that are not in that plan's text), so this plan cites the file, not the other plan; `CheckedItems::check`'s exact check (kind membership, then provider-string equality against the manifest's `id`), read from `crates/hop-core/src/pipeline.rs`; `IconSpec`/`IconName`/`IconPath`'s exact constructors and the "roots are documented, not enforced" reasoning, read from `crates/hop-protocol/src/content.rs`; `MAX_ITEM_ID` (4 096), `MAX_TITLE` (1 024), `MAX_ICON_NAME` (256), `MAX_ICON_PATH` (4 096), `MAX_PROVIDER_ID` (64), read from `crates/hop-protocol/src/limits.rs`; the exact allow list and comment style in `deny.toml`; `inotify` 0.11.4's actual published source — `Cargo.toml.orig` (dependencies, features, the ISC license field), `src/lib.rs` (the public API and the `stream` feature gate), `src/inotify.rs` (`Inotify::init`, `Watches::add`/`read_events_blocking`'s exact signatures), and `src/stream.rs` (confirming `EventStream` is built on `tokio::io::unix::AsyncFd`, not runtime-agnostic) — fetched and read directly rather than guessed at; `inotify-sys` 0.1.8's ISC license, confirmed separately; `crates/hopd/tests/host.rs` and `lifecycle.rs`'s client-harness shape (plain blocking `#[test]`s, `daemon_with`-style helpers, `connect` setting a 2 s read timeout), read in full so Task 8 does not invent a second harness; the current test count (430, across all crates) obtained by actually running `cargo test --workspace` before writing this plan, not carried over from issue #56's plan unverified (it happens to still be accurate, which was confirmed rather than assumed).

## What I could not verify or fully resolve, for the maintainer's attention

- **The `WatchMask::CLOSE_WRITE` vs. `WatchMask::MODIFY` choice in `watch_mask()` (Task 6) is a judgment call, not something the plan tested both ways.** `CLOSE_WRITE` avoids seeing a `.desktop` file mid-write, but a tool that writes via `write()` without ever closing the descriptor in a way inotify observes (unlikely for normal package-manager or `cp`-style writes, but not something this plan proves impossible) would not trigger a rescan until something else does. Worth a second look if a real-world "installed but not yet indexed" report ever surfaces.
- **The `inotify` crate is new to this workspace's dependency graph, and this is the only issue plan so far to add a `deny.toml` license.** Every fact behind that decision (the exact license strings, the feature-flag shape, why `default-features = false` avoids `futures-util`) was verified against the crate's actual published source rather than its docs.rs prose, but the maintainer should still give the `deny.toml` diff in Task 6 Step 2 a direct read rather than trust this plan's summary of it, since it is the one edit in this plan that changes what the whole workspace — not just `hopd` — is allowed to depend on.
- **`QUERY_RESULT_CAP = 50` (Task 3) is chosen, not derived.** It is comfortably under `MAX_ITEMS_PER_RESULTS_FRAME` (1 000) and larger than the salvaged JS/Rust precedent's 12–24, but nothing in the issue specifies a number, and until #103 wires ranking, this cap is the only thing standing between "the query matches broadly" and "the batch is enormous." Worth revisiting once ranking exists and this cap's job shrinks back to what its own doc comment claims.
- **The M2→M5 `WindowSource` seam (Design decision 4) is this plan's own design, not dictated by the issue.** The two-tier `windows_for_app`/`all_windows` split is what makes the ported test suite portable faithfully; a different M5 implementer might reasonably want a different shape once real compositor data exists. Worth a design review at that point rather than treating this trait as frozen.
