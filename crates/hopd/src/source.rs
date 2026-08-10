//! The seam between a connection and whatever answers its queries.
//!
//! A [`ResultSource`] answers one query with a stream of item batches behind
//! an `mpsc::Receiver`. The channel is the whole contract: batches arrive on
//! it, the source finishing closes it, and the *caller dropping it is
//! cancellation* — a source notices its next `send` fail and stops working.
//! That makes cancellation a property of the seam rather than a protocol
//! bolted onto it, and it is what issue #55's "a new query cancels the old
//! one server-side" hangs off.
//!
//! The one production source is [`HostSource`], over `hop-core`'s
//! [`ProviderHost`]: it routes the query text and hands the routed query to
//! the host, which runs every provider the query reaches under an enforced
//! budget. [`SkeletonProvider`] is the one provider registered today — the
//! walking skeleton's hardcoded item, re-expressed as a real provider so the
//! daemon's production path runs through the host from issue #56 onward rather
//! than waiting for #57's apps provider to make it real.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hop_core::host::{ProviderEvent, ProviderHost, ProviderLog};
use hop_core::pipeline::{CheckedItems, FailedCheck, Pipeline, Rejection};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery, route};
use hop_protocol::{
    Action, ActionId, ActionKind, ExecOutcome, Item, ItemId, Kind, MAX_ITEMS_PER_QUERY,
    MAX_ITEMS_PER_RESULTS_FRAME, QueryText,
};
use tokio::sync::{Mutex, mpsc};

/// Answers queries with streams of item batches — each one, per issue #103's
/// **replace-frame** contract, the complete current result list for that
/// query rather than an increment on top of the last one. A caller receiving
/// a batch swaps its whole retained list for it; it does not append. See
/// [`HostSource::start`] for the one production implementation of that half
/// of the contract, and `hop-protocol`'s `DaemonMsg::Results` docs for the
/// wire half.
///
/// `Clone` because every connection gets its own handle; implementations are
/// expected to be cheap handles over shared state, not the state itself.
///
/// # What an implementation owes the daemon
///
/// Four obligations, none of which this seam checks. [`HostSource`] is the
/// first implementation with enough surface to break any of them, and here is
/// where it actually stands against each — read this rather than assume
/// landing issue #56 settled all of them, because it did not.
///
/// **Items must respect `hop_protocol::limits`' per-item field bounds.**
/// [`Item`]'s fields are public, and those bounds are applied where an item is
/// *parsed*, so an item handed back through this trait has passed nothing
/// merely by virtue of implementing it. The daemon bounds its retained set by
/// item *count*
/// ([`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY)), and
/// the byte figure that count is justified against is the count multiplied by
/// those per-item bounds — so a source producing a 100 MB title makes that
/// arithmetic, and the bound it justifies, meaningless. The only thing below
/// it is
/// [`MAX_FRAME_BYTES`](hop_protocol::limits::MAX_FRAME_BYTES) at encode time,
/// which surfaces as an `io::Error` that kills the connection with no error
/// frame — a worse outcome than refusing the item would have been.
/// [`HostSource`] closes this gap for the field-length half, as of issue
/// #61: `ProviderHost::run_one`'s per-provider turn now calls
/// [`CheckedItems::check`], which rejects an item whose `title`, `subtitle`,
/// `copy_text`, an action's `label`, or action count is over the same bound
/// this module already applies at the parse (see
/// [`FailedCheck::FieldTooLong`]) — a provider that returns an oversized
/// title is refused before it ever reaches this trait's channel. This is
/// still a property of [`HostSource`] specifically, not a guarantee this
/// *trait*'s contract makes: it holds because [`HostSource`] happens to
/// route every provider's answer through [`CheckedItems::check`] before
/// sending, not because implementing [`ResultSource`] requires it. A
/// hypothetical future implementation that hands back items some other way —
/// bypassing that check — would reach this trait exactly as unchecked as
/// this paragraph used to describe for every implementation.
///
/// **What a source buffers is daemon memory the cap does not see.** The
/// receiver returned here lives inside the connection's exchange for the life
/// of the query, so the channel's capacity and the size of each batch are
/// daemon memory chosen by the source. `MAX_ITEMS_PER_QUERY` counts only what
/// the daemon has *forwarded*; a `mpsc::channel(1_000)` carrying 1 000-item
/// batches parks a million items the cap never counts. Every source in this
/// crate uses capacity 1, and a source with more should have a reason.
/// [`HostSource`] honours the capacity half — its `start` opens exactly
/// `mpsc::channel(1)` — and, as of issue #61, the *single-batch* half too: a
/// batch is still whatever one provider returns, but `ProviderHost::run_one`
/// now routes every provider's answer through [`CheckedItems::check`] before
/// this trait's channel ever sees it, and that check truncates the answer to
/// [`MAX_ITEMS_PER_PROVIDER_ANSWER`](hop_core::pipeline::MAX_ITEMS_PER_PROVIDER_ANSWER)
/// (1 000 items) before anything is sent. So one `send` here can now park at
/// most 1 000 items, not "however many a provider sent" — issue #30's gap,
/// closed at the seam upstream of this trait rather than inside it. Nothing
/// about *this* trait's contract enforces the cap, the same caveat the
/// paragraph above ends on: it holds because [`HostSource`] happens to route
/// through [`CheckedItems::check`], not because implementing [`ResultSource`]
/// requires it.
///
/// **`send` points must be frequent enough for cancellation to be prompt.**
/// The channel is the cancellation mechanism (see the module docs), and a
/// source learns it was cancelled only when a `send` fails — never between
/// sends. A source that computes for ten seconds and then sends once is
/// therefore not cancelled at all: it runs to completion and discovers at the
/// end that nobody wanted the answer. Issue #55's criterion is that a
/// superseded query's work *stops* rather than completing and being discarded,
/// and this seam delivers that only for sources that reach a `send` often
/// relative to how long a query stays on screen. [`HostSource`] honours this
/// one squarely: [`ProviderHost::spawn_query`] runs each selected provider as
/// its own task and sends the moment that provider's `run_one` resolves, so a
/// cancellation is only ever as stale as the slowest *individual* provider,
/// never the whole query.
///
/// **Each batch replaces the last one, in full — never an increment.** This
/// is the seam's half of issue #103's replace-frame contract: a batch is not
/// "what's new since the last one", it is the whole answer as of now, and a
/// caller that concatenated batches instead of swapping them would grow an
/// unbounded, ever-duplicating list. [`HostSource`] delivers this by
/// re-assembling over everything received so far on every arrival rather than
/// over just the newest batch — see the accumulator inside its `start`
/// implementation below. One consequence worth stating rather than leaving
/// implied: the per-item field-bound obligation above is unchanged by this —
/// it was never about how a batch relates to the one before it — and the
/// accumulator's own cost grows with it. Building the complete list every
/// arrival means cloning everything received so far once per arrival
/// (`CheckedItems::clone`, inside [`HostSource::start`]), so that cost is
/// proportional to what providers have sent for the query, not to what
/// changed. Nothing in this crate bounds that input yet — issues #30 and #61
/// are the slice that will.
pub trait ResultSource: Clone + Send + Sync + 'static {
    /// Starts answering one query. Batches arrive on the returned receiver,
    /// each the complete current result list rather than an increment on the
    /// last (see this trait's docs); the channel closing means the source is
    /// done; dropping the receiver cancels the work — subject to the
    /// obligations on this trait, which say what "cancels" costs an
    /// implementation that never sends.
    fn start(&self, text: QueryText) -> mpsc::Receiver<Vec<Item>>;

    /// Executes `action_id` on `item_id`, which the connection has already
    /// resolved against the items `provider` produced in a prior `start`.
    ///
    /// Resolution is the connection's job — this seam never mints an item, it
    /// only acts on one the connection has already bound to a delivered
    /// `Item` (see `crates/hopd/src/connection.rs`'s `Exchange::delivered`
    /// for that retained-set rule). `provider` is that item's `provider`
    /// string; both ids were validated against the retained set before this
    /// is ever called. An implementation answers with the action's
    /// [`ExecOutcome`], or a [`ProviderError`] describing why the provider
    /// could not perform it. [`HostSource`] dispatches through its
    /// [`ProviderHost`]; a test or scripted source answers however its
    /// scenario wants.
    fn execute(
        &self,
        provider: &str,
        item_id: ItemId,
        action_id: ActionId,
    ) -> impl Future<Output = Result<ExecOutcome, ProviderError>> + Send;

    /// Records that the user reached `item_id`, produced by `provider`,
    /// while typing `query` — a launch, in `hop-core`'s
    /// [`Learning`](hop_core::learning::Learning) vocabulary. `provider`
    /// matches [`execute`](Self::execute)'s parameter order and naming
    /// rather than a new convention of its own.
    ///
    /// This is issue #60's seam for turning a successful [`execute`](Self::execute)
    /// into learning: `crates/hopd/src/connection.rs`'s Execute arm calls this
    /// only once `execute` has already answered `Ok`, before it sends
    /// `Executed` back to the peer — a launch is a successful action, not an
    /// attempted one, so a refused or failed execute never reaches this
    /// method. `query` is the accepted text of the query the item was
    /// resolved under (`Exchange::text`), and `item_id` and `provider` are
    /// the same ids `execute` was just called with; the connection is the
    /// only place that holds all three, which is why this seam is driven
    /// from there rather than folded into `execute` itself.
    ///
    /// `provider` is issue #72's addition, forwarded to
    /// [`Learning::record_launch`](hop_core::learning::Learning::record_launch)
    /// so that a launch is recorded, and later looked up, under the provider
    /// that actually produced the item — not the bare item id alone, which
    /// let one provider collect boosts another had earned.
    ///
    /// [`HostSource`] records the launch against its `Pipeline`'s
    /// [`Learning`](hop_core::learning::Learning) store and, when it was
    /// built with a store path, persists it — see its impl for what happens
    /// if that persist fails. A test or scripted source is free to make this
    /// a no-op where its scenario does not care about learning, the same way
    /// most of this crate's scripted sources already treat `execute`'s
    /// outcome as the only thing worth scripting.
    fn record_launch(
        &self,
        provider: &str,
        query: &str,
        item_id: &ItemId,
    ) -> impl Future<Output = ()> + Send;
}

/// The walking skeleton's item, as a real [`Provider`].
///
/// # Why this exists rather than a source that returns the item directly
///
/// It is what makes the provider host the daemon's *production* path in this
/// slice instead of a component only tests reach. Issue #32's criterion is that
/// the enforcement predicate has a caller outside tests, and a host nothing
/// registers with does not have one. Keeping the old direct source alongside
/// would also have meant `hop query` returning results that never passed a
/// manifest check, which is precisely the arrangement `hop-core`'s
/// [`CheckedItems`](hop_core::pipeline::CheckedItems) exists to make
/// unreachable.
///
/// It is also the tree's worked example of a well-behaved provider for issue
/// #57 to copy, so its manifest and the item it returns must agree — the item's
/// `provider` string equals this manifest's `id` and its kind is one this
/// manifest declares. Pinned by
/// `tests::the_skeleton_providers_own_item_passes_its_own_manifest`.
///
/// `budget` is 1 ms because the work is constructing one struct. A provider
/// asking for more than [`MAX_PROVIDER_BUDGET`](hop_core::host::MAX_PROVIDER_BUDGET)
/// would be clamped to it, and one asking for less is taken at its word.
#[derive(Clone)]
pub struct SkeletonProvider;

impl Provider for SkeletonProvider {
    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: "skeleton",
            kinds: vec![Kind::Action],
            // `Mode::All` because this provider answers ordinary, unprefixed
            // search — a provider that omits it is never asked to run for a
            // query that did not route to one of its other modes.
            modes: vec![Mode::All],
            min_term_len: 0,
            budget: Duration::from_millis(1),
            // Opts in (issue #72): this provider's one item id,
            // `hop:walking-skeleton`, is a compile-time literal `hardcoded_item`
            // writes verbatim — never derived from a query, from disk
            // enumeration, or from anything else that varies at runtime. A
            // constant has no user-authored content to leak, so there is
            // nothing here for a shape rule to have guessed wrong about.
            ids_are_safe_to_persist_in_the_clear: true,
        }
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        Ok(vec![hardcoded_item()])
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        // Dispatch is issue #59's slice and is now wired up, but this
        // walking-skeleton provider has no real action to perform — its
        // hardcoded item is a placeholder. It fails honestly (surfacing as a
        // query-scoped `ProviderFailed`) rather than pretending to have done
        // something; the real executor is the apps provider.
        Err(ProviderError::Failed(
            "the skeleton provider has no action to perform".to_string(),
        ))
    }
}

/// The default `max_results` the daemon passes to [`Pipeline::assemble`]
/// on every arrival — the value [`HostSource`] built without the config-aware
/// constructor ([`HostSource::with_config`]) uses, and what an absent config's
/// `Config` falls back to.
///
/// A launcher renders tens of rows, not thousands, so this is sized for what
/// a person can look at rather than for what the pipeline could produce.
/// A config that sets its own `max_results` uses that value instead; this
/// constant remains the default and the value the compile-time
/// replace-frame assertion below guards.
pub const MAX_RESULTS: usize = 50;

// Design decision 3: one assembled list must fit one `results` frame — a
// replacement can never be split across frames, because a client would then
// have no way to tell "the rest of the current list" from "a new list
// replacing it". This assertion is what makes that true by construction: a
// `MAX_RESULTS` raised past `MAX_ITEMS_PER_RESULTS_FRAME` fails the build
// rather than failing a query at runtime, the day it happens rather than
// habitually staying safe.
const _: () = assert!(MAX_RESULTS <= MAX_ITEMS_PER_RESULTS_FRAME);

/// The production [`ResultSource`]: a routed query handed to a
/// [`ProviderHost`].
///
/// `Clone` is cheap — the host sits behind an `Arc`, so every connection's
/// handle shares one registry rather than one per connection.
#[derive(Clone)]
pub struct HostSource {
    host: Arc<ProviderHost>,
    pipeline: Arc<Mutex<Pipeline>>,
    /// How many results [`Pipeline::assemble`] is asked for on every arrival.
    /// [`HostSource::new`] and [`HostSource::with_pipeline`] default this to
    /// [`MAX_RESULTS`]; [`HostSource::with_config`] sets it from the config.
    max_results: usize,
    /// Where this daemon's `Learning` store lives, when this source was built
    /// from the real config. `None` for a test-built source that only wants
    /// the `Learning` in-memory behavior without a store to save to.
    ///
    /// Read by [`HostSource::record_launch`], which saves the store back to
    /// this path whenever it is `Some` — see that method for what happens if
    /// the save fails.
    learning_path: Option<PathBuf>,
    /// Serializes saves to `learning_path` against each other — held across
    /// the blocking save, but only ever that, never across `pipeline`'s lock
    /// — so two launches can't write out of order. See
    /// [`HostSource::record_launch`] for why this is enough on its own,
    /// with no generation bookkeeping needed.
    save_lock: Arc<Mutex<()>>,
}

impl HostSource {
    /// A source over `host`, with a fresh, empty [`Pipeline`]. The host is
    /// already built and its providers already registered: registration
    /// happens once at startup, which is what makes a captured manifest a
    /// startup-time fact rather than a per-query one.
    pub fn new(host: Arc<ProviderHost>) -> Self {
        HostSource {
            host,
            pipeline: Arc::new(Mutex::new(Pipeline::default())),
            max_results: MAX_RESULTS,
            learning_path: None,
            save_lock: Arc::new(Mutex::new(())),
        }
    }

    /// A source over `host`, sharing a caller-supplied `pipeline` rather than
    /// building an empty one.
    ///
    /// This is the seam issue #60 loads a persisted `Learning` store through
    /// — building the real daemon's `Pipeline` once at startup and handing it
    /// here, instead of every connection getting [`Pipeline::default`]'s
    /// empty one. It is also how a test reaches ranking behavior
    /// [`HostSource`] cannot otherwise drive: seeding aliases or learning
    /// needs a `Pipeline` built by hand, and this is the only way in.
    pub fn with_pipeline(host: Arc<ProviderHost>, pipeline: Arc<Mutex<Pipeline>>) -> Self {
        HostSource {
            host,
            pipeline,
            max_results: MAX_RESULTS,
            learning_path: None,
            save_lock: Arc::new(Mutex::new(())),
        }
    }

    /// A source over `host`, sharing a caller-supplied `pipeline` and the
    /// config-derived assembly cap and learning-store path.
    ///
    /// This is what `run()` wires (Design decision 7 of issue #60): the
    /// pipeline carries the `Learning` store loaded from `learning_path`, and
    /// `max_results` is the config's value rather than the [`MAX_RESULTS`]
    /// default. `learning_path` is `None` for a source (a test, say) that
    /// wants in-memory `Learning` behavior without a store to persist to.
    pub fn with_config(
        host: Arc<ProviderHost>,
        pipeline: Arc<Mutex<Pipeline>>,
        max_results: usize,
        learning_path: Option<PathBuf>,
    ) -> Self {
        HostSource {
            host,
            pipeline,
            max_results,
            learning_path,
            save_lock: Arc::new(Mutex::new(())),
        }
    }
}

impl ResultSource for HostSource {
    fn start(&self, text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        // Two channels, each capacity 1 for the reason this trait's docs
        // give: what a source buffers is daemon memory the retained-set cap
        // does not see, so a deeper channel would only let providers park
        // items the cap never counts. `ProviderHost::spawn_query` speaks
        // `CheckedItems` — the only route to `Pipeline::assemble`, which
        // nothing downstream of the host can build one of any other way —
        // while this trait still promises a bare `Vec<Item>`, so the
        // accumulator task below is what turns one into the other.
        let (host_tx, mut host_rx) = mpsc::channel(1);
        let (tx, rx) = mpsc::channel(1);

        // Routing happens here rather than inside the host because the host's
        // vocabulary is a `RoutedQuery` — the same value every provider sees,
        // shared rather than cloned per provider. `Pipeline::assemble` below
        // routes `text` again, from the raw text rather than from this
        // `RoutedQuery` — it accepts nothing else — so the query is routed
        // twice per arrival. Routing is pure and cheap (`hop_core::router`'s
        // module docs: it runs on every keystroke), so the second call is
        // accepted rather than threaded around.
        let routed = Arc::new(route(text.as_str()));
        self.host.spawn_query(routed, host_tx);
        let pipeline = Arc::clone(&self.pipeline);
        // Captured alongside `pipeline` so the spawned accumulator reads the
        // caller-configured cap without borrowing `self` across the spawn.
        let max_results = self.max_results;

        // The task returns — dropping `host_rx`, its receiving half of the
        // host's own channel — the moment a downstream send fails, rather
        // than draining the rest of what the host has to say. That drop is
        // what carries cancellation across the extra hop: each provider's
        // clone of `host_tx` then finds its own send failing in turn, which
        // is `ProviderHost::spawn_query`'s documented cancellation mechanism
        // — a caller dropping `results` — applied here to `host_tx` instead
        // of to `rx` directly, with this task standing in as the caller.
        // Ignoring the send's error and looping instead (`let _ = tx.send(
        // ...).await;`) would leave every provider running until it finished
        // or was cut off at its own budget, silently breaking the
        // `ResultSource` cancellation contract for every query that goes
        // through this hop —
        // `tests::dropping_the_forwarded_receiver_cancels_the_query` is
        // written against exactly that regression.
        //
        // This is the accumulator issue #103 is about: per query, it owns the
        // raw query text, a running `CheckedItems` built up across every
        // arrival, and the shared `Pipeline` handle. Each arrival re-runs
        // `assemble` over the *whole* accumulated set — never just the batch
        // that just arrived — so the frame sent is always the complete
        // current list a client can swap its own list for wholesale, per the
        // replace-frame contract `ResultSource`'s docs describe below.
        tokio::spawn(async move {
            let mut accumulated = CheckedItems::check(Vec::new());

            while let Some(mut checked) = host_rx.recv().await {
                // `MAX_ITEMS_PER_QUERY` bounds what this task accumulates,
                // not what any one frame carries — `MAX_RESULTS` already
                // bounds that far lower. Filling the room exactly is still a
                // cap: an accumulator with no room left has nothing to give a
                // later batch, so ending the query now is the same answer
                // arrived at one batch later, and it costs the client one
                // fewer round trip to learn it.
                let room = MAX_ITEMS_PER_QUERY.saturating_sub(accumulated.items().len());
                let capped = checked.items().len() >= room;
                if capped {
                    checked.truncate_items(room);
                }
                accumulated.absorb(checked);

                // Locked only across `assemble` itself, never across the
                // `send` below — `assemble` is synchronous, so holding the
                // guard past it would block every other query sharing this
                // `Pipeline` for the length of an `.await` for no reason.
                let assembly = {
                    let mut pipeline = pipeline.lock().await;
                    pipeline.assemble(text.as_str(), accumulated.clone(), max_results)
                };
                // `Assembly::rejections` is discarded here: the host already
                // logged the manifest-check half of it through its own log
                // seam before this task ever saw the item
                // (`ProviderHost::run_one`), and the pin-budget half —
                // mintable only inside `assemble` — stays unlogged, as it is
                // today (out of scope; see the design plan's Scope section).

                if tx.send(assembly.items).await.is_err() {
                    return;
                }

                // Sent *before* returning, not after: returning first would
                // drop the last assembled list on the floor instead of
                // delivering it, leaving the client's final view one batch
                // stale.
                if capped {
                    return;
                }
            }
        });

        rx
    }

    async fn execute(
        &self,
        provider: &str,
        item_id: ItemId,
        action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        self.host.execute(provider, item_id, action_id).await
    }

    /// Records the launch against the pipeline's `Learning` store and, when
    /// this source was built with a store path ([`HostSource::with_config`]),
    /// saves it back out. `pipeline` is locked twice, briefly, rather than
    /// once for the whole method: once to make the in-memory record, and —
    /// only if there is somewhere to save to — again, later, just to clone
    /// the `Learning` a save needs. Neither lock is held anywhere near the
    /// save itself, which runs on a blocking-pool thread via
    /// [`tokio::task::spawn_blocking`]. This is the same discipline
    /// [`HostSource::start`] documents above ("locked only across `assemble`
    /// itself... holding the guard past it would block every other query
    /// sharing this `Pipeline`"): `Learning::save` is synchronous and does a
    /// blocking write + fsync + rename, so holding `pipeline`'s guard across
    /// it — or blocking a runtime worker thread on it directly — would stall
    /// every other connection's query assembly, or other scheduled tokio
    /// tasks, for as long as the disk takes.
    ///
    /// What serializes concurrent launches' saves against each other is
    /// `save_lock`, held across the clone *and* the blocking save (that is
    /// the point of it — nothing but a save ever waits on it, so holding it
    /// across I/O costs nothing `pipeline`'s other callers can feel). Because
    /// the clone is taken after winning `save_lock`, not before, it always
    /// reads whatever is currently in `pipeline.learning` — which already
    /// includes every launch recorded by the time this call wins the race,
    /// including ones from other calls still ahead of it in the queue. So a
    /// concurrent save can only ever write a superset of what came before
    /// it; there is nothing to compare or skip, and no recorded launch can
    /// be lost.
    ///
    /// A save failure is logged via `eprintln!` (the same seam
    /// [`StderrLog`] uses) and otherwise ignored, as is a panic inside the
    /// blocking task: this method's caller, `connection.rs`'s Execute arm,
    /// calls it only after `execute` already answered `Ok` and is about to
    /// send `Executed` — a persistence hiccup here must not turn that
    /// already-successful execute into a client-visible error, and the
    /// in-memory record above still took, so the next launch (or the next
    /// successful save) still has it.
    async fn record_launch(&self, provider: &str, query: &str, item_id: &ItemId) {
        {
            let mut pipeline = self.pipeline.lock().await;
            pipeline.learning.record_launch(provider, query, item_id);
        }

        let Some(path) = self.learning_path.as_ref() else {
            return;
        };

        let _save_guard = self.save_lock.lock().await;
        let learning = self.pipeline.lock().await.learning.clone();

        let path_for_save = path.clone();
        match tokio::task::spawn_blocking(move || learning.save(&path_for_save)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => eprintln!(
                "hopd: failed to save the learning store to {}: {err}",
                path.display()
            ),
            Err(join_err) => eprintln!(
                "hopd: learning store save task for {} panicked: {join_err}",
                path.display()
            ),
        }
    }
}

/// The daemon's [`ProviderLog`]: one line per event on stderr.
///
/// Deliberately the crudest thing that satisfies issue #34's criterion, and
/// consistent with how this crate already reports — [`crate::server::serve_with`]
/// logs accept and connection errors with `eprintln!` too. Spec §9's
/// `tracing` with an env-filter is the eventual backend, and the
/// [`ProviderLog`] seam is what lets it arrive without touching a call site.
pub struct StderrLog;

impl ProviderLog for StderrLog {
    fn record(&self, event: ProviderEvent<'_>) {
        match event {
            // Answered is the common *successful* case — most selected
            // providers answer most queries — so, like Skipped below,
            // logging it per keystroke would bury the events worth reading.
            ProviderEvent::Answered { .. } => {}
            ProviderEvent::Failed(failure) => eprintln!(
                "hopd: provider {} failed ({:?}) after {:?}: {}",
                failure.provider(),
                failure.kind(),
                failure.elapsed(),
                failure.message()
            ),
            ProviderEvent::BudgetMiss {
                provider,
                budget,
                elapsed,
            } => eprintln!(
                "hopd: provider {provider} missed its {budget:?} budget after {elapsed:?}"
            ),
            ProviderEvent::Rejected {
                provider,
                rejections,
            } => eprintln!("{}", rejection_summary_line(provider, rejections)),
            // Skipped is the common case by design — most keystrokes reach
            // most providers not at all — so logging it per keystroke would
            // bury everything above it.
            ProviderEvent::Skipped { .. } => {}
        }
    }
}

/// Builds [`StderrLog`]'s one line for a [`ProviderEvent::Rejected`] event.
///
/// A plain `rejections.len()` used to be the whole count, reported as "N
/// item(s) refused by its own manifest". That was accurate back when
/// [`FailedCheck`] had only [`FailedCheck::Kind`] and
/// [`FailedCheck::Provenance`] — both, genuinely, the item's own manifest
/// lying. It stopped being accurate once [`FailedCheck::FieldTooLong`] and
/// [`FailedCheck::TooManyItems`] existed: a `FieldTooLong` rejection has
/// nothing to do with the manifest (it's a field-size violation), and one
/// `TooManyItems` rejection stands for its whole `excess` — potentially
/// thousands of dropped items — not the single item `rejections.len()` would
/// count it as. Both the count and the stated cause were wrong.
///
/// This reports a truthful total (a `TooManyItems` rejection contributes its
/// `excess`, not 1) broken down by the four causes that can actually reach
/// [`ProviderEvent::Rejected`] here, one line, in [`StderrLog`]'s existing
/// `"hopd: provider ..."` voice.
fn rejection_summary_line(provider: &str, rejections: &[Rejection]) -> String {
    let (mut bad_kind, mut forged_provenance, mut field_too_long, mut over_the_count_cap) =
        (0usize, 0usize, 0usize, 0usize);

    for rejection in rejections {
        match rejection.check {
            FailedCheck::Kind => bad_kind += 1,
            FailedCheck::Provenance => forged_provenance += 1,
            FailedCheck::FieldTooLong { .. } => field_too_long += 1,
            FailedCheck::TooManyItems { excess } => over_the_count_cap += excess,
            // `ProviderHost::run_one` builds this event straight from
            // `CheckedItems::check(vec![output])`'s own rejections, for one
            // provider's one answer — and `check` itself never produces a
            // `PinBudget` rejection; that variant is only ever produced
            // later, inside `Pipeline::assemble`'s pin-budget split, which
            // this single-provider `check()` call never runs. If that ever
            // stops being true, this should fail loudly rather than silently
            // under-count.
            FailedCheck::PinBudget => unreachable!(
                "a PinBudget rejection reached ProviderEvent::Rejected, which \
                 only ever carries CheckedItems::check's own rejections — \
                 check() never produces PinBudget"
            ),
        }
    }

    let total = bad_kind + forged_provenance + field_too_long + over_the_count_cap;
    format!(
        "hopd: provider {provider} had {total} item(s) rejected \
         ({bad_kind} bad kind, {forged_provenance} forged provenance, \
         {field_too_long} field too long, {over_the_count_cap} dropped over \
         the per-answer item cap)"
    )
}

/// The walking skeleton's one and only result: every `query` frame gets
/// exactly this item back, regardless of what was typed.
pub(crate) fn hardcoded_item() -> Item {
    Item {
        id: ItemId::new("hop:walking-skeleton").expect("within bounds by construction"),
        kind: Kind::Action,
        title: "Hello from hopd".to_string(),
        subtitle: Some("M2.2 walking skeleton".to_string()),
        icon: None,
        actions: vec![Action {
            id: ActionId::new("open").expect("within bounds by construction"),
            kind: ActionKind::Open,
            label: "Open".to_string(),
        }],
        default_action: ActionId::new("open").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: "skeleton".to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    use hop_core::host::{NoopLog, ProviderHost};
    use std::sync::Arc;

    /// A [`Rejection`] naming `check`, otherwise filled with placeholder
    /// values `rejection_summary_line` never reads.
    fn rejection(check: FailedCheck) -> Rejection {
        Rejection {
            item_id: ItemId::new("provider:item-0").unwrap(),
            claimed_kind: Kind::App,
            claimed_provider: "provider".to_string(),
            producer_id: "provider".to_string(),
            check,
        }
    }

    #[test]
    fn the_rejection_summary_line_counts_a_too_many_items_rejection_by_its_excess_not_as_one() {
        // Pins the defect this function exists to fix: a single
        // `TooManyItems` rejection used to be counted as exactly 1 item by
        // `rejections.len()`, undercounting a dropped tail of thousands down
        // to 1. `excess` here is deliberately far larger than 1 to make an
        // undercount impossible to miss.
        let line = rejection_summary_line(
            "files",
            &[rejection(FailedCheck::TooManyItems { excess: 4_321 })],
        );
        assert_eq!(
            line,
            "hopd: provider files had 4321 item(s) rejected (0 bad kind, \
             0 forged provenance, 0 field too long, 4321 dropped over the \
             per-answer item cap)"
        );
    }

    #[test]
    fn the_rejection_summary_line_states_field_too_long_as_its_own_cause_not_the_manifest() {
        // The stale message called every rejection "refused by its own
        // manifest" — true for Kind/Provenance, never true for a field-size
        // violation. This pins that the cause is now named accurately.
        let line = rejection_summary_line(
            "files",
            &[rejection(FailedCheck::FieldTooLong {
                field: "Item.title",
            })],
        );
        assert_eq!(
            line,
            "hopd: provider files had 1 item(s) rejected (0 bad kind, \
             0 forged provenance, 1 field too long, 0 dropped over the \
             per-answer item cap)"
        );
    }

    #[test]
    fn the_rejection_summary_line_sums_a_mix_of_causes_truthfully() {
        let line = rejection_summary_line(
            "files",
            &[
                rejection(FailedCheck::Kind),
                rejection(FailedCheck::Kind),
                rejection(FailedCheck::Provenance),
                rejection(FailedCheck::FieldTooLong {
                    field: "Item.subtitle",
                }),
                rejection(FailedCheck::TooManyItems { excess: 10 }),
            ],
        );
        assert_eq!(
            line,
            "hopd: provider files had 14 item(s) rejected (2 bad kind, \
             1 forged provenance, 1 field too long, 10 dropped over the \
             per-answer item cap)"
        );
    }

    #[tokio::test]
    async fn the_skeleton_provider_answers_through_the_host() {
        // The walking skeleton's item, reached the way every later provider
        // will be: registered with the host, selected by its captured
        // manifest, streamed, and — since issue #103 — assembled. The term
        // has to fuzzy-match the item's haystack (`Hello from hopd` + `M2.2
        // walking skeleton`) now that `Ranker::rank` drops anything that
        // doesn't; unlike the canary tests over a real daemon, this is a
        // single hand-registered provider, so an exact count is safe to
        // assert here.
        let mut host = ProviderHost::with_log(Arc::new(NoopLog));
        host.register(SkeletonProvider).unwrap();
        let source = HostSource::new(Arc::new(host));

        let mut rx = source.start(QueryText::new("walking skeleton").unwrap());
        let batch = rx.recv().await.expect("one batch must arrive");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].title, "Hello from hopd");
        assert!(
            rx.recv().await.is_none(),
            "the channel closes once the one provider has finished"
        );
    }

    #[tokio::test]
    async fn the_skeleton_providers_own_item_passes_its_own_manifest() {
        // If it did not, the host would refuse it and the walking skeleton
        // would silently answer nothing — so this is what keeps the in-tree
        // worked example honest, the same guarantee `hop-core`'s
        // `a_providers_own_output_passes_its_own_manifests_checks` gives.
        let manifest = SkeletonProvider.manifest();
        let item = hardcoded_item();
        assert_eq!(item.provider, manifest.id);
        assert!(manifest.kinds.contains(&item.kind));
    }

    // --- Task 1 (issue #103): the forwarding hop must not break the
    // `ResultSource` cancellation contract. ---
    //
    // `HostSource::start` now has two channels and a task between them where
    // it used to have one channel and nothing. The three providers below
    // exist only to make that hop's cancellation observable from outside it,
    // since `start` no longer hands out the `CancellationFlag`
    // `ProviderHost::spawn_query` returns (nothing downstream of `HostSource`
    // ever held it, even before this task).

    use hop_core::host::HostPolicy;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Answers immediately with one well-formed item. Its purpose is to give
    /// the forwarding task something to receive and try (and, once the test
    /// has dropped its receiver, fail) to forward — the trigger that makes
    /// the task discover the downstream send is dead.
    struct InstantProvider;

    impl Provider for InstantProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "instant",
                kinds: vec![Kind::Action],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
                ids_are_safe_to_persist_in_the_clear: false,
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Ok(vec![Item {
                id: ItemId::new("instant:item").expect("within bounds by construction"),
                kind: Kind::Action,
                title: "Instant".to_string(),
                subtitle: None,
                icon: None,
                actions: vec![],
                default_action: ActionId::new("open").expect("within bounds by construction"),
                copy_text: None,
                append_to_end: false,
                provider: "instant".to_string(),
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

    /// Answers after a short sleep — long enough that, in a correctly
    /// cancelling implementation, [`InstantProvider`]'s answer has already
    /// collapsed the forwarding task and closed the host's own channel by
    /// the time this provider's `run_one` tries to send. *That* send is the
    /// one whose failure sets the shared [`CancellationFlag`](hop_core::provider::CancellationFlag)
    /// — see `ProviderHost::spawn_query`'s docs: cancellation is a property
    /// of a provider's own send failing, not of the channel merely existing
    /// in a closed state, so a lone fast provider whose send always succeeds
    /// (because nothing has failed yet when it sends) could never trigger it
    /// on its own.
    struct DelayedProvider;

    impl Provider for DelayedProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "delayed",
                kinds: vec![Kind::Action],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(100),
                ids_are_safe_to_persist_in_the_clear: false,
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(vec![Item {
                id: ItemId::new("delayed:item").expect("within bounds by construction"),
                kind: Kind::Action,
                title: "Delayed".to_string(),
                subtitle: None,
                icon: None,
                actions: vec![],
                default_action: ActionId::new("open").expect("within bounds by construction"),
                copy_text: None,
                append_to_end: false,
                provider: "delayed".to_string(),
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

    /// Never completes on its own: loops polling `ctx.cancel` and records
    /// that it saw cancellation before giving up. The one provider here that
    /// actually reads its `QueryCtx`, and so the only way this test can tell
    /// "the providers were told to stop" apart from "the providers happened
    /// to finish, or were eventually cut off at their own budget" — the
    /// latter would still close the outer channel, so watching the channel
    /// alone cannot distinguish a genuine cancellation from a slow one.
    struct CooperativeProvider {
        saw_cancellation: Arc<AtomicBool>,
    }

    impl Provider for CooperativeProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "cooperative",
                kinds: vec![Kind::Action],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(200),
                ids_are_safe_to_persist_in_the_clear: false,
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            while !ctx.cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            self.saw_cancellation.store(true, Ordering::SeqCst);
            Err(ProviderError::Cancelled)
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// The seam this task adds — the forwarding task between `HostSource`'s
    /// two channels — must not break the `ResultSource` contract that
    /// dropping the receiver cancels the providers behind it. This is the
    /// test named in the brief; see its own docs for the buggy body ("ignore
    /// a failed send and keep looping") that was confirmed to fail it before
    /// the forwarding task was written to return on a failed send instead.
    #[tokio::test]
    async fn dropping_the_forwarded_receiver_cancels_the_query() {
        // The default `HostPolicy` ceiling (50 ms, `MAX_PROVIDER_BUDGET`) is
        // narrower than `CooperativeProvider`'s declared budget, which would
        // clamp it down and leave no room to tell "cancelled promptly" apart
        // from "cut off at budget" — the same reason `hop-core`'s own
        // `a_hanging_provider_does_not_delay_a_fast_providers_batch` widens
        // it.
        let policy = HostPolicy {
            max_budget: Duration::from_millis(300),
            ..HostPolicy::default()
        };
        let mut host = ProviderHost::new(policy, Arc::new(NoopLog));
        host.register(InstantProvider).unwrap();
        host.register(DelayedProvider).unwrap();
        let saw_cancellation = Arc::new(AtomicBool::new(false));
        host.register(CooperativeProvider {
            saw_cancellation: saw_cancellation.clone(),
        })
        .unwrap();

        let source = HostSource::new(Arc::new(host));
        let rx = source.start(QueryText::new("anything").unwrap());
        drop(rx);

        // Poll rather than sleep a fixed amount, the same pattern
        // `hop-core`'s own cancellation tests use: succeed the moment the
        // flag goes true, and only fail after giving a correct
        // implementation ample room.
        for _ in 0..100 {
            if saw_cancellation.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "dropping the receiver HostSource::start returns must cancel the \
             providers behind the forwarding task, not just eventually close \
             the outer channel once they happen to finish or are cut off at \
             their own budget"
        );
    }

    // --- Task 2 (issue #103): the accumulator — `assemble` on every
    // arrival. ---

    /// A provider that answers with a fixed, pre-built list of items after an
    /// optional delay. The single-purpose providers above (`InstantProvider`
    /// and friends) each hardcode one item; the tests below need providers
    /// whose item *count* and *content* vary per test, so this is the
    /// general-purpose stand-in for all of them.
    struct ItemsProvider {
        id: &'static str,
        kinds: Vec<Kind>,
        items: Vec<Item>,
        delay: Duration,
        budget: Duration,
    }

    impl Provider for ItemsProvider {
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
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
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

    /// An `ItemsProvider` item: `provider` is a separate argument rather than
    /// read off `id`'s namespace, so a test can deliberately mismatch it —
    /// none here do, but every caller has to say the two agree rather than
    /// getting that for free.
    fn item(kind: Kind, id: &str, title: &str, provider: &str) -> Item {
        Item {
            id: ItemId::new(id).unwrap(),
            kind,
            title: title.to_string(),
            subtitle: None,
            icon: None,
            actions: vec![Action {
                id: ActionId::new("open").unwrap(),
                kind: ActionKind::Open,
                label: "Open".into(),
            }],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: provider.to_string(),
        }
    }

    #[tokio::test]
    async fn each_arrival_re_assembles_over_every_item_received_so_far() {
        // Both items fuzzy-match "fire", so both survive ranking; the point
        // under test is which provider's items are present in each frame,
        // not their order.
        let fast = ItemsProvider {
            id: "fast",
            kinds: vec![Kind::App],
            items: vec![item(Kind::App, "fast:item", "Firefox", "fast")],
            delay: Duration::ZERO,
            budget: Duration::from_millis(20),
        };
        let slow = ItemsProvider {
            id: "slow",
            kinds: vec![Kind::App],
            items: vec![item(Kind::App, "slow:item", "Fire Alarm", "slow")],
            delay: Duration::from_millis(60),
            budget: Duration::from_millis(200),
        };
        let policy = HostPolicy {
            max_budget: Duration::from_millis(200),
            ..HostPolicy::default()
        };
        let mut host = ProviderHost::new(policy, Arc::new(NoopLog));
        host.register(fast).unwrap();
        host.register(slow).unwrap();

        let source = HostSource::new(Arc::new(host));
        let mut rx = source.start(QueryText::new("fire").unwrap());

        let first = rx
            .recv()
            .await
            .expect("the fast provider's arrival must send a frame");
        let first_ids: Vec<&str> = first.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            first_ids,
            vec!["fast:item"],
            "the first frame must hold only the fast provider's items — the \
             slow one has not arrived yet"
        );

        let second = rx
            .recv()
            .await
            .expect("the slow provider's arrival must send a second frame");
        let mut second_ids: Vec<&str> = second.iter().map(|i| i.id.as_str()).collect();
        second_ids.sort_unstable();
        assert_eq!(
            second_ids,
            vec!["fast:item", "slow:item"],
            "the second frame must hold both providers' items, assembled \
             together — an implementation that sends only the newly-arrived \
             provider's assembled items would drop \"fast:item\" here"
        );

        assert!(
            rx.recv().await.is_none(),
            "channel closes once both providers have finished"
        );
    }

    #[tokio::test]
    async fn the_first_frame_is_sent_without_waiting_for_the_slow_provider() {
        let fast = ItemsProvider {
            id: "fast2",
            kinds: vec![Kind::App],
            items: vec![item(Kind::App, "fast2:item", "Quickstart", "fast2")],
            delay: Duration::ZERO,
            budget: Duration::from_millis(20),
        };
        let slow = ItemsProvider {
            id: "slow2",
            kinds: vec![Kind::App],
            items: vec![item(Kind::App, "slow2:item", "Quickstart Plus", "slow2")],
            delay: Duration::from_millis(250),
            budget: Duration::from_millis(400),
        };
        let policy = HostPolicy {
            max_budget: Duration::from_millis(400),
            ..HostPolicy::default()
        };
        let mut host = ProviderHost::new(policy, Arc::new(NoopLog));
        host.register(fast).unwrap();
        host.register(slow).unwrap();

        let source = HostSource::new(Arc::new(host));
        let mut rx = source.start(QueryText::new("quickstart").unwrap());

        // The slow provider's own 250 ms delay (and its 400 ms budget) are
        // both far longer than this 100 ms timeout, so the first frame
        // arriving inside it proves nothing gated on the slow provider.
        let first = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect(
                "the first frame must arrive without waiting for the slow \
                 provider — a gate on the slowest provider would time out here",
            )
            .expect("a frame must arrive");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, ItemId::new("fast2:item").unwrap());
    }

    #[tokio::test]
    async fn max_results_is_applied_to_the_whole_assembled_set_not_per_provider() {
        // Two providers, 30 items each — each under `MAX_RESULTS` (50) on its
        // own, 60 together, over it. `low` is `Kind::File` (rank weight 12),
        // `high` is `Kind::Window` (weight 30), so with the empty query term
        // (`Matching::Everything` — fuzzy score plays no part) every `high`
        // item outranks every `low` item on kind weight alone, and equal
        // weight within a provider ties on title, ascending. The correct top
        // 50 is therefore deterministic: all 30 `high` items, plus the 20
        // alphabetically-first `low` items.
        //
        // `low` answers immediately and `high` after a short delay, so
        // `low`'s own 30-item batch is assembled and sent *alone* on the
        // first frame. An implementation that assembles each arrival's batch
        // separately and concatenates the running results — instead of
        // re-running `assemble` over the whole accumulated set on every
        // arrival — would keep all 30 already-sent `low` items on `high`'s
        // arrival and only admit `high`'s first 20 (in `high`'s own
        // internal order) to fill the remaining room, dropping ten `high`
        // items that outrank every `low` item. That produces a *different
        // 50 items* than the correct assembly, at the same length — which
        // is why this test asserts identity, not count: a naive
        // `assert_eq!(second.len(), 50)` would pass against that bug too.
        let low_items: Vec<Item> = (0..30)
            .map(|n| {
                item(
                    Kind::File,
                    &format!("low:{n:02}"),
                    &format!("low-{n:02}"),
                    "low",
                )
            })
            .collect();
        let high_items: Vec<Item> = (0..30)
            .map(|n| {
                item(
                    Kind::Window,
                    &format!("high:{n:02}"),
                    &format!("high-{n:02}"),
                    "high",
                )
            })
            .collect();

        let low = ItemsProvider {
            id: "low",
            kinds: vec![Kind::File],
            items: low_items,
            delay: Duration::ZERO,
            budget: Duration::from_millis(20),
        };
        let high = ItemsProvider {
            id: "high",
            kinds: vec![Kind::Window],
            items: high_items,
            delay: Duration::from_millis(60),
            budget: Duration::from_millis(200),
        };
        let policy = HostPolicy {
            max_budget: Duration::from_millis(200),
            ..HostPolicy::default()
        };
        let mut host = ProviderHost::new(policy, Arc::new(NoopLog));
        host.register(low).unwrap();
        host.register(high).unwrap();

        let source = HostSource::new(Arc::new(host));
        let mut rx = source.start(QueryText::new("").unwrap());

        let _first = rx
            .recv()
            .await
            .expect("the low provider's arrival must send a frame");
        let second = rx
            .recv()
            .await
            .expect("the high provider's arrival must send a second frame");

        assert_eq!(
            second.len(),
            MAX_RESULTS,
            "the assembled frame must hold exactly MAX_RESULTS items"
        );

        let mut actual: Vec<String> = second.iter().map(|i| i.id.as_str().to_string()).collect();
        actual.sort_unstable();
        let mut expected: Vec<String> = (0..30)
            .map(|n| format!("high:{n:02}"))
            .chain((0..20).map(|n| format!("low:{n:02}")))
            .collect();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "the survivors must be every \"high\" item plus the 20 \
             highest-ranked \"low\" items — a per-batch-then-concatenate \
             implementation would instead keep every \"low\" item and only \
             \"high\"'s first 20"
        );
    }

    #[tokio::test]
    async fn with_config_assembles_to_the_configured_max_results_not_the_default() {
        // `with_config`'s `max_results` is what the accumulator hands the
        // pipeline on every assembly, so a value other than `MAX_RESULTS`
        // must be honored: two providers give 30 items each (60 together,
        // over both the configured 25 and the 50 default), and the final
        // assembled frame must hold exactly the configured 25 — not
        // `MAX_RESULTS`. This is the wiring issue #60's `run()` drives, so
        // proving it here pins the seam in-process without spawning a binary.
        let low_items: Vec<Item> = (0..30)
            .map(|n| {
                item(
                    Kind::File,
                    &format!("low:{n:02}"),
                    &format!("low-{n:02}"),
                    "low",
                )
            })
            .collect();
        let high_items: Vec<Item> = (0..30)
            .map(|n| {
                item(
                    Kind::Window,
                    &format!("high:{n:02}"),
                    &format!("high-{n:02}"),
                    "high",
                )
            })
            .collect();

        let low = ItemsProvider {
            id: "low",
            kinds: vec![Kind::File],
            items: low_items,
            delay: Duration::ZERO,
            budget: Duration::from_millis(20),
        };
        let high = ItemsProvider {
            id: "high",
            kinds: vec![Kind::Window],
            items: high_items,
            delay: Duration::from_millis(60),
            budget: Duration::from_millis(200),
        };
        let policy = HostPolicy {
            max_budget: Duration::from_millis(200),
            ..HostPolicy::default()
        };
        let mut host = ProviderHost::new(policy, Arc::new(NoopLog));
        host.register(low).unwrap();
        host.register(high).unwrap();

        let source = HostSource::with_config(
            Arc::new(host),
            Arc::new(Mutex::new(Pipeline::default())),
            25,
            None,
        );
        let mut rx = source.start(QueryText::new("").unwrap());

        let _first = rx
            .recv()
            .await
            .expect("the low provider's arrival must send a frame");
        let second = rx
            .recv()
            .await
            .expect("the high provider's arrival must send a frame");

        assert_eq!(
            second.len(),
            25,
            "with_config must assemble to its configured 25 max_results, \
             not the MAX_RESULTS default of 50"
        );
    }

    #[tokio::test]
    async fn the_accumulator_caps_at_max_items_per_query_and_ends_the_query() {
        // `MAX_ITEMS_PER_QUERY` items of low rank weight (`Kind::File`), plus
        // one more of much higher weight (`Kind::Window`) appended last. If
        // the accumulator's cap did not apply to the *incoming* batch before
        // absorbing it, that last, highest-weight item would win the
        // assembled top `MAX_RESULTS` outright — observing its absence is
        // what proves truncation happened, rather than trusting a length
        // assertion `assemble`'s own `max_results` truncation would satisfy
        // either way.
        let mut items: Vec<Item> = (0..MAX_ITEMS_PER_QUERY)
            .map(|n| {
                item(
                    Kind::File,
                    &format!("flood:{n:04}"),
                    &format!("file-{n:04}"),
                    "flood",
                )
            })
            .collect();
        items.push(item(
            Kind::Window,
            "flood:winner",
            "should-be-dropped",
            "flood",
        ));

        let flood = ItemsProvider {
            id: "flood",
            kinds: vec![Kind::File, Kind::Window],
            items,
            delay: Duration::ZERO,
            budget: Duration::from_millis(50),
        };
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(flood).unwrap();

        let source = HostSource::new(Arc::new(host));
        let mut rx = source.start(QueryText::new("").unwrap());

        let frame = rx
            .recv()
            .await
            .expect("the flooding provider's arrival must still send a frame");
        assert_eq!(frame.len(), MAX_RESULTS);
        assert!(
            frame.iter().all(|i| i.title != "should-be-dropped"),
            "the item past MAX_ITEMS_PER_QUERY must be truncated away by the \
             accumulator before assembly ever sees it, even though its \
             Window weight would otherwise make it the single top-ranked \
             survivor"
        );

        assert!(
            rx.recv().await.is_none(),
            "a capped query ends the exchange: the accumulator returns \
             after sending the capped frame, dropping the host's receiver \
             and closing this channel"
        );
    }

    #[tokio::test]
    async fn a_provider_answering_with_no_items_still_sends_a_frame() {
        // "zzzz" cannot fuzzy-match "Widget" — no 'z' anywhere in its
        // haystack — so `Ranker::rank` drops the item and assembly's output
        // is empty. Design decision 6: the arrival still produces a frame
        // rather than being silently suppressed for having "nothing new" to
        // say.
        let provider = ItemsProvider {
            id: "no_match",
            kinds: vec![Kind::Action],
            items: vec![item(Kind::Action, "no_match:widget", "Widget", "no_match")],
            delay: Duration::ZERO,
            budget: Duration::from_millis(20),
        };
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(provider).unwrap();

        let source = HostSource::new(Arc::new(host));
        let mut rx = source.start(QueryText::new("zzzz").unwrap());

        let frame = rx
            .recv()
            .await
            .expect("the provider's arrival must still trigger a frame");
        assert!(
            frame.is_empty(),
            "the term matches nothing, so the assembled frame is empty"
        );

        assert!(
            rx.recv().await.is_none(),
            "channel closes once the one provider has finished"
        );
    }
}
