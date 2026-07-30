# hop

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

Both are currently empty stubs.

## Build and test

```sh
cargo check --workspace
cargo test --workspace
```
