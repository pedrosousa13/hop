# Read-only config load and a real state directory (Issue #60) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development (recommended) or superpowers:executing-plans, plus superpowers:test-driven-development. Work task-by-task, red-green-refactor, and keep the workspace green at every task boundary.

**Goal:** Give hopd its first contact with the user's real filesystem, per issue #60's acceptance criteria: a read-only TOML config load at `$XDG_CONFIG_HOME/hop/config.toml` (absent → documented defaults; malformed → explicit startup error, never a silent fallback), a state directory computed once from `$XDG_STATE_HOME` (fallback documented), a learning store loaded at startup and persisted as launches are recorded, and an integration test proving persistence across a daemon restart plus a loudly-failing malformed config.

**Architecture today (what #60 wires in):**
- `hopd::run()` (`crates/hopd/src/lib.rs`) resolves the runtime dir, builds a tokio runtime, then `server::serve(&runtime_dir)`.
- `server::serve` builds `HostSource::new(Arc::new(build_host()))` — a **fresh empty `Pipeline`**. The persisted-learning seam already exists: `HostSource::with_pipeline` (`crates/hopd/src/source.rs:256`) was built for this exact issue (#60) and is used only by `crates/hopd/tests/assembly.rs`.
- `Pipeline` (`crates/hop-core/src/pipeline.rs:556`) has `pub learning: Learning`; `Learning::load(path)` / `load_reporting(path)` / `save(path)` already degrade-to-empty, load, and persist atomically at 0600 (`crates/hop-core/src/learning.rs:879-1131`). `record_launch(query, item_id)` is the entry point.
- `MAX_RESULTS` is a `pub const usize = 50` in `crates/hopd/src/source.rs:213`, whose own doc says *"Issue #60's config load is where it becomes a setting a user can change"* — this slice must make it one.
- **No daemon code records a launch today.** The connection's Execute arm (`crates/hopd/src/connection.rs:305-390`) resolves item + action and dispatches via `ResultSource::execute`, then re-sends nothing about learning. `Exchange` retains only `id`, `source`, `delivered` — **no query text** — so launch recording needs the query text retained on the exchange.
- **No `toml` dependency exists** anywhere in the workspace (checked `Cargo.lock`). Config is TOML per spec §9, so `toml` (MIT/Apache-2.0, fits `deny.toml`'s allow list) is the first real addition.
- Integration tests use two harnesses: in-process `serve_with` with scripted sources (`crates/hopd/tests/common/mod.rs`, `exec.rs`, `lifecycle.rs`), and a spawned real binary (`crates/hopd/tests/socket.rs`, `spawn_daemon`). `spawn_daemon` pins `HOME`, `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, `XDG_DATA_DIRS` — but **not** `XDG_CONFIG_HOME`/`XDG_STATE_HOME`, which it must once `run()` reads them.

## Global Constraints

- **One new dependency, and only one:** `toml` (workspace + `crates/hopd`). Justify it in `deny.toml`'s own prose the way the existing entries are justified (spec §9 says config is TOML; there is no hand-rolled TOML parser). Every other crate stays untouched.
- **Gate commands, all four required at every task boundary:**
  `cargo test --workspace` · `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny check`.
- **No `.unwrap()` in production code** (`clippy::unwrap_used` + `-D warnings`). Test files / test modules open with `#![allow(clippy::unwrap_used)]`.
- **No AI attribution** in commits or the PR.
- **No silent fallback from a malformed config.** An absent config is *documented defaults*; a config that exists but does not parse is an explicit startup error that refuses to start (`run()` returns `ExitCode::FAILURE`, message on stderr). Same posture `runtime_dir.rs` takes toward a missing `XDG_RUNTIME_DIR`, and `Aliases::from_json` takes toward invalid JSON.
- **Do not widen `MAX_RESULTS` past `MAX_ITEMS_PER_RESULTS_FRAME`.** The existing compile-time `assert!(MAX_RESULTS <= ...)` (source.rs:222) protects the replace-frame invariant; the config value must enforce the same bound at load time, as a parse error, so a config cannot break the frame contract at runtime.

## In scope — the issue's acceptance criteria

1. Config loads read-only at startup from the standard config path.
2. An absent config yields documented defaults; a malformed config is an explicit startup error, not a silent default.
3. The state directory path is computed once from the standard state path, with its fallback documented.
4. The learning store loads at startup and persists recorded launches across a daemon restart.
5. The store file is written atomically and owner-only.
6. An integration test asserts persistence across a restart, and that a malformed config fails loudly.

Explicitly **out of scope**: config watching, config writing, `hop config get/set`, `hop doctor`, socket activation (all named in the issue or spec as later work).

## Design decisions (read before any task)

**1. `Config` is one struct with one setting today.** `Config { max_results: usize }`, default `MAX_RESULTS` (50). TOML shape is flat — `max_results = 50` at top level. No invented sections: the only future-proofing that is *not* speculative is keeping the struct `#[non_exhaustive]`-free and letting more keys arrive with the slices that read them (watching/writing). A value that does not parse, or parses but exceeds `MAX_ITEMS_PER_RESULTS_FRAME`, is `ConfigError` → `run()` refuses to start.

**2. `Config::load()` resolves the standard config path itself.** `$XDG_CONFIG_HOME` if set and non-empty, else `$HOME/.config` per the XDG Base Directory spec — then `hop/config.toml`. Absent file → `Ok(Config::default())`. File present but unreadable → error (same class as a permission error anywhere else). File present but invalid TOML → error naming the path. `std::fs::read_to_string` + `toml::from_str`. Returns `Result<Config, ConfigError>`; the error type lives in the module and is `thiserror`-derived.

**3. `state_dir::resolve()` mirrors `runtime_dir::resolve()`'s posture but *with* the documented XDG fallback.** Runtime dir deliberately refuses a fallback (socket location is a security decision); the state dir is not the socket boundary, and the XDG spec *defines* `$XDG_STATE_HOME`'s fallback as `$HOME/.local/state`, which this repo's own `learning.rs` prose already names (`~/.local/state/hop`). So: `$XDG_STATE_HOME` set + non-empty → `<it>/hop`; else `<HOME>/.local/state/hop`; neither set → error (house in a box, same as `runtime_dir`). The module creates the `hop` dir at 0700 via `DirBuilder::mode(0o700)`, tolerating `AlreadyExists`, exactly like `runtime_dir.rs` — with the same asymmetry documented (a dir this code created is narrowed to 0700; one the environment already provided is left as found). It is a new module `crates/hopd/src/state_dir.rs`, sibling to `runtime_dir.rs`.

**4. `HostSource` grows `max_results` and an optional learning store path.** `HostSource { host, pipeline, max_results: usize, learning_path: Option<PathBuf> }`. `new(host)` keeps today's behavior (`max_results = MAX_RESULTS`, `learning_path = None`) — this is what keeps every existing test and the assembly.rs harness compiling unchanged. `with_pipeline(host, pipeline)` likewise gains the two defaults. A new constructor `with_config(host, pipeline, max_results, learning_path)` is what `run()` uses. The accumulator inside `start` uses `self.max_results` (replacing the `MAX_RESULTS` literal at source.rs:333); the `pub const MAX_RESULTS` stays as the documented default and for the compile-time frame-bound assertion.

**5. Launch recording is a new `ResultSource` seam, driven by the connection.** The learning store keys on `(query, item_id)`; the connection is the only place that holds both (the query it accepted, the item it resolved). Add `async fn record_launch(&self, query: &str, item_id: &ItemId)` to `ResultSource`. `HostSource` implements it: lock the pipeline, `learning.record_launch(query, item_id)`, and if `learning_path` is `Some`, `learning.save(path)` — logging (via the existing `eprintln!` seam, matching `StderrLog`) rather than failing, because a persistence hiccup after a launch has already happened must not turn a successful execute into a client-visible error. The connection calls it on the Execute arm only when `source.execute` returned `Ok` — a launch is a successful action, not an attempted one (matches how `record_launch` is seeded in every existing test). A scripted/test source implements the seam however its scenario wants (a no-op is fine where the test drives success without caring about learning).

**6. `Exchange` retains the query text.** Add `text: String` to `Exchange`, set in the `ClientMsg::Query` arm from `text.into_string()`. The Execute arm reads it back for `record_launch`. This is the one structural change to the connection; it mirrors the existing "two halves are one struct because they are one invariant" reasoning — the query that produced `delivered` is exactly the query a launch under that exchange must be keyed on.

**7. Production wiring: `server::serve` gains a config-aware path, `run()` drives it.** `run()`: (1) `config::load()` — error → stderr + `ExitCode::FAILURE`; (2) `state_dir::resolve()` — error → stderr + `FAILURE`; (3) build tokio runtime; (4) `let store_path = state_dir.join(STORE_FILE_NAME)` where `STORE_FILE_NAME = "learning.json"` (the name this repo's own learning tests use); (5) `let mut pipeline = Pipeline::default(); pipeline.learning = Learning::load(&store_path);` (6) `HostSource::with_config(Arc::new(build_host()), Arc::new(Mutex::new(pipeline)), config.max_results, store_path)`; (7) `server::serve_with(&runtime_dir, source)`. `build_host` stays where it is (private to `server.rs`); `serve_with` already exists and is the integration seam — `run()` now calls it directly, and `serve(&runtime_dir)` becomes a thin documented convenience that keeps compiling for any test that used it (none do today beyond `run()`; re-point `run()` to the new path and keep `serve` for API-compat or delete it — the implementer decides, but no dead code may remain, so if nothing calls it, delete it).
- Existing in-process integration tests (exec.rs, lifecycle.rs, assembly.rs, host.rs, apps.rs) are unaffected: they build their own sources and call `serve_with` — they never touch `run()`/config/state.
- `socket.rs::spawn_daemon` MUST pin `XDG_CONFIG_HOME` and `XDG_STATE_HOME` to paths under `runtime_dir` (alongside the existing `HOME`/`XDG_DATA_HOME`/`XDG_DATA_DIRS` pins) so the spawned binary's new config/state resolution stays hermetic and never touches a developer's real `~/.config` or `~/.local/state`.

## File structure

**Created:**
- `crates/hopd/src/config.rs` — `Config`, `ConfigError`, `Config::load()`. Unit tests in-module.
- `crates/hopd/src/state_dir.rs` — `resolve()`, `STORE_FILE_NAME` (or the name lives in the module that loads/saves). Unit tests in-module.
- `crates/hopd/tests/state.rs` — integration: persistence across restart + malformed config fails loudly (criterion 6). Reuses `common` harness where possible.
- `crates/hopd/tests/config.rs` (or fold into `state.rs`) — binary-level test that a malformed config makes the spawned `hopd` exit non-zero / never bind (criterion 2's "fails loudly" half at the process level).

**Modified:**
- `Cargo.toml` (workspace) — `toml = { version = "1", default-features = false, features = ["parse"] }` in `[workspace.dependencies]`.
- `crates/hopd/Cargo.toml` — `toml.workspace = true` under `[dependencies]`.
- `crates/hopd/src/lib.rs` — `run()` wires config → state → pipeline → source (Design decision 7); `pub mod config;` `pub mod state_dir;`.
- `crates/hopd/src/server.rs` — `serve`/`serve_with` re-pointing per Design decision 7; `build_host` unchanged.
- `crates/hopd/src/source.rs` — `HostSource` gains `max_results` + `learning_path`; new constructor; `ResultSource::record_launch` added; `HostSource::record_launch` impl; accumulator uses `self.max_results`; `MAX_RESULTS` doc updated to "default".
- `crates/hopd/src/connection.rs` — `Exchange.text`; Query arm sets it; Execute arm calls `source.record_launch` on `Ok`.
- `crates/hopd/tests/socket.rs` — `spawn_daemon` pins `XDG_CONFIG_HOME` + `XDG_STATE_HOME`.
- `deny.toml` — no allow-list change needed (toml is MIT); the `[graph]` comment needs no change either; but add nothing unless `cargo deny check` demands it. If the license allow list already covers `toml` + its transitive deps (`serde_spanned`, `toml_datetime`, `toml_edit`, `winnow` — all MIT/Apache-2.0), nothing to add.
- `CONTEXT.md` — only if the slice introduces a glossary term (per `/domain-modeling`); likely none beyond what exists (`state dir`, `config` are plain words).

## Tasks (work in order; every boundary green)

### Task 1 — Workspace deps + `config.rs`
- [ ] Add `toml` to `[workspace.dependencies]` and `crates/hopd/Cargo.toml`.
- [ ] `Config { max_results: usize }` with `Default = 50`, `ConfigError` (`thiserror`), `Config::load()` reading `$XDG_CONFIG_HOME` (fallback `$HOME/.config`) `/hop/config.toml`.
- [ ] Absent file → `Ok(Config::default())`; invalid TOML → error naming the path; `max_results > MAX_ITEMS_PER_RESULTS_FRAME` → error. Read-only — never writes anything.
- [ ] Unit tests: absent → defaults; malformed → `Err`; valid flat TOML → parsed value; over-frame `max_results` → `Err`; missing `XDG_CONFIG_HOME` + `$HOME` → explicit error; env fallback to `$HOME/.config` honored.
- [ ] All four gates green.

### Task 2 — `state_dir.rs`
- [ ] `resolve() -> io::Result<PathBuf>`: `$XDG_STATE_HOME` set+non-empty → `<it>/hop`; else `$HOME/.local/state/hop`; neither → error. Creates the `hop` dir at 0700 via `DirBuilder`, tolerating `AlreadyExists`, exactly per `runtime_dir.rs`'s documented asymmetry.
- [ ] `STORE_FILE_NAME` const (`"learning.json"`) lives here or in the config/learning wiring module — one location, named once.
- [ ] Unit tests: `XDG_STATE_HOME` honored; fallback to `$HOME/.local/state`; dir created 0700; pre-existing dir left as found; missing both envs → error.
- [ ] All four gates green.

### Task 3 — `HostSource` config-aware + `run()` wiring
- [ ] `HostSource` gains `max_results: usize` and `learning_path: Option<PathBuf>`; `new`/`with_pipeline` keep defaults; add `with_config`.
- [ ] Accumulator uses `self.max_results`; `MAX_RESULTS` doc → "the default"; the frame-bound `assert!` stays against `MAX_RESULTS`.
- [ ] `server.rs`: re-point `run()`'s call site; remove dead `serve` if unused or keep a documented convenience (no dead code).
- [ ] `lib.rs::run()`: config → state → pipeline load → `HostSource::with_config` → `serve_with`. Errors exit `FAILURE` with stderr lines (config error names the path and the parse reason).
- [ ] `socket.rs::spawn_daemon` pins `XDG_CONFIG_HOME` + `XDG_STATE_HOME` under `runtime_dir`.
- [ ] Tests: `run()` with a malformed config returns `FAILURE`; `with_config` source assembles with the configured `max_results` (mirror the existing `max_results_is_applied_to_the_whole_assembled_set_not_per_provider` test with a non-default value); default (`new`) behavior unchanged.
- [ ] All four gates green.

### Task 4 — Launch recording + persistence
- [ ] `ResultSource::record_launch(&self, query, item_id)` added; trait docs updated.
- [ ] `Exchange.text` retained; Query arm sets it; Execute arm calls `source.record_launch(text, item_id)` only on `Ok` from `execute`, before sending `Executed`.
- [ ] `HostSource::record_launch` records and, when `learning_path` is `Some`, saves (logging via `eprintln!`, never failing the execute).
- [ ] Every existing `ResultSource` impl (test sources in connection.rs tests, common/mod.rs's, exec.rs, lifecycle.rs) implements the seam (no-op where the test doesn't care).
- [ ] Unit/real-socket test: a launch through the socket lands in the store file and the file is 0600 and atomic (one temp file during write); a negative test that no `record_launch` fires on a refused/`Err` execute.
- [ ] All four gates green.

### Task 5 — Integration: persistence across restart + malformed config fails loudly (criterion 6)
- [ ] Real-socket test (`crates/hopd/tests/state.rs`): start a daemon with a scripted source over a temp state dir, query a query text that matches, execute successfully, stop the daemon, reload `Learning::load(state_dir/learning.json)` and assert the recorded launch survived (boost or recent-launches shape). This is the "across a restart" proof at the load/save boundary: same store file, two process lifetimes.
- [ ] Binary-level test (spawned `hopd`): `XDG_CONFIG_HOME` pointing at a dir with a malformed `config.toml` → process exits non-zero and the socket never appears (fails loudly, criterion 2's process half).
- [ ] An absent-config spawned `hopd` still starts and serves (the existing `socket.rs` round-trip test covers this; keep it passing).
- [ ] All four gates green.

## Acceptance mapping

| Criterion | Where |
| --- | --- |
| 1 config loads read-only at startup | Task 1, Task 3 |
| 2 absent → defaults; malformed → loud error | Task 1, Task 5 |
| 3 state dir computed once, fallback documented | Task 2, Task 3 |
| 4 store loads at startup, persists launches across restart | Task 3, Task 4, Task 5 |
| 5 atomic + owner-only writes | Task 4 (pinned by existing learning.rs tests) |
| 6 integration: restart persistence + malformed config loud | Task 5 |

## Verification

At the end, all four gates green:
`cargo test --workspace` · `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny check`.
Then issue #60's brief is the review Spec for `/review`.
