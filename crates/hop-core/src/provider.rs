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
///
/// `PartialEq`/`Eq` because a host compares the manifest it captured at
/// registration against what [`Provider::manifest`] answers later — see
/// [`ProviderHost`](crate::host::ProviderHost). That comparison is how a
/// manifest built from interior mutability is caught, so equality here is
/// load-bearing rather than a convenience: a field added to this struct and
/// left out of the comparison would be a field a provider could change
/// undetected, and deriving is what keeps the two in step automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// The two async methods are written as `-> impl Future<...> + Send + 'static`
/// (native async-in-trait, stabilized without the bounds baked in) rather than
/// as bare `async fn`. A bare `async fn` in a public trait produces a future
/// type with no bounds at all, which would block the daemon from spawning it
/// onto a Tokio runtime. Writing the desugared form here, once, means every
/// implementor gets a spawnable future automatically instead of everyone
/// needing to route around the gap later. It also avoids the
/// `async_fn_in_trait` lint, which flags exactly this problem and which
/// `-D warnings` turns into a hard error.
///
/// # Why the arguments are owned
///
/// `tokio::spawn` requires `'static` as well as `Send`, and a future capturing
/// `&self`, `&RoutedQuery` or `&QueryCtx` is neither — so the borrowed
/// signature this trait shipped with made the panic isolation its own docs
/// reached for unavailable, and forced a host to poll every provider's future
/// in one task. That is issue #29, and closing it is a breaking change to this
/// seam that spec §6's 2026-07-31 amendment sanctions by name: the lock takes
/// effect when the extension store ships, not now, and #29 is one of the two
/// gaps that amendment says can only be closed by changing these types.
///
/// `Arc<Self>` rather than `Self` so one registered provider serves every
/// query without being cloned; `Arc<RoutedQuery>` so the same routed query
/// reaches every selected provider without one clone per provider on the
/// keystroke path; `QueryCtx` by value because it is two cheap fields, one of
/// them already `Arc`-backed.
///
/// `'static` on the trait itself is what `Arc<dyn ...>` erasure needs
/// downstream, and every provider is a long-lived registered object anyway.
pub trait Provider: Send + Sync + 'static {
    /// This provider's static description — see [`ProviderManifest`].
    ///
    /// **Stability is part of this contract: every call must return the same
    /// manifest.** A host may call this once — at registration, before any
    /// query has run — and treat the value as constant for the life of the
    /// provider. Returning a stored manifest, or rebuilding one fixed value
    /// per call, satisfies this; deriving any field from state that changes
    /// while the provider is alive does not, whatever the intent.
    ///
    /// Unlike when this comment was written, something does now check:
    /// [`ProviderHost`](crate::host::ProviderHost) compares its captured
    /// manifest against a fresh call before it accepts a provider's items, and
    /// refuses the answer on a mismatch. What that does *not* do is make the
    /// contract enforced everywhere —
    /// [`ProviderOutput::from_provider`](crate::pipeline::ProviderOutput::from_provider)
    /// still reads the manifest off the object it is handed, and a caller that
    /// is not the host still gets whatever the provider answers with. Read on
    /// for the abuse that recovers.
    ///
    /// [`ProviderOutput::from_provider`](crate::pipeline::ProviderOutput::from_provider)
    /// reads the manifest *after* [`Provider::query`] has returned, so a
    /// provider answering differently on two calls gets to choose what it is
    /// checked against once it has seen what it wants to return. Concretely,
    /// this is issue #31's exclusive-mode bypass rebuilt from honest-looking
    /// parts: declare `kinds: [Calculator]` at registration, before the host
    /// captures it as constant, return `Kind::Window` items from `query`, then
    /// answer `kinds: [Window]` when the check asks. Each answer is
    /// self-consistent in isolation, the kind check passes, and the Window
    /// items go on to survive a `w `-exclusive filter and inherit Window's
    /// ranking weight — which is the whole of what that check exists to
    /// prevent.
    fn manifest(&self) -> ProviderManifest;

    /// Answers a routed query with the items this provider can find.
    ///
    /// # The implementation is the escaping party for its own sink
    ///
    /// Both of `q`'s string fields are unvalidated, untrusted text.
    /// [`crate::router::route`] applies no **content rule**, no escaping and
    /// no **refusal**: `q.term` has been trimmed, and stripped of whatever
    /// prefix or suffix named the mode where one did (and on the timezone
    /// alias branches replaced by the key it matched), while `q.raw` has not
    /// even had the trim. Neither field has been checked against anything.
    /// Do not read stripping as a signal of exclusivity or of cleanliness:
    /// an **inferred** route strips too, on the timezone phrase branches, and
    /// stripping only removes what named the mode. See [`RoutedQuery`] for
    /// the worked examples.
    ///
    /// `q.exclusive` is the user having named the mode explicitly — a prefix,
    /// a sigil, or a trailing phrase — so results are filtered to that mode's
    /// kinds and nothing else shows. It is not a finding that the text is fit
    /// for whatever answers it.
    ///
    /// Escaping therefore has to happen here, in the implementation, and
    /// cannot be lifted into `hop-core`: only the provider knows what its
    /// sink is, and the correct treatment differs for each. A path sink needs
    /// traversal segments refused and the result resolved under a root; a
    /// command sink needs the value passed as one argv element rather than
    /// through a shell; a URL sink needs percent-encoding *of the component
    /// being built*, which is not the same set of characters for a path
    /// segment as for a query-string value; an SQL sink needs a parameterized
    /// statement and no string building at all. Each of those is wrong for
    /// the others — percent-encoding a shell word neither makes it safe nor
    /// leaves it usable — so any single escape applied before dispatch would
    /// be the wrong one for most callers, which is why the router applies
    /// none.
    ///
    /// [`Mode`] does not answer the question for you. The sink is a property
    /// of *this* provider, not of the mode it was asked under: a provider
    /// that lists [`Mode::All`] — which every provider answering ordinary,
    /// unprefixed search must, see [`ProviderManifest::modes`] — receives
    /// terms under the one mode that names no sink at all, and owes them the
    /// same escaping it owes a term routed [`Mode::Files`].
    ///
    /// This is a documented obligation and nothing more. Nothing in this
    /// crate enforces it: `q` hands over plain `String`s, and an
    /// implementation that interpolates one straight into a URL compiles and
    /// passes every check [`crate::pipeline::CheckedItems::check`] makes —
    /// those are about an item's kind and its producer, never about what its
    /// fields contain. The gates that do sit downstream are `hop-protocol`'s
    /// **content rules** and the `ALLOWED_URL_SCHEMES` allow-list on an
    /// `ExecOutcome::OpenUrl`, and they constrain the outcome *value* rather
    /// than the interpolation that built it: they refuse a scheme the
    /// contract never heard of, and have nothing to say about an
    /// attacker-chosen extra query parameter on an ordinary `https` URL.
    /// Making the obligation unmissable — a newtype forcing a conversion at
    /// each sink — is a design change deliberately left out of the issue that
    /// wrote this comment (#47), which documents the floor beneath it.
    ///
    /// # The implementation also validates its own term
    ///
    /// A second obligation, and not the one above restated: escaping is about
    /// what the term does to a sink, this is about whether the term is usable
    /// at all. `q.mode` answers neither. A mode is how the query was
    /// *interpreted*, not a finding about the text, and on every explicit
    /// route — a prefix, a sigil, or a trailing phrase — the marker that named
    /// the mode decided it alone, with nothing having read the term that
    /// marker left: `route("$١٠٠ usd to eur")` yields [`Mode::Currency`] with
    /// a numeric portion no `parse::<f64>` accepts, `route("=٢+٢")`
    /// [`Mode::Calculator`] likewise, and `route("zurich weather")` yields
    /// [`Mode::Weather`] having read only the suffix its term came before.
    /// Only the currency *inference* predicate checks for a parseable number,
    /// and `q.mode` is the same value whichever route produced it.
    ///
    /// So parse a routed term defensively or not at all: handle the failure
    /// and answer with no items. A failed parse here is an ordinary outcome
    /// rather than an impossible state, and this method runs on every
    /// keystroke, so an `unwrap` on one is a panic any keyboard that types `٢`
    /// can reach. Shape is the smaller half of the job regardless —
    /// `100 xyz to abc` satisfies the currency shape check and still names no
    /// real currency pair, so a term that parses is not yet a term this
    /// provider can answer. [`RoutedQuery`] carries the reasoning and the
    /// decision behind it (issue #67), under "An exclusive mode filters
    /// results; it never checks the term's shape".
    fn query(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        ctx: QueryCtx,
    ) -> impl Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static;

    /// Executes `action_id` on `item_id`, both of which this provider must
    /// have produced from a prior [`Provider::query`] call.
    fn execute(
        self: Arc<Self>,
        item_id: ItemId,
        action_id: ActionId,
    ) -> impl Future<Output = Result<ExecOutcome, ProviderError>> + Send + 'static;
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

        async fn query(
            self: Arc<Self>,
            q: Arc<RoutedQuery>,
            ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            if ctx.cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            Ok(vec![Item {
                id: ItemId::new("app:fake").unwrap(),
                kind: Kind::App,
                title: q.term.clone(),
                subtitle: None,
                icon: None,
                actions: vec![],
                default_action: ActionId::new("open").unwrap(),
                copy_text: None,
                append_to_end: false,
                provider: "fake".into(),
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

    #[tokio::test]
    async fn provider_trait_is_implementable_and_runnable_on_an_executor() {
        let provider = Arc::new(FakeProvider);
        let ctx = QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let routed = Arc::new(route("firefox"));
        let items = provider.clone().query(routed, ctx).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "firefox");

        let outcome = provider
            .execute(items[0].id.clone(), ActionId::new("open").unwrap())
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
        let provider = Arc::new(FakeProvider);
        let ctx = QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let items = provider
            .clone()
            .query(Arc::new(route("firefox")), ctx)
            .await
            .unwrap();
        assert_eq!(items.len(), 1, "the fixture must actually produce an item");

        let checked = CheckedItems::check(vec![ProviderOutput::from_provider(&*provider, items)]);
        assert_eq!(
            checked.rejections(),
            &[],
            "a provider's own honest output must survive its own manifest"
        );
        assert_eq!(checked.items().len(), 1);
    }

    /// The criterion #29 exists for: a provider's future can be handed to
    /// `tokio::spawn`, which requires `'static` as well as `Send`. Under the
    /// old borrowed signature this did not compile, and the trait's own doc
    /// comment reached for the isolation it made unavailable.
    ///
    /// It is written as a spawn rather than an `assert_static` helper because
    /// spawning is the thing the host actually does, and a bound assertion
    /// would still pass if some later change made the future `'static` but
    /// un-spawnable for another reason.
    #[tokio::test]
    async fn a_provider_query_future_can_be_spawned_as_its_own_task() {
        let provider = Arc::new(FakeProvider);
        let routed = Arc::new(route("firefox"));
        let ctx = QueryCtx {
            cancel: CancellationFlag::default(),
            deadline: Instant::now() + Duration::from_secs(1),
        };

        let handle = tokio::spawn(provider.query(routed, ctx));
        let items = handle.await.unwrap().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "firefox");
    }

    /// `ProviderManifest` has to be comparable for the host to detect a
    /// provider whose `manifest()` answers differently after registration —
    /// #32's interior-mutability abuse. Equality is the whole mechanism, so it
    /// is pinned here rather than assumed at the call site.
    #[test]
    fn two_manifests_with_the_same_fields_are_equal_and_a_changed_field_is_not() {
        let a = manifest(vec![Mode::Apps], 3);
        assert_eq!(a, manifest(vec![Mode::Apps], 3));
        assert_ne!(a, manifest(vec![Mode::Apps], 4));
        assert_ne!(a, manifest(vec![Mode::All], 3));
    }
}
