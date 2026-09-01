# OWASP Top 10:2025 sweep — M3 + M4 — Frontend

**Date:** 2026-09-01
**Issue:** [#237](https://github.com/pedrosousa13/hop/issues/237)
**Milestones:** M3 — Frontend (GNOME) and M4 — Frontend (cross-platform), swept together
**Verdict:** Thirteen findings filed and accepted for triage; nothing fixed by this sweep.

## Scope and boundary

This point-in-time sweep covers the frontend surface at commit
`a8259f29cb414c9fc426c14185d06ef3b6d4e76f`:

- all tracked Rust source and tests in `apps/hop-gtk` and `crates/hop-hotkeyd`;
- `crates/hop-cli/src/dbus.rs`, the hand-rolled D-Bus client added in M4 (#234)
  that `hop toggle` runs on every hotkey press — M3/M4 code in an M2 crate,
  swept here rather than left in the gap between two scopes;
- the three session paths the milestone brief names: hop-gtk on GNOME Wayland
  (ordinary window plus activation-token toggle), on wlroots/KDE (layer-shell
  overlay holding exclusive keyboard focus), and on X11 (self-positioned
  window); plus hop-hotkeyd on both of its backends (X11 grab loop, and the
  GlobalShortcuts portal over D-Bus);
- the workspace-wide `unsafe`/FFI inventory, `deny.toml`, `Cargo.lock`, and CI,
  re-checked for what M3+M4 added; and
- untrusted-boundary inputs on this surface: daemon and provider-supplied item
  content, keybinding and hotkey configuration files, environment-derived
  paths, D-Bus peers and portal responses, X11 servers and Wayland compositors,
  and icon and font files on disk.

**The boundary this sweep inherits.** The M2 socket-boundary threat model
(`docs/security/2026-08-02-m2-socket-boundary-threat-model.md`) is the
authority on peer trust, and it defers this side explicitly: under "What this
model does not cover" it names "**The client side.** `hop-gtk`'s handling of
items, icons, clipboard writes and URL opens is M3/M4 work with its own sweep".
That deferral is what this document answers.

A process running as the same UID is inside the boundary and cannot be
distinguished by any protocol here. Several findings below therefore describe
robustness and honesty failures rather than claims that a boundary was
crossed: #272, #274 and #281 all name same-UID actors who could reach the same
outcome more directly, and are filed because of what they cost the *user's*
picture of what the software is doing, not because they grant new capability.
Provider-authored content is a separate matter and is treated as untrusted
throughout, as `hop-protocol`'s own `content.rs` module doc already treats it.

OWASP Top 10:2025 is a web-focused awareness taxonomy applied here as a
cross-check for a desktop application, not a completeness claim. The M2 sweep
used the same 2025 taxonomy; the M1 sweep predates it and used 2021, which is
why its A10 (SSRF) verdict is discharged under A01 below rather than under
this document's A10.

## Method and evidence

Six independent read-only audits ran in parallel over disjoint scopes — IPC and
process lifecycle; content rendering; styling, fonts, icons and the build
script; session, window and keymap; all of hop-hotkeyd; and the cross-cutting
supply-chain, `unsafe`, logging and packaging pass. Every candidate they
returned was then re-verified against the source by the session that filed it,
at the cited lines, before any issue was opened. Three candidates changed shape
under that check and one disconfirming test was run deliberately:

- The A09 finding arrived scoped to `fonts.rs` alone. Re-checking the sinks at
  `app.rs:155`, `:198` and `:216` showed `KeymapError` (seven `path.display()`
  variants) and the *shared* `hop_protocol::socket::SocketPathError` (four
  more) reach the same `eprintln!`. #282 is filed at that wider scope, which
  matters because `socket.rs` is shared with `hopd` and `hop-cli` — #159's
  `hopd`-scoped fix left it uncovered there too.
- The portal finding arrived describing a session handle an attacker would have
  to discover. Reading `create_session` showed `session_handle_token` is the
  fixed literal `"hop"` (`portal.rs:242`) and request tokens are a predictable
  `hop_1`, `hop_2` counter (`portal.rs:400-404`), so the path is derivable
  rather than guessable. #272 is filed on the stronger fact.
- The zombie finding (#273) was tested for its escape hatch before filing: a
  `SIGCHLD` disposition of `SIG_IGN`, or `SA_NOCLDWAIT`, would make the kernel
  auto-reap and the finding would be wrong. `install_signal_fd`
  (`run.rs:107-134`) blocks `SIGINT` and `SIGTERM` only, and no `sigaction`
  call exists in either binary. The finding stands.
- `tempfile` 3.27.0's directory-permission behaviour underlying #283 was
  confirmed in the crate's own source (`src/lib.rs:65-66`) rather than taken
  from the audit's summary, because the finding turns entirely on files and
  directories being treated differently by that crate.

The audited inventory:

| Surface | Evidence inspected | Controls cross-checked |
| --- | --- | --- |
| IPC and lifecycle | `apps/hop-gtk/src/{ipc/mod,ipc/client,app,cli,lib,main,screenshot}.rs` and tests | handshake ordering, pre-allocation gate, execute id binding, argv/env parsing, socket-path derivation, reconnect |
| Content rendering | `apps/hop-gtk/src/ui/{window,row,action_panel,view,model,marker_highlight,mode_label,offline_indicator}.rs` and tests | markup escaping, byte-range slicing, icon loading, clipboard and URL outcomes, list virtualization |
| Styling and assets | `apps/hop-gtk/src/{tokens,stylesheet,style,material,fonts,icon_roots}.rs`, `build.rs` and tests | CSS provenance, fontconfig registration, icon-root allow-list, materialization modes |
| Session and input | `apps/hop-gtk/src/{session,layer_shell,x11,keymap}.rs`, `ui/window.rs` lifecycle paths, and the three smoke tests | session detection, exclusive keyboard focus, activation token, keymap parsing and dispatch |
| hop-hotkeyd | `crates/hop-hotkeyd/src/{config,binding,run,portal,main,lib}.rs` and tests | config bounds, command spawn, X11 grab arbitration, portal conversation, signal handling |
| Cross-cutting | `Cargo.toml` ×3, `Cargo.lock`, `deny.toml`, `.github/workflows/ci.yml`, `contrib/`, `assets/`, every `unsafe` block | dependency gate, license exceptions, action pinning, FFI soundness, log sinks, packaging |

Read-only source probes traced every reported flow. No production or test file
was created or changed by the audit; the only change this sweep lands is this
document.

## Ten-category verdicts

Each OWASP Top 10:2025 category has exactly one verdict.

| Category | Verdict |
| --- | --- |
| [A01 Broken Access Control](https://owasp.org/Top10/2025/A01_2025-Broken_Access_Control/) | Applicable — [#272](https://github.com/pedrosousa13/hop/issues/272) and [#280](https://github.com/pedrosousa13/hop/issues/280) filed |
| [A02 Security Misconfiguration](https://owasp.org/Top10/2025/A02_2025-Security_Misconfiguration/) | Applicable — [#274](https://github.com/pedrosousa13/hop/issues/274) and [#283](https://github.com/pedrosousa13/hop/issues/283) filed |
| [A03 Software Supply Chain Failures](https://owasp.org/Top10/2025/A03_2025-Software_Supply_Chain_Failures/) | Applicable — no finding filed |
| [A04 Cryptographic Failures](https://owasp.org/Top10/2025/A04_2025-Cryptographic_Failures/) | **Not applicable at this surface** — no cryptographic material exists in it |
| [A05 Injection](https://owasp.org/Top10/2025/A05_2025-Injection/) | Applicable — [#277](https://github.com/pedrosousa13/hop/issues/277) filed |
| [A06 Insecure Design](https://owasp.org/Top10/2025/A06_2025-Insecure_Design/) | Applicable — [#279](https://github.com/pedrosousa13/hop/issues/279) and [#284](https://github.com/pedrosousa13/hop/issues/284) filed |
| [A07 Authentication Failures](https://owasp.org/Top10/2025/A07_2025-Authentication_Failures/) | Applicable — no finding filed |
| [A08 Software or Data Integrity Failures](https://owasp.org/Top10/2025/A08_2025-Software_or_Data_Integrity_Failures/) | Applicable — no finding filed |
| [A09 Security Logging & Alerting Failures](https://owasp.org/Top10/2025/A09_2025-Security_Logging_and_Alerting_Failures/) | Applicable — [#282](https://github.com/pedrosousa13/hop/issues/282) filed |
| [A10 Mishandling of Exceptional Conditions](https://owasp.org/Top10/2025/A10_2025-Mishandling_of_Exceptional_Conditions/) | Applicable — [#273](https://github.com/pedrosousa13/hop/issues/273), [#275](https://github.com/pedrosousa13/hop/issues/275), [#276](https://github.com/pedrosousa13/hop/issues/276), [#278](https://github.com/pedrosousa13/hop/issues/278) and [#281](https://github.com/pedrosousa13/hop/issues/281) filed |

## Category evidence

### A01:2025 — Broken Access Control · applicable, #272 and #280 filed

The two new trust boundaries the brief names are where this category landed.

`hop-hotkeyd`'s portal backend decides whether a D-Bus message is a genuine
portal signal from its interface, member and object path, never its sender —
`msg.header().sender()` is not called anywhere in `portal.rs`. Because
`session_handle_token` is the fixed literal `"hop"` (`portal.rs:242`) and
request tokens count up as `hop_1`, `hop_2` (`portal.rs:400-404`), the paths a
forged signal must carry are derivable rather than secret. #272 covers both the
forged activation and the more damaging forged bind verdict, which lets hop
report a working hotkey that is not bound.

`present_with_token` writes `XDG_ACTIVATION_TOKEN` into the process environment
and never removes it (`ui/window.rs:1320`; no `remove_var` for it exists in the
workspace). An activation token is the compositor's focus-steal grant; leaving
a spent one in the environment hands it to every child the process later
spawns, including a URL handler GIO launches directly. #280.

The client's side of execute is sound and is recorded under "Checked and sound
controls" below. The M1 sweep's deferred SSRF obligation is discharged at the
end of this section list, under "Inherited obligations".

### A02:2025 — Security Misconfiguration · applicable, #274 and #283 filed

Both findings are about a default taken rather than decided.

`spawn_toggle` resolves the launcher binary by bare name through `$PATH`, and
re-resolves on every firing (`run.rs:677`). #274.

The bundled-font materialization directory is created by
`tempfile::Builder::tempdir_in` with no `.permissions(...)`
(`fonts.rs:514-520`). `tempfile` 3.27.0 narrows temporary files but not
temporary directories, and says so in its own module doc (`src/lib.rs:64-67`):
"Temporary _files_ created with this library are private by default on all
operating systems. However, temporary _directories_ are created with the
default permissions and will therefore be world-readable by default unless the
user has changed their umask and/or default temporary directory." So this lands
at 0755 under a common umask, where `hopd`'s `runtime_dir.rs` passes
`DirBuilder::mode(0o700)` for the analogous case. `$XDG_RUNTIME_DIR`'s
spec-mandated 0700 is the real backstop, and this crate neither states nor
checks that assumption. #283.

Packaging is not a gap here: `contrib/` and `assets/` contain no `.desktop`
entry, systemd unit, autostart file or D-Bus service file for either new
binary. The v1 design spec scopes packaging to M6, so the absence is scheduled
rather than missing, and it re-checks at that milestone's sweep.

### A03:2025 — Software Supply Chain Failures · applicable, no finding filed

`cargo deny check` passes over the full graph including every M3+M4 addition
(`x11rb`, `gtk4-layer-shell`, `zbus`, `gdkx11`/`gdkwayland`,
`yeslogic-fontconfig-sys`, `async-channel`, `glib-build-tools`) — see
Verification below for the run.

The two license exceptions this milestone added are both scoped to the crate
that needed them rather than widening the allow-list:
`{ allow = ["Apache-2.0"], name = "gethostname" }`, arriving with `x11rb`
(#232), and `{ allow = ["Apache-2.0 WITH LLVM-exception"], name = "target-lexicon" }`,
arriving with `system-deps` (#179). `git log -p -- deny.toml` confirms each was
added in the commit that introduced the dependency needing it.

CI's `cargo-deny-action` remains SHA-pinned
(`3c6349835b2b7b196a839186cb8b78e02f7b5f25 # v2.1.1`) and runs workspace-wide
over one lockfile, so it covers the two new member crates with no per-crate
wiring. The `layer-shell-gate` job clones `gtk4-layer-shell` from GitHub at a
pinned 40-character commit SHA, but that is a C library provisioned for CI
tests only and never a Cargo dependency, so it never enters `Cargo.lock` or
this gate's purview. `apps/hop-gtk/build.rs` resolves `glib-compile-resources`
by bare name off `$PATH` — ordinary build-tool discovery, reading no untrusted
runtime input, with no adversarial scenario short of an already-compromised
build machine, which is outside this boundary.

`actions/checkout@v4` and `dtolnay/rust-toolchain@1.98.0` are still not
SHA-pinned. That predates M3/M4 — both present since M1's `a3a128a` — so it is
recorded here rather than filed as something this milestone introduced.

### A04:2025 — Cryptographic Failures · not applicable at this surface

**No code in the swept surface performs a cryptographic operation, holds key
material, or hashes anything.** `apps/hop-gtk`, `crates/hop-hotkeyd` and
`crates/hop-cli/src/dbus.rs` declare no cryptographic dependency — no `sha2`,
`hmac`, `ring`, or equivalent appears in any of their manifests, and no such
call exists in their source.

The workspace's one cryptographic control, the v2 learning envelope's
HMAC-SHA256 over a sibling `learning.key`, lives entirely in `hop-core` and
`hopd` behind the daemon boundary, and carries the M2 sweep's A04 verdict. No
key, token, or secret crosses into the frontend: the nearest thing that does is
the XDG activation token, which is a compositor grant rather than a
cryptographic credential and is filed under A01 as #280.

The category becomes applicable to this surface the moment the frontend holds a
secret of its own — a stored credential for a network provider being the
obvious future case, which arrives with M5's providers rather than here.

### A05:2025 — Injection · applicable, #277 filed

The markup vector is closed and was confirmed closed: an exhaustive grep across
`apps/hop-gtk/src/ui/` finds no `set_markup`, no `use_markup`, and no
markup-shaped format string. Every rendered field reaches a widget through
`set_text`, `set_tooltip_text` or `set_icon_name`. CSS is equally closed —
`tokens.rs:124` and `stylesheet.rs:53` load `include_str!`-bundled compile-time
assets, and no config, environment or wire value is interpolated into the CSS a
`gtk::CssProvider` is given.

What is open is a plain-text gap. `Action.label` is the one rendered field in
the protocol with no content gate: a bare `String` whose only check is
`de_action_label` → `string()` → `BoundedString`, a byte-length bound
(`item.rs:139-146`, `limits.rs:853-855`). Every sibling is a validating newtype
refusing control characters at construction — `ItemTitle::new`
(`content.rs:288-296`) is the direct comparison. `hop-gtk` renders the
unfiltered label in three places (`row.rs:1552`, `row.rs:1395`,
`action_panel.rs:321`). `QueryRouted.pending_providers` has the same shape.
#277.

One observation recorded rather than filed: `char::is_control()` — the filter
the sibling fields *do* apply — does not flag Unicode bidi overrides such as
U+202E, so a spoofing variant survives even where the gate is applied. That is
a question about the gate's rule rather than about a field escaping it, and it
belongs to whoever takes #277.

Command execution is not an injection surface here. `hop-hotkeyd` spawns
`Command::new("hop").arg("toggle")` — pure argv, no shell anywhere in the
crate — and its config schema supplies only a keybinding *spelling*, resolved
by `Binding::parse` (`binding.rs:172-209`) through closed lookup tables into a
`{modifiers, keysym}` pair. There is no path by which config content becomes
part of a command line. (*Which* binary that argv resolves to is #274, under
A02.)

### A06:2025 — Insecure Design · applicable, #279 and #284 filed

#279 is this sweep's sharpest finding, and it is a composition failure: two
decisions, each defensible alone, that combine into a state with no exit.
`build_lookup` (`keymap.rs:978-985`) collapses two `keymap::Action`s sharing
one key into a single `HashMap` slot, and the module's own doc records that
which one survives depends on a per-process `RandomState`
(`keymap.rs:1509-1519`). Separately, the layer-shell strategy returns `false`
from `dismisses_on_focus_loss` (`session.rs:145`), so
`wire_dismiss_on_focus_loss` is never wired (`window.rs:695-697`) and
`keymap::Action::Dismiss` (`window.rs:866`) is the only remaining close path —
while `KeyboardMode::Exclusive` (`layer_shell.rs:110`) holds every keystroke in
the session. A `[keymap]` collision on Escape
therefore leaves a KDE or wlroots user unable to close hop or reach any other
application, on a coin flip decided at startup. The keymap module flags
conflict detection as M6's work but frames the risk as a rebound key stealing a
printable character, never as `Dismiss` losing its own key.

#284 records a residual rather than a defect: the X11 session path offers no
input isolation between clients, which is a property of X11 and not fixable
here — but the M2 threat model names "Query text … which can be a pasted
credential" as an asset, and nothing in `x11.rs`, `session.rs` or the security
docs tells a user choosing a session type that this path is weaker for it. It
is filed on the same footing as this repo's existing threat-model maintenance
issues (#102, #146, #176), asking for a written acceptance rather than a code
change.

Considered design checked and found sound is recorded below.

### A07:2025 — Authentication Failures · applicable, no finding filed

The client does not compare `HelloAck.api_version` against its own
(`ipc/client.rs:115`), and this is deliberate rather than a gap: the M2 threat
model settles `API_VERSION` as "a compatibility marker … not an authorization
value", version-mismatch refusal is the daemon's job, and `hop-cli` uses the
identical `DaemonMsg::HelloAck { .. } => {}` pattern
(`crates/hop-cli/src/lib.rs:385`). It is one consistent cross-client shape, not
a frontend-specific omission. `connect_and_handshake` does enforce handshake
ordering, treating anything but a `HelloAck` as a connect failure.

`hop-hotkeyd` implements no authentication of its own. Its two peer
relationships rest on the OS's same-UID model: the X server via Xauthority, and
the session bus. The one place peer *identity* genuinely matters — who may emit
a portal signal — is an access-control question and is filed as #272 under A01
rather than counted twice here.

### A08:2025 — Software or Data Integrity Failures · applicable, no finding filed

Nothing in the swept surface updates itself, verifies a signature, or loads
dynamic code. Fonts and the stylesheet are bundled at compile time through
`include_str!` and GResource from the repo's own tracked `assets/`, so no
runtime artifact is fetched or trusted.

Deserialization-time integrity is enforced in `hop-protocol` before a value can
reach a widget, and the two refusal postures on this side are whole-or-nothing
rather than partial: `Keymap::from_path` refuses the entire load on any parse
or shape error rather than silently defaulting or half-applying
(`keymap.rs:99-131`, `:825-854`), and `hop-hotkeyd`'s deliberately softer
posture — log and run as a no-op rather than refuse to start — is reasoned
through in its own module doc (`config.rs:1-56`) and contrasted there against
`hopd` and `hop-gtk`'s stricter stance. The gap #277 names is a *content-rule*
gap, filed under A05; it is not an integrity-of-distribution question.

### A09:2025 — Security Logging & Alerting Failures · applicable, #282 filed

Query text never reaches a log sink. `app.rs` passes `query: &str` only into
the IPC command channel; `cli::Args::Screenshot`'s `--query` is never logged;
`ipc/client.rs:266` deliberately keeps a raw socket-read failure to stderr
while routing the daemon's bounded `ProtoError` text to the UI channel rather
than to stderr (`ipc/client.rs:252`). `hop-hotkeyd` logs no key-press trace and
no command text — `run.rs:509` logs the user's own configured binding spelling
once at grab setup, and `portal.rs:229` logs the static `SHORTCUT_ID` constant.
Neither new binary depends on a logging framework; every sink is a bare,
greppable `eprintln!`.

What is open is path escaping. #159 established that environment- and
filesystem-derived paths must be escaped before reaching diagnostics, and fixed
it for `hopd` through `hop_core::sanitize::escape_path`. The shared bounded-read
seam says outright that it cannot reach that fix:
`crates/hop-protocol/src/config_file.rs:49-88` carries a doc comment titled
"Why refusal messages name the path with `Path::display` rather than
`escape_path`", explaining that `hop-protocol` cannot depend on `hop-core` and
deferring the fix to "a future `hop-gtk` caller". Two such callers now exist —
`hop-gtk::keymap` and `hop-hotkeyd::config` — and neither applies it, alongside
`FontsError` and the *shared* `hop_protocol::socket::SocketPathError`, which
leaves the gap open for `hopd` and `hop-cli` as well. #282.

### A10:2025 — Mishandling of Exceptional Conditions · applicable, #273, #275, #276, #278 and #281 filed

The dominant category for this surface, as A04 (2021 taxonomy) was for M1 and
A10 was for M2. Five findings, in three families.

**Unbounded reads and allocations.** `load_path_texture` buffers a whole icon
file before decoding, with the allow-list gating *where* the file is and never
how large (#278). `read_message` in the M4 D-Bus client sizes a `vec![0u8; …]`
from two peer-supplied `u32`s with no ceiling, where `hop-protocol`'s
`framing::payload_len` gate exists precisely to prevent that shape (#281).

**Unbounded queues.** The daemon→UI event channel is
`async_channel::unbounded` (`ipc/mod.rs:194`), and the bounded `mpsc::channel(8)`
that looks like backpressure supplies none, because the driver's forward onto
an unbounded channel never suspends (#276).

**Failure to recover.** No client-side socket read or write is bounded by a
timeout, so a peer that accepts and then goes silent leaves the launcher open
and permanently unresponsive with no error shown — the mirror of `hopd`'s own
`INBOUND_PAYLOAD_READ_TIMEOUT` (#275). And `spawn_toggle` drops its `Child`
without ever waiting, with no `SIGCHLD` auto-reap installed and no debounce
against X11 autorepeat, reproducing the exact pattern #162 fixed one crate over
(#273).

Sound handling in this category is recorded below.

### Inherited obligations

The M1 sweep's A10 (2021 SSRF) verdict closed with: "**The M4 sweep must re-run
A10 against the real providers** rather than inheriting this verdict." That
obligation is discharged here, and the answer is unchanged: **no code in the
workspace makes, or can make, an outbound request.** No HTTP client dependency
exists in any manifest (`reqwest`, `hyper`, `ureq`, `isahc`, `curl` all absent),
and no weather or web-search provider exists in `hopd` — the two `Mode` values
M1 named as the future exposure route still have no implementation behind them.

Under the 2025 taxonomy SSRF has folded into A01, so the re-run belongs there
rather than under this document's A10. The real providers M1 anticipated arrive
with M5, so the substantive re-run moves to that milestone's sweep — this is a
deferral with the reason restated, not a silent inheritance.

## Checked and sound controls

Recorded so a later sweep does not re-litigate them, and so the "applicable
with no finding" half of each verdict is legible.

**The icon allow-list (#93) is enforced where the threat model says it is, and
correctly.** `AllowedIconRoots::permits` (`icon_roots.rs:261-282`) resolves the
*already-opened* descriptor through `/proc/self/fd/<n>` rather than re-resolving
the path, which closes the TOCTOU window a stat-then-open check would leave; it
refuses an unlinked-then-reopened `"(deleted)"` target; and it runs at its one
call site before a single byte is read (`row.rs:1176-1180`). Symlinked roots,
symlink escape and `/proc/self/mem` are covered by dedicated tests. What it does
not do is bound file size — that is #278, and it is a separate control, not a
hole in this one.

**Execute binds to objects, not indices.** Every action a click can send is read
off the `Item`/`Action` actually bound to the widget, never a client-held index
into a list that may have moved. `resolve_action_icons` sets each button's
action target from the bound pair on every bind —
`(item.id.as_str(), action.id.as_str()).to_variant()` (`row.rs:1396-1398`),
overwriting the empty placeholder the buttons are constructed with
(`row.rs:946`) — and the execute command carries those same ids
(`window.rs:2168`). The daemon separately re-validates membership against its
retained set (Decision 1 / #59). The client asserts no independent
access-control claim, and no path was found where it could be tricked into one.

**#23's and #24's fixes are enforced on this side.** `ExecOutcome::OpenUrl`'s
scheme allow-list and `CopyText`'s control-character filter are enforced by
construction before a client-side value can exist (`wire.rs:364-367`), so
`handle_outcome`'s clipboard write and `launch_default_for_uri` call
(`window.rs:1652-1667`) inherit the guarantee with nothing left to check.
`IconSpec::Path` is gated by `open_regular_file`'s `O_NONBLOCK` + `fstat`
regular-file check (`content.rs:1033-1056`), which closes the FIFO and
device-hang class.

**Wire byte ranges never index a `String` directly.** `marker_highlight.rs:66-71`
uses `text.get(..)` rather than `&text[..]`, so a span off a character boundary
produces no attributes instead of a panic, and it re-checks the span's source
text against the entry's live text (`:103-110`), closing the stale-span race
independently of the wire's `query_id` supersession.

**Rendering is virtualized and doubly bounded.** `MAX_ITEMS_PER_RESULTS_FRAME`
caps a frame at 1,000 items, and `view.rs`'s `GtkListView`/`SignalListItemFactory`
builds only visible slots, so item count never becomes widget count.

**The pre-allocation gate holds on the daemon socket.** `framing::payload_len`
is called before every allocation sized by a peer-supplied length prefix, in
both `read_loop` (`client.rs:51`) and `connect_and_handshake` (`client.rs:108`).
#281 is the *other* client — the hand-rolled D-Bus one — where the convention
did not travel.

**Session detection asks the display, not the environment.**
`SessionKind::detect` downcasts the live `gdk::Display` (`session.rs:23-32`,
`:84-95`) rather than trusting `$GDK_BACKEND`, `$DISPLAY` or `$WAYLAND_DISPLAY`,
and the layer-shell probe asks the compositor (`layer_shell.rs:74-87`). An
attacker-controlled environment variable cannot make hop misdetect its session
and pick the wrong overlay or focus strategy.

**Exclusive keyboard focus is taken late and released by the compositor.** The
window is fully built before the layer surface is configured
(`window.rs:515-527`, `:549-551`) and the grab is granted only at `present()`,
after keyboard wiring completes — no window is ever mapped half-built.
`keymap::Action::Dismiss` dispatches synchronously from the local
`EventControllerKey`, independent of IPC state, so a hung or crashed `hopd`
cannot block Escape
(confirmed against `apply_event`'s `Disconnected`/`ConnectFailed` arms,
`window.rs:1340-1407`). A terminated client's surfaces, grab included, are torn
down by the compositor on connection loss. #279 is about a keymap that removes
`Dismiss` from Escape, which is a different failure from the grab not being
released.

**The X11 grab is checked synchronously.** `run.rs:499-508` forces a round trip
with `.check()`, so a `BadAccess` from a conflicting grab is observed rather
than left as an unobserved async X11 error — the failure mode the crate's
"single-instance by X-level evidence, not lockfiles" design (`run.rs:39-46`)
deliberately depends on catching.

**Config reads are bounded at the shared seam.** `hop_protocol::config_file::read`
(`config_file.rs:243-286`) opens `O_NONBLOCK`, `fstat`s the descriptor, and
bounds the read with `take(max_bytes + 1)`, with a FIFO-hang regression test.
Both new binaries read their configuration through it. Its one gap is the path
*escaping* deferral, which is #282.

**Every `unsafe` block in M3+M4 code is sound.** `unsafe_code = "deny"` gates
every member crate, so each block is declared with an `#[expect]` and a SAFETY
note; `x11.rs` and `layer_shell.rs` contain none at all, speaking X11 through
`x11rb` and layer-shell through the safe `gtk4_layer_shell` bindings.

| Location | Call | Verdict |
| --- | --- | --- |
| `hop-gtk/src/fonts.rs:584` | `FcConfigAppFontAddDir` | Sound — `CString` guarantees a NUL-terminated buffer with no interior NUL; pointer valid for the statement; runs once inside a `LazyLock`; return checked and mapped to a typed error |
| `hop-gtk/src/ui/window.rs:1319-1320` | `env::set_var` | Sound *on the axis it argues*: single-threaded on the GTK main thread, no other thread touches the environment, and the value came from this process's own environment so it cannot contain a NUL. The token's *lifetime* is a separate question and is #280 |
| `hop-cli/src/dbus.rs:162` | `libc::getuid` | Sound — no preconditions, cannot fail |
| `hop-hotkeyd/src/run.rs:113` | `sigemptyset`/`sigaddset`/`sigprocmask`/`signalfd` | Sound — every return checked before proceeding; the fresh fd is wrapped in `OwnedFd`/`File` with no other owner, so no double close |
| `hop-hotkeyd/src/run.rs:149` | `libc::poll` | Sound — valid initialized slice for the call's duration; negative return checked, `EINTR` handled as benign |
| `hop-hotkeyd/src/run.rs:655` | `sigemptyset`/`sigprocmask` | Sound — both async-signal-safe per POSIX, satisfying `pre_exec`'s contract; both returns checked |
| `hop-hotkeyd/src/run.rs:692` | `CommandExt::pre_exec` | Sound — registration cannot fail; a failing closure surfaces through `spawn`'s checked `Result` |

The table above is the M3+M4 production inventory and is deliberately not the
whole workspace's. Two further groups complete the count. The eighth production
block is `crates/hopd/src/server.rs:481`'s `OwnedFd::from_raw_fd` — M2 code
(#62), unchanged by this milestone, carrying the M2 sweep's verdict rather than
one of this document's. Five more exist in test code only
(`libc::mkfifo`/`pre_exec` across `hop-protocol` and `hopd` tests) and ship in
no binary. Seven plus that one plus those five is the thirteen the root
`Cargo.toml`'s own comment tallies — eight production, five test.

## Verification

What was run to produce and check this document, at
`a8259f29cb414c9fc426c14185d06ef3b6d4e76f`:

- Six parallel read-only source audits over disjoint scopes, every returned
  finding then re-verified against the cited lines before filing.
- `cargo deny check` over the full workspace graph — `advisories ok, bans ok,
  licenses ok, sources ok`.
- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D
  warnings`, both clean, and `cargo test --workspace` — 1,124 tests passed, 0
  failed, 5 ignored, across 46 suites — the repo's own CI gates, confirming
  this sweep changed no behaviour.
- `git log -p -- deny.toml` to confirm each M3+M4 license exception was added
  alongside the dependency requiring it.
- `tempfile` 3.27.0's own source read directly for the directory-permission
  claim underlying #283, rather than relied on second-hand.
- Manifest and source greps confirming no HTTP client and no network provider
  exist, discharging M1's deferred SSRF obligation.

Thirteen findings were filed as #272 through #284, each labeled
`needs-triage`, categorized, milestoned to M4, and carrying a suggested
severity. **This sweep reports them and does not fix them**; none was triaged
into a Queue by this session.

## Limits and follow-up

- **This is a point-in-time review of source, not a penetration test.** No
  finding below was exploited against a running system; each is argued from the
  code and its cited lines. The X11 and portal findings in particular describe
  what the code permits, not an observed compromise.
- **The same-UID boundary is inherited, not re-derived.** Several findings name
  actors already inside it. That is deliberate: they are filed for what they
  cost the user's picture of the system, and a reader who disagrees with that
  framing should read them as robustness and honesty issues rather than
  boundary breaks.
- **Real-session coverage is uneven.** The audits read code across all three
  session paths, but #266 (verify the GlobalShortcuts portal arm against a real
  GNOME 48+ session) is still open, so the portal path's findings rest on source
  reading rather than observed behaviour against a real portal.
- **Bidi-override spoofing is out of scope of its own finding.** #277 fixes a
  field escaping the content gate; it does not settle whether `is_control()` is
  the right rule for the fields already inside it. Someone should decide that
  separately.
- **A04 becomes applicable to this surface** the moment the frontend holds a
  secret of its own, which arrives with M5's network providers.
- **A01's SSRF half re-runs at M5**, against real providers, as recorded under
  "Inherited obligations".
- **Packaging re-checks at M6**, when the desktop entries, units and autostart
  files this milestone did not ship actually exist.
