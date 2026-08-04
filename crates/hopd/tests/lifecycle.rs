//! Integration tests for the query lifecycle of issue #55, driven over a
//! real Unix socket against an in-process daemon whose source is scripted.
//! In-process rather than a spawned binary because cancellation must be
//! *observable*: only a source the test owns can report that its work
//! actually stopped.
#![allow(clippy::unwrap_used)]

mod common;

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use common::{hello, recv, send};
use hop_protocol::limits::{MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME};
use hop_protocol::{
    Action, ActionId, ActionKind, ClientMsg, DaemonMsg, Item, ItemId, Kind, QueryText,
};
use hopd::server::serve_with;
use hopd::source::ResultSource;
use tokio::sync::mpsc;

/// An in-process daemon on a scripted source, plus the runtime that hosts
/// it. Dropping this drops the runtime, which tears the server task and its
/// socket down with it.
struct TestDaemon {
    _runtime: tokio::runtime::Runtime,
    socket_path: PathBuf,
    _dir: tempfile::TempDir,
}

fn start_daemon<S: ResultSource>(source: S) -> TestDaemon {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = dir.path().to_path_buf();
    // serve_with expects the runtime dir itself (hopd's runtime_dir::resolve
    // is a binary-startup concern, not serve's); create the 0700 dir the
    // way resolve() would.
    let runtime_dir = root.join("hop");
    std::fs::create_dir(&runtime_dir).unwrap();
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let serve_dir = runtime_dir.clone();
    runtime.spawn(async move {
        let _ = serve_with(&serve_dir, source).await;
    });

    let socket_path = runtime_dir.join("hopd.sock");
    for _ in 0..50 {
        if socket_path.exists() {
            return TestDaemon {
                _runtime: runtime,
                socket_path,
                _dir: dir,
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("in-process hopd socket did not appear at {socket_path:?} within 5s");
}

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
fn a_query_streaming_past_the_cap_is_truncated_and_terminated() {
    // Six batches of one full frame each: 6 000 items offered, the cap is
    // 5 000. The daemon must deliver exactly the cap, drop the source, and
    // send exactly one QueryDone — truncation of the remainder, never
    // eviction of what was delivered, and never a lingering source left to
    // answer a 6th batch nobody asked for.
    let batch: Vec<Item> = (0..MAX_ITEMS_PER_RESULTS_FRAME).map(item).collect();
    let (events, mut events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(ScriptedSource {
        batches: vec![batch; 6],
        events,
        // See ScriptedSource's doc: paced so the cancellation-observability
        // assertion below is a property of the daemon, not a coin flip on
        // which side of the receiver drop the last `send` lands.
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

    let mut total = 0;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id: 3, items, ..
            } => total += items.len(),
            DaemonMsg::QueryDone { query_id: 3 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        total, MAX_ITEMS_PER_QUERY,
        "the exchange must deliver exactly the cap and stop"
    );

    // The source must actually be dropped at the cap, not just have its
    // output truncated on the wire: the still-pending 6th batch must be
    // refused, which ScriptedSource can only report by observing its `send`
    // fail. A daemon that hit the cap but forgot to drop the receiver would
    // instead accept and finish that 6th batch, and this would see
    // "finished" here instead of "cancelled" — or nothing at all, since the
    // source would then be blocked forever offering a batch nobody drains.
    assert_eq!(
        recv_event_within(&mut events_rx, Duration::from_secs(5)),
        Some("cancelled"),
        "the source must observe its work stopping once the cap is hit, \
         not run on past it"
    );

    // And exactly one QueryDone for this id: the regression this catches
    // from the other direction is a daemon that still holds the source live
    // past the cap, drains its pending (empty, capped) batch, and sends a
    // second QueryDone for the same id nobody is expecting.
    let mut buf = [0u8; 1];
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let read = stream.read(&mut buf);
    assert!(
        matches!(read, Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock),
        "no further frame may follow the cap's QueryDone, got: {read:?}"
    );
}

#[test]
fn a_batch_aligned_with_neither_the_frame_bound_nor_the_cap_still_splits_and_truncates() {
    // Every other cap test on this file hands the daemon batches of exactly
    // `MAX_ITEMS_PER_RESULTS_FRAME`, which lets two of `forward_batch`'s
    // paths go unexercised over a socket: `chunks()` always yields one chunk,
    // and the batch that fills the cap fills it exactly, so `truncate` is a
    // no-op. 1 500-item batches are aligned with neither bound and reach
    // both.
    //
    // 1 500 against a 1 000-item frame bound splits every batch into 1 000 +
    // 500. Against a 5 000-item cap, `room` steps 5 000 → 3 500 → 2 000 →
    // 500, so the fourth batch crosses the line with `0 < room <
    // batch.len()` — the truncating branch — and the fifth is refused
    // outright.
    let batch: Vec<Item> = (0..MAX_ITEMS_PER_RESULTS_FRAME + MAX_ITEMS_PER_RESULTS_FRAME / 2)
        .map(item)
        .collect();
    assert!(
        !batch.len().is_multiple_of(MAX_ITEMS_PER_RESULTS_FRAME)
            && !MAX_ITEMS_PER_QUERY.is_multiple_of(batch.len()),
        "this test is worth nothing unless the batch size divides neither bound"
    );
    let (events, _events_rx) = mpsc::unbounded_channel();
    let daemon = start_daemon(ScriptedSource {
        batches: vec![batch; 5],
        events,
        delay: Duration::ZERO,
    });
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    hello(&mut stream);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 4,
            text: QueryText::new("q").unwrap(),
        },
    );

    let mut total = 0;
    let mut frames = 0;
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id: 4, items, ..
            } => {
                // `recv` decodes through `hop_protocol`, whose parse refuses a
                // frame over the per-frame bound outright — so a daemon that
                // stopped splitting batches would fail this test inside the
                // helper. Asserted here as well so the failure names the rule
                // rather than the codec.
                assert!(
                    items.len() <= MAX_ITEMS_PER_RESULTS_FRAME,
                    "a source batch over the per-frame bound must be split, got {} items",
                    items.len()
                );
                total += items.len();
                frames += 1;
            }
            DaemonMsg::QueryDone { query_id: 4 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    assert_eq!(
        total, MAX_ITEMS_PER_QUERY,
        "the crossing batch must be truncated to exactly the room left, not delivered whole"
    );
    assert!(
        frames > MAX_ITEMS_PER_QUERY.div_ceil(MAX_ITEMS_PER_RESULTS_FRAME),
        "a 1 500-item batch takes two frames, so the cap's worth of items takes \
         more frames than the cap divided by the frame bound, got {frames}"
    );

    // Exactly one QueryDone: the fifth batch was refused rather than queued
    // behind the cap, so nothing follows the terminal frame.
    let mut buf = [0u8; 1];
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let read = stream.read(&mut buf);
    assert!(
        matches!(read, Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock),
        "no further frame may follow the cap's QueryDone, got: {read:?}"
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
