# Hop Launcher v1 — Design Spec

Date: 2026-07-30
Status: Approved pending final review
Decisions by: Pedro Sousa

## 1. What this is

Hop Launcher is a standalone launcher / command palette for Linux (X11 and Wayland): a resident daemon plus a GTK4 overlay that opens on a global hotkey, fuzzy-searches apps, windows, and files, evaluates utility queries (calculator, currency, timezone, weather, emoji), learns from usage, and — in later versions — hosts third-party plugins.

It replaces two prior efforts, both retired by audit on 2026-07-30:

- The **GNOME Shell extension** (github.com/pedrosousa13/hop-launcher, `main`): abandoned as a product. Its pure-JS core (fuzzy matcher, query router, learning store, ~1,400 lines of tests) becomes the porting spec for this project.
- The **`feat/cross-linux-hopd-v1` Rust branch**: not taken over. Audit verdict: start over, salvaging specific modules (listed in §10). Its fatal flaws — substring-only matching, blocking subprocess/HTTP calls per keystroke, no plugin seam, non-functional on GNOME Wayland — are each addressed by an explicit design rule here.

### Positioning

**The GNOME-native, trustworthy-plugins launcher that works everywhere.**

Evidence-based (competitive research, July 2026): Raycast has formally ceded Linux. Every layer-shell launcher (rofi, Walker, Fuzzel, Anyrun, Sherlock) refuses to run on GNOME, the largest desktop-Linux population. Vicinae — the strongest competitor — is wlroots/KDE-first with a young GNOME bridge, no plugin sandbox, and focus drifting to macOS/Windows. The empty niches Hop attacks, in order: (1) first-class GNOME alongside wlroots/KDE/X11, (2) a sandboxed, permissioned plugin platform (unoccupied since Gauntlet's abandonment), (3) Raycast-grade TS plugin DX. Hop does **not** pursue Raycast-store compatibility — Vicinae owns that.

### Non-goals for v1

- No plugin API surface exposed to third parties (the seam exists internally; see §6).
- No clipboard history, snippets, AI/MCP (v1.x / v2 — see §12).
- No macOS/Windows. Linux only, permanently by default.
- No settings knob for anything code does not read. No feature that fakes its output.
  (Both prior codebases violated these; both audits flagged it as credibility debt.)

## 2. Supported platforms (v1)

| Platform | Overlay | Global hotkey | Window switching |
|---|---|---|---|
| GNOME Wayland (45+) | Normal window, centered, close-on-focus-loss; focus via activation token from toggle client | DE custom shortcut → `hop toggle` (documented one-liner setup); GlobalShortcuts portal (GNOME 48+) as enhancement | GNOME shim extension (§7) |
| KDE Wayland | layer-shell (overlay layer, exclusive keyboard) | kglobalaccel via portal or DE shortcut → `hop toggle` | KWin D-Bus/scripting source |
| wlroots (Hyprland, Sway, niri, river) | layer-shell | compositor bind → `hop toggle` | foreign-toplevel-management protocol |
| X11 (any WM/DE) | Normal override-positioned window | hop-hotkeyd X11 grab (salvaged, made configurable) | EWMH (`_NET_CLIENT_LIST`, `_NET_ACTIVE_WINDOW`) |

Graceful degradation is a rule: every capability probe (layer-shell, shim, data-control, hotkey backend) has a defined fallback, and `hop doctor` reports what was detected and why.

## 3. Architecture

Fresh repository (recommendation: rename the old GitHub repo to `hop-launcher-gnome` and archive it; the new repo takes the `hop-launcher` name and the site). Cargo workspace:

```
hop-launcher/
├── Cargo.toml            # workspace
├── crates/
│   ├── hop-protocol      # serde types for every IPC message; version handshake; THE contract
│   ├── hop-core          # Provider trait, nucleo-based fuzzy ranking, query router,
│   │                     #   learning/frecency engine, aliases, result/action model
│   ├── hopd              # daemon: tokio Unix-socket server, provider host, indexes,
│   │                     #   config + state persistence, single-instance
│   ├── hop-cli           # `hop` binary: toggle | query | doctor | version
│   └── hop-hotkeyd       # optional hotkey agent: X11 grab loop, portal backend (zbus)
├── apps/hop-gtk          # GTK4 + libadwaita frontend + gtk4-layer-shell; settings UI
├── shims/gnome-shell/    # thin companion extension (D-Bus bridge, no UI)
├── data/                 # emoji dataset, city/timezone dataset (generated, documented)
└── docs/
```

Three processes at runtime, all salvage-validated as the right shape:

1. **hopd** — resident, owns all indexes and search. Systemd user service + socket activation.
2. **hop-gtk** — resident, pre-built hidden window; single instance via GApplication D-Bus activation. Show latency = compositor map time, not process start.
3. **Hotkey path** — the universal pattern: DE-configured shortcut runs `hop toggle`, which pokes hop-gtk's control socket **and inherits the Wayland activation token**, which is what makes focus work on GNOME. `hop-hotkeyd` is an optional enhancement for X11 (real grab) and the GlobalShortcuts portal (KDE, GNOME 48+); it is not required on any platform.

### IPC protocol (hop-protocol)

- Unix socket `$XDG_RUNTIME_DIR/hop/hopd.sock` (0700 dir). Persistent connections, length-prefixed JSON frames (upgradeable to another encoding behind the same types).
- Every message is a typed serde struct. `Hello { api_version }` handshake on connect; mismatch is an explicit error, never silent.
- `Query { id, text }` → zero or more `Results { query_id, partial, items }` → terminal `QueryDone { query_id }`. **Query IDs on every frame; stale frames dropped by the client; a new query cancels the old one server-side** (tokio CancellationToken).
- `Execute { query_id, item_id, action_id }` — Enter resolves against the item list of the *current* query id, never a stale one (fixes extension bug B3).
- Providers stream: fast providers' results render immediately; slow providers append incrementally. No slowest-provider gate (fixes pop-launcher's known flaw).

### The latency contract (fixes the Rust branch's fatal flaw)

- Tier-0 keystroke → ranked results: **< 10 ms**. Toggle → visible overlay: **< 100 ms perceived**.
- On the query path: **no disk reads, no subprocess spawns, no HTTP, ever.** Only in-memory index lookups.
- Indexes are maintained by events, not queries: apps via inotify on XDG `.desktop` dirs; files via notify watcher + periodic rescan of configured roots (with default excludes: dotfiles, `.git`, `node_modules`, caches); windows via compositor event subscriptions (foreign-toplevel events, i3/Hyprland IPC events, EWMH property events, shim signals) into a cached list.
- Network providers (weather, currency geocode/rates) return a cached-or-pending row synchronously and push an update frame when the fetch lands; fetches have timeouts *and* cancellation (`tokio::time::timeout` around abortable futures — real cancellation, not the branch's discard-after-completion).
- Provider budgets enforced per provider per query; a budget miss logs and isolates, never blocks the frame.

## 4. Search core (hop-core)

- **Fuzzy**: `nucleo` matcher (Helix's, the current best-in-class Rust fuzzy engine) wrapped with Hop's ranking layer: source weights, alias boosts, learning boosts, min-score threshold. The extension's `fuzzy.js` behaviors — typo tolerance, boundary/camel bonuses, contiguous-run bonuses, dedupe rules — are ported as **test cases** first; nucleo config + wrapper is tuned until the ported suite passes. The JS tests are the spec.
- **Query router**: ported from `queryRouter.js` with its two audited defects fixed:
  - Inferred modes (bare `tokyo`, `2+2`, `100 usd to eur`) **augment** the general results (utility row pinned on top) instead of exclusively hijacking the result list. Only explicit prefixes (`w `, `a `, `f `, `=`, `tz `, `wx `) are exclusive.
  - Prefix with empty remainder (`w `) lists all items of that kind instead of nothing.
- **Learning**: the branch's `learning.rs` ported near-verbatim (per-query + global frequency, 30/90-day decay, LRU caps, IDs canonicalized, queries never persisted, atomic 0600 writes) into hop-core with its tests. Storage: `$XDG_STATE_HOME/hop/learning.json`.
- **Aliases**: ported from `aliases.js` (rewrite / app-boost / window-boost; alias boost always beats learning boost, as in the extension: 180 > 85 cap).
- **Result model**: every item carries typed `actions: [{id, kind: Open|Focus|Copy|Run|CloseWindow|MoveWorkspace|OpenUrl, label}]` with a designated default. One dispatch path. (Kills the extension's split-brain Enter model and revives its dead window actions.)

## 5. Providers (v1)

All implement one trait in hop-core:

```rust
trait Provider {
    fn manifest(&self) -> ProviderManifest;   // id, kinds, prefixes, regex/min-length pre-filters, budget
    async fn query(&self, q: &RoutedQuery, ctx: &QueryCtx) -> Result<Vec<Item>>;  // ctx carries deadline + cancellation
    async fn execute(&self, item_id: &ItemId, action_id: &ActionId) -> Result<ExecOutcome>;
    // optional: fn on_event(&self, ev: SystemEvent)  // index maintenance
}
```

| Provider | Source | Notes |
|---|---|---|
| Apps | `.desktop` parse (salvaged parser) + inotify; icon via icon-theme lookup | Focus-existing-window-else-launch semantics ported from `appLaunch.js` tests |
| Windows | Per-platform sources (§2 table) feeding one cached window index | Actions: focus (default), close, move-to-workspace |
| Files | notify-watched index of configured roots; sensible default excludes; cap + depth configurable | Substring+fuzzy on names; open via `gio`/`xdg-open` equivalent (no shell) |
| Calculator | `fasteval` (salvaged choice — it was real in the branch) | Handles unary minus, `%`; copy result (default) |
| Currency | **Real rates**: Frankfurter API (ECB, keyless) fetched on TTL (default 12h), cached in state dir; offline = last rates + "as of" label | Never fabricates freshness (extension B9); converts, copy default |
| Timezone | IANA via `chrono-tz` + ported alias table + city dataset (regenerated, license-documented) | Bare-token matches augment, never hijack (B4) |
| Weather | Open-Meteo geocode+forecast, cached per location (10 min TTL), pending row + push update | Timeout + cancellation |
| Emoji | Real dataset: full Unicode CLDR annotation set, generated at build time, searchable by keyword | Copy glyph (10-emoji stub never returns) |
| Web search | User-configurable HTTPS `%s` templates (defaults Google/DuckDuckGo), `keyword` prefixes honored this time | `appendToEnd` actually wired: pinned after ranked results |

## 6. Plugin roadmap (designed now, built later)

The `Provider` trait + `hop-protocol` frames **are** the plugin seam. Locked in v1's protocol so retrofit is never needed (the four rules every predecessor got wrong):

1. Host owns fuzzy filtering by default; plugins opt into raw keystrokes via throttle.
2. Declarative manifest pre-filters (keyword, regex, min-length) — most keystrokes never reach most plugins.
3. Query IDs + cancellation + per-plugin deadlines with incremental merging.
4. Version handshake at every boundary.

- **v2 — Tier 1, trusted TS extensions**: one Node sidecar, one `worker_threads` Worker per extension (heap-capped, pre-warmed), Raycast-shaped TS SDK (`List`, `Detail`, `ActionPanel` catalog), curated monorepo store with PR review. Start `no-view`/list-only; defer full React reconciler until demand exists.
- **v3 — Tier 2, sandboxed plugins**: wasmtime components, versioned WIT (Zed's model), deny-by-default capabilities, epoch deadlines, install-time compilation (Zellij's lesson). This is the differentiator no launcher offers.

## 7. GNOME shim (shims/gnome-shell)

Thin companion extension, D-Bus service only, no UI, no search logic. v1 interface:

- `ListWindows() → a(...)` (id, title, app-id, workspace, monitor, focused) + `WindowsChanged` signal
- `ActivateWindow(id)`, `CloseWindow(id)`, `MoveWindowToWorkspace(id, n)`
- v1.x adds: clipboard-changed events (sole route to clipboard history on Mutter), paste assist.

Published on extensions.gnome.org as "Hop Launcher Integration". hopd probes for it at startup and on D-Bus name-owner changes; absence just disables the windows provider on GNOME with a `hop doctor` explanation. Shim code is written fresh (~200 lines) but informed by the extension's `windows.js` semantics; it is versioned with the same D-Bus interface-versioning rule as everything else.

## 8. Frontend (apps/hop-gtk)

- GTK4 + libadwaita, `gtk4-layer-shell` where supported (probe at startup), styling via GTK CSS with user theme override file (`$XDG_CONFIG_HOME/hop/theme.css`) — hot-reloadable.
- Pre-built hidden window; `hop toggle` → control message → `present()` with activation token when provided.
- **All IPC off the main thread** (`glib::spawn_future_local` over an async channel to a tokio client task). The UI never blocks on the socket (branch's frontend flaw).
- Results list: fixed row widget recycling (GtkListView + factory), not destroy-and-rebuild.
- Keyboard: Up/Down/PgUp/PgDn/Home/End, Enter (default action), configurable secondary-action menu key, Tab completion for prefixes, Escape. Mouse: click activates (extension gap).
- Settings window (libadwaita): keybinding guidance per platform, feature toggles, search tuning, web-search service editor, indexed folders (under Files, not under web search — extension B10), learning controls + insights, theme selection. Every control is wired or it does not ship.
- Empty-query view: recent/frequent items from learning (replaces the extension's blank panel).

## 9. Config, state, errors, logging

- Config: TOML at `$XDG_CONFIG_HOME/hop/config.toml`, watched, persisted (branch's in-memory-only config store is explicitly banned). CLI: `hop config get/set`.
- State (learning, currency cache, weather cache): `$XDG_STATE_HOME/hop/`, atomic writes, 0600.
- Errors: typed in hop-protocol; per-provider isolation (one failing provider never empties a frame); daemon `tracing` with env-filter, `hop doctor` bundles diagnostics (socket health, capability probes, shim presence, hotkey backend, index sizes).
- URL/percent-encoding, string sanitization: one implementation in hop-core, UTF-8-correct, property-tested (branch had 3 copies, one mangling non-ASCII).

## 10. Salvage manifest

| From | Item | Into |
|---|---|---|
| Rust branch | `learning.rs` (+ tests) | hop-core, near-verbatim |
| Rust branch | X11 grab loop (configurable keys), backoff logic | hop-hotkeyd |
| Rust branch | desktop-entry parser, sway/hyprctl JSON parsing | v1 providers (as event-driven sources, not per-query calls) |
| Rust branch | `.xbel` recents parser | recents provider (v1.x) |
| Rust branch | 3-process shape, minimal tokio accept loop | hopd |
| Rust branch | `fasteval` calculator choice, actions-table exec semantics | calculator provider, action dispatch |
| Extension | JS test suites (fuzzy, router, learning, aliases, appLaunch, providers) | ported as Rust test specs before implementation |
| Extension | Fuzzy scoring weights/behaviors, alias>learning precedence, provider inventory | hop-core tuning targets |
| Extension | `windows.js` action semantics | shim + windows provider |
| Not salvaged | Branch's GTK monolith, hotkeyd dbus-monitor scraping, all release scaffolding, 6.6k lines of plan docs, parity matrix, stale review doc | — |

## 11. Testing & CI

- hop-core: unit tests ported from the JS suites + property tests (encoding, parser fuzz). This happens **before/with** implementation, TDD-style.
- hopd: integration tests over a real socket (spawn daemon, drive queries, assert frames, cancellation, budgets).
- hop-gtk: headless smoke test (broadway/offscreen) + `scripts/dev-run.sh` manual loop (salvaged style).
- Latency regression test: scripted 10k-item index, assert p95 query < 10 ms in CI.
- CI (GitHub Actions): fmt, clippy (deny warnings), test, cross-compile check. **No release automation until v1 works on the author's machine** (branch red-flag #1).

## 12. Release & site plan

Rollout (from distribution research; nothing before a daily-drivable v1):

1. GitHub Releases via cargo-dist: tarballs, `.deb`, `.rpm`, checksums, `curl | sh` installer; release-plz for version/changelog PRs.
2. AUR: `hop-launcher` + `hop-launcher-bin`, bumped by CI on tag.
3. Nix flake + Home Manager module.
4. AppImage (FUSE-free static runtime, zsync updates). Then Copr. Shim to e.g.o.
5. Skipped: Snap, PPA. Flathub: revisit ≥12 months post-launch (AI policy + sandbox hostility documented in research).

Site (parallel workstream once v1 alpha exists): upgrade docs-site to Astro 6 + Starlight at `/docs`, typeable launcher demo as landing centerpiece, `/install` per-distro page, changelog. Plugin directory pages arrive with v2. Site repo/dir decision made then.

## 13. Milestones

- **M1 — Core**: workspace, hop-protocol, hop-core with ported test suites green (fuzzy/router/learning/aliases).
- **M2 — Daemon**: hopd serving apps+files+calculator over socket; `hop query` CLI proves the loop; latency test green.
- **M3 — Frontend**: hop-gtk overlay on the author's GNOME session (toggle, search, launch); layer-shell path verified on a wlroots session; X11 session verified.
- **M4 — Full v1 providers**: windows (all platforms incl. shim on e.g.o review queue), web search, emoji, timezone, currency, weather.
- **M5 — Polish + release**: settings UI, theming, doctor, docs, rollout steps 1–3. **Author daily-drives Hop from M3 onward.**

## 14. Risks

- **Rust learning curve** (author from JS): mitigated by salvaged working Rust modules, ported test suites as guardrails, and small crate boundaries. Accepted cost of "best-in-class".
- **GNOME focus/activation quirks**: the toggle-client token pattern is the field-proven mitigation; shim exists if edge cases bite; `hop doctor` surfaces misconfiguration.
- **e.g.o review latency for the shim**: windows-on-GNOME may trail the v1 binary release by weeks; acceptable, degradation is graceful.
- **Scope creep**: the previous two attempts died of it. Contract: nothing enters v1 beyond §5; new ideas go to the v1.x/v2 backlog.
- **Vicinae velocity**: they may improve GNOME support. Hop's bet is depth (native shim + GNOME-first polish + sandbox roadmap) over breadth; re-evaluate positioning at v1 launch.
