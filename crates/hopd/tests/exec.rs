//! Integration tests for the execute flow of issue #59, driven over a real
//! Unix socket against an in-process daemon whose source is scripted. One test
//! exercises a successful execution; the others exercise each refusal path —
//! unknown item (never delivered), stale query id, unknown action, provider
//! failure — and pin that they are query-scoped (non-terminal to the
//! connection). This is the real-socket half of the acceptance criterion;
//! `crates/hopd/src/connection.rs`'s unit tests cover the same resolution
//! logic one layer down, and the wire contract itself lives in
//! `hop-protocol`.
//!
//! The source streams exactly one item (or none, for the delivery-shape the
//! test controls) and records every `execute` call it is handed, so a test can
//! assert that a refusal never reached the source at all.
#![allow(clippy::unwrap_used)]

mod common;

use std::future::Future;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{hello, recv, send, start_daemon};
use hop_core::host::{HostPolicy, NoopLog, ProviderHost};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery};
use hop_protocol::{
    Action, ActionId, ActionKind, ClientMsg, DaemonMsg, ErrorCode, ExecOutcome, Item, ItemId, Kind,
    ProtoError, QueryText,
};
use hopd::source::{HostSource, ResultSource};
use tokio::sync::mpsc;

/// One item that agrees with the source's `provider` string and carries the
/// named actions, so a test can hit the unknown-action path independently of
/// the unknown-item one.
fn item(id: &str, actions: &[&str]) -> Item {
    Item {
        id: ItemId::new(id).unwrap(),
        kind: Kind::Action,
        title: id.to_string(),
        subtitle: None,
        icon: None,
        actions: actions
            .iter()
            .map(|&a| Action {
                id: ActionId::new(a).unwrap(),
                kind: ActionKind::Open,
                label: a.to_string(),
            })
            .collect(),
        default_action: ActionId::new("open").unwrap(),
        copy_text: None,
        append_to_end: false,
        provider: "script".to_string(),
    }
}

/// What [`ExecSource::execute`] does when the connection resolves far enough
/// to dispatch to the source.
#[derive(Clone)]
enum ExecBehavior {
    Done,
    Fail(String),
}

/// A [`ResultSource`] that streams one item (or none) and then finishes, and
/// records every `execute` call it receives. `start_daemon` clones the source
/// per connection, so the shared `calls` list survives to be asserted after a
/// refusal — proving the daemon never dispatched to the source on a refusal.
#[derive(Clone)]
struct ExecSource {
    item: Option<Item>,
    behavior: ExecBehavior,
    calls: Arc<Mutex<Vec<(String, String, String)>>>,
}

impl ResultSource for ExecSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        let (tx, rx) = mpsc::channel(1);
        let items: Vec<Item> = self.item.iter().cloned().collect();
        tokio::spawn(async move {
            let _ = tx.send(items).await;
        });
        rx
    }

    fn execute(
        &self,
        provider: &str,
        item_id: ItemId,
        action_id: ActionId,
    ) -> impl Future<Output = Result<ExecOutcome, ProviderError>> + Send {
        let provider = provider.to_string();
        let calls = self.calls.clone();
        let behavior = self.behavior.clone();
        async move {
            calls.lock().expect("no test panics holding this").push((
                provider,
                item_id.as_str().to_string(),
                action_id.as_str().to_string(),
            ));
            match behavior {
                ExecBehavior::Done => Ok(ExecOutcome::Done),
                ExecBehavior::Fail(msg) => Err(ProviderError::Failed(msg)),
            }
        }
    }
}

/// Connects to `daemon`, handshakes, sends `Query { id }`, reads until that
/// query's `QueryDone`, and returns the items of the last `Results` frame —
/// the live result set an execute resolves against. `id` is used straight
/// through so tests can pick id values that collide or not as the scenario
/// needs.
fn connect_and_run_query(daemon: &common::TestDaemon, id: u64) -> (UnixStream, Vec<Item>) {
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);
    send(
        &mut stream,
        &ClientMsg::Query {
            id,
            text: QueryText::new("q").unwrap(),
        },
    );
    let mut items = Vec::new();
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id,
                items: its,
                ..
            } if query_id == id => items = its,
            DaemonMsg::QueryDone { query_id } if query_id == id => return (stream, items),
            _ => {}
        }
    }
}

fn execute(stream: &mut UnixStream, query_id: u64, item_id: &str, action_id: &str) -> DaemonMsg {
    send(
        stream,
        &ClientMsg::Execute {
            query_id,
            item_id: ItemId::new(item_id).unwrap(),
            action_id: ActionId::new(action_id).unwrap(),
        },
    );
    recv(stream)
}

#[test]
fn a_successful_execute_round_trips_and_reaches_the_source() {
    let source = ExecSource {
        item: Some(item("app:1", &["open"])),
        behavior: ExecBehavior::Done,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let daemon = start_daemon(source.clone());

    let (mut stream, delivered) = connect_and_run_query(&daemon, 7);
    assert_eq!(
        delivered.len(),
        1,
        "the test item must be the live result set"
    );

    let reply = execute(&mut stream, 7, "app:1", "open");
    assert_eq!(
        reply,
        DaemonMsg::Executed {
            query_id: 7,
            outcome: ExecOutcome::Done,
        },
        "a resolved execute must be answered with Executed"
    );
    assert_eq!(
        source.calls.lock().unwrap().as_slice(),
        &[(
            "script".to_string(),
            "app:1".to_string(),
            "open".to_string()
        )],
        "the item's provider and both resolved ids must reach the source"
    );
}

#[test]
fn an_execute_for_an_undelivered_item_is_refused_and_not_acted_on() {
    let source = ExecSource {
        item: Some(item("app:1", &["open"])),
        behavior: ExecBehavior::Done,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let daemon = start_daemon(source.clone());

    let (mut stream, _delivered) = connect_and_run_query(&daemon, 7);
    // The live set holds only "app:1"; "app:2" was never delivered there.
    let reply = execute(&mut stream, 7, "app:2", "open");
    assert!(matches!(
        reply,
        DaemonMsg::Error {
            query_id: Some(7),
            error: ProtoError {
                code: ErrorCode::UnknownItem,
                ..
            }
        }
    ));
    assert!(
        source.calls.lock().unwrap().is_empty(),
        "a refused execute must never reach the source"
    );

    // The refusal is query-scoped, so the connection stays usable: a second
    // query on the same socket still streams normally.
    send(
        &mut stream,
        &ClientMsg::Query {
            id: 9,
            text: QueryText::new("q2").unwrap(),
        },
    );
    let mut seen = false;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 9, .. } => seen = true,
            DaemonMsg::QueryDone { query_id: 9 } => break,
            _ => {}
        }
    }
    assert!(seen, "the connection must survive an execute refusal");
}

#[test]
fn an_execute_naming_a_stale_query_id_is_refused() {
    let source = ExecSource {
        item: Some(item("app:1", &["open"])),
        behavior: ExecBehavior::Done,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let daemon = start_daemon(source.clone());

    // The active exchange is id 7; the frame names id 8 — a stale query id
    // even though the item existed under the live one.
    let (mut stream, _delivered) = connect_and_run_query(&daemon, 7);
    let reply = execute(&mut stream, 8, "app:1", "open");
    assert!(matches!(
        reply,
        DaemonMsg::Error {
            query_id: Some(8),
            error: ProtoError {
                code: ErrorCode::UnknownItem,
                ..
            }
        }
    ));
    assert!(source.calls.lock().unwrap().is_empty());
}

#[test]
fn an_execute_for_an_action_the_item_does_not_offer_is_refused() {
    let source = ExecSource {
        item: Some(item("app:1", &["open"])),
        behavior: ExecBehavior::Done,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let daemon = start_daemon(source.clone());

    let (mut stream, _delivered) = connect_and_run_query(&daemon, 7);
    // The item offers "open" only; the frame asks for "delete".
    let reply = execute(&mut stream, 7, "app:1", "delete");
    assert!(matches!(
        reply,
        DaemonMsg::Error {
            query_id: Some(7),
            error: ProtoError {
                code: ErrorCode::UnknownAction,
                ..
            }
        }
    ));
    assert!(source.calls.lock().unwrap().is_empty());
}

#[test]
fn a_provider_execute_failure_is_a_query_scoped_provider_failed() {
    let source = ExecSource {
        item: Some(item("app:1", &["open"])),
        behavior: ExecBehavior::Fail("boom".to_string()),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let daemon = start_daemon(source.clone());

    let (mut stream, _delivered) = connect_and_run_query(&daemon, 7);
    let reply = execute(&mut stream, 7, "app:1", "open");
    assert!(matches!(
        reply,
        DaemonMsg::Error {
            query_id: Some(7),
            error: ProtoError {
                code: ErrorCode::ProviderFailed,
                ..
            }
        }
    ));
    assert_eq!(
        source.calls.lock().unwrap().as_slice(),
        &[(
            "script".to_string(),
            "app:1".to_string(),
            "open".to_string()
        )],
        "the provider must be reached before it can fail"
    );
}

/// A provider whose `execute` never resolves — the liveness hazard issue
/// #59's execute bound contains. Registered into a real `ProviderHost` and
/// driven through a real `HostSource`, so the integration test exercises the
/// production path the CLI's apps provider uses.
struct HangingExecProvider;

impl Provider for HangingExecProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: "hang",
            kinds: vec![Kind::Action],
            modes: vec![Mode::All],
            min_term_len: 0,
            budget: Duration::from_millis(10),
        }
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(vec![Item {
            id: ItemId::new("hang:1").unwrap(),
            kind: Kind::Action,
            title: "hangapp".to_string(),
            subtitle: None,
            icon: None,
            actions: vec![Action {
                id: ActionId::new("open").unwrap(),
                kind: ActionKind::Open,
                label: "Open".to_string(),
            }],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "hang".to_string(),
        }])
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Through the production path (ProviderHost + HostSource) a provider whose
/// `execute` never resolves is cut off at the host's `max_execute_budget`
/// instead of wedging the connection: the daemon replies ProviderFailed within
/// the bound, and the same socket still serves a subsequent query.
#[test]
fn a_hanging_execute_is_bounded_and_the_connection_stays_responsive() {
    let mut host = ProviderHost::new(
        HostPolicy {
            max_execute_budget: Duration::from_millis(50),
            ..HostPolicy::default()
        },
        Arc::new(NoopLog),
    );
    host.register(HangingExecProvider).unwrap();
    let daemon = start_daemon(HostSource::new(Arc::new(host)));

    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);
    send(
        &mut stream,
        &ClientMsg::Query {
            id: 7,
            text: QueryText::new("hangapp").unwrap(),
        },
    );
    let mut delivered = Vec::new();
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id: 7, items, ..
            } => delivered = items,
            DaemonMsg::QueryDone { query_id: 7 } => break,
            other => panic!("unexpected frame during first query: {other:?}"),
        }
    }
    assert!(
        delivered.iter().any(|i| i.id.as_str() == "hang:1"),
        "the test item must survive assembly into the live result set, got {delivered:?}"
    );

    // The execute never resolves in the provider, but the host bounds it.
    let started = Instant::now();
    send(
        &mut stream,
        &ClientMsg::Execute {
            query_id: 7,
            item_id: ItemId::new("hang:1").unwrap(),
            action_id: ActionId::new("open").unwrap(),
        },
    );
    let reply = recv(&mut stream);
    let elapsed = started.elapsed();
    assert!(
        matches!(
            reply,
            DaemonMsg::Error {
                query_id: Some(7),
                error: ProtoError {
                    code: ErrorCode::ProviderFailed,
                    ..
                }
            }
        ),
        "a bounded execute must surface as provider-failed, got {reply:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "the execute must be bounded instead of hanging the driver, took {elapsed:?}"
    );

    // The connection's driver was not wedged: a second query on the same
    // socket still streams normally after the bounded execute.
    send(
        &mut stream,
        &ClientMsg::Query {
            id: 8,
            text: QueryText::new("hangapp").unwrap(),
        },
    );
    let mut answered_again = false;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 8, .. } => answered_again = true,
            DaemonMsg::QueryDone { query_id: 8 } => break,
            _ => {}
        }
    }
    assert!(
        answered_again,
        "the connection must remain usable after a bounded execute"
    );
}
