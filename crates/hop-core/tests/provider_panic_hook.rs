//! Process-wide behavior of [`hop_core::host::install_provider_panic_hook`]
//! (issue #104) — the half of the fix that cannot be pinned in `host.rs`'s
//! own `#[cfg(test)]` module, because `std::panic::set_hook` is process-wide
//! and this crate's unit test binary runs every other test in the same
//! process. An integration test file gets its own process, so installing a
//! hook here does not disturb anything else in the suite.
//!
//! What lives in `host.rs` instead, unit-tested without ever touching
//! `set_hook`: `format_provider_panic` (the sanitized, bounded line itself —
//! control characters, bidi overrides, the 256-byte bound, `&str` and
//! `String` payloads, a non-string payload's fallback, and the location) and
//! `CURRENT_PROVIDER_ID` (that both of the host's spawn sites scope it around
//! the provider's own future, and that it reads back absent everywhere
//! else). `std::panic::PanicHookInfo` cannot be constructed outside `std`,
//! which is exactly why those two pieces were split out as pure, directly
//! testable units in the first place — see `host.rs`'s own doc comments for
//! the detail this file does not repeat.
//!
//! This file proves five things only a real, installed hook — reached
//! through the host's own public API, never a hand-rolled stand-in for it —
//! can prove:
//!
//! - that a second (and third) [`install_provider_panic_hook`] call does not
//!   chain the previously-installed hook twice or lose it;
//! - that a panic *outside* any provider task still reaches that
//!   previously-installed hook with its payload and location intact;
//! - that a panic *inside* a provider task, on both of the host's real spawn
//!   sites, is recognized and diverted instead of falling through;
//! - that re-installing after something else has taken over the top of the
//!   chain is a genuine no-op rather than merely one that looks like one
//!   (acceptance criterion 5 — see "Proving the `Once` guard actually does
//!   something" below); and
//! - that the composed path — hook installed, reached through a real panic,
//!   written through a real `eprintln!` — lands sanitized, bounded,
//!   located text on the process's *actual* stderr bytes, not just on what
//!   `format_provider_panic` returns in isolation (acceptance criteria 1 and
//!   4 — see the subprocess pair at the bottom of this file).
//!
//! # One test function for everything that shares this process's hook slot
//!
//! `std::panic::set_hook` is one global slot per process. Two `#[test]` fns
//! that each installed their own "previously-installed" recorder would race
//! to clobber each other's, since `cargo test` runs test functions in
//! parallel by default. Rather than adding a `std::sync::Mutex` guard around
//! two or more functions, every assertion that depends on this process's own
//! hook chain — including the criterion-5 sequence below — lives in the one
//! function that owns it.
//!
//! The subprocess pair at the bottom of this file is the deliberate
//! exception: [`panic_hook_composed_stderr_is_sanitized_and_bounded`] never
//! touches *this* process's hook slot at all — it only spawns a child
//! process and reads the bytes that child wrote — so it can run concurrently
//! with the function above without racing it.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hop_core::host::{HostPolicy, NoopLog, ProviderHost, install_provider_panic_hook};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery, route};
use hop_core::sanitize::{BIDI_CONTROLS, MAX_PROVIDER_MESSAGE};
use hop_protocol::{ActionId, ExecOutcome, Item, ItemId, Kind};

/// One recorded panic: its payload text, and its `(file, line)` location if
/// it reported one.
type RecordedPanic = (String, Option<(String, u32)>);

/// Every panic the hook installed *before* this test's own
/// [`install_provider_panic_hook`] call was told about, in arrival order —
/// this is the "previously-installed hook" the acceptance criteria are
/// about, standing in for whatever real hook a consumer had running before
/// it linked `hop-core` (the default hook, in this test's own case).
static RECORDED: Mutex<Vec<RecordedPanic>> = Mutex::new(Vec::new());

fn payload_text(payload: &dyn std::any::Any) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string>".to_string()
    }
}

/// The fixed, distinctive payload [`RECORDED`]'s entry for the non-provider
/// panic below is checked against.
const NON_PROVIDER_PANIC_MESSAGE: &str = "distinctive-non-provider-panic-4f2c9a";

/// A provider whose every method panics with attacker-shaped text: a
/// terminal escape, a right-to-left override, and a payload far past
/// [`hop_core::sanitize::MAX_PROVIDER_MESSAGE`]. Both `query` and `execute`
/// panic, so both of the host's spawn sites
/// ([`ProviderHost::run_one`] and [`ProviderHost::execute`]) get a real
/// panic through their real, production code path — not a stand-in for it.
struct HostileProvider;

impl Provider for HostileProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: "hostile-integration",
            kinds: vec![Kind::App],
            modes: vec![Mode::All],
            min_term_len: 0,
            budget: Duration::from_millis(200),
            ids_are_safe_to_persist_in_the_clear: false,
        }
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        panic!("\u{1b}[31m\u{202e}query panic {}", "x".repeat(4000));
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        panic!("\u{1b}[31m\u{202e}execute panic {}", "x".repeat(4000));
    }
}

#[tokio::test]
async fn install_provider_panic_hook_composes_and_recognizes_provider_panics() {
    // The stand-in "previously-installed hook": recorded, never printed, so
    // the test asserts on data rather than parsing captured stderr.
    std::panic::set_hook(Box::new(|info| {
        let text = payload_text(info.payload());
        let location = info.location().map(|l| (l.file().to_string(), l.line()));
        RECORDED.lock().unwrap().push((text, location));
    }));

    // Called three times, deliberately: a test binary calling this more than
    // once is exactly the case `install_provider_panic_hook`'s own doc
    // comment says must be safe. The count-based assertions just below this
    // (and the query/execute ones further down) cannot by themselves tell a
    // guarded install from an unguarded one — every chained layer forwards
    // to the *next* layer exactly once either way, so a missing `Once` guard
    // would still leave those counts at exactly one. What those assertions
    // do pin is "callable more than once without erroring, panicking, or
    // losing a panic" — a real requirement, just not acceptance criterion 5
    // by itself. The sequence in "Proving the `Once` guard actually does
    // something", after this test's other assertions, is what pins that
    // criterion: it only runs once this function is done needing its own
    // hook to be the one on top.
    install_provider_panic_hook();
    install_provider_panic_hook();
    install_provider_panic_hook();

    // --- A panic outside any provider task reaches the previously-installed
    // hook unchanged: same payload text, same panic location. ---
    //
    // The panic is a literal `panic!()` written directly in the closure
    // handed to `catch_unwind`, deliberately not behind a named helper
    // function: `panic!`'s own location capture is always the line it is
    // textually written on, with no `#[track_caller]` needed — but a
    // *caller's* location only propagates through a `#[track_caller]`
    // function when it is called directly by name. Passed indirectly here,
    // through `catch_unwind`'s own generic `FnOnce` dispatch, that
    // propagation does not reach this test's own source line at all; it
    // reports a location inside `core`'s own `FnOnce` plumbing instead. This
    // way sidesteps needing to know the plumbing's shape.
    let panic_line = line!() + 2;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        panic!("{NON_PROVIDER_PANIC_MESSAGE}");
    }));
    assert!(
        result.is_err(),
        "the panic must still unwind past catch_unwind"
    );

    // The lock is taken, cloned out of, and dropped by the end of this `let`
    // statement — never held across an `assert_eq!` — because the
    // previously-installed hook above also locks `RECORDED`: an assertion
    // failure while still holding this guard would panic *inside* that
    // lock's scope, the panic hook chain would run synchronously as part of
    // that very panic (hooks run before unwinding, so the still-live guard
    // has not been dropped yet), and the previously-installed hook's own
    // `RECORDED.lock()` would then deadlock against itself on this same
    // thread — silently, since `std::sync::Mutex` is not reentrant and
    // reports no error for it, just blocks forever.
    let recorded_after_first_party_panic = RECORDED.lock().unwrap().clone();
    assert_eq!(
        recorded_after_first_party_panic.len(),
        1,
        "exactly one non-provider panic must reach the previously-installed hook exactly \
         once, however many times install_provider_panic_hook was called: \
         {recorded_after_first_party_panic:?}"
    );
    let (text, location) = &recorded_after_first_party_panic[0];
    assert_eq!(text, NON_PROVIDER_PANIC_MESSAGE);
    let (file, line) = location
        .as_ref()
        .expect("a location must still be reported");
    assert_eq!(file, file!());
    assert_eq!(*line, panic_line);

    // --- Both host spawn sites are covered: a panic in a provider's query
    // turn, and a panic in its execute turn, are each recognized as a
    // provider panic and diverted — never reaching the previously-installed
    // hook above, whose count must stay exactly where the assertion above
    // left it. ---
    let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
    host.register(HostileProvider).unwrap();
    let host = Arc::new(host);

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    host.spawn_query(Arc::new(route("x")), tx);
    // Draining to close is what proves `run_one` actually finished — and
    // since the installed panic hook runs synchronously on the panicking
    // thread, inside the panicking task's own poll, well before the
    // `JoinError` that ends `run_one`'s wait even exists, the hook has
    // already run by the time this loop's `None` arrives.
    while rx.recv().await.is_some() {}

    // Extracted to its own statement before asserting, for the same reason
    // as `recorded_after_first_party_panic` above: never hold `RECORDED`'s
    // lock across an `assert_eq!`.
    let recorded_after_query_panic = RECORDED.lock().unwrap().len();
    assert_eq!(
        recorded_after_query_panic, 1,
        "a provider panic in its query turn must not reach the previously-installed hook"
    );

    let execute_err = host
        .execute(
            "hostile-integration",
            ItemId::new("app:1").unwrap(),
            ActionId::new("open").unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(execute_err, ProviderError::Failed(_)),
        "a joined panic must still surface as a provider failure, unchanged by this issue: \
         {execute_err:?}"
    );

    let recorded_after_execute_panic = RECORDED.lock().unwrap().len();
    assert_eq!(
        recorded_after_execute_panic, 1,
        "a provider panic in its execute turn must not reach the previously-installed hook \
         either"
    );

    // --- Proving the `Once` guard actually does something (acceptance
    // criterion 5) ---
    //
    // Everything above already calls `install_provider_panic_hook` three
    // times without incident, but as that call site's comment says, nothing
    // above can tell a guarded install from an unguarded one: every chained
    // layer forwards to the next exactly once regardless, so the counts
    // stay pinned at one either way.
    //
    // What does discriminate the guard: replace the top of the chain with a
    // second recorder *after* this crate's hook is already installed, then
    // call `install_provider_panic_hook` again.
    //   - With the guard (today's code), `PROVIDER_PANIC_HOOK_INSTALLED` has
    //     already fired once, above, so this next call is a no-op. The
    //     second recorder — not this crate's hook — stays on top, and is
    //     the one that sees the next provider panic.
    //   - Without the guard, this call would `take_hook` the second
    //     recorder back as "previous" and install a *fresh* copy of this
    //     crate's own hook on top of it. That fresh copy would recognize the
    //     next provider panic via `CURRENT_PROVIDER_ID` same as ever,
    //     divert it, and the second recorder would never see it at all.
    // The two outcomes differ, so asserting the second recorder *does* see
    // the panic pins the guard rather than merely being consistent with it.
    //
    // This has to run last: `set_hook` replaces the process's one global
    // slot, and once the second recorder takes over, the provider-aware
    // hook the assertions above depend on is gone from the chain for good —
    // nothing before this point could still see its own hook respond after
    // it.
    static SECOND_RECORDED: Mutex<Vec<RecordedPanic>> = Mutex::new(Vec::new());
    std::panic::set_hook(Box::new(|info| {
        let text = payload_text(info.payload());
        let location = info.location().map(|l| (l.file().to_string(), l.line()));
        SECOND_RECORDED.lock().unwrap().push((text, location));
    }));

    // A no-op under the guard: `PROVIDER_PANIC_HOOK_INSTALLED` fired once,
    // earlier in this same function, and never fires again.
    install_provider_panic_hook();

    let execute_err_after_replacement = host
        .execute(
            "hostile-integration",
            ItemId::new("app:1").unwrap(),
            ActionId::new("open").unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(execute_err_after_replacement, ProviderError::Failed(_)),
        "a joined panic must still surface as a provider failure no matter which hook is on \
         top of the chain: {execute_err_after_replacement:?}"
    );

    let second_recorded_after_provider_panic = SECOND_RECORDED.lock().unwrap().len();
    assert_eq!(
        second_recorded_after_provider_panic, 1,
        "the hook installed on top of ours must still be the one running after a repeat \
         install_provider_panic_hook call: if the `Once` guard were missing, that call would \
         have re-chained this crate's own hook on top of this recorder, intercepted the panic \
         as a provider's, and this recorder would have seen nothing"
    );
}

// --- The composed path, on a real child process's real stderr (issue #104,
// acceptance criteria 1 and 4) ---
//
// Everything above proves the hook *diverts* a provider panic — the
// previously-installed hook's recorded count stays flat. None of it observes
// what the installed hook actually *writes*: `format_provider_panic` is unit
// tested against constructed inputs in `host.rs`, and the recorder above
// never touches `eprintln!` or a real stream at all. The pair below closes
// that gap by re-executing this binary as a genuinely separate process,
// letting the child hit a real panic through the host's real spawn sites,
// and reading back the actual bytes its installed hook wrote to its actual
// stderr.

/// Env var the parent test sets before re-executing this binary, so this
/// process's own copy of
/// [`provider_panic_hook_writes_sanitized_stderr_to_a_real_child_process`]
/// below knows it was deliberately invoked rather than picked up by an
/// incidental sweep. See that function's doc comment for what an unguarded
/// run would do.
const CHILD_GUARD_ENV_VAR: &str = "HOP_CORE_PROVIDER_PANIC_HOOK_STDERR_CHILD";

/// The child test's own name, kept as one constant so
/// [`panic_hook_composed_stderr_is_sanitized_and_bounded`]'s `--exact`
/// filter can never drift out of sync with a rename of the function it
/// targets.
const CHILD_TEST_NAME: &str = "provider_panic_hook_writes_sanitized_stderr_to_a_real_child_process";

/// Installs the hook, drives a real panic through both of the host's spawn
/// sites via the same [`HostileProvider`] the rest of this file uses, and
/// exits — leaving whatever it wrote on this process's real stderr for
/// [`panic_hook_composed_stderr_is_sanitized_and_bounded`] to read back.
///
/// `#[ignore]`d and guarded by [`CHILD_GUARD_ENV_VAR`]: this function calls
/// `std::panic::set_hook`, the same process-wide call the primary test above
/// makes, so it must never run inside a shared test process — only inside a
/// freshly re-exec'd child the parent test below spawns for exactly this
/// purpose. Without the env var guard, a developer's own `cargo test --
/// --ignored` — run to sweep every ignored test in the workspace, with no
/// idea this one exists — would still execute it, install a hook, and print
/// raw diagnostic text to whatever terminal ran the command: surprising
/// output for a command that looks like it is just running tests. With the
/// guard, that same sweep hits the early return below and does nothing.
#[tokio::test]
#[ignore = "driven only by panic_hook_composed_stderr_is_sanitized_and_bounded, which \
            re-execs this binary as a child process and reads its stderr"]
async fn provider_panic_hook_writes_sanitized_stderr_to_a_real_child_process() {
    if std::env::var_os(CHILD_GUARD_ENV_VAR).is_none() {
        return;
    }

    install_provider_panic_hook();

    let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
    host.register(HostileProvider).unwrap();
    let host = Arc::new(host);

    // The query turn: `ProviderHost::run_one`'s spawn site.
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    host.spawn_query(Arc::new(route("x")), tx);
    while rx.recv().await.is_some() {}

    // The execute turn: `ProviderHost::execute`'s spawn site.
    let _ = host
        .execute(
            "hostile-integration",
            ItemId::new("app:1").unwrap(),
            ActionId::new("open").unwrap(),
        )
        .await;
}

/// Re-execs this test binary to drive
/// [`provider_panic_hook_writes_sanitized_stderr_to_a_real_child_process`] in
/// a genuinely separate process, then inspects the real bytes that landed on
/// its stderr. This is the only way to pin acceptance criteria 1 and 4
/// against the *composed* path — an installed hook, reached through a real
/// panic, writing through a real `eprintln!` — rather than against
/// `format_provider_panic` in isolation, which `host.rs`'s own unit tests
/// already cover.
///
/// Never touches this process's own panic hook — it only spawns a child and
/// reads what that child wrote — so unlike the primary test above, it does
/// not need to be folded into a single function to avoid racing it.
#[test]
fn panic_hook_composed_stderr_is_sanitized_and_bounded() {
    let exe = std::env::current_exe()
        .expect("the test binary's own path must be available to re-exec itself");

    // `--nocapture` matters here for a reason specific to this test: the
    // installed hook's `eprintln!` runs inside the panicking task's poll, on
    // a tokio worker thread rather than the child test's own thread, and the
    // test harness only captures output on the thread it knows about.
    // Without `--nocapture` that output's fate is not guaranteed by
    // anything this test can rely on; with it, the child writes directly to
    // its real stderr regardless of which thread the hook runs on.
    let output = std::process::Command::new(&exe)
        .arg(CHILD_TEST_NAME)
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(CHILD_GUARD_ENV_VAR, "1")
        .output()
        .unwrap_or_else(|err| panic!("failed to start the child process at {exe:?}: {err}"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // A subprocess test that passes because the child never ran is worse
    // than no test at all, so this fails loudly — with both captured
    // streams and the exit status in the message — rather than letting an
    // empty or unrelated capture read as a silent pass.
    assert!(
        stderr.contains("provider `hostile-integration` panicked"),
        "the child process must have actually run and hit the installed hook; an empty or \
         unrelated capture almost always means the child test never ran (wrong `--exact` \
         name, the guard env var not observed, or the process exiting before the panic) \
         rather than a genuine pass.\nchild exit status: {:?}\nchild stdout:\n{stdout}\nchild \
         stderr:\n{stderr}",
        output.status,
    );

    // --- Criterion 1: none of the attacker-shaped characters survive ---
    assert!(
        !stderr.contains('\u{1b}'),
        "a raw ESC must never reach real stderr: {stderr:?}"
    );
    for c in BIDI_CONTROLS {
        assert!(
            !stderr.contains(*c),
            "{c:?} must never reach real stderr: {stderr:?}"
        );
    }

    // Both of the host's spawn sites, checked independently: a query-turn
    // panic and an execute-turn panic each produce their own line.
    for (turn, marker) in [("query", "query panic"), ("execute", "execute panic")] {
        let line = stderr
            .lines()
            .find(|line| line.contains(marker))
            .unwrap_or_else(|| {
                panic!("no line for the {turn} turn's panic found in stderr:\n{stderr}")
            });

        assert!(
            line.contains("provider `hostile-integration` panicked"),
            "the {turn} turn's line must name the provider: {line:?}"
        );

        // `format_provider_panic` writes `provider `ID` panicked at
        // FILE:LINE: MESSAGE` — a single " at " and, immediately after it,
        // `FILE:LINE` followed by the literal `": "` that starts the
        // message. Neither a Unix path nor a decimal line number can itself
        // contain `": "`, so its first occurrence after " at " is
        // unambiguously that separator.
        let after_at = line
            .split(" at ")
            .nth(1)
            .unwrap_or_else(|| panic!("the {turn} turn's line has no location fragment: {line:?}"));
        let separator = after_at.find(": ").unwrap_or_else(|| {
            panic!("the {turn} turn's line has no location/message separator: {line:?}")
        });
        let location = &after_at[..separator];
        let message = &after_at[separator + ": ".len()..];

        // --- Criterion 4: the panic location (file and line) still appears
        // ---
        assert!(
            location.contains("provider_panic_hook.rs:"),
            "the {turn} turn's location must name this test file and a line number: {line:?}"
        );
        assert!(
            location
                .rsplit(':')
                .next()
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())),
            "the {turn} turn's location must end in a decimal line number: {location:?}"
        );

        // --- Criterion 1, the bound: the provider-controlled portion of the
        // message is truncated to `MAX_PROVIDER_MESSAGE`, not the ~4000
        // bytes `HostileProvider` actually panicked with ---
        assert_eq!(
            message.len(),
            MAX_PROVIDER_MESSAGE,
            "the {turn} turn's provider-controlled text must be truncated to exactly the \
             documented bound: {message:?}"
        );

        // The message is "[31m{marker} " (ESC and the bidi override
        // stripped by sanitization, the rest of that ANSI-looking prefix
        // left as ordinary text) followed by a run of `x` characters,
        // truncated to `MAX_PROVIDER_MESSAGE` bytes total — so the run of
        // `x`s is the bound *minus* that prefix, not the bound itself, and
        // very far from the ~4000 `x`s `HostileProvider` actually panicked
        // with.
        let prefix = format!("[31m{marker} ");
        let expected_x_run = MAX_PROVIDER_MESSAGE - prefix.len();
        let x_run = message.chars().rev().take_while(|&c| c == 'x').count();
        assert_eq!(
            x_run, expected_x_run,
            "the run of provider-controlled `x`s must be bounded well below the ~4000 the \
             provider actually panicked with: {message:?}"
        );
    }
}
