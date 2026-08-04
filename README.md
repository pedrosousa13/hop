# hop

The GNOME-native, trustworthy-plugins launcher that works everywhere.

Pre-alpha. This repository currently contains M2's daemon through the query
lifecycle: a `hopd` daemon that serves streamed, cancellable queries over
`$XDG_RUNTIME_DIR/hop/hopd.sock` (results still come from a placeholder
source until the provider host lands), and a `hop` CLI that speaks to it
(`hop query`, `hop version`). Real providers, a query router wired into the
daemon, and a UI are later M2 slices.

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
does not yet call into it: `hopd` streams query results with server-side
cancellation and a bounded per-query retained set, but its one source is
still the skeleton's placeholder item — the provider host is a later M2
slice. `hop-cli` is the only binary that talks to `hopd` today; a UI comes
later.

## Build and test

```sh
cargo test --workspace
```
