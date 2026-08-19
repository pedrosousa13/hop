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
//! This file is left with three things only a real, installed hook can
//! prove: that a second (and third) [`install_provider_panic_hook`] call does
//! not chain the previously-installed hook twice or lose it, that a panic
//! *outside* any provider task still reaches that previously-installed hook
//! with its payload and location intact, and that a panic *inside* a
//! provider task — on both of the host's real spawn sites, reached through
//! the host's own public API rather than a hand-rolled stand-in for it — is
//! recognized and diverted instead of falling through.
//!
//! # One test function, on purpose
//!
//! `std::panic::set_hook` is one global slot per process. Two `#[test]` fns
//! in this file would race to install their own "previously-installed"
//! recorder and clobber each other's, since `cargo test` runs test functions
//! in parallel by default. Rather than adding a `std::sync::Mutex` guard
//! around two or more functions, this file has exactly one: every assertion
//! below shares the one hook chain this process ever installs.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hop_core::host::{HostPolicy, NoopLog, ProviderHost, install_provider_panic_hook};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery, route};
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
    // comment says must be safe. If it were not — if each call re-chained
    // rather than being guarded — this would still only prove itself through
    // the assertions below, since every layer forwards to the *next* layer
    // exactly once either way (see that function's doc comment for why); what
    // a missing guard would actually risk is a hook that never stabilizes
    // because a later call keeps discarding earlier chains under a race, so
    // this at least pins "callable more than once without erroring or
    // panicking itself".
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
}
