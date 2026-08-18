# OWASP Top 10:2025 sweep — M2 — Daemon

**Date:** 2026-08-18
**Issue:** [#52](https://github.com/pedrosousa13/hop/issues/52)
**Milestone:** M2 — Daemon
**Verdict:** Five findings filed and accepted for triage; nothing fixed by this sweep.

## Scope and boundary

This point-in-time sweep covers the M2 workspace at commit
`e88f7371a2be16645c55692cd803e573c5e89fac`:

- all tracked Rust source and tests in `hop-protocol`, `hop-core`, `hopd`, and
  `hop-cli`;
- the Unix-socket server, client framing, handshake, query/execute lifecycle,
  provider host, calculator, desktop-entry parser/launcher, runtime/config/state
  paths, and systemd socket/service assets;
- workspace manifests, `Cargo.lock`, `deny.toml`, CI, and security/domain
  documents; and
- untrusted-boundary inputs: socket peers, malformed or slow frames, provider
  output/errors, desktop-entry files, persisted state, environment-derived
  paths/configuration, and CLI input.

The daemon's declared peer boundary is the owner-controlled 0700 runtime
directory and 0600 Unix socket. A process with the same UID is already inside
that boundary and cannot be distinguished by the current protocol. Findings
#158, #160, and #162 are therefore robustness/lifecycle findings, not claims
that the socket authenticates against or protects from another same-UID
process. Finding #161 concerns robustness when mutable filesystem entries are
scanned. Finding #159 concerns the integrity of logs containing raw
environment- or filesystem-derived paths.

OWASP Top 10:2025 is a web-focused awareness taxonomy applied here as a
cross-check for a local daemon, not a completeness claim. The [official
introduction](https://owasp.org/Top10/2025/0x00_2025-Introduction/) lists the
2025 categories, records that SSRF moved into A01, and identifies A10 as the
new exceptional-condition category. The category pages used for this sweep
are linked in the verdict table below.

## Method and evidence

The sweep read issue #52 in full, including its pickup comment; the five filed
records [#158](https://github.com/pedrosousa13/hop/issues/158),
[#159](https://github.com/pedrosousa13/hop/issues/159),
[#160](https://github.com/pedrosousa13/hop/issues/160),
[#161](https://github.com/pedrosousa13/hop/issues/161), and
[#162](https://github.com/pedrosousa13/hop/issues/162), including their current
state, labels, milestone, and comments; `AGENTS.md`; `CONTEXT.md`; the full
M1 sweep; the full M2 socket-boundary threat model; and the repository's issue
tracker/domain guidance.

The audited inventory included:

| Surface | Evidence inspected | Controls cross-checked |
| --- | --- | --- |
| Protocol | `crates/hop-protocol/src/{content,framing,item,limits,mode,redaction,wire}.rs` and tests | frame/payload caps, bounded fields, content constructors, action/item shapes, error limits |
| Core | `crates/hop-core/src/{aliases,host,learning,lib,pipeline,provider,rank,router,sanitize}.rs` and tests | provider manifests/budgets/panic isolation, provenance, HMAC learning store, ranking/query limits |
| Daemon | `crates/hopd/src/{activation,apps,calculator,config,connection,lib,runtime_dir,server,source,state_dir,main}.rs` and tests | socket path/modes, activation, handshake, connection resources, config/state, desktop scan/watcher/launcher |
| CLI | `crates/hop-cli/src/{lib,main}.rs` and end-to-end tests | argv parsing, query bounds, frame codec, live-result execute resolution |
| Assets and supply chain | `contrib/systemd/*`, `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.github/workflows/ci.yml` | service/socket ownership, dependency advisories, licenses, sources, bans, CI action pin |

The complete issue inventory contained 81 records. Titles and bodies were
searched for `single-instance`, `another hopd`, `unlink`, `live socket`,
`zombie`, `child process`, `read_to_string`, `config.toml`, `desktop-entry
scan`, `FIFO`, `special file`, and `TOCTOU`. The duplicate rationale is
recorded with each finding below. Read-only source probes traced the five
reported flows; no production or test files were created or changed.

## Ten-category verdicts

Each OWASP Top 10:2025 category has exactly one verdict.

| Category | Verdict |
| --- | --- |
| [A01 Broken Access Control](https://owasp.org/Top10/2025/A01_2025-Broken_Access_Control/) | Applicable — no finding filed |
| [A02 Security Misconfiguration](https://owasp.org/Top10/2025/A02_2025-Security_Misconfiguration/) | Applicable — no finding filed |
| [A03 Software Supply Chain Failures](https://owasp.org/Top10/2025/A03_2025-Software_Supply_Chain_Failures/) | Applicable — no finding filed |
| [A04 Cryptographic Failures](https://owasp.org/Top10/2025/A04_2025-Cryptographic_Failures/) | Applicable — no finding filed |
| [A05 Injection](https://owasp.org/Top10/2025/A05_2025-Injection/) | Applicable — no finding filed |
| [A06 Insecure Design](https://owasp.org/Top10/2025/A06_2025-Insecure_Design/) | Applicable — [#158](https://github.com/pedrosousa13/hop/issues/158) filed |
| [A07 Authentication Failures](https://owasp.org/Top10/2025/A07_2025-Authentication_Failures/) | Applicable — no finding filed |
| [A08 Software or Data Integrity Failures](https://owasp.org/Top10/2025/A08_2025-Software_or_Data_Integrity_Failures/) | Applicable — no finding filed |
| [A09 Security Logging & Alerting Failures](https://owasp.org/Top10/2025/A09_2025-Security_Logging_and_Alerting_Failures/) | Applicable — [#159](https://github.com/pedrosousa13/hop/issues/159) filed |
| [A10 Mishandling of Exceptional Conditions](https://owasp.org/Top10/2025/A10_2025-Mishandling_of_Exceptional_Conditions/) | Applicable — [#160](https://github.com/pedrosousa13/hop/issues/160), [#161](https://github.com/pedrosousa13/hop/issues/161), and [#162](https://github.com/pedrosousa13/hop/issues/162) filed |

## Category evidence

### A01:2025 — Broken Access Control · applicable, no finding filed

`runtime_dir.rs` creates the owner-only runtime directory; standalone
`server.rs` and `contrib/systemd/hopd.socket` use a 0600 socket. `connection.rs`
requires the handshake and checks execute item/action membership against the
retained delivered items. Provider provenance is checked by `source.rs` and
`host.rs`. The same-UID peer model is explicit in the threat model. #158 is an
endpoint replacement design issue, not a newly missing per-request
authorization check.

### A02:2025 — Security Misconfiguration · applicable, no finding filed

Permission-bearing defaults are explicit in runtime, state, and systemd
assets. Configuration validates `max_results` against the frame-count limit
and `max_term_chars` against the ranker ceiling. No new deployment-default,
account, service, permission, or unsafe configuration finding was distinct
from #160's exceptional file-read behavior.

The systemd user socket unit grants `DirectoryMode=0700` and
`SocketMode=0600`, excluding ordinary other UIDs from the runtime directory
and socket. A process already running as the session UID can still connect and
can therefore trigger the user socket unit, including an unintended same-UID
client. That is intentional under the declared boundary, not a new finding.
Socket activation changes socket ownership and lifecycle: systemd creates and
passes the listening descriptor while `hopd` does not bind, unlink, or chmod
the activated path; it does not add peer identity or authentication.

### A03:2025 — Software Supply Chain Failures · applicable, no finding filed

`deny.toml` denies advisories, yanked and unmaintained crates, wildcard
versions, unknown registries, and unknown Git sources, with an explicit
license allowlist. CI uses a SHA-pinned `cargo-deny` action. The dependency
gate passed during this sweep. This is a current-state check, not a claim that
future advisories cannot occur; no new finding was identified.

### A04:2025 — Cryptographic Failures · applicable, no finding filed

The v2 learning envelope uses HMAC-SHA256 with a sibling `learning.key` and
verifies before bounds, timestamp handling, retention, or boost use.
`getrandom` supplies key material and the key is not embedded in source. The
documented process that can read the key remains outside that store-integrity
guarantee. No new algorithm, key-handling, or disclosure finding was found.

### A05:2025 — Injection · applicable, no finding filed

Calculator expressions are bounded and evaluated by `fasteval`; non-finite
results are refused. Desktop `Exec=` is parsed into argv and passed to
`Command::new` without a shell. URL and copy outcomes use validating content
types. Existing parser and desktop-string issues were checked and not
duplicated. #159 is a log-encoding issue, not an injection sink.

### A06:2025 — Insecure Design · applicable, #158 filed

`crates/hopd/src/server.rs:261-274` unconditionally removes the standalone
socket pathname before binding. The source documentation says this is not a
single-instance guard. Starting a second daemon through the supported
standalone path can therefore remove the first listener's name while its open
listener remains alive, then bind a replacement at the expected path. Existing
clients stay attached to the first listener while new clients reach the second.

The primary Linux [`unlink(2)` documentation](https://man7.org/linux/man-pages/man2/unlink.2.html)
describes the underlying behavior: removing the pathname does not invalidate an
open object, so existing users of the listener can continue while a replacement
pathname is bound.

The filed [#158 — Preserve a live hopd socket when starting a second daemon](https://github.com/pedrosousa13/hop/issues/158)
classifies this as a same-UID lifecycle and availability robustness defect,
not authentication against another same-UID process. Its acceptance scope
requires refusing replacement of a live listener, an explicit stale-socket
recovery policy, preservation of existing clients, unchanged systemd ownership,
and tests for live and stale paths. #54/#62 own socket creation, activation,
and modes; #98 owns connection resources; none owns live-listener preservation.

### A07:2025 — Authentication Failures · applicable, no finding filed

Unix ownership and mode gate socket reachability; they do not identify the peer.
Any peer that can open the socket is authorized to use this protocol, and the
daemon performs no peer credential or identity check. The handshake requires
`Hello` before other frames and refuses version mismatch. A same-UID peer is
intentionally accepted and cannot be distinguished by this contract. Socket
activation does not change that: systemd owns creation and passes the listener,
but it adds no daemon-side peer identity or authentication. No new
authentication or session failure was found within that declared boundary.

### A08:2025 — Software or Data Integrity Failures · applicable, no finding filed

Provider manifests are captured and compared, item kind/provider provenance is
checked, provider failures and panics are isolated, and learning persistence
uses authenticated v2 envelopes. The lockfile and dependency source review
found no unverified update or artifact path. Existing provider and learning
issues remain separately owned and were not duplicated.

### A09:2025 — Security Logging & Alerting Failures · applicable, #159 filed

`crates/hopd/src/apps.rs:620-635, 677-710, 758` formats malformed-file
diagnostics with `path.display()` and writes them to `eprintln!`. Linux
filenames can contain newline bytes and terminal control characters. A
malformed desktop entry with such a name can therefore forge log records or
emit terminal controls in stderr/journald. Related raw path diagnostics exist
in configuration and learning-save paths.

The filed [#159 — Sanitize filesystem paths before writing daemon diagnostics](https://github.com/pedrosousa13/hop/issues/159)
classifies this as log integrity for raw filesystem/environment-derived paths.
Its acceptance scope requires escaping newline, carriage-return, C0/C1, DEL,
ESC, and direction-control characters; applying one safe representation across
apps, config, and learning-save diagnostics; and retaining useful Unicode and
non-UTF-8 path identification. #27 covers query redaction, #34/#104 provider
messages and panic payloads, #43 load signaling, and #119 desktop string
escapes; none covers raw filesystem paths in daemon diagnostics.

The pathname claim is grounded in the primary Linux [pathname(7)
documentation](https://man7.org/linux/man-pages/man7/pathname.7.html).

### A10:2025 — Mishandling of Exceptional Conditions · applicable, #160, #161, and #162 filed

The official [A10:2025 page](https://owasp.org/Top10/2025/A10_2025-Mishandling_of_Exceptional_Conditions/)
describes abnormal states including race conditions, resource exhaustion, and
failure to recover. Three separate daemon file/process lifecycle defects meet
that category:

#### #160 — Bound and classify the startup config-file read

`crates/hopd/src/config.rs:231-245` calls unbounded `fs::read_to_string` on a
path derived from `XDG_CONFIG_HOME` or `HOME`, before runtime-directory and
socket setup in `crates/hopd/src/lib.rs:199-210`. A FIFO can block startup; a
symlink to an endless device can keep the read running while memory grows; a
large regular file is fully buffered before parsing.

The filed [#160 — Bound and classify the startup config-file read](https://github.com/pedrosousa13/hop/issues/160)
classifies this as startup resilience for an environment-derived same-user
path, not a separate-user boundary. Its acceptance scope requires that
acquiring a FIFO, device, directory, or other special file cannot block;
classification and reading apply to the same opened object; regular input is
bounded; and valid, absent, malformed, and over-limit configurations retain
explicit outcomes. #60 owns config semantics; #37/#38/#43/#88 own learning
store loading and integrity.

#### #161 — Read desktop entries through a bounded checked descriptor

`crates/hopd/src/apps.rs:677-698` calls `metadata(&path)`, accepts only a
regular file at or below `MAX_DESKTOP_FILE_BYTES`, then separately calls
`read_to_string(&path)`. A concurrent atomic replacement can put a FIFO,
device, or growing file at the path after metadata succeeds. Startup can then
block before socket bind; a watcher rescan can wedge its index thread or exceed
the intended size bound.

The filed [#161 — Read desktop entries through a bounded checked descriptor](https://github.com/pedrosousa13/hop/issues/161)
classifies this as mutable-filesystem apps-index resilience. The remedy must
ensure that acquiring/opening a FIFO, device, or other special file cannot
itself block; merely opening once and then calling `fstat` is insufficient if
the acquisition may block. The same nonblocking-safe acquisition must be
validated and read through an explicit cap from the checked object. Acceptance
also requires deterministic replacement coverage and preservation of watcher,
precedence, malformed logging, and ordinary symlink behavior. #57/#106/#108
own indexing, watcher recovery, and precedence; #119 and #93 own different
desktop-string and icon-root concerns.

#### #162 — Reap detached application children

`crates/hopd/src/apps.rs:2004-2016` maps a successful `Command::spawn()` to
`()`, dropping the returned `Child`. The official Rust [`Child` documentation](https://doc.rust-lang.org/std/process/struct.Child.html)
says it has no `Drop` implementation, dropping it does not wait, and unreaped
exited children remain zombies that may exhaust global resources. Linux
[`wait(2)` documentation](https://man7.org/linux/man-pages/man2/waitpid.2.html)
states the same lifecycle requirement.

The filed [#162 — Reap detached application children](https://github.com/pedrosousa13/hop/issues/162)
classifies this as process-lifecycle resilience inside the same-UID boundary.
Repeated legitimate or buggy `Execute` requests for an immediate-exit entry
such as `Exec=/bin/true` can accumulate zombies under the long-running daemon.
Waiting inline would break detached semantics for long-running GUI programs;
the acceptance scope requires an owned asynchronous reaper or equivalent,
quick-exit stress coverage, nonblocking long-running launches, preserved spawn
failure behavior, and documented shutdown policy.

## Checked and sound controls

The following controls were inspected and not re-filed:

- frame prefixes are rejected over the cap before payload allocation; daemon
  client input uses the narrower 65,536-byte inbound cap;
- handshake-first ordering, query supersession/cancellation, stale-frame
  filtering, live item/action binding, provider budgets and panic isolation,
  provenance checks, field/count bounds, and per-connection limits;
- learning state uses bounded reads, bounded parsed entries, atomic writes,
  restrictive modes, and HMAC verification;
- desktop `Exec=` uses argv without a shell; calculator input has finite query
  and ranking bounds and refuses non-finite results;
- no production `unsafe` exists outside the narrowly documented systemd fd
  transfer, no dynamic regex compilation is used, and M2 has no network client
  or TCP listener; and
- `deny.toml` and CI provide advisory, yanked, unmaintained, license, source,
  and action-integrity gates.

These controls reduce ordinary malformed-input and runaway-client risk but do
not provide the missing singleton, raw-path log encoding, special-file
acquisition, bounded config read, or child reaping guarantees filed above.

## Verification

Commands were run from `/home/pedro/apps/hop` against the audited branch. No
production code or tests were changed.

```text
cargo fmt --all --check
PASS — exit 0, no output

cargo check --workspace
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.02s
PASS — exit 0

cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s
PASS — exit 0

cargo test --workspace --quiet
PASS — exit 0; 770 passed, 0 failed, 4 ignored

cargo deny check
advisories ok, bans ok, licenses ok, sources ok
PASS — exit 0

cargo test --release -p hop-core --test latency -- --ignored --nocapture --test-threads=1
bounded_worst_case: 40.596826ms; query p95: 1.584857ms; files-shaped p95: 5.606763ms; populated-learning p95: 9.63526ms
PASS — exit 0; 4 latency tests passed, 0 failed, 5 filtered out, 10.41s

git diff --check
PASS — exit 0
```

The issue inventory and source-flow probes were read-only. The five issue
records were verified OPEN with labels `Bug` and `needs-triage` and milestone
`M2 — Daemon`. The audit document is the only tracked file changed by this
deliverable.

## Limits and follow-up

- OWASP Top 10:2025 is web-focused; these mappings are taxonomy judgments for
  a local Unix daemon and do not claim coverage of every local IPC, desktop,
  kernel, or supply-chain risk.
- Same-UID robustness findings do not assert authentication or privilege
  separation. The threat model intentionally treats same-UID peers as inside
  the boundary and treats connection limits as robustness controls.
- #161's stat/read replacement window is concrete, but the sweep retained no
  stress harness; implementation should add deterministic replacement tests.
- #160's incremental impact is bounded startup/OOM behavior rather than a new
  ability to alter another user's daemon configuration.
- #162 must preserve detached GUI behavior while giving every successful child
  an owner that reaps it.

All five accepted records remain open for triage. This report records the
current code and controls; it does not claim any remediation landed.
