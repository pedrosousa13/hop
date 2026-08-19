# Issue #180 — a constrained `--socket` override

Spec: GitHub issue **#180** ("Daemon + clients: a constrained `--socket`
override, so a dev instance can run beside a real one"), slice item 7 of the
#80 grill's spec (`docs/superpowers/specs/2026-08-10-hop-m3-frontend-design.md`,
decision D7). The issue body is the binding authority; this plan is the
argument for how it lands.

## What the issue asks for, verbatim

1. `hopd --socket <path>` binds that path instead of the derived one.
2. `hop --socket <path>` connects to it, applying the identical constraint.
3. A path that does not resolve inside `$XDG_RUNTIME_DIR` is **refused**, by
   both binaries, with a message that says why. Resolution happens before the
   comparison — a symlink that leads outside is refused too.
4. The 0700 parent-directory and 0600 socket-mode bounds hold on the override
   path exactly as they do on the derived one.
5. Refusal goes through the existing `Invocation::Usage` path rather than a
   new error channel.
6. Tests cover: an accepted in-runtime-dir path, a rejected outside path, a
   rejected symlink-escape, and that omitting the flag is unchanged behavior.

Out of scope, per the issue: any relaxation of the same-uid socket boundary; a
config-file setting for the socket path; multi-instance discovery.

## Global Constraints

These bind every task. A reviewer reads them as the attention lens.

- **The constraint root is `$XDG_RUNTIME_DIR` itself**, not
  `$XDG_RUNTIME_DIR/hop`. A dev socket at `$XDG_RUNTIME_DIR/hop-dev/hopd.sock`
  is exactly the intended use.
- **Fail toward refusal.** Any resolution step that errors, any path whose
  final component cannot be resolved, any ambiguity — refuse. Never fall back
  to the derived path when an override was given: an override that silently
  becomes the default is the #122 failure mode all over again.
- **No `std::env::set_var` in tests.** It is `unsafe` under edition 2024 and
  this workspace's `unsafe_code = "deny"` lint would reject it, and it is racy
  across parallel tests besides. Every constraint test drives the pure
  `resolve_in(runtime_dir, raw)` seam with `tempfile` directories instead.
- **No new `unsafe`.** If a task appears to need one, it has taken a wrong
  turn — stop and say so.
- **Workspace lints**: `[lints] workspace = true`, `unwrap_used = "warn"`
  (tests may `#![allow(clippy::unwrap_used)]` in their `mod tests`, as the
  existing ones do). `cargo clippy --workspace --all-targets -- -D warnings`
  must pass.
- **Doc-comment culture.** This repo documents *why*, at length, in place —
  read the surrounding module before writing. A new public item with a
  one-line doc comment is under-documented for this codebase. Comments must be
  self-contained: never defer a justification to a document outside the repo.
- **No AI attribution** anywhere in code, comments, commits.

## Design decisions

**D1 — the shared code lives in `hop-protocol`, in a new `socket` module.**
`hop-protocol` is the only crate all three binaries depend on. This is also
what the codebase already asked for: `apps/hop-gtk/src/app.rs`'s `socket_path`
carries the note *"Duplicated rather than shared because `hop-cli` does not
expose it as a library function today; were a third caller to need it, the
pair would be worth promoting into `hop-protocol` instead of copied a second
time."* `hopd` is that third caller. The duplication ends here rather than
tripling.

**D2 — the resolution algorithm.** Given a raw override path and the runtime
directory root:

1. `root = runtime_dir.canonicalize()?` — it must exist; if it does not, refuse.
2. Resolve the raw path as far as it exists, following symlinks:
   - `raw.canonicalize()` — if it succeeds, that is the resolved path, with
     every symlink already followed (so a symlink pointing outside resolves to
     outside, and step 3 refuses it).
   - `NotFound` means the path does not fully exist yet, which is normal: the
     daemon binds a socket file that is not there. But first — if
     `symlink_metadata(raw)` *succeeds* while `canonicalize` said `NotFound`,
     the entry exists as a dangling symlink; refuse, because what it would
     resolve to cannot be checked.
   - Otherwise take `raw.file_name()` (refuse if there is none — a path ending
     in `/`, `.` or `..` names no file), recurse on `raw.parent()` (an empty
     parent means the current directory, `.`), and join the file name onto the
     resolved parent.
   - Any other IO error: refuse, naming it.
3. Refuse unless `resolved.starts_with(&root)` **and** `resolved != root`.

Recursion depth is bounded by the path's component count.

**D3 — the 0700 parent directory.** On the override path the daemon applies
exactly what `hopd::runtime_dir::resolve` applies to the derived one: create
the socket's parent with `DirBuilder::mode(0o700)` — born at 0700, no
create-then-chmod window — and, if it already exists, leave it exactly as
found, whatever its mode. The asymmetry is deliberate and is `runtime_dir`'s
own documented reasoning: this process can reason about a directory it created
itself and cannot safely narrow or redirect one the environment supplied. The
create is not recursive, again matching `runtime_dir::resolve`. The 0600 socket
mode needs nothing new — `server::acquire_listener` already chmods the bound
socket, whatever path it sits at.

**D4 — `serve_with` takes the socket path, not the runtime directory.** Its
signature becomes `serve_with(socket_path: &Path, source: S)`. Deriving the
name inside the server was only ever right while there was one possible path;
with an override there are two, and the caller is the one that knows which.
Adding a second entry point (`serve_with_socket`) instead would leave two ways
to start the same daemon.

**D5 — an inherited (socket-activated) listener wins over `--socket`, with a
warning.** When `LISTEN_FDS` hands the daemon a descriptor, the override names
a path nothing will bind. Warn on stderr, in the same voice as the existing
`LISTEN_FDS declared N descriptors` line, and serve on the inherited
descriptor. Refusing outright would break a systemd unit the moment someone
added the flag to it; silence is the failure #122 exists to end.

**D6 — where the refusal happens, and what criterion 5 means.** Criterion 5
says the refusal "goes through the existing `Invocation::Usage` path rather
than a new error channel". That is read as the refusal's *observable* channel —
a stderr line and the usage exit code — not as a demand that `parse` itself
perform the check. `parse` is documented as pure ("never touches a socket or
prints anything"); giving it an env read and filesystem access would contradict
that contract and would force `std::env::set_var` into its unit tests, which
edition 2024 makes `unsafe` and this workspace's lint denies.

So: `parse` returns `Usage` only for a *malformed* flag — `--socket` with no
value, a repeated `--socket`, an unrecognized argument. Each binary's `main.rs`
then resolves the override immediately after `parse`, before anything else
runs, and on refusal prints `<binary>: <why>` to stderr and returns the same
exit code that binary's own `Usage` arm returns: `2` for `hopd`, `2` for `hop`,
`ExitCode::FAILURE` for `hop-gtk`. No new error channel, no new exit code, and
the message names the rule that was broken.

**D7 — the flag goes before the subcommand in `hop`.** `hop query …` joins
every token after `query` into the query text, so a trailing `--socket` would
become part of the query. `hop --socket <path> query foo` is the accepted
form; `--socket` after the subcommand is query text or a usage error, exactly
as it reads today. `hop-cli::parse` returns a new `Invocation { socket,
command }` struct rather than threading the option through every `Command`
variant, keeping `Command` and its tests as they are.

## Tasks

### Task 1 — `hop_protocol::socket`

Create `crates/hop-protocol/src/socket.rs`, declared `pub mod socket;` in
`lib.rs`.

Public surface:

```rust
/// The environment variable the socket path derives from.
pub const XDG_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR";
/// The subdirectory of `$XDG_RUNTIME_DIR` the derived socket lives in.
pub const RUNTIME_SUBDIR: &str = "hop";
/// The socket's file name.
pub const SOCKET_FILE_NAME: &str = "hopd.sock";

#[derive(Debug, Error)]
pub enum SocketPathError { /* see below */ }

/// `$XDG_RUNTIME_DIR`, read and validated — unset and empty are distinct errors.
pub fn runtime_dir() -> Result<PathBuf, SocketPathError>;

/// `<runtime_dir>/hop/hopd.sock`.
pub fn derived(runtime_dir: &Path) -> PathBuf;

/// D2's algorithm. The seam every constraint test drives.
pub fn resolve_in(runtime_dir: &Path, raw: &Path) -> Result<PathBuf, SocketPathError>;

/// The one call a binary makes: `None` derives, `Some` resolves and constrains.
pub fn socket_path(overridden: Option<&Path>) -> Result<PathBuf, SocketPathError>;
```

`SocketPathError` variants, each with a `Display` that says what went wrong and
why it was refused (`thiserror`, already a dependency):

- `RuntimeDirUnset` — `XDG_RUNTIME_DIR is not set`
- `RuntimeDirEmpty` — `XDG_RUNTIME_DIR is set but empty`
- `RuntimeDirUnresolvable { path, source }` — the root does not resolve
- `Unresolvable { path, source }` — the override does not resolve
- `NoFileName { path }` — the override names no file
- `DanglingSymlink { path }` — an entry exists but resolves to nothing
- `Outside { path, runtime_dir }` — resolved, and it is not inside the root.
  Its message must name both paths and say the rule: a socket path must
  resolve inside `$XDG_RUNTIME_DIR`.

Tests, in `#[cfg(test)] mod tests`, using `tempfile` (already a dev-dependency)
and **never** `set_var`:

1. A path directly inside the root resolves and is accepted.
2. A nested path inside the root (`<root>/hop-dev/hopd.sock`), whose parent
   exists but whose final component does not, is accepted — the daemon binds a
   file that is not there yet.
3. A path whose *parent* does not exist either is accepted while it stays
   inside the root (the recursion works past more than one missing component).
4. A path outside the root is refused as `Outside`.
5. A `..` escape (`<root>/hop/../../elsewhere/hopd.sock`) is refused — the
   canonicalisation, not a textual check, is what catches it.
6. A symlink inside the root pointing at a file outside it is refused
   (`std::os::unix::fs::symlink`; the target exists, so `canonicalize`
   succeeds and lands outside).
7. A symlink inside the root pointing at another location *inside* it is
   accepted, resolving to the target.
8. A dangling symlink inside the root is refused.
9. A path that is the root itself is refused.
10. A path ending in `..` or `/` is refused as `NoFileName`.
11. `derived` composes `<root>/hop/hopd.sock`.

### Task 2 — `hopd` grows the flag

**`crates/hopd/src/lib.rs`:**

- `Invocation` becomes `Serve { socket: Option<PathBuf> }` / `Usage`. It loses
  `Copy` (a `PathBuf` is not `Copy`); keep `Debug, Clone, PartialEq, Eq`.
- `parse` gains the `--socket` arm: exactly `--socket` followed by one value →
  `Serve { socket: Some(_) }`; no arguments → `Serve { socket: None }`;
  `--socket` with no value, a repeated `--socket`, and every other argument →
  `Usage`. Compare against `OsStr` — the function still takes `OsString` and
  must still refuse a non-UTF-8 argument without panicking, but a non-UTF-8
  *value* for `--socket` is a legitimate path and is accepted.
- `USAGE` becomes `usage: hopd [--socket <path>]`. Its doc comment currently
  argues *against* a synopsis, on the grounds that hopd has no arguments; that
  reasoning expires here — rewrite it rather than leaving it contradicting the
  constant beneath it.
- The existing test `a_plausible_but_nonexistent_socket_flag_is_usage` asserts
  behaviour this task deliberately changes. Rewrite it into a test that the
  flag now parses, keeping a comment on what #122 established and what #180
  changed; keep `a_typo_of_a_future_flag_is_usage` (`--socket-path`,
  `-socket`) refusing, and keep every other parse test.
- `run` becomes `run(socket: Option<PathBuf>) -> ExitCode`, where `socket` is
  the **already-resolved** path (`main.rs` resolved and constrained it — D6):
  - `None` → today's behaviour: `runtime_dir::resolve()`, then the derived
    socket path inside it.
  - `Some(path)` → create that path's parent at 0700 per D3, and serve there.
    `runtime_dir::resolve` is not called on this branch: it would create a
    `hop/` directory the override does not use.
  - Either way, pass the socket path to `server::serve_with`.
  - Order stays as it is today: config first, then state dir, then the socket
    path — a malformed config must still refuse to start before anything binds.

**`crates/hopd/src/main.rs`:** dispatch the new `Serve { socket }`. When
`socket` is `Some(raw)`, resolve it with `hop_protocol::socket::runtime_dir()`
plus `resolve_in` right here, per D6; on error print `hopd: {err}` and return
`ExitCode::from(2)` — the same code the `Usage` arm returns. Pass the resolved
path to `run`.

**`crates/hopd/src/server.rs`:** `serve_with(socket_path: &Path, source: S)`,
per D4. `acquire_listener(socket_path, activation)` likewise. `SOCKET_FILE_NAME`
should now come from `hop_protocol::socket` rather than being declared here (the
systemd-unit cross-check test that formats it stays, pointing at the re-used
constant). Add D5's warning in the activation branch — it needs to know whether
an override was given, so thread that in as a parameter rather than reading the
environment twice.

**`crates/hopd/src/runtime_dir.rs`:** unchanged in behaviour. If it is worth
sharing the 0700-create with the override path, extract the create into a
function both call — do not duplicate the `DirBuilder` block.

**Tests:**

- Unit tests on `parse` as listed above.
- `crates/hopd/tests/socket.rs` (or a sibling): a daemon serving on an
  override path inside a temp `XDG_RUNTIME_DIR` accepts a client connection,
  and the bound socket is mode 0600 with its parent directory at 0700.
  Follow the existing integration tests' harness (`tests/common/`) rather than
  inventing a second one.
- Every existing call site of `serve_with` in the test suite updated to the
  new signature.

### Task 3 — the clients, and the docs

**`crates/hop-cli/src/lib.rs`:**

- New `pub struct Invocation { pub socket: Option<PathBuf>, pub command: Command }`;
  `parse` returns it. `Command` is unchanged; existing parse tests become
  `parse(…).command`, and gain: `--socket <p>` before a subcommand parses and
  carries the path; `--socket` with no value is `Command::Usage`; a repeated
  `--socket` is `Usage`; `--socket` after the subcommand is *not* consumed
  (`hop query --socket x` queries the text `--socket x`, unchanged behaviour).
- `socket_path()` is deleted; `connect_and_query` takes the resolved path.
  `run_query` and `run_exec` take `socket: &Path` — already resolved, per D6.
- `ClientError::RuntimeDirUnset` goes with it: the socket path is resolved in
  `main.rs` now, so no client-flow error variant covers it. Per D6, `main.rs`
  resolves `Invocation::socket` (`None` derives the default) before dispatching
  the command; on refusal it prints `hop: {err}` and returns `ExitCode::from(2)`,
  the same code its `Command::Usage` arm returns — criterion 5. Exit codes
  0/1/10/11/12 for the command flows themselves are unchanged.
- `USAGE` gains the flag: `usage: hop [--socket <path>] version | hop [--socket <path>] query <text>... | …` — keep it one line and readable.
- `crates/hop-cli/src/main.rs` updated.
- `crates/hop-cli/tests/e2e.rs`: `hop --socket <path>` reaches a daemon bound
  there, and a path outside `$XDG_RUNTIME_DIR` is refused with a message naming
  the rule. Note that the e2e harness controls the child process's environment,
  so it can set `XDG_RUNTIME_DIR` for the *spawned* binaries without any
  `set_var` in the test process.

**`apps/hop-gtk`:**

- `cli::Args` gains the flag: `Run { socket: Option<PathBuf> }` and
  `Screenshot { path, query, socket }`; `--socket` with no value is `Usage`;
  a repeated `--socket` is `Usage`. `USAGE` updated to
  `usage: hop-gtk [--socket <path>] [--screenshot <path> [--query <text>]]`.
- `app::socket_path` is deleted in favour of `hop_protocol::socket::socket_path`
  — this is the third-caller promotion its own doc comment named, so the note
  goes with the function. `app::run` resolves the override right after `cli::parse`
  (D6): on refusal, `eprintln!("hop-gtk: {err}")` and `ExitCode::FAILURE`, the
  same code its `Usage` arm returns.
- Existing `cli` tests updated; add the two new refusals and one acceptance.

**Docs:**

- `docs/security/2026-08-02-m2-socket-boundary-threat-model.md` gains a short
  amendment recording that the socket path is now operator-selectable, what
  constrains it (resolution inside `$XDG_RUNTIME_DIR`, the 0700 parent and
  0600 socket unchanged), and that the same-uid boundary is untouched. Match
  the file's existing amendment style — read it before writing.
- Anywhere the socket path's derivation is documented as the only one
  (`README.md`, `apps/hop-gtk`'s docs, `contrib/systemd`'s notes) gets a line
  on the override. Do not invent new documents.

## Verification

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all --check`
- `cargo check -p hop-gtk` (the `layer-shell` feature is off by default and
  cannot build on this machine — that is expected, not a regression)
