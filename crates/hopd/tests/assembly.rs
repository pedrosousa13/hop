//! Issue #103's replace-frame contract observed where it matters: over a real
//! socket. Earlier slices prove each assembly step (`Pipeline::assemble`) in
//! `hop-core`'s own unit tests; this file proves the daemon actually runs that
//! assembly on every provider arrival — that the frames a client swaps in
//! wholesale are *ranked*, not arrival-order concatenations, and that the
//! alias, learning, exclusive-filter and inferred-promotion behaviors reach
//! the wire undegraded.
//!
//! Each arrival re-assembles over the whole accumulated set (see
//! `HostSource::start`), so every `results` frame is the complete current
//! list and a test reading frames until `QueryDone` sees the list grow and
//! reorder as providers land. The assertions target the *final* frame (the
//! one after both arrivals) — the frame whose order is a property of ranking,
//! not of which provider happened to finish first.
//!
//! Plain `#[test]` functions driving a blocking
//! `std::os::unix::net::UnixStream` client, matching the other binaries in
//! this crate — see `host.rs` for why this suite has no `#[tokio::test]`.

#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{
    RecordingLog, Script, ScriptedProvider, TestDaemon, hello, recv, scripted_item, send,
    start_daemon,
};
use hop_core::aliases::Aliases;
use hop_core::host::{HostPolicy, ProviderHost};
use hop_core::pipeline::Pipeline;
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery};
use hop_protocol::{ActionId, ClientMsg, DaemonMsg, ExecOutcome, Item, ItemId, Kind, QueryText};
use hopd::source::HostSource;
use tokio::sync::Mutex;

/// A provider that answers after a fixed delay — the copy of
/// [`ScriptedProvider`] this file needs and that fixture deliberately does
/// not have. Issue #103's ranked-together and first-frame-before-slow tests
/// need two providers completing at *different* times, and `Script`
/// (`Answer`/`Fail`/`Panic`/`Hang`) has no delay variant; this minimal local
/// type provides one without widening the shared fixture's surface.
///
/// `budget` is kept larger than `delay` so the host (which aborts a provider
/// at its budget) lets the sleep complete.
#[derive(Clone)]
struct DelayedProvider {
    id: &'static str,
    kinds: Vec<Kind>,
    delay: Duration,
    budget: Duration,
    items: Vec<Item>,
}

impl Provider for DelayedProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: self.id,
            kinds: self.kinds.clone(),
            modes: vec![Mode::All],
            min_term_len: 0,
            budget: self.budget,
            ids_are_safe_to_persist_in_the_clear: false,
        }
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        tokio::time::sleep(self.delay).await;
        Ok(self.items.clone())
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        Ok(ExecOutcome::Done)
    }
}

/// A daemon over `register`'s providers and a hand-built `pipeline`, started
/// the way the other binaries in this crate start one — the one difference is
/// [`HostSource::with_pipeline`], the seam issue #60 (and this file) uses to
/// reach `Pipeline` state (`aliases`, `learning`) a fresh `default()` cannot.
fn daemon(
    policy: HostPolicy,
    pipeline: Pipeline,
    log: Arc<RecordingLog>,
    register: impl FnOnce(&mut ProviderHost),
) -> TestDaemon {
    let mut host = ProviderHost::new(policy, log);
    register(&mut host);
    let source = HostSource::with_pipeline(Arc::new(host), Arc::new(Mutex::new(pipeline)));
    start_daemon(source)
}

/// Connects to `daemon`, completes the handshake, and sets a read timeout.
/// Same rationale as `host.rs::connect`: minus a timeout, a regression that
/// stops the daemon replying would hang the suite instead of failing a named
/// assertion. Two seconds is generous against the providers' own budgets.
fn connect(daemon: &TestDaemon) -> UnixStream {
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    hello(&mut stream);
    stream
}

/// Reads every `results` frame for `query_id`, in order, until `QueryDone`.
///
/// Each arrival re-assembles and sends a full replacement frame, so the last
/// element is the complete current list after every provider answered — the
/// frame whose order is a property of ranking rather than of which provider
/// finished first. A test that collapsed frames into one list with `extend`
/// (as `host.rs` does for its count-only assertions) would lose exactly the
/// *ordering* this file exists to pin, so this returns the frames whole.
fn frames_for(stream: &mut UnixStream, query_id: u64) -> Vec<Vec<Item>> {
    routed_and_frames_for(stream, query_id).1
}

/// The exchange's `QueryRouted` frame (#127) alongside every `Results` batch.
///
/// Reading the routed frame here rather than in each caller means the ordering
/// rule — exactly one `QueryRouted`, **before** any `Results` or `QueryDone`
/// for the id — is asserted once for every test in this file that drives a
/// query, rather than once wherever someone remembered to. A second
/// `QueryRouted`, or one arriving after a `Results`, lands in the `other`
/// arm's panic below.
fn routed_and_frames_for(stream: &mut UnixStream, query_id: u64) -> ((Mode, bool), Vec<Vec<Item>>) {
    let routed = match recv(stream) {
        DaemonMsg::QueryRouted {
            query_id: got,
            mode,
            exclusive,
            ..
        } => {
            assert_eq!(got, query_id, "the routed frame must name this query");
            (mode, exclusive)
        }
        other => panic!("the first frame of a query must be QueryRouted, got {other:?}"),
    };
    (routed, results_until_done(stream, query_id))
}

/// Every remaining `Results` batch for `query_id`, up to `QueryDone`.
///
/// Separate from [`routed_and_frames_for`] because an exchange carries exactly
/// **one** `QueryRouted` (#127), so a caller that has already consumed it — a
/// test timing the first results frame by hand, then draining the rest — must
/// not expect a second. Calling `frames_for` mid-exchange would demand one and
/// fail on the next `Results`.
fn results_until_done(stream: &mut UnixStream, query_id: u64) -> Vec<Vec<Item>> {
    let mut frames = Vec::new();
    loop {
        match recv(stream) {
            DaemonMsg::Results {
                query_id: got,
                items,
                ..
            } => {
                assert_eq!(got, query_id);
                frames.push(items);
            }
            DaemonMsg::QueryDone { query_id: done } => {
                assert_eq!(done, query_id);
                break;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    frames
}

/// Acceptance 5 (ranked together): the daemon's second frame — after both
/// providers have answered — is ordered by rank, not by provider completion.
///
/// The fast provider answers a *weak* match ("Notes about calc", term buried
/// at the end, no start bonus); the slow provider a *strong* one ("Calc",
/// exact). Given two healthy providers, the pre-slice daemon handed batches
/// through in completion order, so the client's final list would have ended
/// `[fast weak, slow strong]` — the strong match last. Assembly flips it: the
/// final frame leads with the strong match even though it arrived second.
#[test]
fn two_providers_items_arrive_ranked_together_not_in_completion_order() {
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon(
        // The slow provider's 300 ms budget must survive the host's clamp,
        // so the policy allows it (as `host.rs`'s slowest-provider test does).
        HostPolicy {
            max_budget: Duration::from_millis(500),
            ..HostPolicy::default()
        },
        Pipeline::default(),
        log,
        |host| {
            host.register(ScriptedProvider::new(
                "fast",
                vec![Kind::App],
                Script::Answer(vec![scripted_item(
                    "fast",
                    Kind::App,
                    "app:notes",
                    "Notes about calc",
                )]),
            ))
            .unwrap();
            host.register(DelayedProvider {
                id: "slow",
                kinds: vec![Kind::App],
                delay: Duration::from_millis(100),
                budget: Duration::from_millis(300),
                items: vec![scripted_item("slow", Kind::App, "app:calc", "Calc")],
            })
            .unwrap();
        },
    );
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("calc").unwrap(),
        },
    );

    let frames = frames_for(&mut stream, 1);
    assert!(
        frames.len() >= 2,
        "both providers must each send a replacement frame, got {}",
        frames.len()
    );
    let final_items = frames.last().unwrap();
    assert_eq!(
        final_items.len(),
        2,
        "both items must be present once the slow provider lands"
    );
    assert_eq!(
        final_items[0].id,
        ItemId::new("app:calc").unwrap(),
        "the strong match must lead even though its provider answered second; \
         completion order would put it last"
    );
}

/// Acceptance 5 (alias boost): a seeded alias outranks the item that would
/// otherwise win, observed over the socket.
///
/// The alias boosts the apps-provider's `app:fireplace`. Without it both app
/// items match "fire" as a clean prefix at equal weight, so the tie-break
/// (title ascending) puts "Fire Alarm" first; the ~`ALIAS_BOOST` bump — far
/// above any fuzzy ceiling — moves "Fireplace" ahead.
#[test]
fn an_alias_boost_takes_effect_through_the_daemon() {
    let pipeline = Pipeline {
        aliases: Aliases::from_json(
            r#"[{"alias":"fire","type":"app","target":{"appId":"fireplace"}}]"#,
        )
        .unwrap(),
        ..Default::default()
    };
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon(HostPolicy::default(), pipeline, log, |host| {
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            Script::Answer(vec![
                scripted_item("apps", Kind::App, "app:fireplace", "Fireplace"),
                scripted_item("apps", Kind::App, "app:alarm", "Fire Alarm"),
            ]),
        ))
        .unwrap();
    });
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 2,
            text: QueryText::new("fire").unwrap(),
        },
    );

    let frames = frames_for(&mut stream, 2);
    let items = frames.last().unwrap();
    assert_eq!(
        items.len(),
        2,
        "both app items must survive; the alias reorders, it does not drop"
    );
    assert_eq!(
        items[0].id,
        ItemId::new("app:fireplace").unwrap(),
        "the alias-boosted item must outrank its would-be winner"
    );
}

/// Acceptance 5 (learning boost): a seeded launch count outranks an
/// otherwise-equal sibling, observed over the socket.
///
/// `record_launch("apps", "fire", "app:learned")` seeds a learning boost
/// keyed on the id and the provider that produced it (issue #72). Without it
/// the two app items match at equal weight and the tie-break (title
/// ascending) puts "Fire Alarm" first; the learned boost moves "Fireplace"
/// ahead. The provider passed here must match the item's own `provider`
/// field (`"apps"`, below) or the boost would not apply at all.
#[test]
fn a_learning_boost_takes_effect_through_the_daemon() {
    let mut pipeline = Pipeline::default();
    for _ in 0..10 {
        pipeline
            .learning
            .record_launch("apps", "fire", &ItemId::new("app:learned").unwrap());
    }
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon(HostPolicy::default(), pipeline, log, |host| {
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            Script::Answer(vec![
                scripted_item("apps", Kind::App, "app:learned", "Fireplace"),
                scripted_item("apps", Kind::App, "app:sibling", "Fire Alarm"),
            ]),
        ))
        .unwrap();
    });
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 3,
            text: QueryText::new("fire").unwrap(),
        },
    );

    let frames = frames_for(&mut stream, 3);
    let items = frames.last().unwrap();
    assert_eq!(
        items.len(),
        2,
        "both app items must survive; the boost reorders, it does not drop"
    );
    assert_eq!(
        items[0].id,
        ItemId::new("app:learned").unwrap(),
        "the learned item must outrank its equal-scoring sibling"
    );
}

/// Acceptance 5 (exclusive filter): an explicit `a ` route keeps only the
/// mode's kind.
///
/// Both providers must be *selected* for the filter (not scheduling) to be
/// under test, so both declare `Mode::Apps` — the exclusive route strips the
/// augmentation rule that would otherwise reach a `Mode::All`-only provider.
/// One serves `Kind::App`, the other `Kind::Window`; step 5 drops the Window
/// item even though it fuzzy-matches the term.
#[test]
fn an_exclusive_route_filters_to_that_modes_kinds() {
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon(HostPolicy::default(), Pipeline::default(), log, |host| {
        host.register(
            ScriptedProvider::new(
                "apps",
                vec![Kind::App],
                Script::Answer(vec![scripted_item(
                    "apps",
                    Kind::App,
                    "app:editor",
                    "Code Editor",
                )]),
            )
            .with_manifest(ProviderManifest {
                id: "apps",
                kinds: vec![Kind::App],
                modes: vec![Mode::Apps],
                min_term_len: 0,
                budget: Duration::from_millis(20),
                ids_are_safe_to_persist_in_the_clear: false,
            }),
        )
        .unwrap();
        host.register(
            ScriptedProvider::new(
                "win",
                vec![Kind::Window],
                Script::Answer(vec![scripted_item(
                    "win",
                    Kind::Window,
                    "window:editor",
                    "Code Editor",
                )]),
            )
            .with_manifest(ProviderManifest {
                id: "win",
                kinds: vec![Kind::Window],
                modes: vec![Mode::Apps],
                min_term_len: 0,
                budget: Duration::from_millis(20),
                ids_are_safe_to_persist_in_the_clear: false,
            }),
        )
        .unwrap();
    });
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 4,
            text: QueryText::new("a code").unwrap(),
        },
    );

    // #127, and acceptance criterion 5's exclusive half: this is an `a `
    // prefix route, so the frame must report the mode that *filtered* and say
    // so. Asserted right where the filtering behaviour below is asserted, so
    // the wire's claim and the daemon's behaviour cannot drift apart.
    let ((mode, exclusive), frames) = routed_and_frames_for(&mut stream, 4);
    assert_eq!(mode, Mode::Apps);
    assert!(
        exclusive,
        "an explicit prefix is an exclusive route, and the client is told so"
    );
    let items = frames.last().unwrap();
    assert_eq!(
        items.len(),
        1,
        "the exclusive filter must drop the Window item; got {items:?}"
    );
    assert_eq!(
        items[0].kind,
        Kind::App,
        "only the App mode's kind survives"
    );
    assert_eq!(items[0].provider, "apps");
}

/// Acceptance 5 (inferred promotion): an inferred route promotes that mode's
/// kind to the front *without removing* the general results.
///
/// `"2+2"` routes to `Mode::Calculator`, non-exclusive, so both providers
/// (`Mode::All`) are reached. App (weight 20) outranks Calculator (6) on the
/// body, so step 7 must promote the calculator item ahead while keeping the
/// app. A buggy promote-and-drop would pass a check of only the first item;
/// asserting both positions is what pins "promote, don't remove".
#[test]
fn an_inferred_route_promotes_without_removing() {
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon(HostPolicy::default(), Pipeline::default(), log, |host| {
        host.register(ScriptedProvider::new(
            "calc",
            vec![Kind::Calculator],
            Script::Answer(vec![scripted_item(
                "calc",
                Kind::Calculator,
                "calc:2plus2",
                "2+2 = 4",
            )]),
        ))
        .unwrap();
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            Script::Answer(vec![scripted_item(
                "apps",
                Kind::App,
                "app:puzzle",
                "2+2 Notes",
            )]),
        ))
        .unwrap();
    });
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 5,
            text: QueryText::new("2+2").unwrap(),
        },
    );

    // Criterion 5's inferred half, and the distinction the whole frame exists
    // for: a bare sum is *inferred* Calculator, so results were promoted and
    // nothing was removed. `exclusive` is therefore false — which is what
    // tells a frontend to show no mode label at all, because claiming a mode
    // over an unfiltered list would misdescribe what the window contains.
    let ((mode, exclusive), frames) = routed_and_frames_for(&mut stream, 5);
    assert_eq!(mode, Mode::Calculator);
    assert!(
        !exclusive,
        "an inferred route filters nothing, so augment-not-hijack holds"
    );
    let items = frames.last().unwrap();
    assert_eq!(
        items.len(),
        2,
        "promotion must not drop the general results"
    );
    assert_eq!(
        items[0].kind,
        Kind::Calculator,
        "the inferred utility result must lead, though App outweighs Calculator"
    );
    assert!(
        items.iter().any(|i| i.kind == Kind::App),
        "the App item must still be present behind it, got {items:?}"
    );
}

/// Acceptance 3 (first frame before the slow provider): over a real socket,
/// the fast provider's frame lands while the slow provider is still sleeping,
/// so the slow provider's item is absent from the first frame.
///
/// The slow provider sleeps 300 ms; the fast provider answers with no await
/// at all, so the first frame is sent long before. The timing bound (100 ms)
/// sits inside that gap — a gated-or-concatenating daemon that waited for the
/// slow provider before sending anything could not produce a first frame this
/// fast.
#[test]
fn the_first_frame_arrives_before_the_slow_provider_finishes() {
    let log = Arc::new(RecordingLog::default());
    let daemon = daemon(
        HostPolicy {
            max_budget: Duration::from_millis(500),
            ..HostPolicy::default()
        },
        Pipeline::default(),
        log,
        |host| {
            host.register(ScriptedProvider::new(
                "fast",
                vec![Kind::App],
                Script::Answer(vec![scripted_item(
                    "fast",
                    Kind::App,
                    "app:fast",
                    "Fast result",
                )]),
            ))
            .unwrap();
            host.register(DelayedProvider {
                id: "slow",
                kinds: vec![Kind::App],
                delay: Duration::from_millis(300),
                budget: Duration::from_millis(400),
                items: vec![scripted_item("slow", Kind::App, "app:slow", "Slow result")],
            })
            .unwrap();
        },
    );
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 6,
            text: QueryText::new("result").unwrap(),
        },
    );

    // `QueryRouted` (#127) is consumed *before* the clock starts, and that is
    // deliberate: this test's claim is about how long the fast provider's
    // *results* wait on the slow one, so the routed frame must not be inside
    // the measured window. It cannot distort the claim either way — the daemon
    // sends it from the query arm before the source is even started, so it is
    // already on the wire — but timing it would mean this test silently
    // measured two things.
    assert_eq!(
        recv(&mut stream),
        DaemonMsg::QueryRouted {
            query_id: 6,
            mode: Mode::All,
            exclusive: false,
            marker_span: None,
        }
    );

    let started = Instant::now();
    let first = match recv(&mut stream) {
        DaemonMsg::Results {
            query_id: 6, items, ..
        } => items,
        other => panic!("expected a results frame first, got {other:?}"),
    };
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(100),
        "the fast provider's frame must not wait on the slow one, took {elapsed:?}"
    );
    let ids: Vec<_> = first.iter().map(|i| i.id.as_str()).collect();
    assert!(
        !ids.contains(&"app:slow"),
        "the slow provider has not finished yet, so its item must be absent; got {ids:?}"
    );
    assert_eq!(first.len(), 1, "only the fast provider answered so far");

    // Drain to completion so the daemon is not left mid-query. This
    // exchange's one `QueryRouted` was consumed above, so this reads only the
    // remaining results frames.
    let frames = results_until_done(&mut stream, 6);
    assert_eq!(
        frames.last().unwrap().len(),
        2,
        "once the slow provider lands, both items are present"
    );
}
