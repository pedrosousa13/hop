# Item Display Field Newtypes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `Item.title` and `Item.subtitle` bounded, single-line validating newtypes so control characters cannot cross the protocol or be introduced by an in-process provider.

**Architecture:** Add transparent `ItemTitle` and `ItemSubtitle` string newtypes in `hop_protocol::content`; each fallible constructor is the single validation gate for both byte length and `char::is_control`, and each `Deserialize` implementation delegates to that same constructor. Change `Item` to carry those types, sanitize provider-generated display text before construction, and remove the now-redundant title/subtitle checks from `CheckedItems` while mechanically migrating all workspace consumers.

**Tech Stack:** Rust 2024 workspace, serde/serde_json, cargo test, rustfmt, Clippy, cargo-deny.

**Spec:** GitHub issue [#110](https://github.com/pedrosousa13/hop/issues/110), including the owner comment dated 2026-08-10 that selects protocol-boundary validating newtypes and sanitization by in-process builders.

## Global Constraints

- Preserve the wire representation exactly: title and subtitle remain bare JSON strings, and missing or explicit-null subtitle remains `None`; do not bump `API_VERSION`.
- Name the public types `ItemTitle` and `ItemSubtitle`, with fields `Item.title: ItemTitle` and `Item.subtitle: Option<ItemSubtitle>`.
- Each type owns one gate: `new(value) -> Result<Self, ContentError>` checks its existing byte maximum first, then refuses the first `char::is_control()` character with `ContentError::ForbiddenChar` naming `Item.title` or `Item.subtitle`.
- Do not normalize, trim, strip, or replace data in protocol constructors or deserialization. A peer-supplied invalid value is refused whole.
- Expose only deliberate read access consistent with the existing content newtypes: `as_str()` and `into_string()`. Do not add infallible `From<String>`, `Deref`, or lossy constructors that can bypass validation.
- In-process production builders must sanitize their own display text through `hop_core::sanitize` before calling the fallible constructor: remove every `char::is_control()` and `BIDI_CONTROLS` character, then truncate at a UTF-8 boundary. The apps provider must retain the desktop entry rather than drop it; its `AppEntry` haystack behavior is otherwise unchanged.
- Keep byte-bound enforcement and content enforcement ordered identically for constructor and serde paths; a value breaking both reports the length error.
- Remove `CheckedItems` title/subtitle length branches and their impossible-state tests after the newtypes land. Keep its action-label/action-count `FieldTooLong` behavior intact.
- Historical implementation plans under `docs/superpowers/plans/` are records: do not rewrite older plans to match the new API.
- No new dependencies, no unsafe code, no AI attribution in commits or pull-request text.

---

### Task 1: Enforce single-line item display fields across the workspace

**Files:**

- Modify: `crates/hop-protocol/src/content.rs`
- Modify: `crates/hop-protocol/src/item.rs`
- Modify: `crates/hop-protocol/src/limits.rs`
- Modify: `crates/hop-protocol/src/wire.rs`
- Modify: `crates/hop-core/src/pipeline.rs`
- Modify: `crates/hop-core/src/rank.rs`
- Modify: `crates/hop-core/src/sanitize.rs`
- Modify as required by compiler-guided migration: `crates/hop-core/src/host.rs`, `crates/hop-core/src/provider.rs`, `crates/hop-core/tests/latency.rs`
- Modify: `crates/hopd/src/apps.rs`
- Modify: `crates/hopd/src/calculator.rs`
- Modify: `crates/hopd/src/connection.rs`
- Modify: `crates/hopd/src/source.rs`
- Modify as required by compiler-guided migration: `crates/hopd/tests/calculator.rs`, `crates/hopd/tests/common/mod.rs`, `crates/hopd/tests/exec.rs`, `crates/hopd/tests/lifecycle.rs`, `crates/hopd/tests/state.rs`
- Modify as required by compiler-guided migration: `crates/hop-cli/tests/e2e.rs`
- Modify: `CONTEXT.md`
- Modify: `docs/security/2026-08-02-m2-socket-boundary-threat-model.md`
- Test in place: `crates/hop-protocol/src/content.rs`, `crates/hop-protocol/src/item.rs`, `crates/hopd/src/apps.rs`, `crates/hopd/src/calculator.rs`

**Interfaces:**

- Consumes: `limits::validated`, `limits::check_len`, `MAX_TITLE`, `MAX_SUBTITLE`, and `ContentError::ForbiddenChar`.
- Produces: `pub struct ItemTitle(String)` and `pub struct ItemSubtitle(String)`, each serde-transparent, serializable/deserializable, clonable, comparable, and constructible only through `new`; `Item` exposes those types in its existing fields.

- [ ] **Step 1: Write protocol-level failing tests for the current defect**

Add focused tests before production changes. At minimum, exercise the real `Item` JSON boundary so the current plain-string implementation fails behaviorally:

```rust
#[test]
fn item_title_carrying_a_control_character_is_refused() {
    let mut json = full_item_json();
    json["title"] = json!("before\nafter");
    let err = serde_json::from_value::<Item>(json)
        .expect_err("a multi-line title must not parse");
    assert!(err.to_string().contains("Item.title"), "got: {err}");
    assert!(err.to_string().contains("U+000A"), "got: {err}");
}

#[test]
fn item_subtitle_carrying_a_control_character_is_refused() {
    let mut json = full_item_json();
    json["subtitle"] = json!(format!("before{}after", '\u{1b}'));
    let err = serde_json::from_value::<Item>(json)
        .expect_err("a subtitle carrying ESC must not parse");
    assert!(err.to_string().contains("Item.subtitle"), "got: {err}");
    assert!(err.to_string().contains("U+001B"), "got: {err}");
}
```

Also add tests that pin: a C1 control such as U+0085 is refused; exactly `MAX_TITLE` / `MAX_SUBTITLE` bytes pass; one byte over fails; a value both over-long and control-bearing reports the length error; ordinary and empty strings retain their bytes; title serializes as a bare string; missing and explicit-null subtitle remain `None`; wrong JSON types name the correct field.

- [ ] **Step 2: Run the protocol tests and verify RED**

Run:

```bash
cargo test -p hop-protocol item::tests::item_title_carrying_a_control_character_is_refused -- --exact
cargo test -p hop-protocol item::tests::item_subtitle_carrying_a_control_character_is_refused -- --exact
```

Expected: both tests fail because the current `de_title` / `de_subtitle` paths enforce length only and accept the control-bearing strings.

- [ ] **Step 3: Add the validating public newtypes and route `Item` through them**

In `content.rs`, implement the two explicit newtypes rather than a public generic wrapper. A private helper may share the identical rule body, but every public constructor and serde error must name its own field:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ItemTitle(String);

impl<'de> Deserialize<'de> for ItemTitle {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(deserializer, Self::FIELD, MAX_TITLE, Self::new)
    }
}

impl ItemTitle {
    pub(crate) const FIELD: &'static str = "Item.title";

    pub fn new(value: impl Into<String>) -> Result<Self, ContentError> {
        let value = value.into();
        check_len(Self::FIELD, MAX_TITLE, value.len())?;
        if let Some(refused) = value.chars().find(|c| c.is_control()) {
            return Err(ContentError::ForbiddenChar {
                field: Self::FIELD,
                codepoint: refused as u32,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str { &self.0 }
    pub fn into_string(self) -> String { self.0 }
}
```

Implement `ItemSubtitle` with the same shape, `FIELD = "Item.subtitle"`, and `MAX_SUBTITLE`. Document that these are single-line display fields, that empty is preserved, that Unicode outside `Cc` is not filtered, and that the wire form remains a string.

In `item.rs`, change the fields to:

```rust
pub title: ItemTitle,
#[serde(default)]
pub subtitle: Option<ItemSubtitle>,
```

Remove `limits::de_title` and `limits::de_subtitle`; update `limits.rs` comments/tests so the newtypes, not duplicate field deserializers, own both bounds. Update `content.rs`'s module-level one-gate documentation and its named-test consistency list.

- [ ] **Step 4: Run focused protocol tests and verify GREEN**

Run:

```bash
cargo test -p hop-protocol content::tests
cargo test -p hop-protocol item::tests
cargo test -p hop-protocol limits::tests
cargo test -p hop-protocol wire::tests
```

Expected: all pass, including the new control-character, bound-order, field-name, null/absence, and unchanged-wire-shape tests.

- [ ] **Step 5: Write failing tests for in-process builder sanitization**

In `apps.rs`, add a test that passes a desktop-entry `Name` containing a control character through the real parse/build path and proves the entry survives with the character removed:

```rust
#[test]
fn build_entry_removes_control_characters_from_the_title_without_dropping_the_app() {
    let source = format!("[Desktop Entry]\nName=Before{}After\nExec=x\n", '\u{0085}');
    let parsed = parsed(&source);
    assert!(parsed.title.chars().any(char::is_control));
    let entry = build_entry("x".to_string(), parsed).expect("the app must survive");
    assert_eq!(entry.item.title.as_str(), "BeforeAfter");
}
```

The raw C1 control is deliberate: the current desktop-entry parser does not resolve `Name=` string escapes, so a literal `\\n` would be inert text and would not reproduce the defect.

In `calculator.rs`, add a test with the evaluator-accepted tab whitespace:

```rust
#[test]
fn build_item_removes_control_whitespace_from_its_display_title() {
    let item = build_item("2+\t2").expect("fasteval accepts tab as whitespace");
    assert_eq!(item.title.as_str(), "2+2 = 4");
    assert!(!item.title.as_str().chars().any(char::is_control));
}
```

Extend one of the builder tests with a `hop_core::sanitize::BIDI_CONTROLS` character such as U+202E and assert it is removed too. This is required by `CONTEXT.md`'s canonical definition of **Sanitize**, even though the protocol newtypes themselves deliberately refuse only `char::is_control()`.

- [ ] **Step 6: Run builder tests and verify RED**

Run the exact new test names with:

```bash
cargo test -p hopd apps::tests::build_entry_removes_control_characters_from_the_title_without_dropping_the_app -- --exact
cargo test -p hopd calculator::item_tests -- --nocapture
```

Expected before sanitization: the apps assertion observes the control-bearing title (or construction fails once the newtype is wired), and the calculator test observes or attempts to construct a control-bearing title.

- [ ] **Step 7: Sanitize production builder inputs, then construct the newtypes**

Refactor `hop-core/src/sanitize.rs` so one reusable function performs the workspace's canonical bounded single-line sanitization: strip `char::is_control()` and `BIDI_CONTROLS`, then truncate to a caller-supplied byte maximum at a UTF-8 boundary. Keep `sanitize_provider_message` as the existing public behavior by delegating to that implementation with `MAX_PROVIDER_MESSAGE`; do not duplicate the filter/truncation logic in `hopd`.

In `apps::build_entry`, sanitize `parsed.title` through that function at `MAX_TITLE` before `ItemTitle::new`; retain the entry even when sanitization changes the title. Keep icon fallback, id failure, and haystack behavior unchanged. In `calculator::build_item`, sanitize the formatted display title through the same function at `MAX_TITLE` before `ItemTitle::new`. Constant in-process titles/subtitles in `source.rs` and other production helpers must use `ItemTitle::new` / `ItemSubtitle::new` with a construction-invariant `expect`, never `unwrap`.

- [ ] **Step 8: Migrate workspace consumers without weakening the new boundary**

Replace every direct `String` construction of an `Item` display field in the listed source and test files with the fallible constructors. Use `.as_str()` at read sites for fuzzy matching, lowercasing, formatting inputs, equality assertions, sorting, and serialization expectations. Use `Option<ItemSubtitle>` mappings such as `subtitle.map(|value| ItemSubtitle::new(value).expect("valid test fixture"))` rather than adding infallible conversion traits.

Run compiler-guided migration until this passes:

```bash
cargo check --workspace --all-targets --all-features
```

Expected: pass with no bypass conversion, `Deref`, or raw-string field construction added.

- [ ] **Step 9: Remove redundant core checks and repair current documentation**

In `pipeline.rs`, delete the `MAX_TITLE` / `MAX_SUBTITLE` imports, per-item length branches, and boundary tests that manufacture states the newtypes make unrepresentable. Rewrite `FailedCheck::FieldTooLong`, `CheckedItems`, and check-order comments so they name only action label and action count. Preserve the manifest-check ordering and action count-before-label scan.

Update current explanatory docs in `CONTEXT.md`, `limits.rs`, `wire.rs`, `source.rs`, and `docs/security/2026-08-02-m2-socket-boundary-threat-model.md`: title/subtitle are now validating newtypes for every construction path; provider-side sanitization supplies valid display text; `CheckedItems` no longer duplicates their checks. In `CONTEXT.md`, add `ItemTitle` and `ItemSubtitle` to **Validating newtype** and narrow **Rejection**'s `FieldTooLong` description to the fields it can still report. Do not edit historical plan files.

- [ ] **Step 10: Run focused and full verification**

Run, in this order:

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --release -p hop-core --test latency -- --ignored --test-threads=1
cargo deny check
```

Expected: every command exits 0 with no warnings. Review `git diff --check` and `git diff --stat`; confirm `Cargo.toml` and `Cargo.lock` did not gain dependencies and `API_VERSION` did not change.

- [ ] **Step 11: Self-review and commit**

Check the diff against every Global Constraint and against issue #110's owner comment. Confirm tests would fail if the constructors stopped scanning controls or if apps/calculator stopped sanitizing. Then commit all implementation, tests, documentation, and this plan with a concise non-AI-attributed message such as:

```bash
git add crates docs/security docs/superpowers/plans/2026-08-18-issue-110-item-display-field-newtypes.md
git commit -m "fix(protocol): validate item display fields"
```
