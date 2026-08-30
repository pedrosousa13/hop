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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hop_core::host::{ProviderEvent, ProviderHost, ProviderLog};
use hop_core::pipeline::{CheckedItems, FailedCheck, Pipeline, Rejection};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery, route};
use hop_core::sanitize::escape_path;
use hop_protocol::{
    Action, ActionId, ActionKind, ExecOutcome, Item, ItemId, ItemSubtitle, ItemTitle, Kind,
    MAX_ITEMS_PER_QUERY, MAX_ITEMS_PER_RESULTS_FRAME, MAX_PENDING_PROVIDERS, QueryText,
    RecentItem,
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
/// Four obligations. Issue #85 promoted the first of them — the per-item
/// field-bound check — from prose to a contract the type system enforces;
/// the other three remain obligations this seam does not check, and
/// [`HostSource`] is the first implementation with enough surface to break
/// any of them. Read this rather than assume landing issue #56 settled all
/// of them, because it did not.
///
/// **Items must respect `hop_protocol::limits`' per-item field bounds — and,
/// as of issue #85, this is no longer prose asking an implementation to
/// remember it.** [`Item`]'s action fields are public, and those bounds are
/// applied where an item is *parsed*, so an `Item` built in-process carries
/// no proof of its own that it ever crossed that check. `ItemTitle` and
/// `ItemSubtitle` enforce their own bounds and single-line content rule on
/// every construction path regardless of origin, but an item's `actions` —
/// their count, and each label's length — are plain `String`s and `Vec`s
/// with nothing enforcing either outside the parse. The daemon bounds its
/// retained set by item *count*
/// ([`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY)), and
/// the byte figure that count is justified against is the count multiplied by
/// those per-item bounds — so a source producing a 100 MB action label makes
/// that arithmetic, and the bound it justifies, meaningless. The only thing
/// below it is
/// [`MAX_FRAME_BYTES`](hop_protocol::limits::MAX_FRAME_BYTES) at encode time,
/// which surfaces as an `io::Error` that kills the connection with no error
/// frame — a worse outcome than refusing the item would have been.
///
/// [`start`](Self::start) closes this gap by construction rather than by
/// convention: the channel it returns carries
/// [`CheckedItems`], not a bare
/// `Vec<Item>`, and that type's only constructors are
/// [`CheckedItems::check`] and the combinators
/// ([`CheckedItems::absorb`], [`CheckedItems::truncate_items`],
/// [`CheckedItems::truncate_items_recording_overflow`]) that only ever
/// recombine values `check` already produced. There is no way to build a
/// `CheckedItems` holding an item whose action label or action count is over
/// [`FailedCheck::FieldTooLong`]'s bound — the compiler refuses an
/// implementation that tries, the same way it refuses one that tries to
/// construct [`hop_core::pipeline::CheckedItems`]'s private fields directly.
/// Before this issue, this held only because [`HostSource`] happened to
/// route every provider's answer through [`CheckedItems::check`] before
/// sending; a hypothetical implementation that built its own `Vec<Item>` and
/// handed it back some other way reached this trait exactly as unchecked as
/// every other one. That escape no longer typechecks: an implementation can
/// still hand back a *forged* item — the wrong `kind`, the wrong `provider`
/// — nothing here catches a lie about content, only a bound. But it cannot
/// hand back one whose action fields were never measured against the bound
/// at all, because there is no route to this trait's channel that does not
/// pass through the check that measures them.
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
/// closed at the seam upstream of this trait rather than inside it. This
/// half remains a property of [`HostSource`] specifically, not a guarantee
/// the *trait*'s contract makes — unlike the field-bound obligation above,
/// nothing about this trait's channel type stops an implementation from
/// opening a deeper channel or sending an oversized batch; the source-side
/// buffering obligation is related and still open (issue #85 is explicit
/// that it is out of that issue's scope).
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
    ///
    /// The channel carries [`CheckedItems`], not `Vec<Item>` — issue #85's
    /// enforcement of this trait's per-item field-bound obligation (see this
    /// trait's own docs, above). Building a `CheckedItems` with an item that
    /// never crossed [`CheckedItems::check`] does not typecheck, so an
    /// implementation reaches this return type only by constructing one for
    /// real: call `check` over what a provider (or a test's own fixture)
    /// returned, or combine values that already went through it
    /// ([`CheckedItems::absorb`], [`CheckedItems::truncate_items`],
    /// [`CheckedItems::truncate_items_recording_overflow`]). A source with
    /// no `hop-core` [`Provider`] behind it — every scripted or test source
    /// in this crate — still owes the check; see [`HostSource::start`]'s own
    /// implementation for the production path, and any test source in this
    /// crate for the pattern a fixture-driven one uses instead.
    fn start(&self, text: QueryText) -> mpsc::Receiver<CheckedItems>;

    /// Returns the provider ids this source selected for `text`, in the order
    /// it will ask them. The connection sends this snapshot with
    /// `QueryRouted`, before [`ResultSource::start`] can yield an arrival, so
    /// a client can attribute pending work without inventing a provider list.
    ///
    /// Scripted sources default to no attribution so existing tests and narrow
    /// sources do not claim providers they never schedule. A production source
    /// that knows its scheduler must override this with the scheduler's actual
    /// selection, not infer it from result items after the fact.
    fn pending_providers(&self, _text: &QueryText) -> Vec<String> {
        Vec::new()
    }

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

    /// Resolves persisted learning launches against the live items in an
    /// empty-query result. The default keeps scripted sources unchanged; the
    /// production source performs canonical-key matching inside its daemon
    /// boundary and never exposes unresolved or hashed keys.
    fn recent_items(&self, _items: &[Item]) -> impl Future<Output = Vec<RecentItem>> + Send {
        async { Vec::new() }
    }
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

const MAX_RECENT_ITEMS: usize = 5;
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

/// Applies `cap` to `accumulated`'s running total before absorbing
/// `checked`'s newly-arrived items into it, and reports whether doing so
/// left the accumulator completely full.
///
/// Split out of [`HostSource::start`]'s accumulator loop as its own function
/// so this cap-and-record step — the daemon's half of issue #85's ruling on
/// overflow — is unit-testable directly, against plain [`CheckedItems`]
/// values, without a [`ProviderHost`] or a `tokio` runtime behind it. See
/// the tests below for exactly that.
///
/// `cap` bounds `accumulated.items().len()` after this call returns, never
/// before: the newly-arrived `checked` is truncated to whatever room was
/// left, not `accumulated` itself, so an item this daemon already delivered
/// under an earlier arrival is never retroactively dropped — only ever an
/// item from the batch that pushed the total over. Truncating with
/// [`CheckedItems::truncate_items_recording_overflow`] rather than
/// [`CheckedItems::truncate_items`] is issue #85's whole point here: the
/// items that fit are kept exactly as before, but what did not fit is now
/// named by a [`Rejection`] ([`FailedCheck::TooManyItemsPerQuery`]) riding
/// along inside `accumulated` after `absorb`, never a silent truncation and
/// never a refusal of the whole set.
///
/// Returns `true` when `checked`'s own length reached or passed the room
/// that was left, `accumulated`'s cap included — filling the room exactly
/// is still reported as capped even though nothing was dropped and no
/// rejection was recorded, because an accumulator with no room left has
/// nothing to give a later arrival: ending the query now is the same answer
/// [`HostSource::start`]'s caller would reach one arrival later, at the cost
/// of a round trip this saves.
fn absorb_capped(accumulated: &mut CheckedItems, mut checked: CheckedItems, cap: usize) -> bool {
    let room = cap.saturating_sub(accumulated.items().len());
    let capped = checked.items().len() >= room;
    if capped {
        checked.truncate_items_recording_overflow(room);
    }
    accumulated.absorb(checked);
    capped
}

impl ResultSource for HostSource {

    fn pending_providers(&self, text: &QueryText) -> Vec<String> {
        let routed = route(text.as_str());
        self.host
            .selected_ids(&routed)
            .into_iter()
            .take(MAX_PENDING_PROVIDERS)
            .map(str::to_owned)
            .collect()
    }
    fn start(&self, text: QueryText) -> mpsc::Receiver<CheckedItems> {
        // Two channels, each capacity 1 for the reason this trait's docs
        // give: what a source buffers is daemon memory the retained-set cap
        // does not see, so a deeper channel would only let providers park
        // items the cap never counts. Both now speak `CheckedItems` — issue
        // #85 made this trait's own channel carry it too, so the accumulator
        // task below no longer has to unwrap `ProviderHost::spawn_query`'s
        // `CheckedItems` down to a bare `Vec<Item>` before it can send.
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

            while let Some(checked) = host_rx.recv().await {
                let capped = absorb_capped(&mut accumulated, checked, MAX_ITEMS_PER_QUERY);

                // Locked only across `assemble_checked` itself, never across
                // the `send` below — assembly is synchronous, so holding the
                // guard past it would block every other query sharing this
                // `Pipeline` for the length of an `.await` for no reason.
                let checked_assembly = {
                    let mut pipeline = pipeline.lock().await;
                    pipeline.assemble_checked(text.as_str(), accumulated.clone(), max_results)
                };
                // `checked_assembly`'s rejections travel out with it now
                // (issue #85), rather than being discarded here the way the
                // old `Assembly::rejections` was: `absorb_capped` folded in
                // one `FailedCheck::TooManyItemsPerQuery` rejection whenever
                // this arrival overflowed the per-query cap, and
                // `assemble_checked` carries that straight through alongside
                // anything `Pipeline::assemble`'s own pin-budget step minted.
                // Nothing downstream of this `send` reads them yet — the
                // host already logged the manifest-check half through its
                // own log seam before this task ever saw the item
                // (`ProviderHost::run_one`), and the pin-budget half stays
                // unlogged, as it was before this issue (out of scope; see
                // the design plan's Scope section) — but they are no longer
                // thrown away either: a caller of this channel (a test, or a
                // future log seam) can read `checked_assembly.rejections()`
                // for itself.

                if tx.send(checked_assembly).await.is_err() {
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
        // `path` is `learning_path`: environment-derived (issue #159), the
        // same way the config path is, so it runs through `escape_path`
        // rather than `path.display()` before reaching stderr.
        match tokio::task::spawn_blocking(move || learning.save(&path_for_save)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => eprintln!("{}", learning_save_failed_line(path, &err)),
            Err(join_err) => eprintln!("{}", learning_save_panicked_line(path, &join_err)),
        }
    }

    fn recent_items(&self, items: &[Item]) -> impl Future<Output = Vec<RecentItem>> + Send {
        let items = items.to_vec();
        let pipeline = Arc::clone(&self.pipeline);
        async move {
            let pipeline = pipeline.lock().await;
            pipeline
                .learning
                .recent_items_for(&items, MAX_RECENT_ITEMS)
                .into_iter()
                .map(|(item, launched_at_ms)| RecentItem {
                    item,
                    launched_at_ms,
                })
                .collect()
        }
    }
}

/// Builds the line [`HostSource::record_launch`] writes to stderr when
/// saving the learning store to `path` returns `err`. Extracted as a pure
/// function — mirroring `apps.rs`'s `malformed_log_line`, which exists for
/// the identical reason — because capturing stderr in a unit test needs
/// either a new dependency or `unsafe` fd redirection, and this workspace
/// forbids both. Asserting on this function's return value is as close as a
/// test can get to pinning what `record_launch` actually sends to stderr.
///
/// `path` runs through [`escape_path`], not `path.display()` (issue #159):
/// `learning_path` is environment-derived, the same way the config path is
/// — see `record_launch`'s own doc comment.
fn learning_save_failed_line(path: &Path, err: &io::Error) -> String {
    format!(
        "hopd: failed to save the learning store to {}: {err}",
        escape_path(path)
    )
}

/// Builds the line [`HostSource::record_launch`] writes to stderr when the
/// blocking save task itself panicked (`join_err`) rather than `save`
/// returning an error. Same extraction rationale, and the same issue #159
/// treatment of `path`, as [`learning_save_failed_line`].
fn learning_save_panicked_line(path: &Path, join_err: &tokio::task::JoinError) -> String {
    format!(
        "hopd: learning store save task for {} panicked: {join_err}",
        escape_path(path)
    )
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
            // Same reasoning as the `PinBudget` arm above, for the same
            // reason: `TooManyItemsPerQuery` (issue #85) is minted by
            // `CheckedItems::truncate_items_recording_overflow`, called only
            // by this module's own per-query accumulator
            // (`HostSource::start`) — never by `check()` itself, and so
            // never by anything `ProviderEvent::Rejected` can carry.
            FailedCheck::TooManyItemsPerQuery { .. } => unreachable!(
                "a TooManyItemsPerQuery rejection reached ProviderEvent::Rejected, \
                 which only ever carries CheckedItems::check's own rejections — \
                 check() never produces TooManyItemsPerQuery"
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
        title: ItemTitle::new("Hello from hopd").expect("constant title is valid"),
        subtitle: Some(
            ItemSubtitle::new("M2.2 walking skeleton").expect("constant subtitle is valid"),
        ),
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
                field: "Action.label",
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
                    field: "Action.label",
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

    #[test]
    fn learning_save_failed_line_names_the_path_and_the_error() {
        // A focused unit test of the pure line-building function, mirroring
        // `apps.rs`'s `malformed_log_line_names_the_path_and_the_reason` —
        // see `learning_save_failed_line`'s own doc comment for why a pure
        // function is what a test can assert on here.
        let err = io::Error::other("no space left on device");
        let line = learning_save_failed_line(Path::new("/home/pedro/.local/state/hop.json"), &err);
        assert!(
            line.contains("/home/pedro/.local/state/hop.json"),
            "{line:?}"
        );
        assert!(line.contains("no space left on device"), "{line:?}");
    }

    /// Issue #159: `learning_path` is `$XDG_STATE_HOME`-derived and not
    /// otherwise validated before this function runs, so a newline in it
    /// must not reach stderr unescaped — it would otherwise look like a
    /// second, independent `hopd:` log line.
    #[test]
    fn learning_save_failed_line_escapes_a_newline_in_the_path() {
        let err = io::Error::other("no space left on device");
        let line = learning_save_failed_line(
            Path::new("/home/pedro/.local/state/evil\nname/hop.json"),
            &err,
        );
        assert!(
            !line.contains('\n'),
            "a raw newline must never reach the logged line: {line:?}"
        );
        assert!(line.contains("evil\\x0aname"), "{line:?}");
    }

    #[tokio::test]
    async fn learning_save_panicked_line_escapes_a_newline_in_the_path() {
        // A genuine `JoinError`, from a task that actually panicked —
        // constructing one any other way is not available off the public
        // API, and this is the same way `hop-core`'s own panic-isolation
        // tests get one.
        let handle = tokio::spawn(async { panic!("synthetic panic for this test") });
        let join_err = handle.await.expect_err("the spawned task panicked");

        let line = learning_save_panicked_line(
            Path::new("/home/pedro/.local/state/evil\nname/hop.json"),
            &join_err,
        );
        assert!(
            !line.contains('\n'),
            "a raw newline must never reach the logged line: {line:?}"
        );
        assert!(line.contains("evil\\x0aname"), "{line:?}");
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
        assert_eq!(batch.items().len(), 1);
        assert_eq!(batch.items()[0].title.as_str(), "Hello from hopd");
        assert!(
            rx.recv().await.is_none(),
            "the channel closes once the one provider has finished"
        );
    }

    #[test]
    fn host_source_reports_the_provider_selected_for_pending_attribution() {
        let mut host = ProviderHost::with_log(Arc::new(NoopLog));
        host.register(SkeletonProvider).unwrap();
        let source = HostSource::new(Arc::new(host));

        assert_eq!(
            source.pending_providers(&QueryText::new("walking skeleton").unwrap()),
            vec!["skeleton"]
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

    #[tokio::test]
    async fn host_source_resolves_persisted_recents_against_live_items() {
        let mut pipeline = Pipeline::default();
        pipeline
            .learning
            .sync_plaintext_providers(Vec::<String>::new());
        let item = item(Kind::App, "third-party:firefox", "Firefox", "third-party");
        pipeline.learning.record_launch("third-party", "", &item.id);

        let host = ProviderHost::with_log(Arc::new(NoopLog));
        let source = HostSource::with_pipeline(Arc::new(host), Arc::new(Mutex::new(pipeline)));
        let recents = source.recent_items(std::slice::from_ref(&item)).await;

        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].item, item);
        assert!(recents[0].launched_at_ms > 0);
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
                title: ItemTitle::new("Instant").expect("constant title is valid"),
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
                title: ItemTitle::new("Delayed").expect("constant title is valid"),
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
            title: ItemTitle::new(title).unwrap(),
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
        let first_ids: Vec<&str> = first.items().iter().map(|i| i.id.as_str()).collect();
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
        let mut second_ids: Vec<&str> = second.items().iter().map(|i| i.id.as_str()).collect();
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
        assert_eq!(first.items().len(), 1);
        assert_eq!(first.items()[0].id, ItemId::new("fast2:item").unwrap());
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
            second.items().len(),
            MAX_RESULTS,
            "the assembled frame must hold exactly MAX_RESULTS items"
        );

        let mut actual: Vec<String> = second
            .items()
            .iter()
            .map(|i| i.id.as_str().to_string())
            .collect();
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
            second.items().len(),
            25,
            "with_config must assemble to its configured 25 max_results, \
             not the MAX_RESULTS default of 50"
        );
    }

    #[tokio::test]
    async fn the_accumulator_caps_at_max_items_per_query_and_ends_the_query() {
        // Five providers of low rank weight (`Kind::File`, 999 items each —
        // `4 995` total, each individually under
        // `MAX_ITEMS_PER_PROVIDER_ANSWER` (1 000) so none of them is
        // truncated by `CheckedItems::check` before this accumulator ever
        // sees it), plus one more of much higher weight (`Kind::Window`, 10
        // items) that arrives last, once only 5 slots of room remain.
        //
        // A single flooding provider does not exercise this cap honestly:
        // `MAX_ITEMS_PER_PROVIDER_ANSWER` would already truncate one
        // provider's answer to 1 000 items before this accumulator's own
        // `MAX_ITEMS_PER_QUERY` (5 000) cap ever got a chance to matter,
        // proving the wrong cap. Splitting the flood across providers, each
        // under that inner cap, is what makes this genuinely about
        // `MAX_ITEMS_PER_QUERY`.
        fn filler(id: &'static str) -> ItemsProvider {
            ItemsProvider {
                id,
                kinds: vec![Kind::File],
                items: (0..999)
                    .map(|n| {
                        item(
                            Kind::File,
                            &format!("{id}:{n:03}"),
                            &format!("{id}-{n:03}"),
                            id,
                        )
                    })
                    .collect(),
                delay: Duration::ZERO,
                budget: Duration::from_millis(50),
            }
        }
        let fillers = [
            filler("filler0"),
            filler("filler1"),
            filler("filler2"),
            filler("filler3"),
            filler("filler4"),
        ];
        // Ten Window items, well under MAX_ITEMS_PER_PROVIDER_ANSWER, so
        // none of them is truncated before the accumulator's own cap gets a
        // chance to run. Its 50 ms delay is long enough that every filler
        // above (delay `Duration::ZERO`) has already been absorbed by the
        // time this arrives, leaving exactly 5 of MAX_ITEMS_PER_QUERY's room.
        let winner = ItemsProvider {
            id: "winner",
            kinds: vec![Kind::Window],
            items: (0..10)
                .map(|n| {
                    item(
                        Kind::Window,
                        &format!("win:{n:03}"),
                        &format!("win-{n:03}"),
                        "winner",
                    )
                })
                .collect(),
            delay: Duration::from_millis(50),
            budget: Duration::from_millis(100),
        };

        let policy = HostPolicy {
            max_budget: Duration::from_millis(200),
            ..HostPolicy::default()
        };
        let mut host = ProviderHost::new(policy, Arc::new(NoopLog));
        for filler in fillers {
            host.register(filler).unwrap();
        }
        host.register(winner).unwrap();

        const {
            assert!(
                5 * 999 < MAX_ITEMS_PER_QUERY,
                "the five fillers alone must not already reach the cap"
            );
            assert!(
                5 * 999 + 10 > MAX_ITEMS_PER_QUERY,
                "the winner's own 10 items must be what pushes the total over"
            );
        }

        let source = HostSource::new(Arc::new(host));
        let mut rx = source.start(QueryText::new("").unwrap());

        // Five frames for the five fillers, then the capped frame for the
        // winner's arrival — order among the fillers is not asserted, only
        // that the winner's is last and capped.
        let mut frame = rx
            .recv()
            .await
            .expect("the first filler's arrival must send a frame");
        for _ in 0..4 {
            frame = rx
                .recv()
                .await
                .expect("each filler's arrival must send its own frame");
        }
        let final_frame = rx
            .recv()
            .await
            .expect("the winner's arrival must still send a frame");
        let _ = frame; // every frame but the last is superseded; only the last matters below.

        assert_eq!(final_frame.items().len(), MAX_RESULTS);
        let surviving_window_ids: Vec<&str> = final_frame
            .items()
            .iter()
            .filter(|i| i.kind == Kind::Window)
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(
            surviving_window_ids,
            vec!["win:000", "win:001", "win:002", "win:003", "win:004"],
            "only the first 5 of the winner's 10 items fit in the 5 slots \
             left by the fillers — the accumulator truncates the incoming \
             batch to the room left, not by picking winners after the fact"
        );

        assert_eq!(
            final_frame.rejections().len(),
            1,
            "the overflow must be recorded as exactly one rejection, not \
             left silent and not one per dropped item"
        );
        assert_eq!(
            final_frame.rejections()[0].check,
            FailedCheck::TooManyItemsPerQuery { excess: 5 },
            "5 of the winner's 10 items did not fit in the room left"
        );
        assert_eq!(
            final_frame.rejections()[0].item_id.as_str(),
            "win:005",
            "the rejection samples the first item that did not fit"
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
            frame.items().is_empty(),
            "the term matches nothing, so the assembled frame is empty"
        );

        assert!(
            rx.recv().await.is_none(),
            "channel closes once the one provider has finished"
        );
    }
}
