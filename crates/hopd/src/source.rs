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

use std::sync::Arc;
use std::time::Duration;

use hop_core::host::{ProviderEvent, ProviderHost, ProviderLog};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery, route};
use hop_protocol::{Action, ActionId, ActionKind, ExecOutcome, Item, ItemId, Kind, QueryText};
use tokio::sync::mpsc;

/// Answers queries with streams of item batches.
///
/// `Clone` because every connection gets its own handle; implementations are
/// expected to be cheap handles over shared state, not the state itself.
///
/// # What an implementation owes the daemon
///
/// Three obligations, none of which this seam checks. [`HostSource`] is the
/// first implementation with enough surface to break any of them, and here is
/// where it actually stands against each — read this rather than assume
/// landing issue #56 settled all three, because it did not.
///
/// **Items must respect `hop_protocol::limits`' per-item field bounds.**
/// [`Item`]'s fields are public, and those bounds are applied where an item is
/// *parsed*, so an item handed back through this trait has passed nothing. The
/// daemon bounds its retained set by item *count*
/// ([`MAX_ITEMS_PER_QUERY`](hop_protocol::limits::MAX_ITEMS_PER_QUERY)), and
/// the byte figure that count is justified against is the count multiplied by
/// those per-item bounds — so a source producing a 100 MB title makes that
/// arithmetic, and the bound it justifies, meaningless. The only thing below
/// it is
/// [`MAX_FRAME_BYTES`](hop_protocol::limits::MAX_FRAME_BYTES) at encode time,
/// which surfaces as an `io::Error` that kills the connection with no error
/// frame — a worse outcome than refusing the item would have been.
/// [`HostSource`] does not close this gap: [`ProviderHost`]'s per-provider
/// turn checks an item's `kind` and `provider` against its producer's
/// manifest and nothing about its field lengths, so a provider that returns
/// an oversized title reaches this trait exactly as unchecked as this
/// paragraph warns.
///
/// **What a source buffers is daemon memory the cap does not see.** The
/// receiver returned here lives inside the connection's exchange for the life
/// of the query, so the channel's capacity and the size of each batch are
/// daemon memory chosen by the source. `MAX_ITEMS_PER_QUERY` counts only what
/// the daemon has *forwarded*; a `mpsc::channel(1_000)` carrying 1 000-item
/// batches parks a million items the cap never counts. Every source in this
/// crate uses capacity 1, and a source with more should have a reason.
/// [`HostSource`] honours the capacity half — its `start` opens exactly
/// `mpsc::channel(1)` — but a *single* batch is still whatever one provider
/// returns: neither [`ProviderHost`] nor this source caps how many items one
/// provider may answer with, so nothing here stops one `send` from parking
/// however many a provider sent. Closing that is issue #30's, and it is worth
/// naming rather than leaving implied that capacity 1 already bounds it.
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
pub trait ResultSource: Clone + Send + Sync + 'static {
    /// Starts answering one query. Batches arrive on the returned receiver;
    /// the channel closing means the source is done; dropping the receiver
    /// cancels the work — subject to the obligations on this trait, which say
    /// what "cancels" costs an implementation that never sends.
    fn start(&self, text: QueryText) -> mpsc::Receiver<Vec<Item>>;
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
        // Action dispatch is issue #59's slice; until then this provider
        // produces items nothing can act on, and says so rather than
        // pretending to have done something.
        Err(ProviderError::Failed(
            "action dispatch is not implemented yet".to_string(),
        ))
    }
}

/// The production [`ResultSource`]: a routed query handed to a
/// [`ProviderHost`].
///
/// `Clone` is cheap — the host sits behind an `Arc`, so every connection's
/// handle shares one registry rather than one per connection.
#[derive(Clone)]
pub struct HostSource {
    host: Arc<ProviderHost>,
}

impl HostSource {
    /// A source over `host`. The host is already built and its providers
    /// already registered: registration happens once at startup, which is what
    /// makes a captured manifest a startup-time fact rather than a per-query
    /// one.
    pub fn new(host: Arc<ProviderHost>) -> Self {
        HostSource { host }
    }
}

impl ResultSource for HostSource {
    fn start(&self, text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        // Capacity 1 for the reason this trait's docs give: what a source
        // buffers is daemon memory the retained-set cap does not see, so a
        // deeper channel would only let providers park items the cap never
        // counts.
        let (tx, rx) = mpsc::channel(1);
        // Routing happens here rather than inside the host because the host's
        // vocabulary is a `RoutedQuery` — the same value every provider sees,
        // shared rather than cloned per provider.
        let routed = Arc::new(route(text.as_str()));
        self.host.spawn_query(routed, tx);
        rx
    }
}

/// The daemon's [`ProviderLog`]: one line per event on stderr.
///
/// Deliberately the crudest thing that satisfies issue #34's criterion, and
/// consistent with how this crate already reports — [`crate::server::serve`]
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
            } => eprintln!(
                "hopd: provider {provider} had {} item(s) refused by its own manifest",
                rejections.len()
            ),
            // Skipped is the common case by design — most keystrokes reach
            // most providers not at all — so logging it per keystroke would
            // bury everything above it.
            ProviderEvent::Skipped { .. } => {}
        }
    }
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

    #[tokio::test]
    async fn the_skeleton_provider_answers_through_the_host() {
        // The walking skeleton's item, reached the way every later provider
        // will be: registered with the host, selected by its captured
        // manifest, and streamed.
        let mut host = ProviderHost::with_log(Arc::new(NoopLog));
        host.register(SkeletonProvider).unwrap();
        let source = HostSource::new(Arc::new(host));

        let mut rx = source.start(QueryText::new("anything").unwrap());
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
}
