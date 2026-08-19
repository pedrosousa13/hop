# hop

The GNOME-native, trustworthy-plugins launcher that works everywhere.

Pre-alpha. This repository currently contains M2's daemon through the query
lifecycle and its provider host: a `hopd` daemon that serves streamed,
cancellable queries over `$XDG_RUNTIME_DIR/hop/hopd.sock` (issue #180 lets
`--socket <path>` override that default, on `hopd` and both its clients, to
any path that still resolves inside `$XDG_RUNTIME_DIR`), routed through
`hop-core`'s query router and provider host (results come from the walking
skeleton's item and, as of issue #57, a real apps provider indexing
installed `.desktop` files), and a `hop` CLI that speaks to it (`hop query`,
`hop version`). A calculator provider and a UI are later M2 slices.

## Design

The full design is at
[`docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md`](docs/superpowers/specs/2026-07-30-hop-launcher-v1-design.md).
The normative [v1 theme token contract](docs/theme-token-contract.md) defines
the author-facing theme boundary.

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
placeholder source, and as of issue #57 the apps provider is registered
alongside it, indexing installed `.desktop` files and keeping that index
current via filesystem watching — windows and the rest of the provider
table are later M2 slices. `hop-cli` is the only binary that talks to
`hopd` today; a UI comes later.

## Build and test

```sh
cargo test --workspace
```

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
hopd.service` confirms it is running afterward. If the unit ever declares
more than one socket, hopd uses only the first and warns on stderr.

A standalone `hopd --socket <path>` (with the matching `hop --socket <path>`
or `hop-gtk --socket <path>` on the client side) binds a different socket
instead of the derived one — useful for running a development `hopd`
alongside a real session's own, at `$XDG_RUNTIME_DIR/hop-dev/hopd.sock` or
similar. `<path>` must still resolve inside `$XDG_RUNTIME_DIR`; anything else
is refused rather than silently falling back to the derived path.
