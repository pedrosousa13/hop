//! Integration test for issue #60 criterion 6's "across a restart" half:
//! a launch recorded through a real daemon must survive that daemon
//! stopping, and be visible to a *second*, independent daemon that loads
//! the same on-disk store.
//!
//! `crates/hopd/tests/exec.rs`'s
//! `a_successful_execute_persists_a_launch_to_the_learning_store` already
//! proves persistence *within* one daemon's lifetime: the file lands
//! correctly after one execute, while that same daemon (and its
//! `TestDaemon`) are still alive. What it does not exercise is the
//! load/save boundary itself — a fresh `Learning::load` reading back what an
//! entirely separate in-memory `Pipeline` wrote. This file's test makes that
//! boundary the thing under test: the first daemon (and everything it holds
//! in memory — its `Pipeline`, its `ProviderHost`) is dropped before
//! anything is reloaded, and a second daemon, built from a fresh
//! `Learning::load` of the same store file, is what proves the launch
//! survived — not just the bytes on disk, but a whole new daemon lifetime
//! actually seeing them.
#![allow(clippy::unwrap_used)]

mod common;
use common::{hello, recv, send, start_daemon};

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use hop_core::host::{HostPolicy, NoopLog, ProviderHost};
use hop_core::learning::Learning;
use hop_core::pipeline::Pipeline;
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery};
use hop_protocol::{
    Action, ActionId, ActionKind, ClientMsg, DaemonMsg, ExecOutcome, Item, ItemId, Kind, QueryText,
};
use hopd::source::HostSource;

/// A provider that always answers with the same one item and always
/// succeeds its `execute` — the production dispatch path
/// (`ProviderHost` + `HostSource`) this test needs, since it is
/// `HostSource::record_launch`'s load/save wiring under test, not a
/// scripted source's own (test-defined) behavior.
struct RestartableProvider;

impl Provider for RestartableProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: "restartable",
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
            id: ItemId::new("restartable:1").unwrap(),
            kind: Kind::Action,
            title: "Restartable".to_string(),
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
            provider: "restartable".to_string(),
        }])
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        Ok(ExecOutcome::Done)
    }
}

/// Builds a `HostSource` wired the way `run()` wires the real daemon
/// (Design decision 7 of the issue-60 plan): a fresh `ProviderHost`
/// registering [`RestartableProvider`], and a `Pipeline` whose `learning` is
/// loaded from `store_path` right now — so each call is its own honest
/// "daemon startup reads whatever is on disk at this instant" moment,
/// exactly like `lib.rs::run()`'s own `Learning::load(&store_path)` call.
fn build_source(store_path: &std::path::Path) -> HostSource {
    let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
    host.register(RestartableProvider).unwrap();
    let pipeline = Arc::new(tokio::sync::Mutex::new(Pipeline {
        learning: Learning::load(store_path),
        ..Pipeline::default()
    }));
    HostSource::with_config(
        Arc::new(host),
        pipeline,
        hopd::source::MAX_RESULTS,
        Some(store_path.to_path_buf()),
    )
}

/// Queries `stream` for `"restartable"`, then executes `"restartable:1"`'s
/// `"open"` action against `query_id`, and returns the `Executed` reply.
/// Shared by both lifetimes below since the round trip is identical in each.
fn query_and_execute(stream: &mut UnixStream, query_id: u64) -> DaemonMsg {
    send(
        stream,
        &ClientMsg::Query {
            id: query_id,
            text: QueryText::new("restartable").unwrap(),
        },
    );
    let mut delivered = Vec::new();
    loop {
        match recv(stream) {
            DaemonMsg::Results {
                query_id: id,
                items,
                ..
            } if id == query_id => delivered = items,
            DaemonMsg::QueryDone { query_id: id } if id == query_id => break,
            other => panic!("unexpected frame during query {query_id}: {other:?}"),
        }
    }
    assert!(
        delivered.iter().any(|i| i.id.as_str() == "restartable:1"),
        "the provider's item must survive assembly, got {delivered:?}"
    );

    send(
        stream,
        &ClientMsg::Execute {
            query_id,
            item_id: ItemId::new("restartable:1").unwrap(),
            action_id: ActionId::new("open").unwrap(),
        },
    );
    recv(stream)
}

/// The restart proof: a launch recorded by one daemon lifetime is visible —
/// via a fresh `Learning::load`, and via a second, wholly independent daemon
/// built from that fresh load — after the daemon that recorded it has
/// already stopped. This pins issue #60's criterion 6's "persistence across
/// a restart" half (and, together with `exec.rs`'s within-lifetime test,
/// criterion 4 in full: the store loads at startup and persists across a
/// restart).
#[test]
fn a_launch_recorded_in_one_daemon_lifetime_survives_a_restart_into_a_second() {
    // Stands in for the real state dir `state_dir::resolve()` would hand
    // `run()`; the store's file name matches production so this test is
    // exercising the same path shape `lib.rs::run()` builds.
    let state_dir = tempfile::tempdir().unwrap();
    let store_path = state_dir.path().join(hopd::state_dir::STORE_FILE_NAME);

    // --- Lifetime 1: over an absent store, matching run()'s own startup
    // sequence (`Learning::load` degrades to an empty store on a missing
    // file — see hop-core's own tests for that contract).
    let daemon1 = start_daemon(build_source(&store_path));
    let mut stream1 = UnixStream::connect(&daemon1.socket_path).unwrap();
    hello(&mut stream1);

    let reply1 = query_and_execute(&mut stream1, 1);
    assert_eq!(
        reply1,
        DaemonMsg::Executed {
            query_id: 1,
            outcome: ExecOutcome::Done,
        },
        "the launch must succeed in lifetime 1, or there is nothing for \
         lifetime 2 to have inherited"
    );

    // Ends lifetime 1. Dropping `daemon1` tears down its tokio runtime,
    // which aborts the spawned server task and drops the listener, so
    // nothing accepts on that socket again — everything this daemon held in
    // memory (its `Pipeline`, its `ProviderHost`) goes with it. Only what
    // `record_launch` already wrote
    // to `store_path` survives past this point, exactly what a real process
    // restart would leave behind.
    drop(stream1);
    drop(daemon1);

    assert!(
        store_path.exists(),
        "the launch must have reached disk before the daemon that recorded \
         it was stopped"
    );

    // The load/save boundary itself: a fresh `Learning::load`, from a
    // process state that never touched lifetime 1's in-memory `Pipeline`.
    let reloaded_directly = Learning::load(&store_path);
    assert!(
        reloaded_directly
            .recent_launches(10)
            .iter()
            .any(|(id, _)| id == "restartable:1"),
        "the launch recorded in lifetime 1 must survive a fresh \
         Learning::load in lifetime 2, got {:?}",
        reloaded_directly.recent_launches(10)
    );

    // --- Lifetime 2: a second, independent daemon — its own `ProviderHost`,
    // its own `Pipeline` — built over the very same `store_path`.
    // `build_source` calls `Learning::load(&store_path)` again here, so this
    // is genuinely a fresh daemon's startup read, not a reuse of
    // `reloaded_directly` above.
    let daemon2 = start_daemon(build_source(&store_path));
    let mut stream2 = UnixStream::connect(&daemon2.socket_path).unwrap();
    hello(&mut stream2);

    let reply2 = query_and_execute(&mut stream2, 1);
    assert_eq!(
        reply2,
        DaemonMsg::Executed {
            query_id: 1,
            outcome: ExecOutcome::Done,
        },
        "the second daemon must still be able to execute and record its own launch"
    );

    drop(stream2);
    drop(daemon2);

    // Both launches — lifetime 1's and lifetime 2's — must be present in the
    // one store file that outlived both daemons: exactly two, not one
    // (lifetime 2 forgot lifetime 1's) and not a fresh one apiece (lifetime
    // 2's save clobbered lifetime 1's instead of building on it).
    let final_store = Learning::load(&store_path);
    let frequent = final_store.frequent_launches(1, &[]);
    assert_eq!(
        frequent,
        vec![("restartable:1".to_string(), 2)],
        "both the lifetime-1 launch and the lifetime-2 launch, recorded \
         across two independent daemon lifetimes sharing one store file, \
         must both be present, got {frequent:?}"
    );
}
