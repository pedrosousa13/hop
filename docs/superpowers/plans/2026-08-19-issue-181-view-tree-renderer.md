# Issue #181 — a view-tree renderer whose only node type is `Row`

Spec: GitHub issue **#181**, slice item 4 of the #80 grill's spec
(`docs/superpowers/specs/2026-08-10-hop-m3-frontend-design.md`, decision D2).
The issue body is the binding authority; this plan is the argument for how it
lands.

## What the issue asks for, verbatim

1. A view-tree node type exists with exactly one variant, `Row`.
2. The `GtkListView` factory renders through the dispatch point rather than
   constructing a row directly.
3. Adding a hypothetical second node type would require no change to the
   factory's structure — demonstrated by a test or by the shape of the code,
   not by adding one.
4. No node type other than `Row` exists anywhere in the change.
5. Row recycling still holds — the renderer must not reintroduce
   destroy-and-rebuild.

Out of scope, per the issue: any second node type; the v2 Tier 1 authoring
contract; styling of the row; mode labels and marker highlighting.

## Global Constraints

- **Build the seam, not the catalog.** One variant. No `Detail`, no
  `ActionPanel`, no speculative node of any kind — the issue names
  over-abstracting against a catalog that does not exist as the real risk, and
  an implementation that adds a second node has misread it. This constraint
  outranks any argument that a second variant would "prove" the seam works.
- **Recycling is not negotiable.** `GtkSignalListItemFactory` reuses one widget
  per visible slot across many items. Nothing in this change may make `bind`
  create, replace, or destroy the slot's widget.
- **No animation in `setup` or `bind`** — `ui/row.rs`'s module doc explains why
  (a recycled row would replay an entrance animation on every scroll step).
  That reasoning survives this change and must survive the rewrite of that file.
- `unsafe_code = "deny"`, `clippy::unwrap_used = "warn"`; no new `unsafe`, no
  new dependency, no AI attribution.
- **Doc-comment culture.** This repo documents *why*, at length, in place, and
  comments must be self-contained — never deferring a justification to a
  document outside the repo.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo fmt --all --check` must all pass. The `layer-shell` feature stays
  off — `gtk4-layer-shell` is not installed on this machine and that is expected.

## Design decisions

**D1 — the dispatch container, and why `bind` must not swap children.** This is
the one real design question in the issue, because GTK constrains the answer.
`connect_setup` runs *before* the slot has an item, so it cannot know which node
type it will hold; only `connect_bind` knows. That leaves two shapes:

- *Swap the child in `bind`* — `list_item.set_child(...)` per node type. This is
  rejected: it destroys and rebuilds the slot's widget on every bind whose node
  type differs from the last, which is exactly the destroy-and-rebuild criterion
  5 forbids, and it would silently undo the fixed-height reservation `setup`
  makes (`tokens::ROW_HEIGHT_PX`), reintroducing the layout shift that
  reservation exists to prevent.
- *Build every node type's widget in `setup`, and select one in `bind`* — a
  `gtk::Stack` per slot, one page per node type, `set_visible_child_name` in
  `bind`. The widget tree is created once per slot and reused forever; a second
  node type is a page added in `setup` and an arm added in `bind`, with the
  factory's structure untouched. This is what criterion 3 asks for.

Take the second. With one node type the stack holds exactly one page, which is
the structure the issue knowingly pays for ("M3 carries structure it does not
yet need"). Say so in place: a reader who finds a one-page stack deserves to
know it is a seam, deliberately paid for, and not a leftover.

**D2 — where the node type lives.** A new `apps/hop-gtk/src/ui/view.rs`, exposing
the node enum and the renderer. `ui/row.rs` stays as the thing that builds and
populates a row's widget — it becomes the `Row` arm's implementation rather than
the factory itself. `ui/mod.rs`'s doc comment lists what the module holds and
must be updated to match.

**D3 — the node carries the item, not a widget.** `Node::Row` holds what the row
needs to render (today, the `Item` — see `ui/model.rs`'s `item_of`). A view tree
is *data*; that is the whole basis of D2's ruling that the catalog belongs to
the protocol rather than the tier, since data crosses a sandbox boundary and a
widget handle does not. A node variant holding a `gtk::Widget` would quietly
destroy that property, and with it the reason this seam is being built.

**D4 — how criterion 3 is demonstrated.** By the shape of the code *and* by a
test, not by adding a second variant (criterion 4 forbids that). The test asserts
what the seam guarantees: that `bind` selects a page on the slot's existing
stack rather than replacing the slot's child, and that the same widget instance
survives being bound to one item and then another. That is recycling and the
dispatch point in one assertion, and it is the property a second node type would
depend on.

## Tasks

### Task 1 — the view module and the renderer

**New `apps/hop-gtk/src/ui/view.rs`:**

- `pub enum Node { Row(Item) }` — one variant, per criteria 1 and 4.
- A function that, given a `gtk::ListItem`'s child stack and a `Node`, dispatches
  on the variant to populate and select the right page. This is the dispatch
  point criterion 2 names.
- A `setup`-side function that builds the stack with one page per node type.
- Page names come from one place — a node knowing its own page name is what
  keeps `setup` and `bind` from drifting apart when a second type is added.

**`apps/hop-gtk/src/ui/row.rs`:** keeps building and populating the row widget,
now as the `Row` arm rather than as the factory. Its two module-doc sections
(fixed-height reserved rows, and `setup`/`bind` never animate) both still apply
and must survive — reword where the mechanism moved, but do not drop the
reasoning.

**`apps/hop-gtk/src/ui/mod.rs`:** declare `pub mod view;` and update the module
doc, which currently describes `row` as "the recycling factory that draws it".

**`apps/hop-gtk/src/ui/window.rs`:** unchanged if the factory builder keeps its
name and signature; if it moves to `view`, update the one call site at line 78.

**Tests** — these run under GTK, so follow whatever harness the existing
`apps/hop-gtk` tests use, and check `tests/headless_smoke.rs` for the
`gtk4-broadwayd` + `GDK_BACKEND=broadway` recipe this repo depends on (GTK4's
`offscreen` backend is not compiled into Ubuntu's package, and the `broadwayd`
on `PATH` is GTK3's incompatible server):

1. The slot's child after `setup` is the dispatch container, not a bare label.
2. Binding a slot to one item and then to a different item leaves the slot's
   child the **same widget instance** — recycling, criterion 5.
3. `bind` selects the `Row` page rather than calling `set_child`.
4. The rendered row shows the bound item's title, and unbind clears it — the
   behavior `ui/row.rs` has today, preserved through the refactor.

If a test cannot run in this environment, say so plainly in the report rather
than weakening it into something that passes.

## Verification

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all --check`
- `cargo check -p hop-gtk`
