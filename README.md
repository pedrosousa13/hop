# hop

The GNOME-native, trustworthy-plugins launcher that works everywhere.

Pre-alpha. This repository currently contains M2's walking skeleton: a `hopd`
daemon that serves one hardcoded item over `$XDG_RUNTIME_DIR/hop/hopd.sock`,
and a `hop` CLI that speaks to it (`hop query`, `hop version`). Real
providers, a query router wired into the daemon, and a UI are later M2
slices.

## Design

The full design is at
[`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`](docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md).

## Crates

- `crates/hop-protocol` — the typed IPC contract: every type that crosses a
  process boundary.
- `crates/hop-core` — the search behavior: query router, fuzzy ranking,
  learning engine, aliases, provider trait, search pipeline.
- `crates/hopd` — the launcher daemon: binds the Unix socket, runs the
  handshake, and serves connections.
- `crates/hop-cli` — the `hop` command-line client that speaks to `hopd`.

`hop-protocol` carries the item/action model and the client/daemon IPC
message frames. `hop-core` carries all six pieces listed above, but `hopd`
does not yet call into it — the walking skeleton answers every query with the
same hardcoded item, regardless of what was typed. `hop-cli` is the only
binary that talks to `hopd` today; a UI comes later.

## Build and test

```sh
cargo test --workspace
```
