//! The provider trait: the plugin seam every future extension tier (apps,
//! windows, files, and the "smart" utility providers arriving from M2
//! onward) adapts to.
//!
//! This module ships the trait and its supporting types only. Provider
//! *scheduling* — running providers in parallel, enforcing `budget`,
//! streaming partial results back to a client — is explicitly M2 daemon
//! work; nothing here spawns a task or owns a runtime.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hop_protocol::{ActionId, ExecOutcome, Item, ItemId, Kind};

use crate::router::{Mode, RoutedQuery};

/// The [`ProviderManifest::id`] the apps provider will answer to once it
/// exists (M2.5, issue #57). That provider isn't implemented yet, but
/// [`AliasTarget::AppBoost`](crate::aliases::AliasTarget::AppBoost) already
/// needs to name the namespace it targets — `Aliases::apply` tags every
/// `AppBoost` it resolves with this id, and [`crate::rank::Boosts`] only
/// applies that boost to an item whose own (already-verified) `provider`
/// matches it. Defined here, ahead of the provider it names, so both sides
/// share one constant instead of a string literal each has to remember to
/// keep in sync. **Issue #57 must construct its `ProviderManifest` with
/// `id: APPS_PROVIDER_ID`**; a hand-written literal that ever drifts from
/// this constant silently stops every existing app alias from boosting
/// anything.
///
/// **This constant identifies a namespace, not a specific provider, and that
/// distinction is load-bearing.** [`crate::pipeline::CheckedItems::check`]
/// checks each item against its own producer's manifest, but never checks
/// that two answering providers declare *distinct* `id`s — nothing in this
/// crate enforces manifest-id uniqueness across a query's `ProviderOutput`s.
/// If a future provider registry ever lets two providers both declare
/// `id: APPS_PROVIDER_ID`, both pass `CheckedItems::check` and both collect
/// every alias boost this constant tags — the exact boost-theft failure
/// issue #31 exists to close, just moved one level up, from "which item" to
/// "which provider". **Rejecting a second registration under an id already
/// in use is load-bearing for boost correctness**, not just registry
/// hygiene, and whatever builds the M2 provider registry needs to enforce
/// it.
pub const APPS_PROVIDER_ID: &str = "apps";

/// Static description of what a provider serves and how the (future)
/// scheduler should treat it. Nothing here is enforced by this module —
/// [`should_query`] is the one piece of scheduling logic that lives at M1;
/// budget enforcement, cancellation propagation and parallel dispatch are
/// M2 daemon work.
///
/// `id` and `kinds` are what
/// [`crate::pipeline::CheckedItems::check`] holds each of this provider's
/// items to, so a manifest is a promise the provider's own output is checked
/// against, not just a hint to a scheduler.
///
/// `Clone` because [`Provider::manifest`] hands a caller its own value —
/// implementors that keep one prepared can return a copy of it rather than
/// rebuilding it per query.
#[derive(Debug, Clone)]
pub struct ProviderManifest {
    pub id: &'static str,
    pub kinds: Vec<Kind>,
    /// Which routed modes this provider serves. Mode matching is literal
    /// containment (see [`should_query`]): a provider that wants to
    /// participate in ordinary, unprefixed search must list [`Mode::All`]
    /// among its modes, or it will never be asked to run for a query that
    /// didn't route to one of its other modes. This is easy to miss because
    /// nothing else about the manifest hints at it.
    pub modes: Vec<Mode>,
    /// Pre-filter: [`should_query`] returns `false` if the routed term is
    /// shorter than this (character count). `0` means "always run,
    /// regardless of term length".
    pub min_term_len: usize,
    /// Per-query deadline. Not enforced here — a future scheduler reads it.
    pub budget: Duration,
}

/// What a provider's async methods receive for one in-flight query: a
/// cooperative cancellation signal and the deadline it should respect.
/// Neither is enforced by this module; a provider implementation is
/// expected to poll `cancel` and compare against `deadline` itself.
pub struct QueryCtx {
    pub cancel: CancellationFlag,
    pub deadline: Instant,
}

/// A cheap, cloneable cancellation signal shared between a scheduler and the
/// providers it dispatches to.
///
/// `Relaxed` ordering is deliberate: this is a cooperative hint a provider
/// polls between steps of its own work, not a synchronization primitive
/// protecting shared data. Nothing else is ordered relative to it, so a
/// stronger ordering would add cost without changing correctness.
#[derive(Clone, Default)]
pub struct CancellationFlag(Arc<AtomicBool>);

impl CancellationFlag {
    /// Whether [`CancellationFlag::cancel`] has been called on this flag or
    /// any of its clones.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Signals cancellation. Visible to every clone of this flag.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Why a provider's [`Provider::query`] or [`Provider::execute`] failed.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider timed out")]
    Timeout,
    #[error("provider cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

/// The plugin seam: anything that can answer a routed query with items, and
/// execute an action on one of the items it produced.
///
/// The two async methods are written as `-> impl Future<...> + Send`
/// (native async-in-trait, stabilized without the `Send` bound baked in)
/// rather than as bare `async fn`. A bare `async fn` in a public trait
/// produces a future type with no `Send` bound at all, which would block
/// M2's daemon from spawning it onto a Tokio runtime — `tokio::spawn`
/// requires the spawned future to be `Send`. Writing the desugared form
/// here, once, means every implementor gets a `Send` future automatically
/// instead of everyone needing to route around the gap later. It also
/// avoids the `async_fn_in_trait` lint, which flags exactly this problem and
/// which `-D warnings` turns into a hard error — silencing that lint with
/// `#[allow]` would be silencing a warning about the exact issue this trait
/// exists to avoid.
///
/// Lifetimes were not awkward here: edition 2024's RPITIT capture rules
/// automatically capture the lifetimes of `&self` and the by-reference
/// arguments into the returned `impl Future`, so no explicit `+ '_` or
/// higher-ranked bound was needed for either method to compile.
pub trait Provider: Send + Sync {
    /// This provider's static description — see [`ProviderManifest`].
    ///
    /// **Stability is part of this contract: every call must return the same
    /// manifest.** A host may call this once — at registration, before any
    /// query has run — and treat the value as constant for the life of the
    /// provider. Returning a stored manifest, or rebuilding one fixed value
    /// per call, satisfies this; deriving any field from state that changes
    /// while the provider is alive does not, whatever the intent.
    ///
    /// Nothing in this crate enforces that, and it is
    /// [`crate::pipeline::CheckedItems::check`] that an implementation
    /// breaking it defeats.
    /// [`ProviderOutput::from_provider`](crate::pipeline::ProviderOutput::from_provider)
    /// reads the manifest *after* [`Provider::query`] has returned, so a
    /// provider answering differently on two calls gets to choose what it is
    /// checked against once it has seen what it wants to return. Concretely,
    /// this is issue #31's exclusive-mode bypass rebuilt from honest-looking
    /// parts: declare `kinds: [Calculator]` when a scheduler asks whether to
    /// run (see [`should_query`]), return `Kind::Window` items from `query`,
    /// then answer `kinds: [Window]` when the check asks. Each answer is
    /// self-consistent in isolation, the kind check passes, and the Window
    /// items go on to survive a `w `-exclusive filter and inherit Window's
    /// ranking weight — which is the whole of what that check exists to
    /// prevent.
    fn manifest(&self) -> ProviderManifest;

    /// Answers a routed query with the items this provider can find.
    fn query(
        &self,
        q: &RoutedQuery,
        ctx: &QueryCtx,
    ) -> impl Future<Output = Result<Vec<Item>, ProviderError>> + Send;

    /// Executes `action_id` on `item_id`, both of which this provider must
    /// have produced from a prior [`Provider::query`] call.
    fn execute(
        &self,
        item_id: &ItemId,
        action_id: &ActionId,
    ) -> impl Future<Output = Result<ExecOutcome, ProviderError>> + Send;
}

/// The pre-filter helper: should a scheduler even bother asking this
/// provider to run for this routed query?
///
/// Returns `false` when the manifest doesn't list the routed query's mode
/// (see the doc comment on [`ProviderManifest::modes`] about `Mode::All`),
/// or when the routed term is shorter than `min_term_len`. `min_term_len ==
/// 0` always passes the length check, including for an empty term.
/// Otherwise returns `true`.
pub fn should_query(m: &ProviderManifest, q: &RoutedQuery) -> bool {
    if !m.modes.contains(&q.mode) {
        return false;
    }
    if q.term.chars().count() < m.min_term_len {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::pipeline::{CheckedItems, ProviderOutput};
    use crate::router::route;

    fn manifest(modes: Vec<Mode>, min_term_len: usize) -> ProviderManifest {
        ProviderManifest {
            id: "test",
            kinds: vec![Kind::App],
            modes,
            min_term_len,
            budget: Duration::from_millis(50),
        }
    }

    #[test]
    fn provider_manifest_prefilter_helper() {
        let m = manifest(vec![Mode::Apps], 3);

        // Wrong mode: false, regardless of term length.
        let wrong_mode = route("w term");
        assert!(!should_query(&m, &wrong_mode));

        // Right mode, term shorter than min_term_len: false.
        let short_term = route("a hi");
        assert_eq!(short_term.term, "hi");
        assert!(!should_query(&m, &short_term));

        // Right mode, term at/above min_term_len: true.
        let long_enough = route("a firefox");
        assert!(should_query(&m, &long_enough));
    }

    #[test]
    fn should_query_with_zero_min_term_len_runs_for_empty_term() {
        let m = manifest(vec![Mode::Apps], 0);
        let empty_term = route("a ");
        assert_eq!(empty_term.term, "");
        assert!(should_query(&m, &empty_term));
    }

    #[test]
    fn cancellation_flag_reports_cancelled_after_cancel_and_clone_shares_state() {
        let flag = CancellationFlag::default();
        assert!(!flag.is_cancelled());

        let clone = flag.clone();
        assert!(!clone.is_cancelled());

        flag.cancel();
        assert!(flag.is_cancelled());
        assert!(
            clone.is_cancelled(),
            "a clone must observe cancellation performed through the original"
        );
    }

    /// A minimal fake provider, proving the trait is actually implementable
    /// with the native async-in-trait syntax and runnable on a real
    /// executor — a trait nobody has implemented is a trait that might not
    /// compile for implementors.
    ///
    /// It is also the tree's one worked example of a well-behaved provider,
    /// so its manifest `id` and the `provider` string on the items its
    /// `query` returns must agree, and every item's kind must be one the
    /// manifest declares. Those are exactly the two things
    /// [`crate::pipeline::CheckedItems::check`] holds a provider to; an
    /// example that failed its own checks would be a template for getting
    /// this wrong. Pinned by
    /// [`tests::a_providers_own_output_passes_its_own_manifests_checks`].
    struct FakeProvider;

    impl Provider for FakeProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "fake",
                ..manifest(vec![Mode::All], 0)
            }
        }

        async fn query(&self, q: &RoutedQuery, ctx: &QueryCtx) -> Result<Vec<Item>, ProviderError> {
            if ctx.cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            Ok(vec![Item {
                id: ItemId("app:fake".into()),
                kind: Kind::App,
                title: q.term.clone(),
                subtitle: None,
                icon: None,
                actions: vec![],
                default_action: ActionId("open".into()),
                copy_text: None,
                append_to_end: false,
                provider: "fake".into(),
            }])
        }

        async fn execute(
            &self,
            _item_id: &ItemId,
            _action_id: &ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    #[tokio::test]
    async fn provider_trait_is_implementable_and_runnable_on_an_executor() {
        let provider = FakeProvider;
        let ctx = QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let routed = route("firefox");
        let items = provider.query(&routed, &ctx).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "firefox");

        let outcome = provider
            .execute(&items[0].id, &ActionId("open".into()))
            .await
            .unwrap();
        assert_eq!(outcome, ExecOutcome::Done);
    }

    /// The provider seam end to end, and the only test that exercises the
    /// association [`crate::pipeline::ProviderOutput`] carries with items a
    /// [`Provider`] really returned: dispatch a provider, pair what it
    /// answered with itself, and both manifest checks pass.
    ///
    /// This is what makes the association *right* rather than merely present.
    /// `ProviderOutput::from_provider` reads the manifest off the object it
    /// is handed, so this fails the moment `FakeProvider::query` returns an
    /// item whose `provider` string or kind disagrees with what
    /// `FakeProvider::manifest` declares — which is precisely the mistake a
    /// real provider is most likely to make.
    #[tokio::test]
    async fn a_providers_own_output_passes_its_own_manifests_checks() {
        let provider = FakeProvider;
        let ctx = QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let items = provider.query(&route("firefox"), &ctx).await.unwrap();
        assert_eq!(items.len(), 1, "the fixture must actually produce an item");

        let checked = CheckedItems::check(vec![ProviderOutput::from_provider(&provider, items)]);
        assert_eq!(
            checked.rejections(),
            &[],
            "a provider's own honest output must survive its own manifest"
        );
        assert_eq!(checked.items().len(), 1);
    }

    #[tokio::test]
    async fn provider_query_future_is_send() {
        fn assert_send<T: Send>(_: T) {}
        let provider = FakeProvider;
        let ctx = QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let routed = route("firefox");
        assert_send(provider.query(&routed, &ctx));
    }
}
