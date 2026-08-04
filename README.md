# hop

The GNOME-native, trustworthy-plugins launcher that works everywhere.

Pre-alpha. This repository currently contains M2's daemon through the query
lifecycle and its provider host: a `hopd` daemon that serves streamed,
cancellable queries over `$XDG_RUNTIME_DIR/hop/hopd.sock`, routed through
`hop-core`'s query router and provider host (results come from the walking
skeleton's item and, as of issue #57, a real apps provider indexing
installed `.desktop` files), and a `hop` CLI that speaks to it (`hop query`,
`hop version`). A calculator provider and a UI are later M2 slices.

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
message frames. `hop-core` carries all six pieces listed above, and `hopd`
now depends on it: `hopd` streams query results with server-side
cancellation and a bounded per-query retained set, routing every query
through `hop-core`'s provider host, which runs each registered provider
under an enforced budget and streams back what passes its manifest checks.
The skeleton's item is now a real registered provider rather than a
placeholder source, and it is still the only one registered — apps,
windows and the rest of the provider table are later M2 slices. `hop-cli`
is the only binary that talks to `hopd` today; a UI comes later.

## Build and test

```sh
cargo test --workspace
```
