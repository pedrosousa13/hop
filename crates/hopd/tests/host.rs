//! The provider host over a real socket: what a client actually receives when
//! a provider hangs, panics, fails with hostile text, or answers honestly
//! alongside one that does not.
//!
//! `hop-core`'s own tests cover the host's units. These cover the daemon: the
//! frames that reach a peer, which is the only place "one failing provider
//! never empties a frame for the others" can actually be observed.
//!
//! Plain `#[test]` functions driving a blocking `std::os::unix::net::UnixStream`
//! client, matching `lifecycle.rs`'s shape — there is no `#[tokio::test]` or
//! async client in this crate's test suites, and inventing one here would be
//! a second harness where `tests/common` exists to prevent exactly that.

#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{
    RecordingLog, Script, ScriptedProvider, TestDaemon, hello, recv, scripted_item, send,
    start_daemon,
};
use hop_core::host::{HostPolicy, ProviderHost};
use hop_core::provider::ProviderManifest;
use hop_core::router::Mode;
use hop_protocol::{ClientMsg, DaemonMsg, Kind, QueryText};
use hopd::source::HostSource;

/// A daemon serving a host with `providers` registered under the default
/// [`HostPolicy`], plus the log the test reads back.
fn daemon_with(providers: Vec<ScriptedProvider>, log: Arc<RecordingLog>) -> TestDaemon {
    daemon_with_policy(HostPolicy::default(), providers, log)
}

/// [`daemon_with`], but with the host's own policy under the test's control.
///
/// Needed by the no-slowest-provider-gate test below: [`HostPolicy::default`]
/// clamps every provider's budget to
/// [`MAX_PROVIDER_BUDGET`](hop_core::host::MAX_PROVIDER_BUDGET) (50 ms), so a
/// provider asking for a wider budget to build a generous timing margin needs
/// a policy that actually allows it — otherwise the clamp silently discards
/// the margin the test asked for.
fn daemon_with_policy(
    policy: HostPolicy,
    providers: Vec<ScriptedProvider>,
    log: Arc<RecordingLog>,
) -> TestDaemon {
    let mut host = ProviderHost::new(policy, log);
    for provider in providers {
        host.register(provider).unwrap();
    }
    let source = HostSource::new(Arc::new(host));
    start_daemon(source)
}

/// Connects to `daemon`, completes the handshake, and sets a read timeout.
///
/// `common::recv` has no timeout of its own — a daemon that stops responding
/// would otherwise make a test *hang* rather than fail, which is intolerable
/// specifically in this file:
/// `a_hanging_provider_is_cut_off_and_the_query_still_terminates` exists to
/// catch exactly the daemon bug that would produce a hang here, and a hang is
/// a CI timeout with no diagnostic where a failed assertion names the
/// problem. Two seconds is generous relative to the providers' 20 ms budgets
/// (the host has ample room to answer well inside it) and far below any CI
/// job's patience, so it never fires on a passing run.
fn connect(daemon: &TestDaemon) -> UnixStream {
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    hello(&mut stream);
    stream
}

#[test]
fn a_panicking_provider_does_not_empty_the_frame_for_the_others() {
    // Spec §9's per-provider isolation rule, observed where it matters: at the
    // client. Before issue #29 this panic would have unwound through the
    // connection driver and taken the connection with it.
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon_with(
        vec![
            ScriptedProvider::new("panicking", vec![Kind::App], Script::Panic),
            ScriptedProvider::new(
                "apps",
                vec![Kind::App],
                Script::Answer(vec![scripted_item(
                    "apps",
                    Kind::App,
                    "app:firefox",
                    "Firefox",
                )]),
            ),
        ],
        log.clone(),
    );
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("firefox").unwrap(),
        },
    );

    let mut items = Vec::new();
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id: 1,
                items: batch,
                ..
            } => items.extend(batch),
            DaemonMsg::QueryDone { query_id: 1 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(items.len(), 1, "the honest provider's item still arrives");
    assert_eq!(items[0].title, "Firefox");
    assert!(
        log.lines()
            .iter()
            .any(|l| l.starts_with("failed panicking")),
        "and the panic is reported: {:?}",
        log.lines()
    );
}

#[test]
fn a_hanging_provider_is_cut_off_and_the_query_still_terminates() {
    // #28's criterion at the socket: the exchange reaches `QueryDone` without
    // the provider ever cooperating, and without the client waiting on it.
    // The 2s read timeout `connect` sets is what turns a regression here into
    // a named assertion failure instead of a hang: if the host stopped
    // enforcing the budget, this test fails loudly rather than never
    // returning.
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon_with(
        vec![ScriptedProvider::new(
            "hanging",
            vec![Kind::App],
            Script::Hang,
        )],
        log.clone(),
    );
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 7,
            text: QueryText::new("anything").unwrap(),
        },
    );

    let done = recv(&mut stream);
    assert_eq!(done, DaemonMsg::QueryDone { query_id: 7 });
    assert!(log.lines().iter().any(|l| l == "budget-miss hanging"));
}

#[test]
fn a_providers_hostile_error_text_never_reaches_the_client() {
    // #34 at the boundary it is about: the text is bound for a UI label, so
    // what matters is that no frame carries it. This slice reports provider
    // failures on the log seam and sends no error frame for one, so the
    // assertion is that the exchange completes carrying no provider text at
    // all — and that what the seam recorded is bounded and stripped.
    let log = Arc::new(RecordingLog::default());
    let hostile = format!("\u{1b}[31m\u{202e}{}", "x".repeat(4096));
    let daemon = daemon_with(
        vec![ScriptedProvider::new(
            "nasty",
            vec![Kind::App],
            Script::Fail(hostile),
        )],
        log.clone(),
    );
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 2,
            text: QueryText::new("anything").unwrap(),
        },
    );

    let done = recv(&mut stream);
    assert!(
        matches!(done, DaemonMsg::QueryDone { query_id: 2 }),
        "a provider failure ends the exchange cleanly; it does not send the \
         provider's words to the client, got {done:?}"
    );

    let lines = log.lines();
    let failure = lines
        .iter()
        .find(|l| l.starts_with("failed nasty"))
        .expect("the failure was recorded");
    assert!(!failure.contains('\u{1b}'));
    assert!(!failure.contains('\u{202e}'));
    assert!(
        failure.len() < 512,
        "the 4 KB message was bounded before it was recorded: {}",
        failure.len()
    );
}

#[test]
fn a_fast_providers_items_arrive_before_a_slow_providers_budget_expires() {
    // "No slowest-provider gate", spec §3, observed as frame timing. There is
    // no `tokio::time::timeout` available against a blocking client, so this
    // measures wall-clock elapsed around the blocking `recv` call instead.
    //
    // The hanging provider is given a 500 ms budget here — well above the
    // fixture's 20 ms default — specifically to widen the gap this
    // assertion relies on, rather than to model anything realistic. This
    // binary's four tests build their own multi-thread runtimes and can run
    // concurrently with each other and with `lifecycle.rs`'s and
    // `socket.rs`'s tests, so on a contended CI runner a bound with only a
    // few milliseconds of margin (an earlier version used 15 ms against a
    // 20 ms budget) is a real flake risk, not a theoretical one. A gated
    // implementation cannot produce *any* frame before the hanging
    // provider's budget expires — it never finishes on its own — so with a
    // 500 ms budget a gated implementation needs 500 ms while the ungated
    // path (the fast provider's `Script::Answer` future resolves with no
    // await at all) still takes microseconds. The 100 ms bound below sits
    // in that gap: a ~5x margin over the fast path's realistic latency, and
    // still a fifth of the hanging provider's budget, so it discriminates
    // the two shapes reliably under contention instead of passing either
    // way. Do not tighten these numbers back down; they are chosen for
    // margin, not to measure anything.
    //
    // 500 ms is above `HostPolicy::default`'s 50 ms clamp
    // (`MAX_PROVIDER_BUDGET`), so this test builds its host with a raised
    // `max_budget` via `daemon_with_policy` — registering the hanging
    // provider with a 500 ms manifest budget under the *default* policy would
    // silently clamp it back down to 50 ms and quietly narrow the margin this
    // test exists to widen.
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon_with_policy(
        HostPolicy {
            max_budget: Duration::from_millis(500),
            ..HostPolicy::default()
        },
        vec![
            ScriptedProvider::new("hanging", vec![Kind::App], Script::Hang).with_manifest(
                ProviderManifest {
                    id: "hanging",
                    kinds: vec![Kind::App],
                    modes: vec![Mode::All],
                    min_term_len: 0,
                    budget: Duration::from_millis(500),
                },
            ),
            ScriptedProvider::new(
                "apps",
                vec![Kind::App],
                Script::Answer(vec![scripted_item(
                    "apps",
                    Kind::App,
                    "app:firefox",
                    "Firefox",
                )]),
            ),
        ],
        log,
    );
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 3,
            text: QueryText::new("firefox").unwrap(),
        },
    );

    let started = Instant::now();
    let frame = recv(&mut stream);
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(100),
        "the fast provider's frame must not wait on the hanging one, took {elapsed:?}"
    );
    match frame {
        DaemonMsg::Results { items, partial, .. } => {
            assert!(partial);
            assert_eq!(items[0].title, "Firefox");
        }
        other => panic!("expected a results frame first, got {other:?}"),
    }
}
