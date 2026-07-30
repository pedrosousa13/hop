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

/// Static description of what a provider serves and how the (future)
/// scheduler should treat it. Nothing here is enforced by this module —
/// [`should_query`] is the one piece of scheduling logic that lives at M1;
/// budget enforcement, cancellation propagation and parallel dispatch are
/// M2 daemon work.
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
    struct FakeProvider;

    impl Provider for FakeProvider {
        fn manifest(&self) -> ProviderManifest {
            manifest(vec![Mode::All], 0)
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
