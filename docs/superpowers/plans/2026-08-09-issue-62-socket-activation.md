# Socket activation and systemd user units (Issue #62) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `hopd` accept a listening socket systemd's service manager already bound, instead of always binding its own — per issue #62's six acceptance criteria — while leaving the standalone path (no inherited listener; the path every existing test in this crate exercises) byte-for-byte unchanged.

**Architecture:** A new pure module, `crates/hopd/src/activation.rs`, decides *whether* activation applies — `inherited_fd(lookup, self_pid) -> Option<InheritedFd>`, implementing the sd_listen_fds(3) protocol against an injected environment lookup, no process, no unsafe, unit-tested with a fake. `crates/hopd/src/server.rs` gains one new function, `acquire_listener`, which is the *only* place in this crate — and, after this plan lands, in this workspace's production code — that contains `unsafe`: turning the raw fd `activation::inherited_fd` names into a `tokio::net::UnixListener` needs exactly one `OwnedFd::from_raw_fd` call, after which every remaining step (`UnixListener::from`, `set_nonblocking`, `tokio::net::UnixListener::from_std`) is safe. `serve_with`'s accept loop — the code every existing test already exercises — does not change at all; only how it obtains its listener does. A new pair of unit files, `contrib/systemd/hopd.socket` and `contrib/systemd/hopd.service`, and a new `README.md` section, are this repository's first non-Rust shipped assets and satisfy criteria 1 and 6. A new integration test, `crates/hopd/tests/activation.rs`, spawns a real `hopd` binary with a real, already-bound listener placed at file descriptor 3 and `LISTEN_FDS`/`LISTEN_PID` set correctly — reproducing the sd_listen_fds(3) contract exactly, without a live systemd user session, which this repository's CI does not have.

**Tech Stack:** No new crate. `tokio::net::UnixListener::from_std` is available under the `net` feature `hopd` already enables (`crates/hopd/Cargo.toml`); `std::env`, `std::process::id()` and `std::os::fd` cover the whole sd_listen_fds(3) protocol. `libc` (already a workspace dependency, used today only by `hop-protocol`) is added to `hopd`'s `[dev-dependencies]` for one test-only `pre_exec` closure in Task 4 — never to `hopd`'s `[dependencies]`.

## Global Constraints

- **No new production dependency.** Verified before writing this plan, not assumed: `hopd`'s effective tokio features are `sync, time, macros, rt` (workspace) plus `net, rt-multi-thread, io-util` (`crates/hopd/Cargo.toml`) — `net` is what gates `UnixListener::from_std` (confirmed by reading `tokio-1.53.1`'s own source directly, the exact version this workspace's `Cargo.lock` pins: `~/.cargo/registry/src/…/tokio-1.53.1/src/net/unix/listener.rs:116-162`). `libc = "0.2"` is already a `[workspace.dependencies]` entry (root `Cargo.toml`) that `hopd` does not currently draw on; this plan adds `libc.workspace = true` to `crates/hopd/Cargo.toml`'s `[dev-dependencies]` only, for Task 4's test-only `pre_exec` closure — `cargo deny check` therefore sees no new crate anywhere in the graph.
- **Gate commands, all four, run after every task:** `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace` (615 tests today, all green — verified by running the suite before writing this plan) · `cargo deny check`.
- **No `.unwrap()`/`.expect()` in production code** (`clippy::unwrap_used` + `-D warnings`). Test modules open with `#![allow(clippy::unwrap_used)]`, matching every existing test file in this crate.
- **Exactly one new `unsafe`, in production code, isolated to one call.** Root `Cargo.toml`'s `[workspace.lints.rust] unsafe_code = "deny"` carries a doc comment stating plainly what the lint covers and why `deny` rather than `forbid` was chosen — quoted in full under Design decision 1, because this plan is the first to spend the exception the comment describes in `src/`. Today's only `unsafe` in the tree is test-only (`crates/hop-protocol/src/content.rs:1673-1677`, a narrow `#[expect(unsafe_code, reason = "…")]` around a single `libc::mkfifo` statement). This plan's one production `unsafe` — `acquire_listener` in `crates/hopd/src/server.rs` — takes the same narrow, statement-scoped `#[expect(unsafe_code, reason = "…")]` shape, with a `SAFETY:` comment, matching that precedent exactly.
- **Test-only `unsafe` in Task 4**, on the same footing as `hop-protocol`'s own precedent: a `pre_exec` closure (itself an `unsafe fn` on `CommandExt`) calling `libc::dup2`/`libc::fcntl` to hand a spawned test process a pre-bound fd at a fixed number. Test-only; production code carries none of this.
- **No AI attribution** in commits.

## Scope: what this slice is and is not

**In scope**, the six acceptance criteria on issue #62:

1. A socket unit and a service unit are installed to the correct user-unit path — Task 3.
2. Starting the socket unit and then issuing a query activates the daemon — Task 4 (with the honest limits on what an automated test run in this CI can prove — see Design decision 5 — plus Task 3's README section as the manual-verification half).
3. The daemon accepts a listener inherited from the service manager — Task 1 (the parsing), Task 2 (the conversion and the unit test that proves the resulting listener actually accepts connections), Task 4 (a real process, over a real inherited fd, with an inode check that rules out a coincidental standalone rebind).
4. The daemon still runs standalone, with no inherited listener — Task 2 (unchanged code path, plus a new fast unit test), and, unmodified, every test in `crates/hopd/tests/socket.rs`, `lifecycle.rs`, `host.rs`, `apps.rs`, `calculator.rs`, `assembly.rs`, `exec.rs`, `state.rs` — all of which spawn or drive a standalone `hopd` today and none of which set `LISTEN_FDS`/`LISTEN_PID`, so this plan's own gate (`cargo test --workspace`) re-proves this criterion on every task, not only once.
5. The socket directory mode is 0700 under activation, not only under standalone start — Design decision 3 (the daemon does nothing at all to the directory or socket file under activation, deliberately; the unit's own `DirectoryMode=`/`SocketMode=` carry it instead — Task 3, verified with `systemd-analyze verify --user`), Task 4 (an integration-level assertion against the real files).
6. The install step is documented — Task 3 (`README.md`).

**Not in scope, deliberately:**

- **Orderly shutdown and signal handling.** `crates/hopd/src/lib.rs`'s own doc comment on `run()` currently says shutdown "belong[s] to issue #62 (socket activation and lifecycle)" — that sentence conflated two different things under one issue number before either was scoped. Issue #62's six acceptance criteria are all about *activation*; none mentions `SIGTERM`, `sd_notify`, or graceful drain. This plan corrects that doc comment (Task 2) to say plainly that shutdown remains unowned by any filed issue, rather than silently implementing it as a side effect of this slice or silently leaving the stale claim in place.
- **`Type=notify` / readiness notification.** No acceptance criterion asks for `sd_notify(READY=1)`, and `Type=simple` (this plan's choice for `hopd.service`) needs none: under socket activation the socket itself — not the service — is what a client interacts with first, and it is already listening (bound by the `.socket` unit, or in Task 4's test, by the test itself) before the service unit ever starts. Adding `sd_notify` would be a new production dependency (`libc`'s `sendto` on `NOTIFY_SOCKET` by hand, or the `sd-notify` crate) for a signal nothing in this issue asks for.
- **A general install/build script.** No `scripts/`, `Makefile`, `justfile` or asset directory exists in this repository today, and no earlier plan has added one. This plan does not add one either — `README.md` documents the (short) manual steps directly, the same way `README.md`'s existing "Build and test" section documents `cargo test --workspace` directly rather than through a wrapper script.
- **Amending the threat model.** `docs/security/2026-08-02-m2-socket-boundary-threat-model.md` names #62 in five places (`"The boundary"`'s entry-points list, T13's exposure note, and three Follow-up-table rows) and states its own amendment convention explicitly: "the M2 slices are what turn the rest of it into behaviour... the M2 OWASP sweep ([#52]) audits the real code rather than inheriting this model's verdicts." That sweep, not this slice, is where this document's own convention says the correction belongs. This plan does not edit that file.
- **Multiplexing more than one inherited socket.** `hopd` listens on exactly one socket; Design decision 2 states what happens if a `.socket` unit ever declares more than one (`LISTEN_FDS > 1`) — the first is used and the rest are named in a warning, never an error.

## Design decisions (read before any task)

**1. The one `unsafe` call, taken directly, not through a crate that hides the same call.**

Root `Cargo.toml`'s `[workspace.lints.rust]` block, in full, is the standard this plan holds itself to:

```
# Production code in this workspace contains no `unsafe`, and this is what keeps
# it that way rather than a claim in a document that drifts. What the lint covers
# is normal compilation of both members — `src/` and `#[cfg(test)]` code alike,
# which is why the workspace's one `unsafe` block, a `libc::mkfifo` call inside
# `#[cfg(test)] mod tests` in `hop-protocol`, needs a narrow
# `#[expect(unsafe_code)]` at the statement to build at all. That `expect` is
# itself the proof the lint reaches test code: were it not firing there, the
# unfulfilled expectation would warn, and CI's `-D warnings` would fail.
#
# What it does not cover is doc tests. […]
#
# `deny` rather than `forbid` so that an exception is *possible* — a crate that
# has to talk to a C API should be able to, with a `SAFETY:` comment and a
# reviewer. `forbid` cannot be overridden anywhere, which would mean the next
# genuine need for FFI turns into a pull request that weakens this line for the
# whole workspace instead of one that annotates a single call.
unsafe_code = "deny"
```

Turning `LISTEN_FDS`'s first descriptor into a usable listener needs exactly one call the standard library has no safe spelling for: `OwnedFd::from_raw_fd(fd)`. Everything after that — `std::os::unix::net::UnixListener::from(owned_fd)` (a safe `From` impl), `.set_nonblocking(true)` (safe), `tokio::net::UnixListener::from_std(std_listener)` (safe; returns `io::Result`, does not itself need `unsafe`) — is ordinary safe Rust, verified by compiling this exact chain as a standalone probe before writing it into this plan (`rustc --edition 2021`, zero errors, zero warnings). This is production code's first `unsafe` in the workspace; the doc comment above is written for exactly this moment, and this plan's `acquire_listener` (Task 2) is scoped to one statement with an `#[expect(unsafe_code, reason = "…")]` and a `SAFETY:` comment, the same shape `content.rs`'s test-only exception already takes.

**Rejected alternative: a crate that does the same fd-to-listener conversion (`listenfd`, `sd-notify`).** Whichever crate is picked, turning a bare integer the kernel handed the process into an owned socket type has no safe spelling at that boundary either — the unsafe call does not go away, it moves into a dependency's source this plan's own reviewer does not read line-by-line as part of this change. (This argument is structural, about what the FFI boundary requires, not a claim this plan verified against either crate's actual source — see "What I could not verify" below.) What such a crate would cost here, concretely: a new line in `deny.toml`'s dependency graph for a wrapper around three lines of code this plan can write directly, and a `cargo update` surface this repository's own `deny.toml` preamble says it would rather not carry ("Nothing here is grandfathered… checked by it like everything else" — a sentence about `libc`/`tempfile`, arriving before the gate existed, that this plan does not want to be quoted against a *new* dependency added *after* the gate exists for a three-line need). Three lines, one `#[expect]`, one `SAFETY:` comment, reviewed in this diff, beats a dependency carrying the identical unsafe out of view.

**2. `LISTEN_PID` is checked; a mismatch, or anything else that doesn't check out, means standalone — never an error.**

The protocol, implemented by `activation::inherited_fd` (Task 1): activation applies only when `LISTEN_PID` parses as `u32` **and** equals `std::process::id()`, **and** `LISTEN_FDS` parses as `usize` **and** is at least `1`. The first inherited descriptor is always `SD_LISTEN_FDS_START = 3`, fixed by the protocol itself, not a choice this daemon makes.

Every other outcome — either variable absent, either failing to parse, a `LISTEN_PID` naming a different process, `LISTEN_FDS` parsing to `0` — is treated as "no inherited listener," on the same refuse-to-guess footing `runtime_dir.rs`'s own doc comment already states for `XDG_RUNTIME_DIR`: *"the variable is still environment the user controls, not a guarantee this process can make for itself"* — and the threat model's own framing, cited by that same doc comment, that a value derived from user-controlled environment "is not one the process can reason about unaided." The case a wrong guess here would actually hurt: a child process that merely *inherits* `LISTEN_PID`/`LISTEN_FDS` through `fork()` from a genuinely activated `hopd` (an unlikely but not impossible shape — a supervisor, a shell wrapper) has no fd of its own at 3 and must not mistake its parent's activation for its own. Checking `LISTEN_PID == std::process::id()` is exactly the check the sd_listen_fds(3) protocol specifies for this reason, and it is the only one of the two variables whose absence-vs-mismatch actually matters operationally — get it wrong, and `acquire_listener` calls `OwnedFd::from_raw_fd` on a descriptor number that may not refer to anything this process owns.

**`LISTEN_FDS > 1`:** used, not refused. `hopd` listens on exactly one socket, so only the first inherited descriptor (fd 3) is ever passed to `acquire_listener`. A `.socket` unit that declares more sockets than this daemon consumes is a unit-file authoring mistake, not a reason to refuse to start — `acquire_listener`'s caller logs one `eprintln!` line naming how many were declared and that only the first is used, and proceeds. This mirrors `build_host`'s own existing posture on a registration error ("a daemon that refuses to start over one misconfigured provider is worse than one that serves the rest") applied to a misconfigured unit file instead of a misconfigured provider.

**3. Under activation, `acquire_listener` does not remove, bind, or `chmod` the socket file at all — the service manager owns it.**

`server.rs`'s existing doc comment on `serve_with` ("The socket's mode is decided, not inherited") documents *why* the standalone path removes a stale file, binds, and narrows the mode to 0600 after `bind` — because nothing upstream of that call has already decided the mode. Under activation, something upstream *has* already decided it: the `.socket` unit's own `SocketMode=0600`/`DirectoryMode=0700` (Task 3), enforced by systemd before `hopd` is ever spawned. Verified real systemd directives before writing them into a unit file (`man systemd.socket`, this machine's installed systemd 255.4): `SocketMode=` — "the file system access mode used when creating the file node… Defaults to 0666"; `DirectoryMode=` — "the file system access mode used when creating [parent] directories… Defaults to 0755." Both default wider than this daemon wants, which is exactly why this plan sets them explicitly rather than leaving them at systemd's own defaults — the same reasoning `server.rs`'s existing doc gives for not leaving the standalone socket at the umask's mode.

So under activation `acquire_listener` does nothing to the path at all: no `remove_file` (nothing is stale — the fd it was handed *is* the live socket), no `bind` (already done), no `set_permissions` (already 0600/0700 by the unit). This is also what makes the Task 4 integration test's inode check meaningful: if `acquire_listener` ever regressed to removing-and-rebinding under activation too, that test's inode-stability assertion (Design decision 5) fails immediately, in CI, without a systemd session.

**4. The testability split: parsing is a pure function; the unsafe conversion is a separate function, tested separately, by a different method.**

`activation::inherited_fd(lookup: impl Fn(&str) -> Option<String>, self_pid: u32) -> Option<InheritedFd>` takes the environment as a parameter rather than reading it, exactly the shape `crates/hopd/src/state_dir.rs`'s own `resolve_from_env` already established for the identical reason, stated in that module's own doc comment: *"the workspace denies `unsafe_code` (and Rust 2024 makes `env::set_var` `unsafe`), so tests cannot safely mutate process env."* Nine unit tests (Task 1) exercise every branch — matching pid and count, an excess count, missing/unparseable `LISTEN_PID`, missing/unparseable/zero `LISTEN_FDS`, a mismatched pid — with a fake `lookup` closure, no process, no unsafe, no filesystem.

`acquire_listener(runtime_dir: &Path, activation: Option<activation::InheritedFd>) -> io::Result<tokio::net::UnixListener>` is the one function containing `unsafe`, and it also takes its input as a parameter rather than reading env — `serve_with` is the only production caller that ever supplies `Some`/`None` from real `std::env::var` calls. This means Task 2's own unit tests can exercise the unsafe path with a **real** fd (a genuine `std::os::unix::net::UnixListener` bound in the test, its raw fd handed in directly) without needing real `LISTEN_FDS`/`LISTEN_PID` environment variables or a subprocess at all — proving the fd-to-listener conversion actually produces a working, connectable listener, entirely in-process. Task 4's integration test is the only one that needs a real second process, because it is the only one proving the *environment-reading* half against a real spawned `hopd` rather than against `inherited_fd` in isolation.

**5. Criterion 2 cannot run under real systemd in this CI — covered by a real inherited-fd integration test plus documented manual verification, and this plan is explicit about what each proves.**

`.github/workflows/ci.yml`'s three jobs (`ci`, `supply-chain`, `latency-gate`) all run on bare `ubuntu-latest` with no `systemctl --user`, no D-Bus session bus, and no lingering enabled — confirmed by reading the workflow file directly, not assumed. A test that requires a live systemd user session cannot run there.

**What Task 4's test does instead, and why it is not a weaker substitute:** it spawns the real `hopd` binary as a genuinely separate OS process, with a real `std::os::unix::net::UnixListener` — bound by the test, exactly the way a `.socket` unit's `ListenStream=`/`SocketMode=`/`DirectoryMode=` would have — handed to it at file descriptor 3, and `LISTEN_FDS=1`/`LISTEN_PID=<hopd's own real pid>` set correctly. This is the actual sd_listen_fds(3) contract, reproduced exactly, not mocked.

**The one real engineering problem this raises, solved and verified, not assumed:** `LISTEN_PID` must equal the value `hopd` itself reads back from `std::process::id()` — but `std::process::Command` fixes a child's environment before `spawn()` returns, and the child's own pid is not knowable to the parent until `spawn()` has already forked; there is no hook between "the fork happened" and "the child execs" for a caller of `Command` to inject a pid it just learned. The fix, verified with a throwaway probe before writing it into this plan (not assumed): spawn `sh -c "export LISTEN_FDS=1; export LISTEN_PID=$$; exec $HOPD_PATH"`. `$$` in a shell is the shell's own pid, and `exec` replaces the shell's process image with `hopd`'s **without changing the pid** — so the value `$$` resolves to immediately before `exec` is exactly what `hopd` reads back from `std::process::id()` after it. The probe confirmed both halves directly: the inner process's `getpid()` matched the `$$` captured before its own `exec`, and a file descriptor `dup2`'d onto fd 3 before the outer `sh` starts survives the shell's own `exec` unchanged (`ls -la /proc/self/fd/3` inside the exec'd process showed the same socket inode throughout).

**The one `unsafe` this requires is `pre_exec`'s closure** — itself an `unsafe fn` on `std::os::unix::process::CommandExt`, because its closure runs between `fork` and `exec` and must stick to async-signal-safe calls, which `dup2`/`fcntl` are. Test-only, the same footing as `hop-protocol`'s existing `libc::mkfifo` precedent — not production code, and this workspace's `unsafe_code = "deny"` lint reaches test code identically (per the doc comment quoted in Design decision 1), so this needs the same narrow `#[expect(unsafe_code, reason = "…")]`.

**What this test does and does not prove, stated plainly rather than implied:** it proves a real `hopd` process, started with exactly the environment and descriptor systemd's protocol specifies, serves a real query round trip over that descriptor — and, via an inode-stability check (the socket file's inode before spawning must equal its inode after a successful round trip), that it did so *without* falling back to a standalone bind that happened to still work at the same path. **It does not prove that systemd itself invokes `hopd` this way**, that `.socket`/`.service` activation triggers correctly under a real user session, or that `DirectoryMode=`/`SocketMode=` behave as documented in a real systemd runtime — `systemd-analyze verify --user` (installed on this development machine, systemd 255.4) confirms `contrib/systemd/hopd.socket` and `contrib/systemd/hopd.service` parse as valid unit syntax and reference a real, executable `ExecStart=` target (verified directly, output recorded in Task 3), but no code in this repository, before or after this plan, exercises a live systemd user session. That gap is closed by the manual verification steps `README.md` documents (Task 3), not by an automated test, and this plan does not claim otherwise.

**Rejected alternative: shell out to `systemd-socket-activate` and skip when absent.** `systemd-socket-activate(1)` exists on this development machine and could plausibly drive a real activation-shaped test. Rejected because it would make the *strongest* test in this plan the one most likely to silently skip in CI (no systemd on `ubuntu-latest` runners per the check above), which is exactly backwards for the criterion this issue cares most about proving automatically; the `pre_exec`/`dup2` approach runs everywhere `sh` and `libc` do, which is everywhere this crate's other integration tests already require.

**6. Unit files live in `contrib/systemd/`; install is documented in `README.md`, not a script.**

`contrib/` is this plan's choice for the repository's first non-Rust shipped asset: a directory name with wide precedent across daemons that ship systemd units without a packaging pipeline (nginx, redis, postgres all use it), signaling "ships with the project, not required to build or test it" — distinct from `docs/`, which this repository already uses exclusively for prose. No `scripts/`, `Makefile` or asset directory exists today (verified by listing the repo root), so there is no existing convention this plan could instead follow or would be breaking.

`%t` is confirmed, from `man systemd.unit` on this machine, as systemd's runtime-directory specifier — "This is either `/run/` … or the path `"$XDG_RUNTIME_DIR"`" for a user unit — so `ListenStream=%t/hop/hopd.sock` names exactly `$XDG_RUNTIME_DIR/hop/hopd.sock`, the same path `crate::runtime_dir::resolve()` computes and `server.rs`'s `SOCKET_FILE_NAME` constant names, cross-checked by a test (Task 3) that fails if either drifts from the other. `%h` (confirmed the same way — "the home directory of the user running the service manager instance") lets `hopd.service`'s `ExecStart=%h/.cargo/bin/hopd` match `cargo install --path crates/hopd`'s default output location without a hardcoded username, which is what `README.md`'s install step (Task 3) uses rather than inventing an install script this repository has no other precedent for.

Both files, in their final form, were validated with `systemd-analyze verify --user contrib/systemd/hopd.socket contrib/systemd/hopd.service` on this development machine (systemd 255.4) — exit 0, no warnings — with `ExecStart=` pointed at a real, executable file for the check (the `%h` form cannot be verified end-to-end without a real `~/.cargo/bin/hopd`, so the literal-path form was verified and the specifier substitution separately confirmed via systemd's own reported expansion in a deliberately-failing run naming the exact expanded path). This is a one-time check performed while writing this plan, not part of the gate — `cargo deny`/`clippy`/`fmt`/`test` say nothing about unit-file syntax, and CI has no systemd to run `systemd-analyze` against.

## File Structure

**Created:**
- `crates/hopd/src/activation.rs` — the pure sd_listen_fds(3) parser and its unit tests.
- `contrib/systemd/hopd.socket` — the socket unit.
- `contrib/systemd/hopd.service` — the service unit.
- `crates/hopd/tests/activation.rs` — the integration test driving a query over a real inherited listener.

**Modified:**
- `crates/hopd/src/lib.rs` — `pub(crate) mod activation;`; the `run()` doc comment's `# Shutdown` section corrected (Scope, above).
- `crates/hopd/src/server.rs` — `acquire_listener` (the one `unsafe` call), `serve_with` calls it instead of binding inline; doc comments extended with a new `# Socket activation` section; new unit tests.
- `crates/hopd/Cargo.toml` — `libc.workspace = true` under `[dev-dependencies]`.
- `README.md` — a new "Running hopd as a systemd user service" section (install steps, satisfying criterion 6).

**Not modified, deliberately:** `deny.toml` (no new dependency graph node), `.github/workflows/ci.yml` (no systemd needed by any test this plan adds — see Design decision 5), `docs/security/2026-08-02-m2-socket-boundary-threat-model.md` (Scope, above), `crates/hopd/tests/common/mod.rs` (Self-review notes, below, records a pre-existing inaccuracy in its module doc that this plan found but did not cause and does not fix).

---

### Task 1: The pure activation parser

**Files:**
- Create: `crates/hopd/src/activation.rs`
- Modify: `crates/hopd/src/lib.rs` (module declaration only)

**Interfaces:**
- Produces, for Task 2:
  ```rust
  pub(crate) struct InheritedFd { pub(crate) fd: std::os::fd::RawFd, pub(crate) declared: usize }
  pub(crate) fn inherited_fd(lookup: impl Fn(&str) -> Option<String>, self_pid: u32) -> Option<InheritedFd>;
  pub(crate) const SD_LISTEN_FDS_START: std::os::fd::RawFd; // = 3
  ```

No `hop-core`/`hop-protocol` types, no I/O, no `unsafe`. Pure function of its two parameters.

- [ ] **Step 1: Write the module, tests included**

Create `crates/hopd/src/activation.rs`:

```rust
//! Parsing systemd's socket-activation environment — sd_listen_fds(3) — as
//! a pure function, kept apart from [`crate::server::acquire_listener`],
//! which is the only place in this crate (and, after this module lands,
//! this workspace's production code) that contains `unsafe`.
//!
//! systemd hands an activated process an already-bound, already-listening
//! socket as an inherited file descriptor, named by two environment
//! variables: `LISTEN_PID` (this process's own pid, so a descendant that
//! merely *inherits* the variable through `fork` does not mistake its
//! parent's activation for its own) and `LISTEN_FDS` (how many descriptors,
//! starting at [`SD_LISTEN_FDS_START`], were passed). Both must check out
//! for activation to apply. Anything else — either variable absent,
//! unparseable, or a pid that does not match this process — is not an
//! error, it is simply "no inherited listener," the same refuse-to-guess
//! footing [`crate::runtime_dir`] already takes with `XDG_RUNTIME_DIR`:
//! environment is input from a user-controlled process tree
//! (`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "The
//! boundary"), not a fact this module can verify, so a value that does not
//! check out is treated as absence rather than corruption. See this crate's
//! implementation plan
//! (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
//! Design decision 2) for the full reasoning, including what happens when
//! `LISTEN_FDS` declares more than this daemon's one socket.

use std::os::fd::RawFd;

/// The first (and, for this daemon, only) inherited descriptor's number,
/// fixed by the sd_listen_fds(3) protocol itself — not configurable, and
/// not this daemon's choice.
pub(crate) const SD_LISTEN_FDS_START: RawFd = 3;

/// What [`inherited_fd`] found.
pub(crate) struct InheritedFd {
    /// Always [`SD_LISTEN_FDS_START`] — hopd listens on exactly one socket,
    /// so this is the only descriptor it ever reads.
    pub(crate) fd: RawFd,
    /// The value `LISTEN_FDS` parsed to. `1` is what this daemon's own
    /// `.socket` unit (`contrib/systemd/hopd.socket`) produces; anything
    /// higher means the unit file declared more sockets than this daemon
    /// consumes. [`Self::fd`] is still valid and still used either way —
    /// see [`crate::server::acquire_listener`]'s caller for what it does
    /// with a `declared` above `1`.
    pub(crate) declared: usize,
}

/// Checks whether `lookup` (a stand-in for [`std::env::var`], taken as a
/// parameter so this function stays pure and testable with a fake, rather
/// than mutating real process environment — the same reason
/// [`crate::state_dir::resolve_from_env`] takes its inputs as parameters)
/// describes systemd socket activation for a process whose own pid is
/// `self_pid`.
///
/// Returns `Some` only when **both** hold: `LISTEN_PID` parses as a `u32`
/// and equals `self_pid`, **and** `LISTEN_FDS` parses as a `usize` that is
/// at least `1`. Every other case returns `None`, meaning "bind
/// standalone" — never an error. See this module's own doc comment for why
/// a value that fails to check out is treated as absence rather than
/// something worth reporting.
pub(crate) fn inherited_fd(
    lookup: impl Fn(&str) -> Option<String>,
    self_pid: u32,
) -> Option<InheritedFd> {
    let listen_pid: u32 = lookup("LISTEN_PID")?.parse().ok()?;
    if listen_pid != self_pid {
        return None;
    }
    let listen_fds: usize = lookup("LISTEN_FDS")?.parse().ok()?;
    if listen_fds == 0 {
        return None;
    }
    Some(InheritedFd {
        fd: SD_LISTEN_FDS_START,
        declared: listen_fds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn matching_pid_and_a_positive_count_is_activation() {
        let found = inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "1")]), 42)
            .expect("this is exactly the activated case");
        assert_eq!(found.fd, SD_LISTEN_FDS_START);
        assert_eq!(found.declared, 1);
    }

    #[test]
    fn a_count_above_one_is_still_activation_with_the_full_count_reported() {
        let found = inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "3")]), 42)
            .expect("still activation; the caller decides what to do about the extra fds");
        assert_eq!(
            found.fd, SD_LISTEN_FDS_START,
            "only the first fd is ever named"
        );
        assert_eq!(found.declared, 3, "the full count is reported, not clamped");
    }

    #[test]
    fn no_listen_pid_is_not_activation() {
        assert!(inherited_fd(env(&[("LISTEN_FDS", "1")]), 42).is_none());
    }

    #[test]
    fn no_listen_fds_is_not_activation() {
        assert!(inherited_fd(env(&[("LISTEN_PID", "42")]), 42).is_none());
    }

    #[test]
    fn a_mismatched_pid_is_not_activation() {
        // The case that matters most in practice: a descendant of a truly
        // activated hopd that inherited both variables through fork(),
        // with no fd of its own at SD_LISTEN_FDS_START.
        assert!(inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "1")]), 43).is_none());
    }

    #[test]
    fn an_unparseable_pid_is_not_activation() {
        assert!(
            inherited_fd(env(&[("LISTEN_PID", "not-a-number"), ("LISTEN_FDS", "1")]), 42)
                .is_none()
        );
    }

    #[test]
    fn an_unparseable_count_is_not_activation() {
        assert!(
            inherited_fd(
                env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "not-a-number")]),
                42
            )
            .is_none()
        );
    }

    #[test]
    fn a_zero_count_is_not_activation() {
        assert!(inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "0")]), 42).is_none());
    }

    #[test]
    fn a_negative_count_is_not_activation() {
        // LISTEN_FDS is documented as a non-negative count; a negative or
        // otherwise garbage string must fail usize::parse the same way an
        // unparseable one does, not be accepted as some other meaning.
        assert!(inherited_fd(env(&[("LISTEN_PID", "42"), ("LISTEN_FDS", "-1")]), 42).is_none());
    }
}
```

- [ ] **Step 2: Confirm the tests do not run yet**

Run: `cargo test -p hopd activation::`
Expected: matches zero tests — `activation` is not declared as a module in `lib.rs` yet, so `cargo` does not compile this file into the crate at all.

- [ ] **Step 3: Declare the module**

In `crates/hopd/src/lib.rs`, add `pub(crate) mod activation;` to the module list, alphabetically first (`activation`, `apps`, `calculator`, `config`, `connection`, `runtime_dir`, `server`, `source`, `state_dir`) — matching `connection`'s existing `pub(crate)` visibility, since nothing outside this crate needs either.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p hopd activation::`
Expected: PASS, all nine tests.

- [ ] **Step 5: Run the gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```
Expected: all four green.

- [ ] **Step 6: Commit**

```bash
git add crates/hopd/src/activation.rs crates/hopd/src/lib.rs
git commit -m "hopd: parse the systemd socket-activation environment"
```

---

### Task 2: `acquire_listener` — the one `unsafe` call, wired into `serve_with`

**Files:**
- Modify: `crates/hopd/src/server.rs`
- Modify: `crates/hopd/src/lib.rs` (doc comment only)

**Interfaces:**
- Consumes: Task 1's `activation::InheritedFd`, `activation::inherited_fd`.
- Produces: `fn acquire_listener(runtime_dir: &Path, activation: Option<activation::InheritedFd>) -> io::Result<tokio::net::UnixListener>` (crate-private; `serve_with`'s only caller).

- [ ] **Step 1: Write the failing tests**

In `crates/hopd/src/server.rs`, add a new test module (placed after the existing `build_host_tests`):

```rust
#[cfg(test)]
mod acquire_listener_tests {
    #![allow(clippy::unwrap_used)]

    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::activation::InheritedFd;

    #[tokio::test]
    async fn with_no_activation_it_binds_and_chmods_the_socket_path_exactly_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let _listener = acquire_listener(dir.path(), None).unwrap();

        let socket_path = dir.path().join(SOCKET_FILE_NAME);
        assert!(socket_path.exists(), "the standalone path must still bind the socket file");
        let mode = std::fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the standalone path must still narrow the mode");
    }

    #[tokio::test]
    async fn with_an_inherited_fd_it_never_touches_the_runtime_dir_path() {
        let backing = tempfile::tempdir().unwrap();
        let std_listener =
            std::os::unix::net::UnixListener::bind(backing.path().join("preexisting.sock"))
                .unwrap();
        let fd = std_listener.into_raw_fd();

        let unrelated_dir = tempfile::tempdir().unwrap();
        let never_created = unrelated_dir.path().join("never-created-subdir");

        let result = acquire_listener(&never_created, Some(InheritedFd { fd, declared: 1 }));
        assert!(
            result.is_ok(),
            "acquire_listener must accept the inherited fd: {:?}",
            result.err()
        );
        assert!(
            !never_created.exists(),
            "activation must never create, bind inside, or otherwise touch the runtime dir path"
        );
    }

    #[tokio::test]
    async fn a_listener_built_from_an_inherited_fd_actually_accepts_connections() {
        let backing = tempfile::tempdir().unwrap();
        let path = backing.path().join("real.sock");
        let std_listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let fd = std_listener.into_raw_fd();

        let listener = acquire_listener(backing.path(), Some(InheritedFd { fd, declared: 1 }))
            .unwrap();

        let accept_task = tokio::spawn(async move { listener.accept().await });
        let _client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let accepted = accept_task.await.unwrap();
        assert!(
            accepted.is_ok(),
            "a listener rebuilt from an inherited fd must actually accept: {:?}",
            accepted.err()
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p hopd acquire_listener`
Expected: FAIL to compile — `acquire_listener` does not exist yet.

- [ ] **Step 3: Implement `acquire_listener` and wire it into `serve_with`**

In `crates/hopd/src/server.rs`, add imports (`std::os::fd::{FromRawFd, OwnedFd}`, `crate::activation`), then replace the inline bind logic inside `serve_with` with a call to a new function:

```rust
pub async fn serve_with<S: ResultSource>(runtime_dir: &Path, source: S) -> io::Result<()> {
    let activation = activation::inherited_fd(|k| std::env::var(k).ok(), std::process::id());
    let listener = acquire_listener(runtime_dir, activation)?;

    // ... the accept loop below is byte-for-byte unchanged ...
}

/// Turns either an inherited descriptor or `runtime_dir` into a working
/// listener. See this crate's implementation plan
/// (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
/// Design decisions 1 and 3) for the full reasoning behind both branches.
fn acquire_listener(
    runtime_dir: &Path,
    activation: Option<activation::InheritedFd>,
) -> io::Result<UnixListener> {
    match activation {
        Some(found) => {
            if found.declared > 1 {
                eprintln!(
                    "hopd: LISTEN_FDS declared {} descriptors; hopd listens on one \
                     socket, so only the first (fd {}) is used",
                    found.declared, found.fd
                );
            }

            // SAFETY: `found.fd` came from `activation::inherited_fd`, which
            // only returns `Some` once `LISTEN_PID` has matched this
            // process's own pid — the sd_listen_fds(3) contract's own
            // guarantee at that point is that the named descriptor is a
            // valid, open, already-bound-and-listening socket this process
            // now owns exclusively. This is the only `unsafe` in this crate,
            // and the only one in this workspace's production code
            // (root `Cargo.toml`'s `[workspace.lints.rust] unsafe_code`
            // doc comment; the tree's one prior `unsafe` is test-only, in
            // `hop-protocol`'s `content.rs`). See this crate's
            // implementation plan, Design decision 1, for why this is taken
            // directly rather than through a crate that hides the same call.
            #[expect(
                unsafe_code,
                reason = "sd_listen_fds(3) hands the daemon a raw fd; OwnedFd::from_raw_fd \
                          is the only way to take ownership of it, and every step after \
                          this one is safe"
            )]
            let owned = unsafe { OwnedFd::from_raw_fd(found.fd) };

            let std_listener = std::os::unix::net::UnixListener::from(owned);
            // tokio::net::UnixListener::from_std requires non-blocking mode
            // (tokio 1.53.1's own doc comment on that function: "Passing a
            // listener in blocking mode is always erroneous... it could
            // panic" — and its `check_socket_for_blocking` helper
            // `debug_assert`s on exactly this in a debug build, which is
            // what `cargo test` runs under). A descriptor inherited from
            // systemd is not guaranteed to already be non-blocking, so this
            // is set explicitly rather than assumed.
            std_listener.set_nonblocking(true)?;
            tokio::net::UnixListener::from_std(std_listener)
        }
        None => {
            // Exactly today's standalone path, unchanged: see this
            // function's own module doc ("The socket's mode is decided,
            // not inherited") for the stale-removal and chmod reasoning.
            let socket_path = runtime_dir.join(SOCKET_FILE_NAME);
            match std::fs::remove_file(&socket_path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            let listener = UnixListener::bind(&socket_path)?;
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
            Ok(listener)
        }
    }
}
```

- [ ] **Step 4: Extend `serve_with`'s doc comment with a new `# Socket activation` section**

Immediately after the existing `# The socket's mode is decided, not inherited` section (retitle its opening sentence to note it now describes the standalone branch only), add:

```rust
/// # Socket activation
///
/// When `LISTEN_PID`/`LISTEN_FDS` describe activation for this exact
/// process ([`activation::inherited_fd`]), the standalone bind above does
/// not run at all: `acquire_listener` turns the inherited descriptor
/// directly into a listener and never removes, binds, or `chmod`s anything
/// at `runtime_dir`. The socket's mode is still 0600 and its directory
/// still 0700 in this case too — carried by `contrib/systemd/hopd.socket`'s
/// own `SocketMode=`/`DirectoryMode=` instead of by this function. See this
/// crate's implementation plan
/// (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
/// Design decision 3) for why ownership of the socket file itself switches
/// entirely to the service manager under activation, rather than this
/// function reconciling two owners.
```

- [ ] **Step 5: Correct `lib.rs`'s stale `# Shutdown` doc comment**

`run()`'s doc comment currently reads: *"Signal handling and any orderly shutdown belong to issue #62 (socket activation and lifecycle) — this daemon's only contribution to 'restart works' is the stale-socket removal `server::serve_with` documents in place."* That sentence conflated two different things under one issue number before either was scoped. Replace it:

```rust
/// # Shutdown
///
/// None beyond the process being killed, still. [`server::serve_with`]'s
/// accept loop has no exit beyond an unrecoverable startup error, so under
/// normal operation this function does not return at all. Issue #62 added
/// *activation* — [`server::acquire_listener`] accepting a listener systemd
/// already bound, instead of always binding one itself — not lifecycle: no
/// signal handler exists, and nothing tears this process down when its
/// `.socket` unit stops. Orderly shutdown remains unowned by any filed
/// issue as of this writing. This daemon's only contribution to "restart
/// works" is still the stale-socket removal `server::serve_with`'s
/// standalone path documents in place — unreachable, now, on the activated
/// path, which never touches the socket file at all (see
/// [`server::acquire_listener`]).
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p hopd acquire_listener`
Expected: PASS, all three tests.

- [ ] **Step 7: Run the gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```
Expected: all four green — this is the point at which every existing test in `crates/hopd/tests/` (none of which set `LISTEN_FDS`/`LISTEN_PID`) re-proves criterion 4 through the new `None` branch of `acquire_listener`.

- [ ] **Step 8: Commit**

```bash
git add crates/hopd/src/server.rs crates/hopd/src/lib.rs
git commit -m "hopd: acquire a listener from an inherited fd, or bind standalone"
```

---

### Task 3: The unit files and the documented install step

**Files:**
- Create: `contrib/systemd/hopd.socket`, `contrib/systemd/hopd.service`
- Modify: `crates/hopd/src/server.rs` (a new test module only), `README.md`

**Interfaces:** none new — this task ships static assets and documentation, cross-checked against `server.rs`'s existing `SOCKET_FILE_NAME` constant by a test.

- [ ] **Step 1: Write the failing test**

In `crates/hopd/src/server.rs`, add:

```rust
#[cfg(test)]
mod systemd_unit_tests {
    use super::*;

    const SOCKET_UNIT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contrib/systemd/hopd.socket"
    ));
    const SERVICE_UNIT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contrib/systemd/hopd.service"
    ));

    #[test]
    fn the_socket_unit_names_the_same_path_this_module_binds_to_standalone() {
        // A cross-check, not a duplicate literal: if SOCKET_FILE_NAME ever
        // changes, this fails instead of the unit file silently drifting
        // from what a standalone-started hopd actually binds to.
        assert!(
            SOCKET_UNIT.contains(&format!("ListenStream=%t/hop/{SOCKET_FILE_NAME}")),
            "the socket unit's ListenStream= must name %t/hop/{SOCKET_FILE_NAME}"
        );
    }

    #[test]
    fn the_socket_unit_declares_the_modes_activation_must_carry() {
        // Design decision 3: under activation hopd itself sets neither
        // mode, so the unit file is the only place these are enforced.
        assert!(SOCKET_UNIT.contains("SocketMode=0600"));
        assert!(SOCKET_UNIT.contains("DirectoryMode=0700"));
    }

    #[test]
    fn the_socket_unit_is_enablable_on_its_own() {
        assert!(
            SOCKET_UNIT.contains("WantedBy=sockets.target"),
            "without an [Install] target, `systemctl --user enable hopd.socket` has nothing to link"
        );
    }

    #[test]
    fn the_service_unit_has_an_exec_start() {
        assert!(SERVICE_UNIT.contains("ExecStart="));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hopd systemd_unit`
Expected: FAIL to compile — `include_str!` cannot find either file.

- [ ] **Step 3: Create the unit files**

Create `contrib/systemd/hopd.socket`:

```ini
# hop launcher daemon — user socket unit.
#
# Installed and enabled per README.md's "Running hopd as a systemd user
# service" section. `%t` is systemd's runtime-directory specifier — the
# same $XDG_RUNTIME_DIR/hop that `runtime_dir::resolve()` computes, so this
# path always matches what a standalone-started hopd would have bound
# itself (crates/hopd/src/server.rs, SOCKET_FILE_NAME).
#
# `Service=` is left unset: it defaults to the service unit with the same
# name (hopd.service), which is what this pair uses.
[Unit]
Description=hop launcher daemon socket

[Socket]
ListenStream=%t/hop/hopd.sock
# 0600/0700 close the socket and its directory to every uid but this one's
# — the same bound a standalone-started hopd enforces itself
# (crates/hopd/src/server.rs). Under activation hopd never touches either
# (see server.rs's "Socket activation" doc section), so this unit is the
# only place these are set.
SocketMode=0600
DirectoryMode=0700

[Install]
WantedBy=sockets.target
```

Create `contrib/systemd/hopd.service`:

```ini
# hop launcher daemon — user service unit, started by hopd.socket on first
# connection (socket activation). Not meant to be enabled or started
# directly; `systemctl --user enable --now hopd.socket` is the entry point
# — see README.md.
[Unit]
Description=hop launcher daemon

[Service]
Type=simple
# %h is systemd's user-home-directory specifier. This assumes
# `cargo install --path crates/hopd` (README.md), which places the binary
# at ~/.cargo/bin/hopd — edit this line if hopd is built or installed
# elsewhere.
ExecStart=%h/.cargo/bin/hopd
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p hopd systemd_unit`
Expected: PASS, all four tests.

Then, outside the automated gate (systemd is not part of CI — Design decision 5), confirm both files parse as valid unit syntax:

```bash
systemd-analyze verify --user contrib/systemd/hopd.socket contrib/systemd/hopd.service
```
Expected: exit 0. (`ExecStart=%h/…` cannot resolve to an executable until `hopd` is actually installed at that path; verify against a real build first if this reports a missing-executable error rather than a syntax error.)

- [ ] **Step 5: Document the install step in `README.md`**

Add a new section after "Build and test":

```markdown
## Running hopd as a systemd user service

`hopd` can run standalone (bind its own socket — the path every test in
this repository exercises) or under systemd socket activation, where the
`.socket` unit binds the socket and starts `hopd` on first connection.

```sh
cargo install --path crates/hopd
mkdir -p ~/.config/systemd/user
cp contrib/systemd/hopd.socket contrib/systemd/hopd.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now hopd.socket
```

`hopd.service` is never started or enabled directly — only its socket is.
The daemon starts the first time something connects to
`$XDG_RUNTIME_DIR/hop/hopd.sock`; `hop query <text>` (once `hop-cli` is
installed the same way) is enough to trigger it. `systemctl --user status
hopd.service` confirms it is running afterward.
```

- [ ] **Step 6: Run the gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```
Expected: all four green.

- [ ] **Step 7: Commit**

```bash
git add contrib/systemd/hopd.socket contrib/systemd/hopd.service crates/hopd/src/server.rs README.md
git commit -m "hopd: ship a systemd user socket/service unit pair and document install"
```

---

### Task 4: Integration test — a real query over a real inherited listener

**Files:**
- Create: `crates/hopd/tests/activation.rs`
- Modify: `crates/hopd/Cargo.toml` (`libc.workspace = true` under `[dev-dependencies]`)

**Interfaces:**
- Consumes: `crates/hopd/tests/common/mod.rs`'s `hello`, `recv`, `send`; `hop_protocol::{ClientMsg, DaemonMsg, QueryText, API_VERSION}`; `hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len}`.

- [ ] **Step 1: Add the test-only dependency**

In `crates/hopd/Cargo.toml`'s `[dev-dependencies]`, add:

```toml
# Task 4 of docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md:
# a pre_exec closure needs dup2/fcntl to hand a spawned test process a
# pre-bound fd at a fixed number, reproducing sd_listen_fds(3) without a
# real systemd session. libc is already a workspace dependency (used today
# by hop-protocol); this crate did not need it until this test.
libc.workspace = true
```

Run `cargo build -p hopd --tests` once to confirm this changes nothing else in `Cargo.lock` (`libc` is already resolved workspace-wide).

- [ ] **Step 2: Write the test**

Create `crates/hopd/tests/activation.rs`:

```rust
//! An integration test proving hopd actually uses a listener inherited via
//! systemd's socket-activation protocol (sd_listen_fds(3)) — acceptance
//! criteria 2, 3 and 5 on issue #62 — not merely that
//! `hopd::activation::inherited_fd` parses the right environment variables
//! in isolation (that module's own unit tests) and not merely that a
//! listener built from an arbitrary raw fd works in-process
//! (`server.rs`'s own `acquire_listener_tests`). This spawns the real
//! `hopd` binary as a separate process with a real, already-bound-and-
//! listening `UnixListener` handed to it at file descriptor 3,
//! `LISTEN_FDS=1` and `LISTEN_PID` set to the daemon's own post-exec pid —
//! see this crate's implementation plan
//! (`docs/superpowers/plans/2026-08-09-issue-62-socket-activation.md`,
//! Design decision 5) for what this does and does not prove, and why this
//! is the mechanism used rather than a real systemd user session (this
//! crate's CI has none).

#![allow(clippy::unwrap_used)]

mod common;

use std::io::Write;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{recv, send};
use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, QueryText};

/// The same protocol constant `hopd::activation::SD_LISTEN_FDS_START`
/// names — duplicated here rather than imported, because that module is
/// `pub(crate)` inside `hopd` and this file is a separate crate. Fixed by
/// sd_listen_fds(3) itself, not a choice either side makes.
const SD_LISTEN_FDS_START: RawFd = 3;

/// A `hopd` started via a real inherited descriptor, deliberately not
/// `tests/socket.rs`'s `spawn_daemon` (which lets hopd bind its own
/// socket) — this helper's whole point is that hopd must *not* do that.
struct ActivatedDaemon {
    child: Child,
    socket_path: PathBuf,
}

impl Drop for ActivatedDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Binds the socket **in this test process** — the way a `.socket` unit's
/// own `ListenStream=`/`SocketMode=`/`DirectoryMode=` would have — then
/// spawns `hopd` with that descriptor duped onto fd 3 and
/// `LISTEN_FDS=1`/`LISTEN_PID=<hopd's own pid>` set.
///
/// # Why a shell wrapper, not `Command::new(hopd_path)` directly
///
/// `LISTEN_PID` must equal the pid `hopd` itself reads back from
/// `std::process::id()` — but `std::process::Command` fixes its child's
/// environment before `spawn()` returns, and the child's own pid is not
/// knowable until `spawn()` has already forked; there is no seam a caller
/// of `Command` can hook between "the fork happened" and "the child
/// execs" to inject a pid it just learned.
///
/// The fix used here: spawn `sh -c "export LISTEN_FDS=1; export
/// LISTEN_PID=$$; exec <hopd>"`. `$$` in a shell is the shell's own pid,
/// and `exec` replaces the shell's process image with `hopd`'s **without
/// changing the pid** — so the value `$$` resolves to immediately before
/// `exec` is exactly what `hopd` reads back from `std::process::id()`
/// afterward. Verified directly with a throwaway probe before writing this
/// into this crate's implementation plan: the exec'd process's own
/// `getpid()` matched `$$`, and a file descriptor `dup2`'d onto fd 3
/// before the outer `sh` starts survived the shell's own `exec` unchanged.
///
/// # The one `unsafe` in this file
///
/// `pre_exec`'s closure runs between `fork` and `exec` in the child, so it
/// must stick to async-signal-safe calls — `dup2`/`fcntl` are.
/// `CommandExt::pre_exec` is itself an `unsafe fn` for exactly this reason.
/// Test-only, the same footing as the workspace's other test-only
/// `unsafe` (`hop-protocol`'s `content.rs`, a `libc::mkfifo` call): neither
/// is production code, and both need a narrow `#[expect(unsafe_code)]` to
/// build at all under this workspace's `unsafe_code = "deny"` lint.
fn spawn_activated_daemon(runtime_dir: &Path) -> ActivatedDaemon {
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-config-home")).unwrap();

    let hop_dir = runtime_dir.join("hop");
    // Mirrors what the .socket unit's DirectoryMode=0700 would have
    // produced before hopd ever runs.
    std::fs::create_dir(&hop_dir).unwrap();
    std::fs::set_permissions(&hop_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let socket_path = hop_dir.join("hopd.sock");
    // Mirrors the .socket unit's own ListenStream=/SocketMode=0600: the
    // socket exists, bound and listening, before hopd ever starts.
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let raw_fd: RawFd = listener.as_raw_fd();

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "export LISTEN_FDS=1; export LISTEN_PID=$$; exec {:?}",
            env!("CARGO_BIN_EXE_hopd")
        ))
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", runtime_dir.join("isolated-home"))
        .env("XDG_DATA_HOME", runtime_dir.join("isolated-xdg-data-home"))
        .env("XDG_DATA_DIRS", "")
        .env(
            "XDG_CONFIG_HOME",
            runtime_dir.join("isolated-xdg-config-home"),
        )
        .env(
            "XDG_STATE_HOME",
            runtime_dir.join("isolated-xdg-state-home"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: the closure calls only dup2 and fcntl, both async-signal-safe
    // per signal-safety(7), between this process's fork and its exec — the
    // one window pre_exec exists for. It captures a plain integer
    // (`raw_fd`), no allocation or heap state.
    #[expect(
        unsafe_code,
        reason = "pre_exec is how a test hands a spawned process a pre-bound fd at a fixed \
                  number, reproducing sd_listen_fds(3) without a real systemd session; \
                  test-only, matching the precedent already set by hop-protocol's mkfifo test"
    )]
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(raw_fd, SD_LISTEN_FDS_START) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(SD_LISTEN_FDS_START, libc::F_GETFD);
            if flags < 0
                || libc::fcntl(SD_LISTEN_FDS_START, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().expect("failed to spawn hopd via sh");
    // The parent's own copy of the listener is no longer needed once
    // spawn() has forked; the child's dup2'd copy at fd 3 is independent.
    drop(listener);

    ActivatedDaemon { child, socket_path }
}

/// Attempts one handshake without panicking, so [`connect_when_ready`] can
/// retry instead of failing on the first attempt that catches hopd
/// mid-startup. `common::hello` cannot be reused here — it `.expect()`s a
/// reply, which would panic on exactly the timeout this function needs to
/// treat as "not ready yet."
fn try_hello(stream: &mut UnixStream) -> bool {
    let Ok(frame) = encode_frame(&ClientMsg::Hello {
        api_version: API_VERSION,
    }) else {
        return false;
    };
    if stream.write_all(&frame).is_err() {
        return false;
    }
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    if stream.read_exact(&mut prefix).is_err() {
        return false;
    }
    let Ok(len) = payload_len(prefix) else {
        return false;
    };
    let mut payload = vec![0u8; len];
    if stream.read_exact(&mut payload).is_err() {
        return false;
    }
    matches!(decode_payload(&payload), Ok(DaemonMsg::HelloAck { .. }))
}

/// Connects and completes the handshake, retrying until it succeeds or the
/// budget runs out.
///
/// `socket_path.exists()` — `tests/socket.rs`'s own readiness check — is
/// not usable here: `UnixListener::bind`'s backlog accepts a `connect()`
/// the instant it is bound, which in this test happens **before hopd is
/// even spawned**, since this test does the binding itself. A completed
/// `hello`/`hello_ack` round trip is the earliest observable proof hopd's
/// accept loop is actually running over the inherited fd.
fn connect_when_ready(daemon: &ActivatedDaemon) -> UnixStream {
    use std::io::Read;

    for _ in 0..50 {
        if let Ok(mut stream) = UnixStream::connect(&daemon.socket_path) {
            stream
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            if try_hello(&mut stream) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                return stream;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("hopd (over the inherited listener) did not answer a handshake within 5s");
}

#[test]
fn a_query_over_an_inherited_listener_is_served_without_hopd_rebinding_the_socket() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = spawn_activated_daemon(runtime_dir.path());
    let ino_before = std::fs::metadata(&daemon.socket_path).unwrap().ino();

    let mut stream = connect_when_ready(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("walking skeleton").unwrap(),
        },
    );
    let results = recv(&mut stream);
    let DaemonMsg::Results {
        query_id, items, ..
    } = results
    else {
        panic!("expected a results frame, got {results:?}");
    };
    assert_eq!(query_id, 1);
    assert!(
        items.iter().any(|item| item.title == "Hello from hopd"),
        "expected the skeleton item among the results, got {items:?}"
    );
    assert_eq!(recv(&mut stream), DaemonMsg::QueryDone { query_id: 1 });

    // Criterion 3, made specific: hopd used the fd this test handed it,
    // rather than falling back to standalone and coincidentally still
    // working at the same path. serve_with's standalone path always
    // removes and rebinds the socket file first (server.rs, unchanged by
    // this plan) — which would mint a *new* inode at the same path. An
    // unchanged inode is only possible if that removal never ran, i.e.
    // activation was genuinely taken.
    let ino_after = std::fs::metadata(&daemon.socket_path).unwrap().ino();
    assert_eq!(
        ino_before, ino_after,
        "the socket file must be the exact one this test bound, never rebound by hopd"
    );

    // Criterion 5, under activation specifically.
    let socket_mode = std::fs::metadata(&daemon.socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600);
    let dir_mode = std::fs::metadata(runtime_dir.path().join("hop"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
}
```

- [ ] **Step 3: Run the test to verify it fails, then passes**

Run: `cargo test -p hopd --test activation`
Expected: FAIL first if any name is wrong or a symbol is not `pub` where needed (`hopd::server::acquire_listener` is crate-private and this test never references it directly, only `hopd`'s public binary — so this should compile cleanly the first time once Task 2 has landed); PASS once corrected.

- [ ] **Step 4: Run the gate**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```
Expected: all four green — this is the landing gate for the whole issue.

- [ ] **Step 5: Commit**

```bash
git add crates/hopd/Cargo.toml crates/hopd/tests/activation.rs Cargo.lock
git commit -m "hopd: integration test driving a query over a real inherited listener"
```

---

## Acceptance criteria coverage (from issue #62)

| Criterion | Where |
| --- | --- |
| A socket unit and a service unit are installed to the correct user-unit path | Task 3 (`contrib/systemd/hopd.{socket,service}`, `README.md`'s `~/.config/systemd/user/` install step, `systemd-analyze verify --user` confirming valid syntax — documentation and manual verification, not an automated test) |
| Starting the socket unit and then issuing a query activates the daemon | **Covered by documentation and manual verification for the "starting the socket unit" half** (`README.md`, Task 3 — no systemd user session exists in this repository's CI, Design decision 5), **and by a real automated integration test for "issuing a query activates the daemon"** (Task 4, `a_query_over_an_inherited_listener_is_served_without_hopd_rebinding_the_socket`, reproducing the exact sd_listen_fds(3) contract without systemd itself) |
| The daemon accepts a listener inherited from the service manager | Task 1 (`activation::inherited_fd`'s tests); Task 2 (`acquire_listener`'s unsafe conversion and `a_listener_built_from_an_inherited_fd_actually_accepts_connections`); Task 4 (a real process, real fd, inode-stability proof it was actually used) |
| The daemon still runs standalone, with no inherited listener | Task 2 (`with_no_activation_it_binds_and_chmods_the_socket_path_exactly_as_before`); **every existing test in `crates/hopd/tests/{socket,lifecycle,host,apps,calculator,assembly,exec,state}.rs`**, unmodified and re-run at every task's gate, none of which set `LISTEN_FDS`/`LISTEN_PID` |
| The socket directory mode is 0700 under activation, not only under standalone start | Design decision 3 (the daemon does nothing to the directory under activation; the `.socket` unit's `DirectoryMode=0700` carries it — Task 3, `the_socket_unit_declares_the_modes_activation_must_carry`); Task 4 (a real-filesystem assertion against the directory a real activated `hopd` ran under) |
| The install step is documented | Task 3 (`README.md`'s "Running hopd as a systemd user service" section) |

## Self-review notes

- **Spec coverage.** The v1 design spec names "Systemd user service + socket activation" (§1) and "Socket activation + systemd user unit" (§13, M2) with no further detail — every concrete choice in this plan (unit directory, `%t`/`%h` specifiers, `Type=simple`, no `sd_notify`) is this plan's own, argued in Design decisions 5 and 6, not inherited from the spec.
- **Deliberate omissions**, each argued in Scope: orderly shutdown / signal handling (explicitly not this issue, and the stale doc comment claiming otherwise is corrected in Task 2), `Type=notify`/readiness notification, a general install script, an amendment to the threat model document.
- **No new dependency.** `libc` is added to `hopd`'s `[dev-dependencies]` only (Task 4); `deny.toml` needs no edit, since `libc` is already a workspace dependency checked by the existing `Supply chain` CI job.
- **Verified against real tools, not assumed:** `tokio::net::UnixListener::from_std`'s exact requirement (non-blocking mode) and its `debug_assert`-shaped enforcement, read directly from `tokio-1.53.1`'s own source (the version this workspace's `Cargo.lock` actually pins) rather than from documentation alone; the `OwnedFd::from_raw_fd` → `UnixListener::from` → `set_nonblocking` → `tokio::net::UnixListener::from_std` chain, compiled as a standalone probe before being written into this plan; the `sh -c "LISTEN_PID=$$; exec …"` pid-matching trick and fd-3 survival across the shell's own `exec`, proven with a throwaway probe rather than assumed from how systemd is documented to behave; `SocketMode=`, `DirectoryMode=`, `Service=`, `Accept=`, `%t` and `%h`, read from `man systemd.socket`/`man systemd.unit` on this development machine (systemd 255.4, actually installed here) rather than from memory; both final unit files, validated with `systemd-analyze verify --user` on this machine, including a run with a real executable at the `ExecStart=` target to rule out a syntax pass that only looked clean because the binary check never ran; the exact location and shape of the workspace's one existing `unsafe` (`hop-protocol/src/content.rs:1656-1677`), read directly rather than recalled from `Cargo.toml`'s comment alone; `hopd`'s effective tokio features and that `libc` is not currently among `hopd`'s own dependencies, read from `crates/hopd/Cargo.toml` directly; the current test count (615, across all crates, `cargo test --workspace`) obtained by running the suite before writing this plan; that `#[tokio::test]` in this exact crate already drives real `tokio::net::Unix*` I/O without an explicit runtime builder, confirmed by reading `crates/hopd/src/connection.rs`'s own existing tests rather than assumed from tokio's docs.
- **A pre-existing inaccuracy noticed, not caused, and not fixed by this plan.** `crates/hopd/tests/common/mod.rs`'s own doc comment claims `mod common;` "compiles this whole module into each of the three test binaries in this crate (`lifecycle`, `socket`, `host`)." That was already false before this plan: eight files (`apps.rs`, `assembly.rs`, `calculator.rs`, `exec.rs`, `host.rs`, `lifecycle.rs`, `socket.rs`, `state.rs`) already declare `mod common;`, none of which this plan touches. `crates/hopd/tests/activation.rs` (Task 4) becomes the ninth. Fixing the stale count is not this plan's scope-creep to take on — the drift predates it by several issues' worth of test files — but it is recorded here rather than silently added to.

## What I could not verify or fully resolve, for the maintainer's attention

- **The `listenfd`/`sd-notify` crate comparison in Design decision 1 is a structural argument, not a source-code audit.** This plan asserts that either crate's fd-to-socket conversion needs the same `unsafe` this plan takes directly, on the reasoning that no safe Rust spelling exists for "own a socket type from a bare kernel-issued integer" — but neither crate's actual source was fetched and read as part of writing this plan, unlike the tokio and systemd facts above, which were checked against real installed sources on this machine. If a reviewer wants that comparison made concrete rather than structural, pulling either crate's source (as was done for `fasteval` in the sibling calculator plan) would settle it.
- **`%h/.cargo/bin/hopd` in `hopd.service` assumes `cargo install --path crates/hopd`'s default output location** and was verified with `systemd-analyze verify --user` only against a literal stand-in path, not against a real `~/.cargo/bin/hopd` on this machine (that directory was deliberately left untouched rather than writing a real binary into a user's actual toolchain directory as part of planning). A user who builds or installs `hopd` elsewhere edits this one line, per the unit file's own comment — there is no discovery mechanism, because none of the acceptance criteria ask for one.
- **The `sh -c "… exec …"` wrapper embeds the binary's path via Rust's `{:?}` (`Debug`) formatting**, which double-quotes and backslash-escapes the string. This is correct for any path Cargo's own build output produces (no shell metacharacters), but it is not a general-purpose shell-quoting routine — a `CARGO_TARGET_DIR` or workspace path containing a literal double quote or backslash would break it. Not a realistic concern for this repository's own CI or development layout, and no existing test in this crate quotes a path any more carefully, but worth naming rather than silently relying on.
- **This plan does not attempt to prove `Accept=no`'s single-instance behavior**, i.e., that systemd would hand a *second* connection to the same running `hopd` process rather than spawning a second one — that is exactly the socket-unit semantic Task 4's mechanism cannot exercise (only one process is ever spawned by this test's own harness), and is one more reason Design decision 5 is explicit that this integration test proves the fd-inheritance contract, not systemd's own connection-dispatch behavior.
