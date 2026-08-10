# Launcher UI/UX survey — precedent for hop's M3 frontend

Date: 2026-08-10
Feeds: GitHub issue #80 ("Design grill: the UX position hop is defending, and the
plugin DX that has to survive the trust model")
Status: Research complete; not yet reviewed or approved

## What this is

A survey of nine keyboard launchers — Raycast, Alfred, macOS Spotlight, GNOME
Shell's built-in search, Ulauncher, Albert, rofi, PowerToys Run, Flow Launcher —
across seven UI/UX dimensions, gathered against hop's actual constraints so the
comparison is usable rather than abstract. hop's constraints, for reference
throughout:

- **GNOME-native**: GTK4/libadwaita, no separate GNOME Shell extension for the
  overlay itself (`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`
  §3, §8).
- **Plugins return typed data only.** The `Item` shape
  (`crates/hop-protocol/src/item.rs`) is `id`, `kind`, `title`,
  `subtitle: Option<String>`, `icon: Option<IconSpec>` (an externally-tagged
  `Name(IconName) | Path(IconPath)` union — never both, never neither),
  `actions: Vec<Action>` (`id`, `kind`, `label`), `default_action`,
  `copy_text: Option<String>`, `append_to_end: bool`, `provider`. No provider
  or plugin draws pixels; the host renders every row from this struct.
- **p95 < 10ms** on the query path, in-memory only — "no disk reads, no
  subprocess spawns, no HTTP, ever" (design spec §3). Providers run under a
  host-enforced budget; a budget miss isolates the provider rather than
  blocking the frame (`crates/hop-protocol/src/../docs/security/2026-08-02-m2-socket-boundary-threat-model.md`,
  actors table, provider row).
- **An eleven-marker router** (`crates/hop-core/src/router.rs`): explicit
  prefixes `w `, `a `, `f `, `:emoji `/`emoji `, `tz `/`timezone `,
  `weather `/`wx `/` weather`-suffix, `$`, `=`, `>` (windows, apps, files,
  emoji, timezone ×2, weather ×3, currency, calculator, actions) are
  **exclusive** — they hide everything else. Bare math, bare currency-shaped
  text and bare city/timezone names are **inferred** and **augment** the
  general list instead of replacing it. `route()` today attaches no UI
  signal to either case — that is a fact about the code, not a design
  decision yet, and dimension 2 below is where this survey bears on it.
- Peer trust at the socket boundary is "which uid," full stop — the protocol
  supplies no finer control (threat model, "Where peer trust comes from").
  Plugins are trusted no further than that until the wasmtime tier (v3)
  lands.

## Method and honesty notes

Research was dispatched to five parallel sub-agents, each briefed on the
constraints above, each required to cite a URL or `repo:path` for every claim
and to write "unverified" rather than guess. Where a launcher is open source
(Ulauncher, Albert, rofi, PowerToys Run, Flow Launcher, GNOME Shell), findings
come from reading the actual source — file paths and line numbers are given
where the agent captured them, understanding that upstream `main` branches
move and a line number is a snapshot as of 2026-08-10. Where a launcher is
closed source (Raycast, Alfred, Spotlight), findings come from official
documentation only; anything from a blog, forum or AI-summarized source is
labeled **[secondary]** inline. A handful of claims from official domains
were captured via a search tool's digest rather than a direct page fetch —
these are flagged **[medium confidence]** rather than presented as verbatim
quotes. The full source list, with primary/secondary marks, is in §7.

Two premises stated in this document's original research brief turned out to
be wrong and are corrected in place rather than silently dropped: Ulauncher's
alt-action is Alt+Enter, not Ctrl+Enter; PowerToys Run has no Alt+Enter
binding at all; Spotlight's reveal-in-app shortcut is Cmd+R, not Cmd+Return;
and PowerToys Run ships a fade-in `Storyboard` in its source that is
currently commented out, so no animation plays despite common belief that it
has one. These corrections are evidence the underlying sources were actually
read, not restated from memory.

---

## 1. Summary

**What the survey actually settles:**

- **A fixed, typed row (icon + title + subtitle, at minimum) is the industry
  default**, not a hop-specific constraint invented under duress. Alfred's
  Script Filter JSON and GNOME Shell's `SearchProvider2` D-Bus contract — the
  two most directly comparable precedents, because both are genuinely
  cross-process, third-party-facing contracts rather than in-process
  conveniences — land on almost exactly hop's shape: id/uid, title, subtitle,
  one icon (name-or-path, or GIcon-or-string-or-pixels for GNOME), a default
  action, and little else. hop is not an outlier for constraining plugins
  this way; it is in the company of the two precedents worth taking most
  seriously.
- **Rich, code-driven plugin rendering (Raycast's React tree, Flow's
  `Lazy<UserControl>`, PowerToys Run's `IconDelegate`) is bought at the direct
  cost of the sandbox**, every time, without exception in this survey. Every
  launcher that lets a plugin draw more than icon+text+text also grants that
  plugin full, unsandboxed process trust (Raycast: real Node.js file/network
  access, by its own security docs; Flow: the `UserControl` path is
  in-process-.NET only; PowerToys Run: in-process C# DLL via
  `AssemblyLoadContext`). No launcher in this survey has found a way to offer
  richer-than-rows rendering *and* keep plugins sandboxed. That tension is
  structural, not an oversight any of them could easily fix — which matters
  because it means hop cannot expect to find a documented trick that avoids
  the same trade.
- **A hard, uniform per-query latency budget with no exceptions is not
  something any competitor has committed to.** Every other launcher either
  accepts blocking, in-process or per-keystroke-subprocess plugin calls as
  routine (Alfred re-invokes an external script process on every keystroke by
  default; rofi's compiled-mode ABI is synchronous with no per-call timeout;
  PowerToys Run explicitly plans UI around slow plugins via its "Immediate
  plugins" vs. "Background execution plugins" wait-time settings) or has no
  visible latency contract at all. hop's `p95 < 10ms` rule is not validated by
  precedent; it's a genuinely load-bearing design choice this survey cannot
  vouch for and the design grill should treat as such.
- **Usage-based ranking is either invisible or, when visible, always paired
  with a user-facing reset/tuning control.** Every launcher that surfaces
  learning to the user (Alfred's usage graph + "clear his Knowledge," Albert's
  memory-decay slider + SQLite log, PowerToys Run's "Selected item weight"
  setting) also gives the user a lever over it. Every launcher that hides it
  (GNOME Shell's `Shell.AppUsage`, rofi's raw frequency-only history file)
  gives the user nothing. There is no launcher in this survey that shows
  ranking *without* a control, or hides ranking *while* offering a reset —
  the two properties travel together, which narrows hop's choice to "both or
  neither" rather than a spectrum.

**What the survey shows is genuinely contested, with real launchers on each
side:**

- **Command-first (Enter does the obvious thing, a secondary menu holds the
  rest) vs. object-first (Alfred's "choose the item, then decide how to act,"
  a 60+-action universal panel).** Every launcher but Alfred is command-first.
  hop's `Item.default_action` + `actions` list is already structurally
  command-first, so this is a "confirm and move on" question rather than an
  open redesign — but it is worth confirming deliberately rather than by
  default, since Alfred's model is a real, shipped alternative, not a
  hypothetical.
- **Does the window resize as results stream in?** Flow Launcher and rofi
  (via its `dynamic` listview property) both do, by default in Flow's case.
  PowerToys Run resizes up to a capped height. Ulauncher and Albert (so far as
  this research found) do not. hop's design spec already commits to "zero
  jank... pending rows reserve their height" (§8a) — i.e., fixed-size slots,
  not a resizing window — which puts hop on the *non*-resizing side of a real
  split, not an uncontested default.
- **How much chrome does mode-switching get?** Answers range from "none
  found" (Flow Launcher — no in-box icon swap or placeholder change located)
  through "placeholder text only" (Alfred, Raycast) to "a dedicated prompt
  label plus per-source coloring and live bang-filtering" (rofi's combi
  mode). hop's eleven markers sit at the high end of this range in raw count
  and currently have zero UI signal in `router.rs` — this is the single
  biggest concrete design gap this survey surfaces, and it is the subject of
  §5's first open question.
- **Where usage-learning legibility should live** is contested by omission
  as much as by disagreement: only two of nine launchers (Alfred, Albert)
  actually expose it, and both are non-GNOME, closed-or-Linux-desktop tools
  without an OS-level HIG constraining them. GNOME Shell — hop's nearest
  native cousin — hides it entirely. hop has no HIG precedent to lean on
  either way here.

---

## 2. Cross-cutting findings

### 2.1 Result list layout

| Launcher | List vs grid | Row anatomy | Density / default count |
|---|---|---|---|
| Raycast | `List` (rows) and sibling `Grid` (image-heavy); `List.Item.Detail` adds a side panel | icon, title (+tooltip), subtitle (+tooltip), right-aligned `accessories` (icon/text/date/Tag) | No documented fixed count; tuned by a "Search Sensitivity" setting (Low/Med/High) |
| Alfred | List only | title (required) + subtitle + icon; no accessories/tags concept | No documented fixed count; "opt to show more or less results" |
| Spotlight | List, with an inline click-to-preview pane; "Top Hit" pinned first | Not itemized beyond title/Top-Hit distinction in official docs | Not documented |
| GNOME Shell | **Hybrid, dispatched by provider type**: app results render as an icon *grid* (`GridSearchResult`, no subtitle at all), every other (D-Bus/remote) provider renders as a *list* (`ListSearchResult`: 24px icon + title + first line of description) | Grid: icon + name label only. List: icon + title + one-line description, grouped under a `ProviderInfo` header naming the source app | Apps capped `maxResults = 6` (`appDisplay.js:1761`); each remote provider list capped `MAX_LIST_SEARCH_RESULTS_ROWS = 5` (`search.js:23`) |
| Ulauncher | List only | icon (40px, 25px if `compact`) + title + optional description + right-aligned `Alt+{jump_key}` shortcut label | **Contested even in-repo**: a maintainer comment says 9 apps/17 files hardcoded (github.com/Ulauncher/Ulauncher/discussions/941 — secondary, dated); current `main`'s `_limit()` instead caps at `len(jump_keys)`, default a 36-character string |
| Albert | Two `QListView`s: one for matches, a second for the alternative-actions view (not a popup) | icon + text + subtext via custom `QStyledItemDelegate` | Default visible count: unverified |
| rofi | List by default (`DEFAULT_MENU_LINES = 15`, `DEFAULT_MENU_COLUMNS = 1`); becomes a true grid purely via theme config (`columns > 1`) | icon + text (+ optional index digit); a "message" area exists but is a **separate global widget**, not a per-row subtitle | 15 rows default; theme-configurable |
| PowerToys Run | List only, virtualized (`VirtualizingStackPanel`) | 3-column row: 48px full-color icon \| title (fuzzy-highlighted) + subtitle (0.6 opacity) \| hover-revealed glyph context-menu buttons; `MinHeight="44"` | User-configurable setting; no fixed default documented |
| Flow Launcher | List only (WPF `ListBox`) | icon (or icon-font "Glyph" + optional badge overlay) \| title + subtitle (subtitle replaced by a `ProgressBar` when set) \| hotkey-number badge (selected row only) | `MaxResultsToShow` defaults to **5** (`Settings.cs`); `ItemHeightSize` default 58px |

**Reading across:** a plain vertical list with icon/title/(subtitle) is the
overwhelming default; grid is either a distinct content mode for
image-heavy data (Raycast) or a theme-level choice over the same data
(rofi), never the default for text results. GNOME Shell is the one launcher
that structurally *mixes* both in a single view, driven by provider type
rather than user choice — worth noting because it is hop's nearest native
precedent and it did not converge on either pure list or pure grid.

### 2.2 Mode/prefix surfacing

| Launcher | Explicit UI signal for "you are in a mode" |
|---|---|
| Raycast | Placeholder text "dynamically changes"; alias match reorders but doesn't chrome; no chip/breadcrumb found |
| Alfred | Placeholder title/subtitle only (author-set per Keyword Input); the workflow's own icon appears on its results; a "Please Wait" subtext during execution. No chip/breadcrumb — official docs describe no such vocabulary at all |
| Spotlight | No leading-prefix mode; real **operator grammar** instead (`kind:`, `date:`, `from:`, `tag:`, boolean `AND/OR/NOT`, `/PDF`-style type narrowing) plus **fixed hotkeys** (Cmd+1..4) that scope to Applications/Files/Actions/Clipboard as a category filter layered on the same query |
| GNOME Shell | **No prefix grammar at all.** "Mode" = which D-Bus provider answered, surfaced only via the `ProviderInfo` header (that provider's own app icon + name) and the list-vs-grid split. Provider *order and visibility* are configurable via GSettings (`org.gnome.desktop.search-providers`), not query syntax |
| Ulauncher | Each extension's trigger keyword is itself a normal searchable result (findable by name, not just by keyword); a slow (>300ms) extension shows a "Loading…" placeholder row carrying *that extension's* icon |
| Albert | Trigger substring is **highlighted inline in the input text itself** — the one launcher in this survey using in-line text styling rather than an icon/placeholder/chip as its signal |
| rofi | The mode's `display_name` renders as the **prompt label**; scripts can also set it live (`\0prompt\x1f<text>`); **combi mode** (merged multi-source view) prefixes each row's text with a colorable `{mode} {text}` label and supports live `!bang` filtering by source |
| PowerToys Run | No in-box chrome found beyond the literal typed keyword still visible in the query text; a separate empty-query "Plugins overview" hint panel lists available keywords when the box is empty |
| Flow Launcher | **No in-box mode indicator found at all** (no icon swap, no placeholder change) — typing `?` opens a separate lookup of all active keywords, filterable; that's the whole mechanism |

**Reading across:** the field splits three ways — (a) *no signal beyond the
typed text* (Flow, PowerToys Run, arguably GNOME Shell for its own search),
(b) *placeholder-text swap* (Alfred, Raycast), (c) *a dedicated, sometimes
colored label plus live filtering* (rofi). Nobody uses a Material-style
"chip" widget. hop, with eleven markers and currently zero signal in
`router.rs`, sits furthest from any of the three groups in marker count
while currently matching group (a) in signal — see open question 1.

### 2.3 Keyboard model

| Launcher | Navigation | Secondary/action menu | Default action |
|---|---|---|---|
| Raycast | Up/Down, Alt+↑/↓ (page), Cmd/Ctrl+↑/↓ (section), Ctrl+N/P | Cmd/Ctrl+K → **Action Panel**: hierarchical, its own fuzzy search, nestable via `Submenu` | Enter = first declared `Action`; Cmd+Enter = second; author-ordered |
| Alfred | Cmd+↓/↑ (file browser in/out), Tab (open actions on selection) | **Universal Actions**: right-arrow in-app, or a system-wide Selection Hotkey on *any* selected text/file/URL, anywhere in macOS. Object-then-verb: choose the item first, then one of 60+ actions | Per-`mods` overrides (cmd/alt/ctrl/shift/fn and combos) can rewrite title/subtitle/arg/icon/valid per modifier |
| Spotlight | Up/Down | No discoverable contextual menu — fixed shortcuts only: Space = Quick Look, hold Cmd = show path, **Cmd+R** (not Cmd+Return) or Cmd+double-click = reveal | Return |
| GNOME Shell | Tab/Shift+Tab, Down; three-level Escape stack (reset search → close app grid → hide overview) | The entry's native `popup-menu` event (right-click / Menu key) opens a **full context menu** (e.g. `AppMenu` for apps), not an inline secondary list | First app match, else first result of the first-*registered* provider with results — apps are always registered first, so there is no cross-provider relevance blend for "the default" |
| Ulauncher | Up/Down/Tab; `Alt+{jump_key}` direct-select (default keys `1234567890a-z`) | **Alt held + Enter** = alt action (not Ctrl+Enter) | Enter/KP_Enter |
| Albert | Shift+Up/Down (input history); holding Super toggles matches↔fallback | Alt (held) or **Ctrl+Return** opens a second `QListView` of alternative actions | Return / Ctrl+O |
| rofi | Extremely configurable `kb-*` bindings (row/page/element nav, mouse bindings too) | `kb-accept-alt` (Shift+Return, e.g. run-in-terminal); `kb-custom-1..19` for script-mode custom keybindings | `kb-accept-entry` (Return, Ctrl+j/m) vs `kb-accept-custom` (Ctrl+Return — run the typed text literally) |
| PowerToys Run | Tab navigates results *and* per-row context buttons | Ctrl+Shift+Enter (run as admin), Ctrl+Shift+U (different user), Ctrl+Shift+E (open folder) — **no Alt+Enter or Ctrl+Enter binding exists** | `Result.Action` |
| Flow Launcher | ListBox cycles; right-arrow *or* right-click (Shift+Enter per the Python-plugin doc — a minor cross-doc inconsistency) opens context menu | `Alt+1..Alt+9,Alt+0` direct-select (modifier configurable: Ctrl/Alt/Space); Ctrl+Backspace up a directory | `Result.Action`/`AsyncAction` |

**Reading across:** direct numeric/letter jump-to-result (Ulauncher's
`Alt+{key}`, Flow's `Alt+{digit}`, rofi's `kb-select-1..10`) is a recurring
pattern hop's design doc does not yet mention explicitly — worth a look
given three of nine competitors ship it. The "hold a modifier to reveal
alt-actions" pattern (Alt or Ctrl+Return in Ulauncher/Albert, Shift+Return in
rofi) versus "a whole separate panel" (Raycast, Alfred) is the other clear
split: hop's spec already commits to "a secondary-action menu key" (§8,
amended to be fully configurable) — closer to the Raycast/Alfred panel model
than to a bare modifier-chord reveal.

### 2.4 Plugin/extension result rendering — the central dimension

| Launcher | Rendering ceiling | Runtime / trust model |
|---|---|---|
| Raycast | Full **React component tree**: `List`/`Grid`/`Detail` (renders CommonMark markdown + a metadata side panel)/`Form` (10 field types) + nested `ActionPanel` with ~15 built-in `Action.*` primitives | Node.js child process; each extension in its **own V8 isolate/worker thread**, capped heap — but explicitly **not sandboxed for file I/O or networking**: "extensions are not further sandboxed as far as policies for file I/O, networking, or other features of the Node runtime are concerned" (developers.raycast.com/information/security). Trust gate is PR/CI review at store-publish time, not a runtime sandbox |
| Alfred | **Script Filter JSON**: `uid, title, subtitle, arg, icon{path,type}, valid, match, autocomplete, type, mods, action, text{copy,largetype}, quicklookurl, variables` — no richer render mode. **This is the closest analog to hop's own `Item` shape in the whole survey** | Any external process (author's language choice), invoked by Alfred and re-run **per keystroke by default** (or self-filtered with a match-mode config); full OS-level trust, no sandbox, no query-path budget of any kind |
| GNOME Shell (`SearchProvider2`) | D-Bus RPC only: `GetInitialResultSet`/`GetSubsearchResultSet` return ID strings; `GetResultMetas` returns a dict per id — `id, name, description, clipboardText`, plus icon as **one of three mutually exclusive forms** (serialized `GIcon`, a `gicon` string, or a raw `icon-data` pixel tuple). **No custom widget is reachable over this interface at all.** This is the strongest real-world precedent for hop's "typed data only" contract, closely structurally resembling `IconSpec`'s name-xor-path union, with a third raw-bitmap fallback GTK needs that hop's socket-transported model does not | Out-of-process D-Bus service, fully OS-sandboxable independent of GNOME Shell itself (Shell imposes no capability restriction of its own — that would be the provider's own confinement, e.g. Flatpak) |
| Ulauncher | Unified `Result` dataclass: `compact, wrap, highlightable, searchable, name, description, keyword, icon, actions: dict`. No richer mode exists | **Separate OS process per extension**, communicating over **WebSockets** — full filesystem/network access, zero capability sandbox, but a misbehaving extension **cannot crash the launcher** (process boundary). Soft 300ms "Loading…" placeholder, hard 10s timeout |
| Albert | `albert::Item`: `id, text, subtext, icon(), actions(), inputActionText()` — plus an **observer pattern** letting an item's displayed data mutate live after creation. Icon system is unusually rich for a typed model: theme/file/file-type/Qt-standard/"grapheme" (monochrome glyph tinted to the palette)/"iconified" (badge)/"composed" (overlay) | **The worst isolation in this survey.** C++ plugins are native Qt plugin **shared libraries `dlopen`'d directly into the Albert process** — full native trust, same address space, zero sandbox. Python plugins are *also* in-process, embedded via `pybind11::embed` (confirmed by a GIL-related crash report, github.com/albertlauncher/albert/issues/1402) rather than run as subprocesses. A plugin crash of either kind can take down the whole launcher — this is the negative precedent hop's wasmtime roadmap is explicitly positioned against |
| rofi | **Two tiers.** (a) Compiled-mode ABI (`struct rofi_mode`, `include/mode-private.h`, `ABI_VERSION 7`): the rendering hook (`_get_display_value`) returns **one** `char*` (plain text or Pango markup + optional inline-style attributes) and `_get_icon` returns **one** icon surface — strictly (icon, one styled text string) per row, no widget slot. (b) `rofi-script` mode: an arbitrary out-of-process executable drives the same fixed shape over a line-oriented stdin/stdout protocol (`\0key\x1fvalue` fields for `icon`, `display`, `meta`, `nonselectable`, etc.) | (a) is `dlopen`'d native code, in-process, full trust, zero sandbox. (b) is out-of-process but **fully blocking and synchronous** — rofi waits for the script's stdout with no async model and no per-call timeout in the plugin ABI itself |
| PowerToys Run | `Result`: `Title, SubTitle, IcoPath, Icon (IconDelegate — a programmatically generated `ImageSource`, richer than a static path), Glyph+FontFamily, Action, Score, SelectedCount, LastSelected, ContextData, TitleHighlightData, ToolTipData (title+body, the closest thing to a "preview"), QueryTextDisplay`. **No markdown or arbitrary-widget preview mode exists** | **Fully in-process.** `AssemblyLoadContext.LoadFromAssemblyPath` loads the plugin DLL directly into the host process — isolation is for *unload capability* only, not security. `PluginManager` filters to `Language == "CSHARP"` exclusively; the vestigial `AllowedLanguage.Executable` (inherited from ancestor project Wox, which *did* support out-of-process script plugins) has **zero live call sites today** — PowerToys Run deliberately dropped the out-of-process tier, ending up *stricter in scope* (C#-only) but *less isolated* (no process boundary at all) than rofi |
| Flow Launcher | **Two tiers, by language.** (a) .NET plugins: `IPlugin`/`IAsyncPlugin`, in-process, full trust — and critically, `Result.PreviewPanel` is typed **`Lazy<UserControl>`**, so an in-process plugin can hand Flow an arbitrary custom WPF widget for the preview pane. This is the one place besides Raycast where a plugin can draw more than typed fields, and it is available *only* to the in-process tier for the same reason Raycast's isn't sandboxed — a widget object cannot cross a process boundary. (b) Non-.NET (Python/JS) plugins: JSON-RPC over a subprocess — V1 spawns a fresh process per call; **V2 keeps one long-lived interpreter process** over `System.IO.Pipelines`/`StreamJsonRpc`-style duplex pipes. Wire schema: `{"Title","SubTitle","IcoPath","ContextData","JsonRPCAction":{"method","parameters"},"score"}` — `JsonRPCAction` is the serializable stand-in for a C# `Action` delegate. **This out-of-process tier is strictly the fixed row shape**, same ceiling as Alfred/GNOME/hop; only the in-process tier gets the `UserControl` escape hatch | Split trust, cleanly divided by language and process boundary — the tiered structure most directly analogous to hop's own roadmap (v2 Tier 1 TS sidecar vs. v3 Tier 2 wasmtime) |

**Reading across, this is the finding that matters most for hop:** every
launcher in this survey draws the same line in the same place. The moment a
plugin can render more than a fixed set of typed fields, it is also running
with full, unsandboxed process trust — Raycast's Node isolate, Flow's
in-process .NET, PowerToys Run's loaded assembly, Albert's `dlopen`'d
shared library. The launchers that keep plugins constrained to typed rows
(Alfred, GNOME `SearchProvider2`, and the out-of-process halves of Ulauncher
and Flow) are exactly the launchers where a real process or protocol
boundary exists. hop's `Item` contract puts it in the second group by
design, and no launcher here has found a way to be in both groups at once.

### 2.5 Empty / loading / no-results states

| Launcher | Before typing | Slow provider | No results |
|---|---|---|---|
| Raycast | Pinned Favorites → recents → today's calendar events | `isLoading` boolean → a loading bar; the default `EmptyView` is explicitly suppressed while `isLoading` is true, to avoid a documented "flickering empty state" bug | Overridable `EmptyView` (title/description/icon/actions) |
| Alfred | Author-set placeholder title/subtitle (optional; without it, nothing shows until a query is typed) | "Please Wait" subtext during script execution | Not explicitly documented beyond global fallback searches (Google/Wikipedia/Amazon) for the base app |
| Spotlight | Not documented | Not documented ("results appear as you type" is the only stated behavior) | Leans toward suggesting web-search variations rather than a bare empty label |
| GNOME Shell | Search isn't "active" until text is typed at all — the overview shows its normal window-picker page instead | Centered spinner + literal "Searching" label, shown **only while zero providers have any result yet**; vanishes the instant *any* result lands, even if other providers are still running — no per-provider loading indicator | Same status container, label swaps to "No results" |
| Ulauncher | **Off by default** — `max_recent_apps = 0`, so no recents view unless the user opts in | Debounced (300ms) "Loading…" placeholder row carrying the extension's own icon; hard 10s timeout with a "{name} failed to start" fallback | Results container is simply **hidden** — no message shown at all |
| Albert | Fallback handlers can show default content (e.g. web search) even on an empty query — opposite default philosophy from Ulauncher's blank-by-default | Not found — no debounce/placeholder mechanism located | Fallback handlers, same mechanism as the empty-query case |
| rofi | Full unfiltered entry list shown immediately (e.g. `drun` shows every desktop app) — no separate "empty" concept | **None** — the plugin ABI is synchronous, so a slow compiled mode blocks the whole UI; `-refilter-timeout-limit` (300ms) only delays *refiltering*, not query latency | List simply renders zero rows; the message widget is opt-in per mode, not a rofi-wide feature |
| PowerToys Run | Results list `Collapsed`; an optional "Plugins overview" hint panel | `IDelayedExecutionPlugin` (fast pass, then a slow pass) + `IResultUpdated` async push; no explicit spinner found | Same `Collapsed` list — **no dedicated "no results" text/XAML found anywhere** in the repo (grepped) |
| Flow Launcher | Real `IAsyncHomeQuery`/`HomeQueryAsync` mechanism — plugins can supply results before any typing; opt-out via `HomeDisabled` | A synthesized placeholder result, "`{plugin} is still initializing`", with an `Action` that re-queries once ready; `StartLoadingBar`/`StopLoadingBar` RPC calls | Not found in source or docs — unverified |

**Reading across:** the "before typing" state is where the survey is most
split — off-by-default (Ulauncher), on-by-default via recents/favorites
(Raycast, Flow, and implicitly Alfred's default-results view), or the
concept simply not existing until a keystroke lands (rofi, GNOME Shell).
hop's design spec already commits to "Empty-query view: recent/frequent
items from learning" (§8), putting it in the Raycast/Flow camp. On
no-results, hop's spec commits to "no results (suggest web search, never a
blank void)" (§8a) — notably *stronger* than half this survey, where "just
show nothing" (rofi, PowerToys Run, Ulauncher) is a real, shipped answer,
not a bug.

### 2.6 Ranking and learning legibility

| Launcher | Visible to user? | User control |
|---|---|---|
| Raycast | "Root Search learns from you" is stated directly in the manual, without a numeric/graphical breakdown | Explicit **pinning** ("Add to Favorites", reorderable) and **aliases** (force-prioritize a command), independent of learned ranking. No documented per-item ranking reset |
| Alfred | **Yes** — "the usage graph you see in Alfred's preferences" | Global **"clear his Knowledge"** reset (Advanced Preferences); "Keyword Latching" can be disabled entirely. No confirmed *per-item* reset — only community forum chatter, inconclusive |
| Spotlight | No — ranking signals undocumented beyond the "Top Hit" label implying *some* learning | Categories can be toggled on/off but **not reordered** (checked directly against docs — contradicts a common assumption). No reset control found |
| GNOME Shell | **No** — app search is usage-ranked via `Shell.AppUsage.compare()` (native C; exact weighting formula unverified from JS source) but this is completely unexplained/unexposed in the UI | None found. (Dash "favorites" is a separate, user-driven pinning mechanism, unrelated to search ranking) |
| Ulauncher | Query-time match ranking is **pure fuzzy text score** — no evidence found of frecency re-ranking of actual search matches, contrary to common assumption. A separate, **opt-in and off-by-default** "frequent apps" home-screen view exists (unrelated to in-query ranking) | N/A for in-query ranking (none exists); a `feat/frequency-ranking-reworked` branch name suggests this is being actively built |
| Albert | **Yes, fully implemented and visible.** SQLite-backed (`activation` table: timestamp, query, extension_id, item_id, action_id), exponential memory-decay scoring | User-tunable **"memory decay" slider** (default 0.5) + **"prioritize perfect matches"** checkbox (default on) in Settings; `clearActivations()` exists at the DB layer (whether wired to a visible button is unverified) |
| rofi | **No** — a flat-file, pure **frequency** counter (not recency), invisible in the UI | Per-entry removal only (`kb-delete-entry`, Shift+Delete); no "clear all" UI command found — only manual deletion of the cache file |
| PowerToys Run | Partially — the *mechanism* (score + `SelectedCount`/`LastSelected`) is user-tunable via a named setting even though the raw numbers aren't shown | **"Selected item weight"** setting (default 5, settable to 0 to fully disable usage-based reordering) — the most direct "turn it off" control in the survey; a built-in **History** meta-plugin (`!!`) separately surfaces "results selected in the past" as its own searchable list |
| Flow Launcher | Partially — `Score.MaxScore` doubles as a documented **"pin to top"** mechanism, and `AddSelectedCount` lets a plugin opt a specific result *out* of the usage bump | Per-plugin manual **`Priority`** weight setting (doc-confirmed). No confirmed per-item "reset ranking" UI found |

**Reading across, the pattern is binary rather than a spectrum:** every
launcher that shows the user anything about ranking (Alfred, Albert, and to
a lesser extent PowerToys Run/Flow) also gives them a lever over it
(reset, weight, decay slider, disable toggle). Every launcher that hides
ranking (GNOME Shell, rofi, Spotlight, Ulauncher's in-query score) gives
them nothing. hop already tracks per-query and global frequency with
30/90-day decay (design spec §4, §11) and its threat model already treats
the learning store as sensitive enough to hash unrecognized ids (threat
model, Decision 2) — the ingredients for an Alfred/Albert-style legible
control already exist; nothing in the design spec commits to exposing them
to the user yet. See open question 4.

### 2.7 Window presentation

| Launcher | Size / position | Icon color | Animation | Resizes with results? |
|---|---|---|---|---|
| Raycast | Compact Mode collapses to just the search bar at empty query, expands on typing **[secondary/medium-confidence — captured via search digest, not a direct fetch]**; no documented default pixel size/position | No documented monochrome/symbolic rule; extension icons are required to be a custom branded 512×512 PNG (full-color norm implied) | Not documented | Implied by Compact Mode, not independently confirmed |
| Alfred | **Center-top** of screen by default, "much like Spotlight's window... Standard mode"; user-repositionable via a grid, per-monitor choice | No explicit rule; `icon.type: "fileicon"` pulls the real macOS file icon, implying full-color is the norm | Not documented | Not documented |
| Spotlight | **Floating, user-draggable and resizable** — explicitly *not* a fullscreen takeover; exact default size/position not documented | Not documented | Not documented | Not documented (results update per-keystroke, but the resize/animation behavior itself isn't documented) |
| GNOME Shell | **Full-screen modal overview takeover** — the overview, not a small window; `ANIMATION_TIME = 250`ms | Search-entry chrome uses **symbolic** icons (`edit-find-symbolic`); app/content result icons are **full-color** — this is GNOME HIG's own documented pattern (developer.gnome.org/hig/guidelines/ui-icons.html: "Symbolic... is the standard for GNOME UI icons... Full-color icons are primarily used for app icons") made concrete in GNOME's own search implementation | 250ms overview transition | N/A (not a resizable window in the launcher sense) |
| Ulauncher | Undecorated, non-resizable, always-on-top; default width 750px, height auto-fit content; **not centered** — horizontally centered but **10% down from the top** of the monitor | Full-color via `GdkPixbuf`, no forced grayscale | None — opacity flips 0→1, not tweened | No (fixed width; height auto-fits, doesn't animate) |
| Albert | Center-of-screen by default, position configurable **[secondary only — primary source not located]** | Flexible per-plugin: theme-adaptive monochrome "grapheme" icons *or* full-color images, author's choice | Not found | Not found |
| rofi | Almost entirely **theme-driven** (`-location` 9-anchor grid, `-monitor` incl. "at mouse position"/"focused window's monitor", `-theme-str` can force fullscreen) | Full-color via cairo/`gdk_pixbuf`, no forced desaturation | **None in rofi itself** — its own docs explain how to *disable the compositor's* animation for it (a Hyprland `layerrule` example) | Opt-in via the `dynamic` listview theme property; off unless a theme sets it |
| PowerToys Run | Fixed width 640px, `SizeToContent="Height"` (dynamic, capped by `MaxHeight`); horizontally centered, vertically **1/4 down** from the top of the working area by default, or restored to the user's last dragged position | Full-color 24×24 app icons; **only the context-menu buttons use monochrome icon-font glyphs** — same chrome-vs-content split as GNOME HIG | A fade-in `Storyboard` **exists in source but its `.Begin()` call is commented out** — no animation currently plays, despite common belief otherwise | **Yes**, up to `MaxHeight` |
| Flow Launcher | Settings window 1000×700 (not the launcher bar); query box 42px; bar height driven by `SizeToContent`. Position: `SearchWindowScreen`×`SearchWindowAlign` enums, defaulting to **Center, on the monitor under the cursor** | Full-color by default, optional icon-font "Glyph" mode (default on, prioritized over bitmap when supplied) + small badge overlay | `UseAnimation` (default **on**) + configurable speed — the one launcher here that animates by default without qualification | **Yes, by default** — grows with results up to `MaxResultsToShow`, then scrolls; a "fixed window size" setting is an explicit opt-*out* |

**Reading across:** GNOME Shell's own search is the single largest outlier
in this table — a fullscreen modal takeover, categorically different from
every other launcher surveyed (all of which are small floating/anchored
windows). hop's ~400×500px compact overlay (design spec §8a) is a
deliberate divergence from its nearest native precedent, not an extension of
it — worth stating plainly for the design grill rather than assuming GNOME
Shell licenses hop's window shape by association. On icon color, GNOME
HIG's symbolic-for-chrome/full-color-for-content rule is independently
echoed by PowerToys Run's own chrome/content split, which is a real
cross-ecosystem convergence hop can adopt with confidence, distinct from the
window-shape question.

---

## 3. Per-launcher notes

Only what the cross-cutting tables above could not hold.

**Raycast.** Command manifests declare a `mode`: `"view"` (pushes a
full-screen component), `"no-view"` (headless, can run on an `interval`), or
`"menu-bar"` (returns a Menu Bar Extra) — a three-way execution-shape split
hop's own provider model doesn't currently distinguish
(developers.raycast.com/information/manifest). Extension review before
publishing is manual/community + CI checks, with "future automated static
security analysis planned" but not yet shipped
(developers.raycast.com/information/security) — i.e. even Raycast's own
roadmap treats "review, not sandbox" as a stopgap.

**Alfred.** The Script Filter's `Queue Mode`/`Queue Delay`/`rerun` fields
exist precisely because Alfred re-invokes the whole script on every
keystroke by default — these are the author's tools for taming a
per-keystroke-subprocess model, not add-on features. Alfred's own docs
describe its action model as inverted relative to "typical" launchers:
object, then verb.

**GNOME Shell.** `RemoteSearchProvider.filterResults` already separately
preserves "regular" and `"special:"`-prefixed result ids up to independent
caps (`remoteSearch.js:245-253`) — i.e., GNOME Shell already distinguishes
an action-like result sub-kind within one flat id list, a real precedent for
hop's `Kind::Action`. Provider identity is entirely borrowed from the
registering app's own `.desktop` file (name, icon) — there is no
provider-authored branding independent of that app.

**Ulauncher.** Extensions are discoverable two ways at once: by their
registered keyword, and by the trigger's own display name as an ordinary
search result — worth noting because it's a low-cost answer to "how do users
find a mode they don't remember the prefix for," a problem hop's eleven
markers will also have.

**Albert.** The `Icon` abstraction's "grapheme" mode (a glyph/emoji
tinted to the current palette's text color) is a genuinely elegant answer to
"how does a plugin icon stay theme-correct without being told the theme" —
worth a look independent of Albert's poor process-isolation story, since the
two are separable ideas.

**rofi.** `combi` mode's live `!bang` source-filtering (typing `!w` to
restrict a merged view to window results) is a second, informal prefix
grammar layered on top of rofi's regular mode-cycling — evidence that even a
launcher with a dedicated mode-switch key still finds users wanting inline,
typed scoping.

**PowerToys Run.** The distinction between "Immediate plugins" and
"Background execution plugins," each with its own configurable wait-time
setting, is PowerToys Run's explicit acknowledgment that some plugins are
slow and the UI should be built around that rather than against it — the
opposite starting premise from hop's uniform budget.

**Flow Launcher.** Its two-tier plugin model (in-process .NET with a
`UserControl` escape hatch vs. out-of-process JSON-RPC strictly limited to
the fixed row shape) is the most direct real-world precedent for the shape
of hop's own roadmap split between a future trusted tier and the sandboxed
wasmtime tier — not a conflict with hop's constraints so much as a working
existence proof that the split itself is a viable product shape.

---

## 4. Conflicts with hop's constraints

Every pattern below is genuinely attractive on its own terms and genuinely
incompatible with at least one of hop's three constraints — sandboxed
typed-data-only plugins, the p95<10ms query budget, or GNOME-native
convention. Listed so the design grill can see exactly what is being
declined and why, rather than declining it by omission.

**(a) Sandboxed plugins that cannot draw UI**

- **Raycast's React component tree** (`List`/`Grid`/`Detail`/`Form` +
  `ActionPanel`) is the headline case the research brief anticipated, and
  it's confirmed at the trust-model level, not just the API level: Raycast's
  own security docs state extensions get real Node.js file/network access,
  unsandboxed. Attractive because it lets a plugin author build a genuinely
  good calendar/GitHub/Linear integration; incompatible because it requires
  exactly the unsandboxed process trust hop's whole plugin roadmap (v3
  wasmtime tier) exists to avoid.
- **Flow Launcher's `Result.PreviewPanel: Lazy<UserControl>`** is the same
  problem in miniature — a single field, gated to the in-process .NET tier
  only, for exactly the reason a widget object can't cross a process
  boundary. It demonstrates the trade is architectural, not a missing
  feature: Flow's *own* out-of-process (JSON-RPC) tier does not get this
  field, because it can't.
- **PowerToys Run's `IconDelegate`** (a plugin hands back a programmatically
  generated `ImageSource`, not just a path) is a smaller version of the same
  tension — richer than hop's `IconSpec` name-xor-path union, and only
  possible because the plugin is a loaded assembly in the same process, not
  a sandboxed peer.
- **Albert's native `dlopen`'d C++ plugins and in-process-embedded Python**
  are the negative case, not a feature to imitate: zero sandbox, shared
  address space, a plugin crash takes the whole launcher down. This is
  explicitly the failure mode hop's threat model's provider-panic-isolation
  work (`ProviderHost`, issue #56) and the wasmtime roadmap are designed
  against — worth citing precisely because it shows what "no sandbox" costs
  in practice, not just in theory.
- **GNOME Shell's own in-process local-extension path**
  (`createResultObject` letting a *trusted, first-party* JS extension
  subclass `SearchResultsBase`'s widget hierarchy) is a subtler version:
  even GNOME's own precedent gives its most-trusted tier (code running
  inside the Shell process) more render freedom than its D-Bus tier. hop's
  current design holds *every* provider — built-in and third-party alike —
  to the same `Item` contract with no richer in-process escape hatch. That
  is a stricter position than GNOME's own, and worth being explicit that
  it's a choice, not an oversight (see open question 2).

**(b) The 10ms query-path budget**

- **Alfred's per-keystroke external-process re-invocation** is fundamentally
  incompatible with any sub-10ms guarantee — spawning a process and reading
  its stdout on every keystroke is, at minimum, single-digit milliseconds of
  OS overhead before the script has done anything, and Alfred's own
  `Queue Mode`/`Queue Delay`/rerun settings exist because this is
  acknowledged to be slow.
- **rofi's synchronous, un-timeout'd compiled-mode ABI** — `_get_display_value`
  and `_get_num_entries` are plain blocking function calls invoked from the
  render loop, with no async facility and no per-call timeout in the plugin
  ABI itself. A slow mode blocks the entire UI. This is the opposite of
  hop's "a budget miss logs and isolates, never blocks the frame" rule
  (design spec §3).
- **PowerToys Run's explicit design-for-slowness**: "Immediate plugins" vs.
  "Background execution plugins" with configurable wait-time settings, and a
  50ms-informal-warning-not-enforcement threshold in `PluginManager`, is a
  host built around the assumption that plugins are sometimes slow and the
  UI should accommodate that gracefully — a coherent, shipped alternative
  philosophy to hop's "never block, isolate on budget miss," not a lesser
  version of it.
- **GNOME Shell's own search debounces at 150ms** before dispatching to
  providers at all (`search.js:809`), and imposes no visible hard per-provider
  deadline beyond that debounce — even hop's nearest native precedent does
  not operate under anything like a 10ms contract.

**(c) GNOME-native conventions**

- **GNOME Shell's own search surface is a fullscreen modal overview**, not a
  compact floating window. hop's ~400×500px overlay (design spec §8a) is
  explicitly *not* built on this precedent — worth naming plainly, because
  "GNOME-native" could be read as "matches GNOME Shell's own search," and it
  does not, by design. The convention hop is actually inheriting from GNOME
  is the HIG's *icon* and *motion* guidance (symbolic-for-chrome,
  full-color-for-content, restrained animation), not its window model —
  and that icon convention happens to be independently corroborated by
  PowerToys Run's chrome/content icon split, a non-GNOME source arriving at
  the same rule for unrelated reasons.
- **Full-color icons as the unqualified default** (Raycast, Alfred, Ulauncher,
  rofi, PowerToys Run's app icons, Flow Launcher) is the norm across this
  survey; GNOME HIG's monochrome-for-chrome rule is a GNOME-specific
  overlay on top of that norm, not a norm any competitor already follows
  wholesale. hop's `IconSpec` supports both a theme name (which can resolve
  to a symbolic icon) and a raw path (typically full-color) but the *design*
  spec does not yet state which UI elements must be symbolic — a gap worth
  closing explicitly given it's the one HIG rule with real cross-ecosystem
  support (see open question 6).

---

## 5. Open questions for the design grill

Framed as tensions the survey surfaces without resolving, for issue #80.

1. **Does eleven exclusive/inferred markers need more UI signal than
   "type it and see"?** `router.rs` today attaches zero UI treatment to
   any of its eleven markers — no chip, no placeholder swap, nothing.
   Every competitor surveyed does *something*, but the range runs from
   "nothing beyond the typed text" (Flow Launcher, PowerToys Run) to "a
   dedicated, sometimes colored prompt label plus live filtering" (rofi's
   combi mode). hop has more exclusive markers than any single competitor
   in this survey and a genuine confusability risk baked into the router's
   own docs (`w ` vs `wx ` reach different modes on one added character).
   Is "type it and trust the results" — the choice of the two launchers with
   the fewest markers — still the right bet at eleven markers, or does the
   marker *count* itself argue for visible feedback regardless of what
   competitors with fewer markers needed?

2. **Is being the most render-constrained launcher in the survey the right
   place to stand, or does something need to sit between "typed rows" and
   "arbitrary code"?** Every launcher that offers richer-than-rows
   rendering also grants full process trust; no exceptions were found. hop's
   sandboxed-plugin roadmap is the explicit differentiator (design spec §1),
   which argues for holding the line at typed data. But Raycast's `Detail`
   component — markdown *text*, not arbitrary code — is a middle ground none
   of the fully-sandboxed precedents (Alfred, GNOME `SearchProvider2`) offer
   either. Is a markdown-only detail view (data, not code, so plausibly
   compatible with a sandbox) worth reserving space for on hop's roadmap, or
   is "title/subtitle/icon/actions, full stop" the position worth defending
   without qualification?

3. **What happens to a genuinely slow-but-valuable provider under a
   categorical budget-miss-isolates rule?** PowerToys Run, Alfred and rofi
   all accept slow plugins as routine rather than exceptional — PowerToys
   Run goes as far as giving them a named settings category. hop's rule
   (§3: "a budget miss logs and isolates, never blocks the frame") protects
   every *other* provider's latency at the cost of quietly starving a slow
   one into never showing up. Is there a middle tier — visibly slow but not
   isolated, the way GNOME Shell's own remote providers stream in
   asynchronously without a hard per-provider deadline — or is "isolate,
   don't degrade the budget" non-negotiable even for a provider a user
   explicitly wants?

4. **Should ranking legibility be a v1 commitment, given the pattern that
   visibility and control always travel together?** hop already computes
   per-query and global frequency with 30/90-day decay and already treats
   the learning store as sensitive (threat model Decision 2's hashing
   rule). No launcher in this survey shows ranking *without* also offering a
   reset/tuning control, and none hides it while offering one — meaning hop's
   real choice is binary (build both, or ship neither) rather than a matter
   of degree. Given GNOME Shell — hop's nearest native cousin — chose
   "neither," and Alfred/Albert (both non-GNOME) chose "both," which
   precedent does a GNOME-native, trust-conscious launcher actually align
   with?

5. **Is hop's command-first `default_action`/`actions` model worth
   re-litigating against Alfred's object-first Universal Actions, or is this
   a "confirm and move on" question?** Every launcher but Alfred converges on
   command-first (Enter does the obvious thing, a secondary menu holds the
   rest); hop's `Item` shape is already structurally command-first. Alfred's
   model is a real, shipped, well-documented alternative — not a
   hypothetical — but adopting it now would mean restructuring the `Item`/
   `Action` contract rather than building on it. Is there anything in
   Alfred's 60-action, choose-then-verb pattern worth stealing piecemeal
   (e.g., a global "act on the current selection" surface independent of
   search) without abandoning command-first as the primary model?

6. **Which specific GNOME HIG rules does hop's overlay actually inherit, and
   which does it deliberately break?** Design spec §8a already states the
   overlay is "not an Adwaita dialog" and deliberately non-stock where
   identity demands it, while also being "HIG-informed where it serves." The
   window-shape question is already settled (compact, not GNOME Shell's
   fullscreen model) — but icon color (symbolic-for-chrome vs.
   full-color-for-content, independently corroborated by PowerToys Run) and
   motion restraint are HIG rules with real cross-ecosystem support this
   survey found. Is there a documented, explicit list of which HIG
   guidelines bind hop's frontend and which don't, or does that stay ad hoc
   through M3's design pass?

7. **Does `IconSpec`'s two-arm union (name xor path) stay sufficient once a
   trusted/built-in tier exists that might need a generated icon?**
   GNOME's own `SearchProvider2` needed a *third* icon form (raw pixel data)
   specifically because in-process extensions sometimes have to hand over a
   bitmap Shell can't otherwise resolve; PowerToys Run's `IconDelegate` and
   Flow's `Icon` delegate solve the same problem by generating an image
   object directly, which only works in-process. hop's `IconSpec` is
   explicitly documented as a breaking change to touch again
   (`item.rs`, `IconSpec` doc comment: "a third arm added later would be
   breaking"). Is two arms a permanent commitment, or does a future
   trusted-tier provider (not a sandboxed one) eventually want a
   generated-icon escape hatch the way three of the nine competitors here
   ended up needing one?

8. **Fixed-size result slots vs. a window that grows with results — is
   jank-avoidance worth giving up a feature some competitors treat as
   delight?** Flow Launcher resizes by default (an explicit setting exists
   to turn it *off*); rofi and PowerToys Run resize up to a cap; Ulauncher
   and Albert apparently don't resize at all. hop's design spec already
   commits to fixed-height reserved slots to avoid layout shift (§8a). Given
   that at least one competitor (Flow) made growing-with-results the
   *default* rather than an edge case, is hop's fixed-slot commitment purely
   a latency/jank decision, or is there a legibility cost (a window that
   never seems to reflect "how many results are there") worth weighing
   against it explicitly rather than assuming jank-avoidance always wins?

---

## 6. Sources

### Raycast — primary (official docs, developers.raycast.com / manual.raycast.com)
- https://developers.raycast.com/api-reference/user-interface/list
- https://developers.raycast.com/api-reference/user-interface/grid
- https://developers.raycast.com/api-reference/user-interface/detail
- https://developers.raycast.com/api-reference/user-interface/form
- https://developers.raycast.com/api-reference/user-interface/action-panel
- https://developers.raycast.com/api-reference/user-interface/actions
- https://developers.raycast.com/api-reference/command
- https://developers.raycast.com/information/manifest
- https://developers.raycast.com/information/security
- https://developers.raycast.com/information/best-practices
- https://developers.raycast.com/basics/prepare-an-extension-for-store
- https://manual.raycast.com/search-bar
- https://manual.raycast.com/keyboard-shortcuts
- https://manual.raycast.com/action-panel
- https://manual.raycast.com/command-aliases-and-hotkeys

### Raycast — secondary / lower confidence
- https://manual.raycast.com/themes (content captured via search digest, not a direct verbatim fetch)
- Compact Mode / window-mode description (search-digest-sourced, not independently re-fetched verbatim)
- https://www.raycast.com/changelog/windows/0-55 (search snippet only)

### Alfred — primary (official docs, alfredapp.com)
- https://www.alfredapp.com/help/workflows/inputs/script-filter/json/
- https://www.alfredapp.com/help/workflows/inputs/script-filter/
- https://www.alfredapp.com/help/workflows/inputs/keyword/
- https://www.alfredapp.com/help/features/default-results/
- https://www.alfredapp.com/help/features/universal-actions/
- https://www.alfredapp.com/help/appearance/
- https://www.alfredapp.com/help/kb/understanding-result-ordering/
- https://www.alfredapp.com/help/workflows/actions/browse-in-alfred/ (summarized, not independently refetched verbatim)

### Alfred — secondary
- https://www.alfredforum.com/topic/5250-forget-preferred-result/ (inconclusive; per-item ranking reset not confirmed as first-party)

### Spotlight — primary (Apple official docs, support.apple.com / developer.apple.com)
- https://support.apple.com/guide/mac-help/search-with-spotlight-mchlp1008/mac
- https://support.apple.com/guide/mac-help/narrow-search-results-mh15155/mac
- https://support.apple.com/guide/mac-help/spotlight-keyboard-shortcuts-mh26783/mac
- https://support.apple.com/guide/mac-help/choose-suggestion-categories-for-spotlight-mchl3e00eae9/mac
- https://support.apple.com/en-us/102650
- https://developer.apple.com/documentation/corespotlight

### Spotlight — secondary / unverifiable
- https://developer.apple.com/library/archive/documentation/General/Conceptual/AppSearch/AppContent.html (live doc pages are JS-rendered SPAs; could not fetch directly, only search-summarized)
- Core Spotlight's UI-rendering contract (whether it enforces "typed data only") — no public spec found; explicitly unverifiable

### GNOME Shell — primary (source code, gitlab.gnome.org/GNOME/gnome-shell, `main` branch as of 2026-08-10)
- `js/ui/search.js` — result display, `ListSearchResult`/`GridSearchResult`, provider dispatch, `_updateSearchProgress`, `_maybeSetInitialSelection`, `navigateFocus`
- `js/ui/appDisplay.js` — `AppSearchProvider`, `Shell.AppUsage.compare()` usage-ranking call, app-grid sort
- `js/misc/remoteSearch.js` — `RemoteSearchProvider`/`RemoteSearchProvider2`, the embedded `SearchProvider2` D-Bus introspection XML, icon deserialization (`icon`/`gicon`/`icon-data`), `filterResults`
- `js/ui/searchController.js` — keyboard handling, Escape stack, `getTermsForSearchString`
- `js/ui/overview.js` — overview show/hide, `ANIMATION_TIME`, `focusSearch`
- https://github.com/GNOME/gnote/blob/master/src/dbus/shell-search-provider-dbus-interfaces.xml (independent corroboration of the `SearchProvider2` XML)
- https://developer.gnome.org/hig/guidelines/ui-icons.html
- https://developer.gnome.org/hig/patterns/nav/search.html

### GNOME Shell — unverified
- `Shell.AppUsage`'s exact scoring formula (native C, not visible from JS source alone)
- Whether a pre-GNOME-40 "Frequent apps" app-picker tab still exists anywhere in current Shell

### Ulauncher — primary (github.com/Ulauncher/Ulauncher, `main` branch; docs.ulauncher.io)
- `ulauncher/ui/result_widget.py` — row layout
- `ulauncher/ui/results_view.py` — `_limit()`, result-container hide-on-empty
- `ulauncher/utils/settings.py` — `jump_keys`, `max_recent_apps` defaults
- `ulauncher/core.py` — `PLACEHOLDER_DELAY`, extension routing
- `ulauncher/modes/extensions/extension_mode.py` — `LOADING_TIMEOUT`, loading/failure placeholders
- `ulauncher/internals/result.py` — the `Result` dataclass, `search_score`
- `ulauncher/api/shared/item/ExtensionResultItem.py` — deprecated legacy wrapper
- `ulauncher/ui/ulauncher_window.py` — keyboard handling, window sizing/position
- https://docs.ulauncher.io/en/stable/extensions/intro.html ("Ulauncher communicates to extensions using WebSockets")

### Ulauncher — secondary
- https://github.com/Ulauncher/Ulauncher/discussions/941 (dated maintainer comment on hardcoded result counts, contradicted by current `main`)

### Albert — primary (github.com/albertlauncher/albert; albertlauncher.github.io)
- `include/albert/item.h` — the `Item` interface
- `include/albert/icon.h` — the `Icon` abstraction (theme/file/grapheme/iconified/composed)
- `src/query/usagedatabase.cpp` — SQLite usage log, `itemUsageScores`
- `src/query/usagescoring.cpp` — `UsageScoring::modifiedMatchScore`
- `src/settings/querywidget/querywidget.cpp` — memory-decay slider, "prioritize perfect matches" checkbox
- https://albertlauncher.github.io/basics/ (trigger/global/fallback handlers, keyboard model)
- https://albertlauncher.github.io/extension/cplusplus/ ("A native plugin is a Qt Plugin, i.e. a shared library...")

### Albert — secondary
- https://github.com/albertlauncher/albert-plugin-widgetsboxmodel-qss (README, row-delegate description)
- https://github.com/albertlauncher/albert/issues/1402 (GIL crash report corroborating in-process Python embedding)
- https://github.com/albertlauncher/python (dependency reference for pybind11)
- Albert's default window size/position (only GitHub issues #1018/#125 and blog mentions found; no primary source located)
- DeepWiki-derived frontend method names (AI-generated; not independently verified)

### rofi — primary (github.com/davatorium/rofi, cloned at commit `f1ee9b8`)
- `doc/rofi-theme.5.markdown` — `listview` properties, `columns`/`lines`/`dynamic`
- `doc/rofi-keys.5.markdown` — full `kb-*` keybinding catalogue
- `doc/rofi.1.markdown` — CLI options, `-location`/`-monitor`, `-sorting-method`, Hyprland animation-disable example
- `doc/rofi-script.5.markdown` — rofi-script protocol, `\0key\x1fvalue` syntax, `ROFI_RETV`/`ROFI_INFO`/`ROFI_DATA`
- `include/settings.h` — `DEFAULT_MENU_LINES`, `DEFAULT_MENU_COLUMNS`
- `include/mode.h`, `include/mode-private.h` — `struct rofi_mode` ABI, `ABI_VERSION`, `_get_display_value`, `_get_icon`
- `source/widgets/listview.c` — default line/column consumption
- `source/modes/drun.c` — app history integration, message area
- `source/modes/combi.c` — `combi_mgrv`, `!bang` filtering, per-source icon delegation
- `source/modes/ssh.c` — history integration
- `source/history.c` — `history_set`, `history_remove`, frequency-based sort
- `source/rofi-icon-fetcher.c` — icon rendering pipeline
- `themes/iggy.rasi` — example grid-mode theme

### PowerToys Run — primary (github.com/microsoft/PowerToys, sparse-checked at `src/modules/launcher/`, commit `731f2e3`; learn.microsoft.com/windows/powertoys/run)
- `PowerLauncher/ResultList.xaml` — row template, tooltip, context-button row
- `PowerLauncher/MainWindow.xaml`, `MainWindow.xaml.cs` — window sizing/position, commented-out `IntroStoryboard`
- `PowerLauncher/ViewModel/MainViewModel.cs` — results visibility, plugin-hints panel, modifier-key plumbing
- `PowerLauncher/Plugin/PluginManager.cs` — query timing/warning, `AllowedLanguage.CSharp` filter
- `Wox.Plugin/Result.cs` — the `Result` object, `GetSortOrderScore`
- `Wox.Plugin/IPlugin.cs`, `IDelayedExecutionPlugin.cs`, `IContextMenu.cs` — plugin interfaces
- `Wox.Plugin/ActionContext.cs`, `SpecialKeyState.cs` — modifier-key plumbing
- `Wox.Plugin/AllowedLanguage.cs`, `PluginLoadContext.cs` — trust model, `AssemblyLoadContext`
- `Wox.Plugin/PluginMetadata.cs` — `ActionKeyword`
- Individual `Plugins/*/plugin.json` files — default keyword table
- https://learn.microsoft.com/windows/powertoys/run — official Settings/shortcuts documentation

### PowerToys Run — unverified
- Exact call site writing `Result.SelectedCount`/`LastSelected` (not located in the sparse-checked tree)
- Whether `AllowedLanguage.Executable` has any call site outside `src/modules/launcher/` (very unlikely but not exhaustively ruled out)

### Flow Launcher — primary (github.com/Flow-Launcher/Flow.Launcher, `master`; github.com/Flow-Launcher/docs)
- `Flow.Launcher/ResultListBox.xaml` — row template, hotkey badge
- `Flow.Launcher/MainWindow.xaml.cs` — `SizeToContent`, window animation
- `Flow.Launcher/ViewModel/ResultsViewModel.cs` — dynamic `MaxHeight` computation
- `Flow.Launcher.Infrastructure/UserSettings/Settings.cs` — all documented defaults (`MaxResultsToShow`, `ItemHeightSize`, window position enums, `UseAnimation`, `UseGlyphIcons`)
- `Flow.Launcher.Infrastructure/KeyConstant.cs` — modifier constants
- `Flow.Launcher.Plugin/Result.cs` — the full `Result` object, `PreviewPanel: Lazy<UserControl>`, `AddSelectedCount`, `RecordKey`
- `Flow.Launcher.Plugin/PluginMetadata.cs` — `ActionKeyword(s)`, `HomeDisabled`
- `Flow.Launcher.Plugin/Interfaces/IPlugin.cs`, `IAsyncPlugin.cs`
- `Flow.Launcher.Core/Plugin/JsonRPCPlugin.cs`, `JsonRPCPluginV2.cs`, `JsonRPCPluginBase.cs` — V1 vs V2 subprocess/pipe model
- `Flow.Launcher.Core/Plugin/PluginManager.cs` — `QueryHomeForPluginAsync`, initializing-placeholder result
- `docs:usage-tips.md`, `json-rpc.md`, `plugin.json.md`, `py-develop-plugins.md`, `py-write-code.md`, `how-to-create-a-theme.md`, `develop-dotnet-plugins.md`

### Flow Launcher — secondary / unverified
- GitHub issues/discussions #2496, #2987, #2998, #2904 — `UserSelectedRecord.json` frecency mechanics, not independently confirmed against source
- Whether theme `.xaml` files hot-reload without a restart — not found
- Explorer plugin's literal default `ActionKeyword` string — inferred from usage docs, not read directly from its `plugin.json`

### hop internal sources (repo:path, this repository)
- `docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md` — v1 design spec (§1, §3, §4, §5, §6, §8, §8a, §11, §13)
- `crates/hop-core/src/router.rs` — `Mode`, `RoutedQuery`, `route()`, the eleven markers and their tests
- `crates/hop-protocol/src/item.rs` — `Item`, `Action`, `IconSpec`, `Kind`, `ActionKind`
- `docs/security/2026-08-02-m2-socket-boundary-threat-model.md` — the socket trust boundary, provider trust, Decision 2 (learning-store hashing)
