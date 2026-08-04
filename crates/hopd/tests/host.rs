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
use hop_protocol::{ClientMsg, DaemonMsg, Kind, QueryText};
use hopd::source::HostSource;

/// A daemon serving a host with `providers` registered, plus the log the test
/// reads back.
fn daemon_with(providers: Vec<ScriptedProvider>, log: Arc<RecordingLog>) -> TestDaemon {
    let mut host = ProviderHost::new(HostPolicy::default(), log);
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
    // The bound is 15 ms against a 20 ms provider budget: a correctly
    // isolated host sends the fast provider's frame as soon as that
    // provider's (essentially instant, no-await) future resolves — a matter
    // of microseconds in practice — while an implementation that gated the
    // frame on every provider finishing could not beat this bound, because
    // the hanging provider never finishes on its own and is only cut off at
    // its 20 ms budget. 15 ms therefore discriminates the two: comfortably
    // above the fast path's real latency, comfortably below the point a gated
    // implementation could first respond.
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon_with(
        vec![
            ScriptedProvider::new("hanging", vec![Kind::App], Script::Hang),
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
        elapsed < Duration::from_millis(15),
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
