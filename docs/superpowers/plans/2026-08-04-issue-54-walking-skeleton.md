# M2.2 Walking Skeleton Implementation Plan (issue #54)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The thinnest end-to-end path through every layer: a `hopd` daemon on a real Unix socket, a framed codec with an enforced byte cap, a mandatory version handshake, and a `hop` CLI that gets one hardcoded item back as JSON.

**Architecture:** Three pieces. (1) `hop-protocol` grows a pure, IO-free framing module (length-prefixed JSON with an exported `MAX_FRAME_BYTES` cap) plus two new `ErrorCode` variants. (2) A new `crates/hopd` binary crate: tokio multi-threaded Unix-socket server in a 0700 runtime dir, handshake-first connection state machine, hardcoded single-item response. (3) A new `crates/hop-cli` binary crate (`hop` binary): blocking std `UnixStream` client, `query` and `version` subcommands, plus the end-to-end test that spawns the real daemon.

**Tech Stack:** Rust edition 2024, tokio (daemon only), serde/serde_json, std blocking sockets (CLI), tempfile (tests).

## Global Constraints

- Landing gate (all four must pass): `cargo test --workspace` · `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo deny check`
- `clippy::unwrap_used` warns workspace-wide and CI runs `-D warnings`: **no `.unwrap()` in production code**. Tests open with `#![allow(clippy::unwrap_used)]` (existing pattern, see `crates/hop-protocol/src/wire.rs` tests).
- `unsafe_code = "deny"` workspace-wide. Every new crate manifest must contain `[lints]\nworkspace = true` (the workspace `Cargo.toml` comment mandates this for new members).
- Dependencies: crates.io only; every license must satisfy the allow list `{GPL-3.0-only, MIT, MPL-2.0, Unicode-3.0}` (an `OR` expression with one allowed arm passes). The only new transitive deps expected are tokio's net stack (`mio` MIT, `socket2` MIT-or-Apache) — both pass.
- Commit message style (from git log): `Area: sentence in imperative-ish prose`, e.g. `Wire: make an icon a name or a path, and check the file before a client reads it`. **No AI attribution in commits.**
- New crate manifests copy `crates/hop-core/Cargo.toml` conventions exactly: `version = "0.1.0"`, `edition.workspace = true`, `license.workspace = true`, `repository.workspace = true`, `[lints] workspace = true`. Path dependencies carry a version too — `hop-protocol = { path = "../hop-protocol", version = "0.1.0" }` — because `cargo deny check bans` flags bare path deps as wildcards (the reasoning is a comment in `crates/hop-core/Cargo.toml`; keep that comment's rule, don't re-litigate it).
- Doc-comment density in this repo is high and rationale-heavy. Match it: every public item documents *why*, and every deliberate gap names the issue that owns the residual.
- The issue brief's acceptance criteria are the spec. The threat model `docs/security/2026-08-02-m2-socket-boundary-threat-model.md` ("The boundary", "Entry points that are not frames") binds the socket-creation details.

---

### Task 1: hop-protocol — MAX_FRAME_BYTES, framing module, two ErrorCode variants

**Files:**
- Modify: `crates/hop-protocol/src/limits.rs` (add `MAX_FRAME_BYTES` + doc + composition test update)
- Modify: `crates/hop-protocol/src/wire.rs` (add `ErrorCode::FrameTooLarge`, `ErrorCode::HandshakeRequired`)
- Create: `crates/hop-protocol/src/framing.rs`
- Modify: `crates/hop-protocol/src/lib.rs` (add `pub mod framing; pub use framing::*;`)

**Interfaces:**
- Consumes: existing `limits` module conventions, `ClientMsg`/`DaemonMsg`.
- Produces (Tasks 2 and 3 rely on these exact names):
  - `hop_protocol::limits::MAX_FRAME_BYTES: usize = 268_435_456` (256 MiB)
  - `hop_protocol::framing::FRAME_PREFIX_LEN: usize = 4` — prefix is a **u32 big-endian** payload byte count
  - `hop_protocol::framing::FrameError` — `#[derive(Debug, Error)]` with variants `TooLarge { len: usize }`, `Encode(#[from] serde_json::Error)` (split decode into its own variant if cleaner: `Decode(serde_json::Error)`)
  - `pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError>` — serializes to JSON, refuses a payload over `MAX_FRAME_BYTES` **before** returning, returns `[4-byte BE prefix][payload]` as one `Vec<u8>`
  - `pub fn payload_len(prefix: [u8; FRAME_PREFIX_LEN]) -> Result<usize, FrameError>` — decodes the prefix and returns `Err(FrameError::TooLarge { .. })` for any value over `MAX_FRAME_BYTES`; **this function is the pre-allocation gate** — callers only allocate after it returns Ok
  - `pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError>` — `serde_json::from_slice` wrapper

Design notes the implementer must carry into doc comments:

- `MAX_FRAME_BYTES = 268_435_456` (256 MiB). Reasoning: `limits.rs`'s "What the bounds compose to" table prices the worst-case in-bounds `results` frame at ~84 MB of field content before JSON syntax and escaping. 256 MiB admits every honest frame with ~3× headroom for syntax and realistic escaping. It deliberately **refuses** the pathological frame that is fully `\uXXXX`-escaped at every field bound (~505 MB): the cap exists to bound what a peer can make the process allocate, and a frame only reachable by adversarial escaping is exactly what it is for. Cross-reference issue #21 (this constant closes it when #54 lands) and the buffering caveat on `ClientMsg`.
- The framing module is **deliberately IO-free**: pure functions over bytes, so the tokio daemon and the blocking-std CLI share one codec and the module needs no async dependency. Transport does the reads; `payload_len` decides before any payload allocation.
- New `ErrorCode` variants: `FrameTooLarge` (daemon refuses an over-cap prefix), `HandshakeRequired` (any frame before `Hello` is refused — folded issue #26's criterion). Adding variants is a wire-contract change; note it in the enum docs the same way `IconSpec` documents its change.

- [ ] **Step 1: Write failing tests** in `framing.rs`'s `#[cfg(test)] mod tests` (open with `#![allow(clippy::unwrap_used)]`):
  - `a_frame_round_trips_through_encode_and_decode` — encode a `ClientMsg::Hello { api_version: 1 }`, split prefix/payload, `payload_len` returns payload's length, `decode_payload` returns the original.
  - `the_prefix_is_the_payload_length_big_endian` — assert the first 4 bytes equal `(payload.len() as u32).to_be_bytes()`.
  - `a_prefix_over_the_cap_is_refused` — `payload_len(((MAX_FRAME_BYTES as u32) + 1).to_be_bytes())` is `Err(FrameError::TooLarge { .. })`.
  - `a_prefix_at_the_cap_is_allowed` — exactly `MAX_FRAME_BYTES` is `Ok`.
  - `encoding_refuses_a_payload_over_the_cap` — construct an over-cap payload cheaply (e.g. a `serde_json::Value` string of `MAX_FRAME_BYTES` bytes) and assert `TooLarge`. If materializing 256 MiB in a unit test is unacceptable (it is: CI memory), instead test the boundary through a helper: factor the check into `fn ensure_within_cap(len: usize) -> Result<(), FrameError>` used by both `encode_frame` and `payload_len`, and unit-test the helper at the boundary values. Public API stays as specified.
  - In `limits.rs`: extend the existing composition test (`the_documented_worst_case_is_what_the_constants_compose_to`) or add a sibling asserting `MAX_FRAME_BYTES >= 3 * worst_case_frame_total` so retuning item bounds cannot silently outgrow the cap.
- [ ] **Step 2: Run tests, verify they fail** (`cargo test -p hop-protocol`) — compile errors for missing module count as failing.
- [ ] **Step 3: Implement** `MAX_FRAME_BYTES`, the two `ErrorCode` variants (with doc comments), and `framing.rs`.
- [ ] **Step 4: Run** `cargo test -p hop-protocol` — all green, including the existing wire/limits suites.
- [ ] **Step 5: Gate locally**: `cargo fmt --all` then `cargo clippy -p hop-protocol --all-targets -- -D warnings`.
- [ ] **Step 6: Commit**: `Protocol: length-prefixed framing with an exported frame cap, checked before allocation`

### Task 2: hopd — the daemon crate

**Files:**
- Modify: `Cargo.toml` (workspace `members` += `"crates/hopd"`)
- Create: `crates/hopd/Cargo.toml`
- Create: `crates/hopd/src/main.rs` (thin: parse nothing, call `hopd::run()`)
- Create: `crates/hopd/src/lib.rs` (modules + `run()`)
- Create: `crates/hopd/src/runtime_dir.rs`
- Create: `crates/hopd/src/server.rs`
- Create: `crates/hopd/tests/socket.rs`

**Interfaces:**
- Consumes (from Task 1): `hop_protocol::{ClientMsg, DaemonMsg, ErrorCode, ProtoError, API_VERSION}`, `hop_protocol::framing::{encode_frame, payload_len, decode_payload, FRAME_PREFIX_LEN}`, `hop_protocol::limits::MAX_FRAME_BYTES` (only via `framing` — **do not** re-check the cap in the daemon; the acceptance criterion says the maximum is the protocol crate's constant, not a value redefined here).
- Produces (Task 3 relies on): a `hopd` binary that, given `XDG_RUNTIME_DIR`, listens on `$XDG_RUNTIME_DIR/hop/hopd.sock` and serves the protocol below. Also `hopd::socket_path_from_env() -> Result<PathBuf, ...>` is internal — the CLI derives the path itself in Task 3 (two lines; a shared crate for one path is not worth the coupling yet).

**Manifest** (`crates/hopd/Cargo.toml`): package `hopd`, workspace-inherited `edition`/`license`/`repository`, `[lints] workspace = true`. Deps: `hop-protocol = { path = "../hop-protocol", version = "0.1.0" }`, `tokio = { workspace = true, features = ["net", "rt-multi-thread", "io-util"] }` (no `serde_json` — the framing module owns all encode/decode) (features add to the workspace set: sync, time, macros, rt). Dev-deps: `tempfile.workspace = true`.

**Behavior spec:**

1. **Runtime dir** (`runtime_dir.rs`): read `XDG_RUNTIME_DIR`; unset or empty → error naming the variable and exit non-zero (the spec assumes a systemd session; guessing a fallback path is a security decision this slice must not make — say so in the doc comment, citing the threat model's note that the variable is user-controlled input). Create `$XDG_RUNTIME_DIR/hop` with `std::fs::DirBuilder` + `std::os::unix::fs::DirBuilderExt::mode(0o700)` so the directory is **born** at 0700 — no create-then-chmod window. A pre-existing dir is left as found, not chmodded (precedent: `learning.rs::persist_atomically`, cited in the threat model).
2. **Socket** (`server.rs`): path `<runtime_dir>/hopd.sock`. If the file already exists, remove it before binding — provisional single-session behavior; the real single-instance guard is a later M2 slice, and the doc comment says so. After `UnixListener::bind`, set the socket file's permissions to **0600** (`std::fs::set_permissions`) — the threat model requires this slice to *decide* the mode rather than inherit the umask; the parent dir's 0700 carries the access control during the bind-to-chmod window, which the comment states.
3. **Runtime**: `#[tokio::main(flavor = "multi_thread")]` on `main` (or `Builder::new_multi_thread` in `run()`) — an acceptance criterion; the provider trait's `Send` bound assumes it.
4. **Per-connection task** (spawned per accept):
   - Read loop: read exactly `FRAME_PREFIX_LEN` bytes → `payload_len(prefix)` → on `Err(TooLarge)` send `DaemonMsg::Error { query_id: None, error: ProtoError { code: ErrorCode::FrameTooLarge, message: ... } }` and **close without reading or allocating for the payload**. Otherwise read exactly that many bytes, `decode_payload::<ClientMsg>` → malformed JSON: send `Error` (code `MalformedFrame`) and close — peer-fault, not daemon-fault, matching `framing.rs`'s `Encode`/`Decode` split (the `MalformedFrame` variant is Task 1's third `ErrorCode` addition).
   - **Handshake state machine**: state starts `AwaitingHello`. Any frame other than `Hello` in that state → `Error { code: HandshakeRequired }` and close. `Hello { api_version }` equal to `API_VERSION` → reply `DaemonMsg::HelloAck { api_version: API_VERSION }`, state becomes `Ready`. Mismatch → `Error { code: VersionMismatch, message: names both versions }` and close.
   - In `Ready`: `Query { id, text: _ }` → reply `Results { query_id: id, partial: false, items: vec![hardcoded_item()] }` then `QueryDone { query_id: id }`. A second `Hello`, or `Cancel`/`Execute` → `Error { code: Internal, message: "not implemented in the walking skeleton" }`, connection stays open. EOF → task ends.
5. **Hardcoded item** (`hardcoded_item()` in `server.rs`; constructors return `Result` — use `expect("within bounds by construction")`, never `unwrap`):

```rust
Item {
    id: ItemId::new("hop:walking-skeleton").expect("within bounds by construction"),
    kind: Kind::Action,
    title: "Hello from hopd".to_string(),
    subtitle: Some("M2.2 walking skeleton".to_string()),
    icon: None,
    actions: vec![Action {
        id: ActionId::new("open").expect("within bounds by construction"),
        kind: ActionKind::Open,
        label: "Open".to_string(),
    }],
    default_action: ActionId::new("open").expect("within bounds by construction"),
    copy_text: None,
    append_to_end: false,
    provider: "skeleton".to_string(),
}
```

6. **Logging**: `eprintln!` for accept/connection errors only — the logging seam is issue #34, blocked on a later slice; don't build one here.
7. **Shutdown**: none beyond process kill. No signal handling in this slice (socket-activation slice #62 owns lifecycle); stale-socket removal above is what makes restart work.

- [ ] **Step 1: Write the integration test first** — `crates/hopd/tests/socket.rs` (`#![allow(clippy::unwrap_used)]`). Helper: spawn `env!("CARGO_BIN_EXE_hopd")` with `XDG_RUNTIME_DIR` set to a fresh `tempfile::tempdir()`, poll for the socket path to appear (50 × 100ms, then panic), return child + path; kill the child in a drop guard. Client side: blocking `std::os::unix::net::UnixStream` + the framing functions. Tests:
  - `the_round_trip_returns_one_item_end_to_end` — connect, `Hello` → `HelloAck { api_version: 1 }`, `Query { id: 7, text }` → `Results` with exactly one item titled `Hello from hopd` and `query_id: 7`, then `QueryDone { query_id: 7 }`.
  - `a_query_before_the_handshake_is_refused` — connect, send `Query` first → `Error` with code `HandshakeRequired`, then EOF (read returns 0).
  - `an_oversize_length_prefix_is_refused_without_the_payload_being_read` — connect, complete the handshake? No: send the oversize prefix as the very first bytes… the handshake gate would also refuse a *valid* frame, so to prove the cap specifically, complete the handshake first, then write `((MAX_FRAME_BYTES as u32) + 1).to_be_bytes()` and nothing else → `Error` with code `FrameTooLarge`, then EOF. (That the daemon never allocates is enforced by construction — `payload_len` before any read — and reviewed, not asserted at runtime.)
  - `a_version_mismatch_is_an_explicit_error` — `Hello { api_version: 999 }` → `Error` with code `VersionMismatch`, then EOF.
  - `the_runtime_dir_is_created_at_mode_0700_and_the_socket_at_0600` — after startup, stat `<tmp>/hop` and assert `mode & 0o777 == 0o700`; stat the socket, assert `0o600`.
  - `an_unset_runtime_dir_is_a_startup_error` — spawn with `XDG_RUNTIME_DIR` removed → non-zero exit, stderr names the variable.
- [ ] **Step 2: Run** `cargo test -p hopd` — fails (nothing exists).
- [ ] **Step 3: Implement** manifest, workspace member, `runtime_dir.rs`, `server.rs`, `lib.rs`, `main.rs` per the behavior spec.
- [ ] **Step 4: Run** `cargo test -p hopd` — green.
- [ ] **Step 5: Gate locally**: `cargo fmt --all`, `cargo clippy -p hopd --all-targets -- -D warnings`, `cargo deny check` (new transitive deps arrived).
- [ ] **Step 6: Commit**: `Daemon: hopd listens on a 0700-dir socket, handshake first, one item end to end`

### Task 3: hop-cli — the `hop` binary, and the CLI-level end-to-end test

**Files:**
- Modify: `Cargo.toml` (workspace `members` += `"crates/hop-cli"`)
- Create: `crates/hop-cli/Cargo.toml`
- Create: `crates/hop-cli/src/main.rs`
- Create: `crates/hop-cli/src/lib.rs` (arg parsing + client, so unit tests reach them)
- Create: `crates/hop-cli/tests/e2e.rs`

**Interfaces:**
- Consumes: Task 1's framing API and wire types; Task 2's running daemon and socket path convention `$XDG_RUNTIME_DIR/hop/hopd.sock`.
- Produces: binary named `hop` (`[[bin]] name = "hop", path = "src/main.rs"`), package name `hop-cli`.

**Manifest:** workspace-inherited fields, `[lints] workspace = true`. Deps: `hop-protocol = { path = "../hop-protocol", version = "0.1.0" }`, `serde_json.workspace = true`. **No tokio** — the CLI blocks on one socket; `std::os::unix::net::UnixStream` with the shared IO-free framing is the whole transport, and the doc comment says why. Dev-deps: `tempfile.workspace = true`.

**Behavior spec:**

- Args (hand-rolled over `std::env::args` — two subcommands don't justify a parser dependency yet; the doc comment notes clap becomes worth it when `exec|toggle|doctor` arrive):
  - `hop version` → print two lines: `hop <CARGO_PKG_VERSION>` and `protocol <API_VERSION>`, exit 0.
  - `hop query <text>` → run the query flow below.
  - Anything else (no args, unknown subcommand, `query` with no text) → usage to stderr, exit **2**.
- Query flow: derive socket path from `XDG_RUNTIME_DIR` (unset → error to stderr, exit 1); connect; send `Hello { api_version: API_VERSION }`; expect `HelloAck` (an `Error` frame or anything else → stderr, exit 1); send `Query { id: 1, text: QueryText::new(text) mapped to a stderr+exit-1 on over-bound input }`; then read frames: each `Results` → print every item as one `serde_json::to_string(&item)` line on stdout; `QueryDone { query_id: 1 }` → exit 0; `Error` frame → stderr, exit 1. Frames with a mismatched `query_id` → skip (stale-frame drop is #55's slice; a one-line comment).

- [ ] **Step 1: Write failing unit tests** in `lib.rs` for arg parsing (parse into a small `enum Command { Version, Query(String), Usage }`): `version_parses`, `query_with_text_parses`, `query_without_text_is_usage`, `no_args_is_usage`, `unknown_subcommand_is_usage`.
- [ ] **Step 2: Write the e2e test** — `crates/hop-cli/tests/e2e.rs` (`#![allow(clippy::unwrap_used)]`):
  - Daemon binary path: derive as sibling of `env!("CARGO_BIN_EXE_hop")` (`Path::new(env!("CARGO_BIN_EXE_hop")).parent().unwrap().join("hopd")`), with an `assert!(path.exists(), "hopd binary not built — run cargo test --workspace")`. A comment explains: `CARGO_BIN_EXE_*` only covers the current package's bins; under the workspace gate both are built, and the assert turns the `-p hop-cli`-only corner into a named failure instead of a confusing spawn error.
  - `the_cli_query_round_trips_and_exits_zero` — tempdir as `XDG_RUNTIME_DIR`, spawn `hopd` (same poll-for-socket helper as Task 2 — duplicate the ~20-line helper; a shared test-util crate for one helper is not yet warranted), run `hop query hello` via `std::process::Command` with the same env, assert: exit status 0, stdout is exactly one line, that line parses as an `Item` with title `Hello from hopd`.
  - `the_version_subcommand_prints_both_versions` — run `hop version` (no daemon needed), assert exit 0, stdout contains `CARGO_PKG_VERSION` value and `protocol 1`.
- [ ] **Step 3: Run** `cargo test -p hop-cli` — fails.
- [ ] **Step 4: Implement** manifest, member entry, `lib.rs`, `main.rs`.
- [ ] **Step 5: Run** `cargo test --workspace` — green (this is the first run proving the whole skeleton: protocol → daemon → CLI).
- [ ] **Step 6: Gate locally**: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check`.
- [ ] **Step 7: Commit**: `CLI: hop query round-trips the socket and prints the item; hop version prints both versions`

---

## Acceptance criteria → task map (issue #54)

| Criterion | Task |
| --- | --- |
| Daemon listens on a socket inside a 0700 dir under the runtime dir | 2 |
| CLI query returns one hardcoded item as JSON end to end, exit 0 | 3 |
| Handshake precedes every other frame; early query refused (#26) | 1 (variant) + 2 (enforcement) |
| Over-cap length prefix refused without allocating (#21) | 1 (gate) + 2 (use) |
| The maximum is the protocol crate's exported constant | 1 |
| Multi-threaded runtime | 2 |
| Version subcommand prints binary + protocol versions | 3 |
| Integration test spawns the daemon, drives a real socket, asserts round trip + both refusals | 2 (daemon-side refusals + round trip); 3 adds the CLI-level round trip |
