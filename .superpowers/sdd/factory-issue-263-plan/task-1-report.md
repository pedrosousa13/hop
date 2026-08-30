# Task 1 Report — Issue #263

## Files changed
- `apps/hop-gtk/src/ui/row.rs`
- `apps/hop-gtk/tests/view_tree_renderer.rs`
- `.superpowers/sdd/factory-issue-263-plan/task-1-report.md`

## What changed
- Added a build-time `(ss)` placeholder target to each freshly built row action button, while keeping the fixed `row.run-action` action name installed at build.
- Kept bind-time target overwrites unchanged, so recycling semantics stay the same.
- Updated the nearby action-button rationale comment to explain why the name is fixed at build and only the target is overwritten during bind.
- Added a focused acceptance assertion that a never-bound row already has the fixed action name and a non-null `(ss)` target before any bind runs.
- In fix round 1, restored constructor-time hiding for empty action buttons and changed both action-button clear paths to restore typed placeholder targets instead of null targets; updated the recycling assertions to prove hidden/unbound buttons remain hidden while keeping placeholder-typed targets and dropping stale ids.

## RED
Command:
```bash
cargo test -p hop-gtk --test view_tree_renderer setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget -- --exact --nocapture
```
Observed output:
```text
thread 'setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget' ... panicked at apps/hop-gtk/tests/view_tree_renderer.rs:261:10:
a freshly built action button must carry a non-null placeholder target
...
test setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget ... FAILED
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
- Covered by the same focused `view_tree_renderer` regression above; the existing 1→0→2→3→1 action-icon recycling assertions stayed green, now also checking that hidden/unbound action icons and the overflow chevron keep typed placeholder targets instead of null.

## Runtime smoke
Command:
```bash
cargo test -p hop-gtk --test headless_smoke captures_the_empty_state_and_a_results_state_headless -- --exact --nocapture
```
Observed output:
```text
running 1 test
test captures_the_empty_state_and_a_results_state_headless ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.84s
```
- The smoke path completed without surfacing any `actionhelper` warning in stderr.

## Commit
- `999332dd93abbcebcb95aa71605eadb058a05dea`

## Checks not run by implementer
- Formatter: not run here; the controller owns the landing-gate formatting pass.
- Lint: not run here; the controller owns the landing-gate lint pass.
- Project-wide build/test suite: not run here; the controller owns the single landing-gate run.

## Concerns
- None beyond the normal headless smoke caveat: the test proves the startup log did not emit `actionhelper`, and it does not attempt to launch or trigger any external action handler.
