# Issue #182 — read the keymap from `config.toml`

Spec: GitHub issue **#182**, implementing §8 of
`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md` and its
2026-07-31 amendment: "The whole keymap is configurable, not just the menu
key." The issue body is the binding authority; this plan argues how it lands.

This is **not the keybinding feature**. It is the structural half, landed early
because "the retrofit cost of unpicking hardcoded handlers after M3 ships is
what forces the config half early."

## What the issue asks for, verbatim

1. No key handler in `hop-gtk` compares against a hardcoded key value.
2. The keymap is read from `config.toml`, with the §8 list as defaults when the
   file says nothing.
3. A rebound key in `config.toml` changes behavior with no code change —
   covered by a test.
4. An unparseable or unknown binding is refused with a message naming it,
   rather than silently ignored or silently defaulted.
5. Mouse click still activates a row — it is not part of the keymap, and must
   not regress.

Out of scope, per the issue: the settings-window capture widget and conflict
detection (M6); global/system hotkeys (`hop-hotkeyd`, M4); persisting a keymap
written back from the UI.

The §8 defaults: Up / Down / PgUp / PgDn / Home / End for list navigation;
Enter for the default action; a secondary-action menu key; Tab for prefix
completion; Escape to dismiss.

## The starting state, which the issue does not describe

Read this before planning any of it — the issue reads as though handlers exist
to be converted, and they do not:

- `apps/hop-gtk/src/ui/window.rs` has **no key handling at all** beyond
  `self.entry.connect_activate` (line ~168), which is `GtkEntry`'s own activate
  signal — Enter, by GTK's definition, not by a comparison this code makes.
  There is no `EventControllerKey` anywhere in the crate, no `keyval` match, no
  Escape, Tab, PgUp, or menu key.
- So criterion 1 is *already* true in the trivial sense, and the real work is
  building the handlers data-driven from the start. That is precisely the
  sequencing the amendment demands.
- `hop-gtk` does not read `config.toml` at all today. `hopd` does
  (`crates/hopd/src/config.rs`), with real care.

## Global Constraints

- **Nothing hardcoded, including the defaults.** The §8 list lives in the
  config schema as defaults, not as constants consulted by handlers. A handler
  asks the keymap what action a key press means; it never names a key.
- **Refuse, name, and do not default.** Criterion 4 is the sharp one: an
  unparseable or unknown binding must be refused with a message naming it.
  Silently ignoring it, or silently falling back to the default, is the failure
  mode the criterion exists to prevent.
- `unsafe_code = "deny"`, `clippy::unwrap_used = "warn"`; no new `unsafe`.
- **No `std::env::set_var` anywhere, including tests** — `unsafe` under edition
  2024. A test needing a controlled `XDG_CONFIG_HOME` drives a path-taking seam
  or sets the variable on a spawned child process.
- **No new external dependency.** `toml` is already a workspace dependency;
  adding it to `hop-gtk`'s manifest from the workspace is not a new dependency.
- **Doc-comment culture.** Comments argue *why*, at length, in place, and must
  be self-contained.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check` all pass. `layer-shell` stays off.

## Design decisions

**D1 — `hop-gtk` reads the file itself; no IPC change.** The issue says the
keymap section is added to the schema of the file `hopd` already loads, and
says nothing about a protocol frame. Routing the keymap over IPC would be a
wire-contract change the issue never asks for. So `hop-gtk` reads
`$XDG_CONFIG_HOME/hop/config.toml` directly, and each binary parses the
sections it cares about: `hopd` its own, `hop-gtk` `[keymap]`. Neither validates
the other's sections, and an unknown *section* is not an unknown *binding* —
criterion 4 is about bindings.

**D2 — the hazard-aware read is shared, not copied.** `hopd`'s
`Config::from_path` does not merely open a file. It opens with `O_NONBLOCK` so
a FIFO at the config path cannot block startup (issue #160), refuses anything
that is not a regular file, and bounds the read through `MAX_CONFIG_BYTES`.
Every one of those protections applies just as much to a frontend reading the
same attacker-influencable path, and copying forty lines of security-relevant
code into a second crate is how the two drift.

Promote **only the bounded, hazard-aware read** — a function that takes a path
and returns the file's bytes or a typed refusal — into `hop-protocol`, the one
crate all three binaries already depend on. `hopd`'s `Config::from_path` is
rewritten to call it and keeps its own schema, its own error type, and its own
`Display` strings. `hop-gtk`'s keymap loader calls the same function.

This is the same rule `hop_protocol::socket` established and that the crate's
own doc comment now states: the crate's core is wire types, and something that
is not a wire type lives there when it is genuinely shared by all three
binaries and has nowhere better to go. Say so in place.

**Do not move the schema, the byte constants, or the error enum.** The
temptation is to unify `Config` too; resist it. `hopd`'s knobs and `hop-gtk`'s
keymap have nothing in common but a file.

**D3 — the config byte budget has to be re-priced, deliberately.**
`crates/hopd/src/config.rs` derives `MAX_CONFIG_BYTES` from
`CONFIG_KEY_LINE_BYTES * MAX_CONFIG_KEYS + CONFIG_COMMENT_BUDGET_BYTES`, and
`MAX_CONFIG_KEYS = 16` is documented as pricing "a config built from several
times today's key count" — today being two keys.

The §8 keymap is about nine bindings. A user who writes the full default keymap
out explicitly takes the file from 2 keys to ~11, and that is a *documented,
expected* config, not an abusive one. Check the arithmetic. If 16 no longer
prices several times the key count, raise it deliberately and rewrite the
reasoning in the doc comment to match — do not leave a constant whose comment
describes a world that ended. **A config a user is invited to write must never
make `hopd` refuse to start**, and `ConfigError::TooLarge` is a startup
refusal.

**D4 — the action vocabulary covers every §8 default, including actions whose
behavior does not exist yet.** Two §8 entries name frontend behavior `hop-gtk`
does not have: the secondary-action menu, and Tab completion for prefixes.

Define, bind and dispatch them anyway. The whole reason this issue is in M3 is
that the *binding* half must not be retrofitted; an action left out of the
keymap now is exactly the hardcoded handler someone adds later. Each such
action resolves to a named handler that does nothing visible yet and says so in
place, naming the slice that will fill it in. This must be honest in both
directions: the action exists and is bound, and the behavior does not exist —
neither fact may be hidden. Do not invent a secondary-action menu or a prefix
completer here; both are their own slices.

**D5 — criterion 5 needs verifying before it can be satisfied.** The criterion
says mouse click "still activates a row … must not regress", but no
`connect_activate` on the `GtkListView` appears in the crate, so it may not
work today at all. §8 lists it as an extension gap hop deliberately closes.

Determine empirically which is true, and say which in the report. If it works,
add the regression test the criterion implies. If it does not, wire it — one
`connect_activate` on the list view, routed to the same action the default-action
key runs — and test it. A criterion naming behavior that is absent cannot be
met by preserving the absence.

## Tasks

### Task 1 — the shared config-file read

**`crates/hop-protocol`**: a new module exposing a bounded, hazard-aware read of
a config file: `O_NONBLOCK` on the open, a regular-file check, and a read
bounded by a caller-supplied maximum, returning bytes or a typed refusal whose
`Display` names the path and what was wrong. Read `crates/hopd/src/config.rs`'s
`from_path` and its long doc comments first — the reasoning there is the spec
for this function, and it must survive the move rather than being summarised
away. Note its use of `escape_path` for paths in messages and preserve that
discipline, or state why it cannot apply here.

**`crates/hopd/src/config.rs`**: `from_path` calls the new function. The byte
cap stays `hopd`'s to choose and pass. Schema, error enum and messages stay put.
Re-price the budget per D3 in the same pass, since the arithmetic is right here.

**Tests**: the existing `config.rs` tests must keep passing unchanged in intent
— they are the proof the move preserved behavior. The new module gets its own
tests for the FIFO case (the existing suite has a `libc::mkfifo` test to model
this on, carrying `#[expect(unsafe_code)]` with a `reason`), the
not-a-regular-file case, the over-cap case, and the ordinary case.

### Task 2 — the keymap, and the handlers that read it

**`apps/hop-gtk`**: a keymap module owning

- the action vocabulary — every §8 default, per D4;
- parsing a binding from its `config.toml` spelling to a GDK key plus
  modifiers, refusing an unparseable or unknown one by name per criterion 4;
- the defaults, as data;
- the lookup a handler uses: given a key press, which action.

Then an `EventControllerKey` on the window that asks the keymap and dispatches.
No handler names a key. Wire the actions that have behavior — list navigation
against the existing `SingleSelection`, the default action through the existing
`activate_selected`, Escape to dismiss — and the two that do not, per D4.

Mouse activation per D5.

**Tests**: criterion 3 is the load-bearing one — a keymap in a temp
`config.toml` that rebinds an action to a different key changes behavior with
no code change. Criterion 4 needs a test per refusal shape (unparseable
spelling; unknown action name), asserting the message names the offending
binding. Plus the mouse-activation test from D5, and a test that the defaults
apply when the file says nothing about the keymap.

Tests needing a GTK display use this repo's `gtk4-broadwayd` +
`GDK_BACKEND=broadway` recipe — GTK4's `offscreen` backend is not compiled into
Ubuntu's package. Follow `apps/hop-gtk/tests/headless_smoke.rs`. Prefer driving
the keymap's own pure lookup where a test does not need a display; say plainly
in the report which tests need one and which do not.

## Verification

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all --check`
- `cargo check -p hop-gtk`
