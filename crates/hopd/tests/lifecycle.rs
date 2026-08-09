//! Integration tests for the query lifecycle of issue #55, driven over a
//! real Unix socket against an in-process daemon whose source is scripted.
//! In-process rather than a spawned binary because cancellation must be
//! *observable*: only a source the test owns can report that its work
//! actually stopped.
#![allow(clippy::unwrap_used)]

mod common;

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use common::{hello, recv, send, start_daemon};
use hop_core::provider::ProviderError;
use hop_protocol::limits::{MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME};
use hop_protocol::{
    Action, ActionId, ActionKind, ClientMsg, DaemonMsg, ExecOutcome, Item, ItemId, Kind, QueryText,
};
use hopd::source::ResultSource;
use tokio::sync::mpsc;

/// A tiny item; `n` differentiates ids so assertions can tell items apart.
fn item(n: usize) -> Item {
    Item {
        id: ItemId::new(format!("test:{n}")).unwrap(),
        kind: Kind::Action,
        title: format!("item {n}"),
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
        provider: "test".to_string(),
    }
}

/// Polls `rx` for up to `deadline` — a regression hangs for seconds, not
/// forever. 10 ms matches the suite's existing poll idiom.
fn recv_event_within<T>(rx: &mut mpsc::UnboundedReceiver<T>, deadline: Duration) -> Option<T> {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if let Ok(event) = rx.try_recv() {
            return Some(event);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// A source that streams each scripted batch when the test releases it, and
/// reports on `events` when it observes cancellation (its send failing).
///
/// `delay` is paced between batches, not just decorative: with a capacity-1
/// channel and no delay, a `send` that lands *just* before the receiver is
/// dropped still returns `Ok` — the item was accepted into the (now-about-
/// to-be-discarded) buffer slot before the drop, which the sender has no way
/// to observe. A source racing the driver like that can report "finished"
/// even though its last batch was silently dropped with the receiver, which
/// makes cancellation-observability assertions against it flaky rather than
/// load-bearing. A short delay before each send gives the driver's (fast,
/// no-sleep) reaction to the previous batch — including dropping the
/// receiver, if that batch hit a cap — time to land first.
#[derive(Clone)]
struct ScriptedSource {
    batches: Vec<Vec<Item>>,
    events: mpsc::UnboundedSender<&'static str>,
    delay: Duration,
}

impl ResultSource for ScriptedSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        let (tx, rx) = mpsc::channel(1);
        let batches = self.batches.clone();
        let events = self.events.clone();
        let delay = self.delay;
        tokio::spawn(async move {
            for batch in batches {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                if tx.send(batch).await.is_err() {
                    let _ = events.send("cancelled");
                    return;
                }
            }
            let _ = events.send("finished");
        });
        rx
    }

    async fn execute(
        &self,
        _provider: &str,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        // Lifecycle tests never drive `Execute`, so this scripted source
        // answers with the failure a real refusal would, rather than
        // pretending an action ran. `ResultSource::execute` is exercised
        // where the seam is genuinely tested — hopd/tests/exec.rs.
        Err(ProviderError::Failed(
            "scripted lifecycle source does not execute".to_string(),
        ))
    }

    // No-op: `execute` above always fails, so `record_launch` — which the
    // Execute arm only calls on `Ok` — can never be reached by any lifecycle
    // test. The seam driving learning off a successful execute is pinned in
    // hopd/tests/exec.rs and src/connection.rs's own tests instead.
    async fn record_launch(&self, _query: &str, _item_id: &ItemId) {}
}

#[test]
fn a_query_streams_several_results_frames_before_its_done_frame() {
    let (events, _events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(ScriptedSource {
        batches: vec![vec![item(1)], vec![item(2)], vec![item(3)]],
        events,
        delay: Duration::ZERO,
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 7,
            text: QueryText::new("q").unwrap(),
        },
    );

    let mut frames = 0;
    let mut total_items = 0;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id,
                partial,
                items,
            } => {
                assert_eq!(query_id, 7, "every frame carries its query id");
                assert!(partial);
                frames += 1;
                total_items += items.len();
            }
            DaemonMsg::QueryDone { query_id } => {
                assert_eq!(query_id, 7);
                break;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert!(
        frames >= 2,
        "a single query must be able to produce several results frames, got {frames}"
    );
    assert_eq!(total_items, 3);
}

#[test]
fn a_re_sent_item_is_not_charged_twice() {
    // Under replace-frame, a source resends the complete current list on
    // every arrival (see `ResultSource`'s docs) — the *same* 50 items here,
    // 200 times over. A connection that still charges every item of every
    // frame against MAX_ITEMS_PER_QUERY (5 000) crosses that cap on the
    // 100th frame (50 * 100 == 5 000) and ends the exchange early, which is
    // exactly the regression acceptance criterion 6 names: a re-sent item
    // must not be charged twice, so all 200 frames must arrive with no
    // cap-driven QueryDone cutting the run short.
    let list: Vec<Item> = (0..50).map(item).collect();
    const {
        assert!(
            50 * 200 > MAX_ITEMS_PER_QUERY,
            "this test proves nothing unless it crosses the old per-connection cap"
        );
    }
    let (events, _events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(ScriptedSource {
        batches: vec![list; 200],
        events,
        delay: Duration::ZERO,
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 5,
            text: QueryText::new("q").unwrap(),
        },
    );

    let mut frames = 0;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 5, .. } => frames += 1,
            DaemonMsg::QueryDone { query_id: 5 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        frames, 200,
        "every re-sent list must produce its own frame; a cap-driven early \
         QueryDone means re-sent items are still being charged against \
         MAX_ITEMS_PER_QUERY at the connection"
    );
}

/// A source that streams batches forever until cancelled — cancellation is
/// the only way its work ever stops, so receiving its "cancelled" event is
/// proof the daemon stopped it rather than letting it run out.
#[derive(Clone)]
struct EndlessSource {
    events: mpsc::UnboundedSender<u64>,
    /// Which query this source is answering, stamped into its event so the
    /// test can tell which query's work stopped.
    query_tag: Arc<AtomicU64>,
}

impl ResultSource for EndlessSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        let (tx, rx) = mpsc::channel(1);
        let events = self.events.clone();
        let tag = self.query_tag.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                n += 1;
                if tx.send(vec![item(n)]).await.is_err() {
                    let _ = events.send(tag);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        rx
    }

    async fn execute(
        &self,
        _provider: &str,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        Err(ProviderError::Failed(
            "endless source does not execute".to_string(),
        ))
    }

    // No-op, for the same reason as `ScriptedSource` above: `execute` always
    // fails, so this is never reached.
    async fn record_launch(&self, _query: &str, _item_id: &ItemId) {}
}

#[test]
fn a_second_query_cancels_the_first_observably() {
    let (events, mut events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(EndlessSource {
        events,
        query_tag: Arc::new(AtomicU64::new(0)),
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("first").unwrap(),
        },
    );
    // At least one frame of query 1 arrives, proving it was running.
    let DaemonMsg::Results { query_id: 1, .. } = recv(&mut stream) else {
        panic!("query 1 must stream before being cancelled");
    };

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 2,
            text: QueryText::new("second").unwrap(),
        },
    );

    // The first source (tag 0) must observe cancellation: its work stops
    // rather than completing — it *cannot* complete; it is endless.
    let cancelled_tag = recv_event_within(&mut events_rx, Duration::from_secs(5))
        .expect("a cancellation event must arrive");
    assert_eq!(
        cancelled_tag, 0,
        "the first query's source is the one cancelled"
    );

    // Frames still flowing belong to query 2 (any late query-1 frames were
    // written before the cancel landed; drain until a query-2 frame shows).
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 2, .. } => break,
            DaemonMsg::Results { query_id: 1, .. } => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

#[test]
fn a_cancel_frame_stops_the_active_query_and_answers_query_done() {
    let (events, mut events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(EndlessSource {
        events,
        query_tag: Arc::new(AtomicU64::new(0)),
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 9,
            text: QueryText::new("q").unwrap(),
        },
    );
    let DaemonMsg::Results { query_id: 9, .. } = recv(&mut stream) else {
        panic!("the query must stream before the cancel");
    };

    send(&mut stream, &ClientMsg::Cancel { id: 9 });

    // The source observes the stop, and the exchange ends with QueryDone —
    // late Results frames for id 9 may precede it (already in flight when
    // the cancel landed); drain them.
    assert_eq!(
        recv_event_within(&mut events_rx, Duration::from_secs(5)),
        Some(0),
        "the cancelled query's source must observe its work stopping"
    );
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 9, .. } => continue,
            DaemonMsg::QueryDone { query_id: 9 } => break,
            other => panic!("expected QueryDone for the cancelled query, got {other:?}"),
        }
    }
}

#[test]
fn a_list_over_the_frame_bound_is_truncated_and_terminates() {
    // One over-long list — MAX_ITEMS_PER_RESULTS_FRAME + 1 items — offered as
    // a single batch, exactly as a complete replace-frame list arrives. A
    // second, smaller batch follows behind it in the script; nobody should
    // ever see it, because a replacement may never be split across frames
    // (Design decision 3: there is nothing on the wire to tell "the rest of
    // this list" apart from "a new list replacing it"), so the daemon's only
    // honest answers are truncate or refuse — and truncate-and-terminate,
    // the same shape it already uses everywhere else, is what this test
    // pins: exactly one frame, truncated to the bound, followed by the
    // exchange's terminal frame and nothing past it.
    let batch: Vec<Item> = (0..MAX_ITEMS_PER_RESULTS_FRAME + 1).map(item).collect();
    let (events, mut events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(ScriptedSource {
        batches: vec![batch, vec![item(999_999)]],
        events,
        // See ScriptedSource's doc: paced so the cancellation-observability
        // assertion below is a property of the daemon, not a coin flip on
        // which side of the receiver drop the second `send` lands.
        delay: Duration::from_millis(5),
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 3,
            text: QueryText::new("q").unwrap(),
        },
    );

    let mut frames = 0;
    let mut total = 0;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id: 3, items, ..
            } => {
                frames += 1;
                total += items.len();
            }
            DaemonMsg::QueryDone { query_id: 3 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        frames, 1,
        "a replacement may never be split across frames, got {frames}"
    );
    assert_eq!(
        total, MAX_ITEMS_PER_RESULTS_FRAME,
        "the over-long list must be truncated to exactly the frame bound"
    );

    // The source must actually be dropped, not just have its output
    // truncated on the wire: the still-pending second batch must be
    // refused, which ScriptedSource can only report by observing its `send`
    // fail. A daemon that truncated the frame but forgot to drop the
    // receiver would instead accept and finish that second batch, and this
    // would see "finished" here instead of "cancelled" — or nothing at all,
    // since the source would then be blocked forever offering a batch
    // nobody drains.
    assert_eq!(
        recv_event_within(&mut events_rx, Duration::from_secs(5)),
        Some("cancelled"),
        "the source must observe its work stopping once the frame bound \
         truncates the exchange, not run on past it"
    );

    // And exactly one QueryDone for this id: nothing follows the terminal
    // frame.
    let mut buf = [0u8; 1];
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let read = stream.read(&mut buf);
    assert!(
        matches!(read, Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock),
        "no further frame may follow the terminal frame, got: {read:?}"
    );
}

/// A source built for the "superseded query stays silent" test. Its first
/// `start` call streams forever, exactly like [`EndlessSource`] — the test
/// needs to control precisely when that query's work stops, by superseding
/// it, rather than let it race a natural finish. Every call after the first
/// answers with a couple of bounded batches and then closes normally, which
/// is what lets the *superseding* query reach its own `QueryDone` instead of
/// also running forever.
#[derive(Clone)]
struct FirstEndlessThenBoundedSource {
    calls: Arc<AtomicUsize>,
}

impl ResultSource for FirstEndlessThenBoundedSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        let (tx, rx) = mpsc::channel(1);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            if call == 0 {
                let mut n = 0;
                loop {
                    n += 1;
                    if tx.send(vec![item(n)]).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            } else {
                let _ = tx.send(vec![item(1), item(2)]).await;
                // Dropping `tx` here (end of scope) closes the channel, which
                // is what lets this call's exchange reach a natural QueryDone.
            }
        });
        rx
    }

    async fn execute(
        &self,
        _provider: &str,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        Err(ProviderError::Failed(
            "bounded test source does not execute".to_string(),
        ))
    }

    // No-op, for the same reason as the other scripted sources above:
    // `execute` always fails, so this is never reached.
    async fn record_launch(&self, _query: &str, _item_id: &ItemId) {}
}

#[test]
fn a_superseded_query_never_emits_query_done_for_its_old_id() {
    let daemon = start_daemon(FirstEndlessThenBoundedSource {
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("first").unwrap(),
        },
    );
    // At least one frame of query 1 arrives, proving it was actually running
    // (and not, say, already finished) at the moment it gets superseded.
    let DaemonMsg::Results { query_id: 1, .. } = recv(&mut stream) else {
        panic!("query 1 must stream before being superseded");
    };

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 2,
            text: QueryText::new("second").unwrap(),
        },
    );

    // Query 2's source is bounded, so this loop terminates on its own
    // QueryDone. Late Results frames for id 1 — already written to the wire
    // before the supersession landed — are legitimate and are drained
    // without comment. The one frame that must never appear, at any point in
    // this stream, is QueryDone for id 1: that is the daemon speaking about
    // an exchange the client has already moved on from.
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 1, .. } => continue,
            DaemonMsg::Results { query_id: 2, .. } => continue,
            DaemonMsg::QueryDone { query_id: 2 } => break,
            DaemonMsg::QueryDone { query_id: 1 } => {
                panic!("a superseded query must never receive QueryDone for its old id")
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

#[test]
fn a_non_matching_cancel_is_dropped_silently() {
    // EndlessSource, not a bounded ScriptedSource: this test needs query 7
    // to still be *provably* active (able to keep producing) when the
    // mismatched cancel lands and for several frames afterward — a bounded
    // source finishing on its own around the same time would leave the "did
    // the cancel disturb it" question unanswered by coincidence rather than
    // by the daemon's behavior.
    let (events, mut events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(EndlessSource {
        events,
        query_tag: Arc::new(AtomicU64::new(0)),
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 7,
            text: QueryText::new("q").unwrap(),
        },
    );
    // Query 7 must actually be streaming when the mismatched cancel lands —
    // otherwise this test could pass for the wrong reason (nothing active to
    // disturb in the first place).
    let DaemonMsg::Results { query_id: 7, .. } = recv(&mut stream) else {
        panic!("query 7 must stream before the non-matching cancel");
    };

    // 999 names neither the active query (7) nor any finished one on this
    // connection: this cancel must be ordinary traffic, not an error, and
    // must not touch query 7's exchange at all.
    send(&mut stream, &ClientMsg::Cancel { id: 999 });

    // Several more frames of query 7 must keep arriving. If the daemon
    // mistakenly matched the cancel to the active exchange regardless of id,
    // the next frame here would be a QueryDone (for 7 or for 999) instead of
    // another Results frame for 7 — EndlessSource has no other way to stop.
    for _ in 0..5 {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 7, .. } => {}
            other => {
                panic!("a non-matching cancel must not disturb the active query, got {other:?}")
            }
        }
    }

    // The connection stays fully usable: query 7's own matching cancel still
    // works normally afterward, proving the mismatched one left no damage
    // behind (no stray Error, no half-closed state).
    send(&mut stream, &ClientMsg::Cancel { id: 7 });
    assert_eq!(
        recv_event_within(&mut events_rx, Duration::from_secs(5)),
        Some(0),
        "query 7's source must still be cancellable after the earlier mismatched cancel"
    );
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results { query_id: 7, .. } => continue,
            DaemonMsg::QueryDone { query_id: 7 } => break,
            other => panic!("expected QueryDone for query 7, got {other:?}"),
        }
    }
}
