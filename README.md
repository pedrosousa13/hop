# hop

The GNOME-native, trustworthy-plugins launcher that works everywhere.

Pre-alpha. This repository currently contains the M1 core scaffold: a cargo
workspace with two library crates and nothing else. There is no binary, no
daemon, and no UI yet.

## Design

The full design is at
[`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`](docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md).

## Crates

- `crates/hop-protocol` — the typed IPC contract: every type that crosses a
  process boundary.
- `crates/hop-core` — the search behavior: query router, fuzzy ranking,
  learning engine, aliases, provider trait, search pipeline.

`hop-protocol` carries the item/action model and the client/daemon IPC
message frames. `hop-core` so far carries the query router, fuzzy ranking, the
learning engine and aliases; the provider trait and search pipeline are still
to come.

## Build and test

```sh
cargo test --workspace
```
