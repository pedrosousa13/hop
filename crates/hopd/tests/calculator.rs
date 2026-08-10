//! The calculator provider through the daemon, over a real socket:
//! acceptance criterion 7 on issue #58. `calculator.rs`'s own unit tests
//! cover evaluation, formatting and the `Provider` impl directly; this file
//! covers what a client receives over the wire — including `execute`,
//! which issue #59 wires all the way from `ClientMsg::Execute` to
//! `Provider::execute`, and which this provider is the first in the tree to
//! answer with `ExecOutcome::CopyText` rather than `ExecOutcome::Done`.
//!
//! Plain `#[test]` functions over a blocking `std::os::unix::net::UnixStream`
//! client, matching `apps.rs`'s, `host.rs`'s and `lifecycle.rs`'s shape — no
//! second harness invented here.

#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use common::{Script, ScriptedProvider, hello, recv, scripted_item, send, start_daemon};
use hop_core::host::{NoopLog, ProviderHost};
use hop_protocol::{ClientMsg, CopyText, DaemonMsg, ExecOutcome, Item, Kind, Mode, QueryText};
use hopd::calculator::CalculatorProvider;
use hopd::source::HostSource;

fn calculator_daemon() -> common::TestDaemon {
    let mut host = ProviderHost::with_log(Arc::new(NoopLog));
    host.register(CalculatorProvider).unwrap();
    start_daemon(HostSource::new(Arc::new(host)))
}

fn connect(daemon: &common::TestDaemon) -> UnixStream {
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    hello(&mut stream);
    stream
}

/// Drives one query to completion, returning the *last* `Results` frame's
/// items — never accumulated across frames. Each `Results` frame is a
/// **full replacement** of the current list, per issue #103's contract
/// (`crates/hopd/src/source.rs`'s own module docs): concatenating batches
/// with `.extend(...)`, the way `tests/apps.rs`'s single-provider suite
/// gets away with (there, only ever one frame is ever sent), would
/// double-count an item across two frames the moment more than one
/// provider is registered, as the augmentation test below does.
fn run_query(stream: &mut UnixStream, id: u64, text: &str) -> Vec<Item> {
    send(
        stream,
        &ClientMsg::Query {
            id,
            text: QueryText::new(text).unwrap(),
        },
    );
    let mut items = Vec::new();
    loop {
        match recv(stream) {
            DaemonMsg::Results {
                query_id,
                items: batch,
                ..
            } if query_id == id => items = batch,
            // #127's routed frame leads every exchange. This helper exists to
            // return items, so the frame is tolerated here; the mode it
            // reports for a math query is asserted in
            // `an_inferred_math_query_reports_calculator_without_exclusivity`.
            DaemonMsg::QueryRouted { query_id, .. } if query_id == id => {}
            DaemonMsg::QueryDone { query_id } if query_id == id => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    items
}

#[test]
fn a_query_over_the_socket_returns_the_calculator_result() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    let items = run_query(&mut stream, 1, "2+2");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "2+2 = 4");
    assert_eq!(
        items[0].provider,
        hop_core::provider::CALCULATOR_PROVIDER_ID
    );
}

#[test]
fn unary_minus_and_percent_are_handled_over_the_socket() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    let minus = run_query(&mut stream, 1, "-5+2");
    assert_eq!(minus.len(), 1);
    assert_eq!(minus[0].title, "-5+2 = -3");

    let percent = run_query(&mut stream, 2, "10%3");
    assert_eq!(percent.len(), 1);
    assert_eq!(percent[0].title, "10%3 = 1");
}

#[test]
fn executing_the_default_action_copies_the_result() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    let items = run_query(&mut stream, 1, "10/4");
    assert_eq!(items.len(), 1);
    let item = &items[0];

    send(
        &mut stream,
        &ClientMsg::Execute {
            query_id: 1,
            item_id: item.id.clone(),
            action_id: item.default_action.clone(),
        },
    );

    assert_eq!(
        recv(&mut stream),
        DaemonMsg::Executed {
            query_id: 1,
            outcome: ExecOutcome::CopyText(CopyText::new("2.5").unwrap()),
        }
    );
}

#[test]
fn input_that_is_not_an_expression_yields_a_clean_query_done_with_no_items() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("just some ordinary text").unwrap(),
        },
    );

    // No Results frame at all: the manifest's Mode::Calculator-only
    // declaration (Design decision 1) means this provider is never even
    // selected for a non-math query, so nothing produces items.
    //
    // The exchange is therefore `QueryRouted` then `QueryDone`, which is #127's
    // whole reason for being a separate frame: had the mode ridden on
    // `Results`, this query would report none at all. `Mode::All` here —
    // ordinary prose names no mode and reaches the routing fallback, which is
    // never exclusive, so a frontend shows no mode label and the user sees a
    // plain "no results" rather than a false claim about a mode.
    assert_eq!(
        recv(&mut stream),
        DaemonMsg::QueryRouted {
            query_id: 1,
            mode: Mode::All,
            exclusive: false,
        }
    );
    assert_eq!(recv(&mut stream), DaemonMsg::QueryDone { query_id: 1 });
}

/// #127 acceptance criterion 5, inferred half, over the socket: a bare sum is
/// *inferred* `Calculator`, so the frame names that mode with `exclusive:
/// false`. The `=` sigil would be the exclusive counterpart — asserted in
/// `assembly.rs` for the `a ` prefix rather than duplicated here.
#[test]
fn an_inferred_math_query_reports_calculator_without_exclusivity() {
    let daemon = calculator_daemon();
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("2+2").unwrap(),
        },
    );

    assert_eq!(
        recv(&mut stream),
        DaemonMsg::QueryRouted {
            query_id: 1,
            mode: Mode::Calculator,
            exclusive: false,
        }
    );
}

/// A second provider standing in for "some other, unrelated provider" —
/// `ScriptedProvider` rather than `SkeletonProvider`, deliberately: the
/// walking skeleton's one hardcoded item (`"Hello from hopd" ` / `"M2.2
/// walking skeleton"`) has no `+` character anywhere in its haystack, and
/// `Ranker::rank` (`crates/hop-core/src/rank.rs`) drops any item whose
/// haystack does not fuzzy-match the term *before* augmentation or
/// promotion ever run — so a query of `"2+2"` would filter the skeleton's
/// item out on relevance grounds alone, and this test would not be testing
/// what its name says. A scripted item whose own title contains the literal
/// term sidesteps that and isolates the one thing this test means to prove:
/// that a second, unrelated provider's item survives alongside the
/// calculator's. `common/mod.rs`'s own module doc names issue #58 as an
/// intended user of this fixture.
fn other_provider_item_for(term: &str) -> Item {
    scripted_item(
        "other",
        Kind::App,
        "other:1",
        &format!("{term} calculator app"),
    )
}

#[test]
fn a_math_looking_query_augments_rather_than_replaces_other_providers_results() {
    let mut host = ProviderHost::with_log(Arc::new(NoopLog));
    host.register(ScriptedProvider::new(
        "other",
        vec![Kind::App],
        Script::Answer(vec![other_provider_item_for("2+2")]),
    ))
    .unwrap();
    host.register(CalculatorProvider).unwrap();
    let daemon = start_daemon(HostSource::new(Arc::new(host)));
    let mut stream = connect(&daemon);

    let items = run_query(&mut stream, 1, "2+2");

    assert!(
        items
            .iter()
            .any(|i| i.provider == hop_core::provider::CALCULATOR_PROVIDER_ID),
        "the calculator's own item must be present, got {items:?}"
    );
    assert!(
        items.iter().any(|i| i.provider == "other"),
        "the other provider's item must still be present — augment, not \
         hijack, got {items:?}"
    );
    assert_eq!(
        items[0].provider,
        hop_core::provider::CALCULATOR_PROVIDER_ID,
        "the inferred-math promotion rule (pipeline.rs's promote_kinds, step \
         7) pins the calculator's item at the front rather than leaving it \
         wherever ranking alone put it, got {items:?}"
    );
}
