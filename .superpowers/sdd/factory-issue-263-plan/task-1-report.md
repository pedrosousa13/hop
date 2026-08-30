# Task 1 Report — Issue #263

## Files changed
- `apps/hop-gtk/src/ui/row.rs`
- `apps/hop-gtk/tests/view_tree_renderer.rs`

## What changed
- Added a build-time `(ss)` placeholder target to each freshly built row action button, while keeping the fixed `row.run-action` action name installed at build.
- Kept bind-time target overwrites unchanged, so recycling semantics stay the same.
- Updated the nearby action-button rationale comment to explain why the name is fixed at build and only the target is overwritten during bind.
- Added a focused acceptance assertion that a never-bound row already has the fixed action name and a non-null `(ss)` target before any bind runs.

## RED
Command:
```bash
cargo test -p hop-gtk --test view_tree_renderer setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget -- --exact --nocapture
```
Observed output:
```text
error[E0425]: cannot find value `actions_wrapper` in this scope
   --> apps/hop-gtk/src/ui/row.rs:929:9
    |
929 |         actions_wrapper.append(&button);
    |         ^^^^^^^^^^^^^^^ not found in this scope
...
error[E0425]: cannot find value `actions_wrapper` in this scope
   --> apps/hop-gtk/src/ui/row.rs:967:23
    |
967 |     container.append(&actions_wrapper);
    |                       ^^^^^^^^^^^^^^^ not found in this scope
error: could not compile `hop-gtk` (lib) due to 3 previous errors
```

## GREEN
Command:
```bash
cargo test -p hop-gtk --test view_tree_renderer setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget -- --exact --nocapture
```
Observed output:
```text
running 1 test
test setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.35s
```

## Recycling regression result
- Covered by the same focused `view_tree_renderer` regression above; the existing 1→0→2→3→1 action-icon recycling assertions stayed green.

## Runtime smoke
Command:
```bash
cargo test -p hop-gtk --test headless_smoke captures_the_empty_state_and_a_results_state_headless -- --exact --nocapture
```
Observed output:
```text
running 1 test
test captures_the_empty_state_and_a_results_state_headless ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.77s
```
- The smoke path completed without surfacing any `actionhelper` warning in stderr.

## Commit
- Pending at report write time.

## Concerns
- The recorded RED output is a compile failure from the temporary pre-fix revert, not the final assertion failure form the acceptance text ideally describes.
