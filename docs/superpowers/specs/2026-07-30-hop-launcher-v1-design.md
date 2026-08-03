# Hop Launcher v1 — Design Spec

Date: 2026-07-30
Status: Approved; amended 2026-07-31, 2026-08-03
Decisions by: Pedro Sousa

**Amendment, 2026-07-31.** Amended after a grilling session over the milestone
structure, held once M1 landed and the M1 OWASP sweep
(`docs/security/2026-07-30-m1-owasp-sweep.md`) had filed its 29 findings.
Seven sections changed: §3 (hop-cli verbs), §5 (Files moves to the providers
milestone), §6 (what "locked" means and when), §8 (the keymap is
configurable), §8a (theming becomes an ecosystem), §11 (the latency test
gains an adversarial arm), §13 (milestones split from five to six). Each
change is marked **[Amended 2026-07-31]** in place.

**Amendment, 2026-08-03.** Amended when issue #35 landed the supply-chain gate
CI never had. One section changed: §11 (the CI list now names `cargo deny`).
The change is marked **[Amended 2026-08-03]** in place.

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
│   ├── hop-cli           # `hop` binary: query | exec | toggle | doctor | version
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
| Files | notify-watched index of configured roots; sensible default excludes; cap + depth configurable | Substring+fuzzy on names; open via `gio`/`xdg-open` equivalent (no shell). **[Amended 2026-07-31]** Moved out of M2 into M5, where it is the *first* slice |
| Calculator | `fasteval` (salvaged choice — it was real in the branch) | Handles unary minus, `%`; copy result (default) |
| Currency | **Real rates**: Frankfurter API (ECB, keyless) fetched on TTL (default 12h), cached in state dir; offline = last rates + "as of" label | Never fabricates freshness (extension B9); converts, copy default |
| Timezone | IANA via `chrono-tz` + ported alias table + city dataset (regenerated, license-documented) | Bare-token matches augment, never hijack (B4) |
| Weather | Open-Meteo geocode+forecast, cached per location (10 min TTL), pending row + push update | Timeout + cancellation |
| Emoji | Real dataset: full Unicode CLDR annotation set, generated at build time, searchable by keyword | Copy glyph (10-emoji stub never returns) |
| Web search | User-configurable HTTPS `%s` templates (defaults Google/DuckDuckGo), `keyword` prefixes honored this time | `appendToEnd` actually wired: pinned after ranked results |

## 6. Plugin roadmap (designed now, built later)

The `Provider` trait + `hop-protocol` frames **are** the plugin seam. Locked in v1's protocol so retrofit is never needed (the four rules every predecessor got wrong):

> **[Amended 2026-07-31] What "locked" means, and when it starts.** The lock exists so that *third-party* plugin authors never face a retrofit. No external consumer exists until v2's Tier 1 extensions ship, so the seam stays **open to change throughout v1 development** — and M2 is precisely when the daemon discovers what the trait actually needs. The M1 sweep found two gaps that can only be closed by changing these types: the protocol has no frame-size cap (#21), and `Provider::query`'s borrowed arguments make the returned future non-`'static`, so it cannot be `tokio::spawn`ed — which is the very panic isolation the trait's own doc comment reaches for (#29). Both are ordinary M2 work. The lock takes effect when the extension store ships, not now.

1. Host owns fuzzy filtering by default; plugins opt into raw keystrokes via throttle.
2. Declarative manifest pre-filters (keyword, regex, min-length) — most keystrokes never reach most plugins.
3. Query IDs + cancellation + per-plugin deadlines with incremental merging.
4. Version handshake at every boundary.

- **v2 — Tier 1, trusted TS extensions**: one Node sidecar, one `worker_threads` Worker per extension (heap-capped, pre-warmed), Raycast-shaped TS SDK (`List`, `Detail`, `ActionPanel` catalog). Start `no-view`/list-only; defer full React reconciler until demand exists.
- **v2 — the Hop Extension Store** (ships with Tier 1, not after it):
  - Distribution: a public `hop-extensions` monorepo; publishing = PR with human review (the Raycast model — review is the quality gate; the WASM tier later relaxes the trust requirement, not the quality bar).
  - Install: in-app store browsing + one-click install/update inside the launcher itself, and `hop ext install <name>`; update model is implicit-latest with SDK-version compatibility checks (the api-version handshake from §6 rule 4 is what makes this safe).
  - Web: `/plugins` directory on the site, generated from the monorepo's manifests (icon, description, author, install command/deep-link, install counts when telemetry exists) — the store pages and the in-app store read the same manifest data.
  - Scaffolding DX: `npm create hop-extension` + hot-reload dev mode against a running hopd — author experience is a launch feature of the store, not an afterthought.
- **v3 — Tier 2, sandboxed plugins**: wasmtime components, versioned WIT (Zed's model), deny-by-default capabilities, epoch deadlines, install-time compilation (Zellij's lesson). This is the differentiator no launcher offers.
- **Raycast-compat option (deliberately preserved, not scheduled)**: Tier 1's view-tree JSON and component names/props SHOULD map 1:1 onto Raycast's equivalents (`List`, `Detail`, `Form`, `ActionPanel`, …) wherever that costs nothing, and the sidecar/worker shape already matches theirs. This keeps a future compat layer (require-patching + API shim, the Vicinae approach) a bounded 2-quarter project instead of a rewrite. Decision point: after v2 ships, based on whether catalog size is the actual bottleneck. Not a v1/v2 commitment.

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
- Keyboard: Up/Down/PgUp/PgDn/Home/End, Enter (default action), secondary-action menu key, Tab completion for prefixes, Escape. Mouse: click activates (extension gap).
- **[Amended 2026-07-31] The whole keymap is configurable, not just the menu key.** M3 reads the keymap from `config.toml` with these as defaults, so every key handler is data-driven from the start and nothing is hardcoded. M6 adds the settings-window capture widget (press a key, it records and writes back) plus conflict detection. The retrofit cost of unpicking hardcoded handlers after M3 ships is what forces the config half early.
- Settings window (libadwaita): keybinding guidance per platform, feature toggles, search tuning, web-search service editor, indexed folders (under Files, not under web search — extension B10), learning controls + insights, theme selection. Every control is wired or it does not ship.
- Empty-query view: recent/frequent items from learning (replaces the extension's blank panel).

### 8a. Design quality bar (UI/UX is a product feature, not a coat of paint)

The launcher window IS the product — users see ~400×500px of it hundreds of times a day. Design investment concentrates there.

- **Design system before pixels**: one `tokens.css` defining the spacing scale, type scale (with a deliberate monospace choice — a launcher brand lives in its mono), radii, elevation/shadow, timing curves, and one committed accent color on a disciplined dark neutral scale (the "first in class vs AI-template" separator from the site research applies to the app itself). Every component consumes tokens; no ad-hoc values.
- **Dark-first, both themes**: dark and light ship in v1, tracking the desktop preference; high-contrast variant in v1.x. User `theme.css` overrides tokens, hot-reloaded.
- **[Amended 2026-07-31] Theming is an ecosystem in v1, not just an override file.** M6 ships a theme format (manifest + css), a themes directory, `hop theme list/use/install <path-or-url>`, the settings picker, and a **documented, versioned token contract** so a theme written today does not break on the next release. The *distribution* half — curated monorepo, PR review, in-app browsing, site gallery — rides along with v2's extension store (§6), which needs identical machinery; building it twice is the thing being avoided, not the feature being cut. **A theme is untrusted input**: GTK CSS executes no code, but it can restyle or hide the very labels §5 relies on to stay honest — the "as of" timestamp on cached rates, the pending-row skeleton, the offline indicator. A theme that makes stale data look fresh defeats "never fabricates freshness". Tracked as its own issue against M3, where `theme.css` hot-reload first lands.
- **Motion with restraint**: one signature open/close animation (starting values inherited from the extension's tuned 140ms open / 110ms close, ease-out), subtle selection/result transitions, zero jank during result streaming (no layout shift when async rows resolve — pending rows reserve their height). `prefers-reduced-motion` respected via the GTK setting.
- **Keyboard-first affordances**: right-side action hints on every row (ported hint system: Focus/Open/Run/Copy + key glyph), visible-but-quiet prefix cheatsheet in the empty state, first-run overlay teaching the 5 core interactions once.
- **States are designed, not defaulted**: empty query (recents/frequents), no results (suggest web search, never a blank void), pending network rows (skeleton with provider icon), error rows (plain language + retry action), offline (cached-data labels with "as of" timestamps).
- **Process**: before GTK implementation in M3, a static design pass produces the visual direction (mock frames of the 6 key states, iterated with Pedro until approved) — GTK CSS then implements the approved direction. Screenshots of real builds reviewed against the mocks at each milestone. HIG-informed where it serves (icon language, a11y), deliberately non-stock where identity demands (the overlay is not an Adwaita dialog).
- **Accessibility**: full keyboard operability (already structural), screen-reader labels on rows and actions, contrast-checked palette in both themes, respects system font scaling.

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

### Agent-testable by construction (principle)

The product must be fully exercisable by an automated agent with no human at the keyboard — this is both how it gets built and a quality forcing-function:

- **Every behavior has a headless path**: `hop query "<text>" --json` returns the exact assembled result list the UI would show; `hop exec <item> <action>` performs the action; meaningful exit codes throughout. If a feature can't be driven through the CLI, it isn't done.
- **hopd runs against scripted fake providers** (a test fixture config) so integration tests are deterministic — fixed clock injection for learning/decay and cache-TTL tests.
- **The GTK frontend runs headless in CI** (offscreen/Broadway GDK backend) and supports `hop-gtk --screenshot <path>` to render its current state to a PNG — agents verify visual states (empty, results, pending, error) by reading the screenshot, and the M3 design-pass mocks are compared against these captures.
- **`hop doctor --json`** exposes every capability probe result machine-readably.

- hop-core: unit tests ported from the JS suites + property tests (encoding, parser fuzz). This happens **before/with** implementation, TDD-style.
- hopd: integration tests over a real socket (spawn daemon, drive queries, assert frames, cancellation, budgets).
- hop-gtk: headless smoke test (broadway/offscreen) + `scripts/dev-run.sh` manual loop (salvaged style).
- Latency regression test: scripted 10k-item index, assert p95 query < 10 ms in CI.
- **[Amended 2026-07-31] The latency gate needs a second, adversarial arm.** A p95 over a normal workload never sees the pathological case: sweep finding #46 measured `Ranker::rank` at 4.09 s for a 100 KB query over 5 000 items — `Pattern::parse` splits on spaces into one atom per word, so cost is `O(atoms × items)` with no ceiling on either factor, and `truncate(max_results)` runs *last*, bounding the output rather than the work. So CI asserts both: p95 < 10 ms over the scripted index, **and** a bounded worst case over pathological input (long query, oversized candidate set, huge per-item strings). This forces #46's input cap to be decided in M2 rather than discovered in production.
- CI (GitHub Actions): fmt, clippy (deny warnings), test, cross-compile check. **[Amended 2026-08-03]** Plus a supply-chain gate (issue #35): `cargo deny check` runs all four checks — advisories, bans, licenses, sources — against `deny.toml` at the repo root, as its own job rather than a step, so "supply chain failed" and "a test failed" report as separate red checks. **No release automation until v1 works on the author's machine** (branch red-flag #1).

## 12. Release & site plan

Rollout (from distribution research; nothing before a daily-drivable v1):

1. GitHub Releases via cargo-dist: tarballs, `.deb`, `.rpm`, checksums, `curl | sh` installer; release-plz for version/changelog PRs.
2. AUR: `hop-launcher` + `hop-launcher-bin`, bumped by CI on tag.
3. Nix flake + Home Manager module.
4. AppImage (FUSE-free static runtime, zsync updates). Then Copr. Shim to e.g.o.
5. Skipped: Snap, PPA. Flathub: revisit ≥12 months post-launch (AI policy + sandbox hostility documented in research).

Site (parallel workstream once v1 alpha exists): upgrade docs-site to Astro 6 + Starlight at `/docs`, typeable launcher demo as landing centerpiece, `/install` per-distro page, changelog. The `/plugins` store directory (§6) arrives with v2, generated from the `hop-extensions` monorepo manifests. Site repo/dir decision made then.

## 13. Milestones

**[Amended 2026-07-31] Six milestones, not five.** The old M3 split in two — GNOME first so daily-driving starts at the earliest possible point, cross-platform second so a slip in X11 grab work cannot block the thing the author actually uses. Files moved out of M2 (it was the milestone's largest piece by far, and §11's latency test runs against scripted fake providers, so it never needed the real indexer) and became the *first* slice of the providers milestone. Milestone numbers in the tracker match their M-numbers.

- **M1 — Core**: workspace, hop-protocol, hop-core with ported test suites green (fuzzy/router/learning/aliases). **Landed 2026-07-30.** Now also carries the 8 `Bug` findings from the M1 sweep; #45, #48 and #49 (query path) land before M2 wires ranking into the daemon, and #36 (unconditional parent-directory chmod) before M2 computes a real state path.
- **M2 — Daemon**: hopd serving apps+calculator over the socket; `hop query --json` and `hop exec` prove the loop headlessly; latency test green including its adversarial arm. Threat model for the socket boundary written *before* the read loop; then a walking skeleton (socket + framed codec + handshake + one hardcoded Item, end to end) that later slices thicken. `rt-multi-thread` tokio runtime — M1's trait already assumes it via its `+ Send` bound. Socket activation + systemd user unit. Read-only config load. Carries 19 sweep findings, 9 folded into slices as acceptance criteria. Ends with its own OWASP sweep.
- **M3 — Frontend (GNOME)**: design pass first (§8a: tokens + mock frames of the 6 key states, approved by Pedro), then the standalone hop-gtk app on the author's GNOME Wayland session (toggle, search, launch). Keymap from config. Headless CI + `--screenshot`. **Not a GNOME Shell extension** — an ordinary desktop application; the only extension in v1 is the ~200-line D-Bus shim in M5 (§7).
- **M4 — Frontend (cross-platform)**: layer-shell path verified on a wlroots session; X11 session verified; hop-hotkeyd (X11 grab loop, zbus portal backend). One OWASP sweep covering M3 and M4 together — the same application on different sessions.
- **M5 — Full v1 providers**: **files first**, then windows (all platforms incl. the shim, submitted to e.g.o as soon as its D-Bus interface is stable, to absorb the review latency §14 flags), web search, emoji, timezone, currency, weather. Datasets are **vendored** — generated artefacts committed alongside their generator scripts and a LICENSES file — because §12's Nix flake builds network-isolated and could not run a build-time download. Its OWASP sweep must re-run **A10 (SSRF)**, which the M1 sweep recorded as not-applicable only because nothing could yet make an outbound request.
- **M6 — Polish + release**: settings UI, keymap capture UI + conflict detection, theme ecosystem, doctor, docs, rollout steps 1–3. Final OWASP sweep covering theming and the release path. **Author daily-drives Hop from M3 onward** — on apps + calculator until M5 lands files.

## 14. Risks

- **Rust learning curve** (author from JS): mitigated by salvaged working Rust modules, ported test suites as guardrails, and small crate boundaries. Accepted cost of "best-in-class".
- **GNOME focus/activation quirks**: the toggle-client token pattern is the field-proven mitigation; shim exists if edge cases bite; `hop doctor` surfaces misconfiguration.
- **e.g.o review latency for the shim**: windows-on-GNOME may trail the v1 binary release by weeks; acceptable, degradation is graceful.
- **Scope creep**: the previous two attempts died of it. Contract: nothing enters v1 beyond §5; new ideas go to the v1.x/v2 backlog.
- **Vicinae velocity**: they may improve GNOME support. Hop's bet is depth (native shim + GNOME-first polish + sandbox roadmap) over breadth; re-evaluate positioning at v1 launch.
