//! The provider host: what owns registered providers, decides which of them a
//! keystroke reaches, runs their queries under a budget it enforces itself,
//! and contains their failures.
//!
//! This module is the enforcement point issues #28, #32 and #34 each found
//! missing. Before it, [`ProviderManifest::budget`] appeared nowhere outside
//! doc comments, [`should_query`] had no caller outside tests, and nothing in
//! the workspace recorded that a provider had failed at all.
//!
//! # What is enforced here rather than asked for
//!
//! A provider is untrusted code. Every guarantee below therefore holds without
//! its cooperation:
//!
//! - **The manifest is read once**, at registration, and every scheduling
//!   decision reads that captured copy. A provider that answers differently
//!   afterwards changes nothing about whether it is asked to run.
//! - **The budget is a host deadline, not a request.** Each provider's future
//!   runs as its own task and is abandoned when its budget expires, whether or
//!   not it ever polled [`QueryCtx::cancel`].
//! - **A panic is contained at the seam** and reported as a failure naming the
//!   provider, because the future runs under [`tokio::spawn`] and a panicking
//!   task surfaces as a [`JoinError`](tokio::task::JoinError) rather than
//!   unwinding into the daemon.
//! - **Provider-supplied text is rewritten before it can leave**, by
//!   [`sanitize_provider_message`](crate::sanitize::sanitize_provider_message).
//! - **One failing provider never empties a frame for the others**, which is
//!   spec §9's per-provider isolation rule: providers are separate tasks
//!   holding separate senders, so nothing about one provider's outcome is on
//!   another's path.
//!
//! # What is not enforced here, and where it goes instead
//!
//! Ranking. This module streams each provider's manifest-checked items as its
//! own batch, in the order providers answer, and never calls
//! [`Pipeline::assemble`](crate::pipeline::Pipeline::assemble) — see the
//! "Scope" section of `docs/superpowers/plans/2026-08-04-issue-56-provider-host.md`
//! for why wiring assembly needs a protocol answer about streaming that issue
//! #56 does not give.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hop_protocol::Item;

use crate::pipeline::{ProviderOutput, Rejection};
use crate::provider::{Provider, ProviderError, ProviderManifest, QueryCtx, should_query};
use crate::router::RoutedQuery;
use crate::sanitize::sanitize_provider_message;

/// Why a provider did not answer, as the host classifies it.
///
/// Deliberately not the same enum as [`ProviderError`]: that one is the
/// provider's own vocabulary, and this one adds the case a provider cannot
/// report about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The provider returned [`ProviderError::Timeout`] on its own, before its
    /// budget ran out. It noticed its deadline and gave up — cooperation, not
    /// enforcement. A host cut-off is [`ProviderEvent::BudgetMiss`] and is a
    /// different event on purpose; only the second one proves the host enforced
    /// anything.
    Timeout,
    /// The provider returned [`ProviderError::Cancelled`], or its task was
    /// abandoned after the query it belonged to went away.
    Cancelled,
    /// The provider returned [`ProviderError::Failed`]. Its text is in
    /// [`ProviderFailure::message`], sanitized.
    Failed,
    /// The provider's future panicked, and [`tokio::spawn`] contained it. No
    /// provider can report this about itself, which is why the variant exists
    /// on this enum and not on [`ProviderError`].
    Panicked,
}

/// One provider's failure, attributed and safe to render.
///
/// # Why the host builds this and a provider cannot
///
/// `provider` is read from the manifest the host **captured at
/// registration** — never from anything the failing provider said at failure
/// time. This is [`ProviderOutput`](crate::pipeline::ProviderOutput)'s argument
/// applied to errors instead of items: a value the untrusted party can name is
/// a value it can forge, so a provider that fails with the text
/// `"apps: index corrupt"` cannot make the daemon attribute its failure to the
/// apps provider. It is also why issue #34's "the error carries the producing
/// provider's id" is met here rather than by a field on [`ProviderError`],
/// which providers construct themselves.
///
/// `message` has been through
/// [`sanitize_provider_message`](crate::sanitize::sanitize_provider_message),
/// so it is within
/// [`MAX_PROVIDER_MESSAGE`](crate::sanitize::MAX_PROVIDER_MESSAGE) bytes and
/// carries no control or direction-override characters.
///
/// # Why the fields are private
///
/// All four fields are private, and [`ProviderFailure::provider`],
/// [`ProviderFailure::kind`], [`ProviderFailure::message`] and
/// [`ProviderFailure::elapsed`] are the only way to read them back. This is
/// [`CheckedItems`](crate::pipeline::CheckedItems)'s argument applied here
/// rather than at assembly: "the compiler enforces it instead of a reviewer
/// noticing." A `pub` field on a `pub` struct in a `pub mod` is writable from
/// anywhere in this crate or its dependents, which would let any caller build
/// `ProviderFailure { provider: "apps".into(), message: raw_text, .. }`
/// directly — skipping [`ProviderFailure::from_error`],
/// [`ProviderFailure::panicked`] and [`ProviderFailure::budget_miss`] and,
/// with them, every sanitization and attribution guarantee this type claims.
/// With the fields private, the three constructors above are the *only* way
/// to produce a value of this type at all, so every `ProviderFailure` in
/// existence has had its text sanitized and its `provider` taken from the
/// caller — the host, reading its own captured manifest — never from the
/// failing provider. That is a fact about the type's shape, not a convention
/// its constructors happen to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    provider: String,
    kind: FailureKind,
    message: String,
    elapsed: Duration,
}

impl ProviderFailure {
    /// Classifies a [`ProviderError`] the provider returned, sanitizing its
    /// text.
    pub fn from_error(provider: &str, error: ProviderError, elapsed: Duration) -> Self {
        let (kind, message) = match error {
            ProviderError::Timeout => (FailureKind::Timeout, String::new()),
            ProviderError::Cancelled => (FailureKind::Cancelled, String::new()),
            ProviderError::Failed(text) => (FailureKind::Failed, sanitize_provider_message(&text)),
        };
        ProviderFailure {
            provider: provider.to_string(),
            kind,
            message,
            elapsed,
        }
    }

    /// A provider whose future panicked. The message is the host's own words —
    /// a panic payload is provider-controlled text that has already escaped one
    /// boundary, and nothing needs it to render a failure.
    pub fn panicked(provider: &str, elapsed: Duration) -> Self {
        ProviderFailure {
            provider: provider.to_string(),
            kind: FailureKind::Panicked,
            message: "the provider panicked".to_string(),
            elapsed,
        }
    }

    /// A provider the host cut off at its budget. Reported as a timeout
    /// because that is what the client needs to know; the host-versus-provider
    /// distinction is carried by [`ProviderEvent::BudgetMiss`] on the log seam.
    pub fn budget_miss(provider: &str, elapsed: Duration) -> Self {
        ProviderFailure {
            provider: provider.to_string(),
            kind: FailureKind::Timeout,
            message: "the provider exceeded its budget".to_string(),
            elapsed,
        }
    }

    /// The [`ProviderManifest::id`](crate::provider::ProviderManifest::id) of
    /// the provider that failed, from the captured manifest.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// How it failed.
    pub fn kind(&self) -> FailureKind {
        self.kind
    }

    /// Human-readable detail, sanitized. Empty for the kinds that carry no
    /// provider-supplied text.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// How long the host waited before this outcome was known.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// What the host reports about one provider on one query.
///
/// Borrowed throughout, and constructed per event on the query path: an
/// implementation that wants to keep a record owns it itself, so a
/// [`NoopLog`] costs a call and no allocation. That matters because this is
/// the keystroke path spec §3 holds to 10 ms.
#[derive(Debug)]
pub enum ProviderEvent<'a> {
    /// A provider answered, with this many items, after this long.
    Answered {
        provider: &'a str,
        items: usize,
        elapsed: Duration,
    },
    /// A provider failed. Covers every [`FailureKind`], including a budget
    /// miss — a miss emits *both* this and [`ProviderEvent::BudgetMiss`],
    /// because one is the failure the client sees and the other is the
    /// enforcement fact only the host knows.
    Failed(&'a ProviderFailure),
    /// The host cut a provider off at its budget. Issue #34 names budget
    /// misses separately from failures, and spec §3 requires that a budget
    /// miss logs.
    BudgetMiss {
        provider: &'a str,
        budget: Duration,
        elapsed: Duration,
    },
    /// Items the manifest checks refused —
    /// [`CheckedItems::check`](crate::pipeline::CheckedItems::check)'s
    /// rejections, which had nowhere to go before this seam existed. This is
    /// the event that makes ignoring them a mistake rather than a one-character
    /// omission, which is what `pipeline.rs` said a logging seam would buy.
    Rejected {
        provider: &'a str,
        rejections: &'a [Rejection],
    },
    /// The pre-filter declined to run a provider for this query — its captured
    /// manifest did not list the routed mode, or the term was shorter than its
    /// minimum. The common case by design ("most keystrokes never reach most
    /// plugins", spec §6 rule 2), so an implementation that records this should
    /// expect volume.
    Skipped { provider: &'a str },
}

/// Where the host reports what providers did.
///
/// # Why a trait and not `tracing`
///
/// Spec §9 makes `tracing` the daemon's logging backend, and issue #34 puts
/// choosing a backend explicitly out of scope. A trait is what separates the
/// two: this crate defines *what* is worth recording, and the daemon decides
/// where it goes. It is also the only shape that delivers what `pipeline.rs`
/// promised a logging seam would — rejections stay ignorable "until there is a
/// logging seam (issue #34) that makes ignoring them a real mistake", and a
/// macro at a call site makes nothing unignorable while a `ProviderLog` the
/// host cannot be constructed without does.
pub trait ProviderLog: Send + Sync + 'static {
    /// Records one event. Called on the query path, so an implementation that
    /// blocks or allocates heavily spends the latency budget spec §3 sets.
    fn record(&self, event: ProviderEvent<'_>);
}

/// A [`ProviderLog`] that discards everything — for tests, and for any host
/// whose caller has not chosen a backend.
///
/// It exists so that "no logging configured" is a visible choice at the
/// construction site rather than an `Option<Arc<dyn ProviderLog>>` every call
/// site has to branch on.
pub struct NoopLog;

impl ProviderLog for NoopLog {
    fn record(&self, _event: ProviderEvent<'_>) {}
}

/// The most a provider's per-query budget may be, whatever its manifest says.
///
/// # Why 50 ms
///
/// Spec §3 holds the whole keystroke path — every provider, plus ranking — to
/// 10 ms, and this ceiling is deliberately looser than that rather than equal
/// to it. The two bound different things: 10 ms is the target for the frame a
/// user sees, and this is the point past which the host stops waiting for one
/// provider that is already late. A ceiling at 10 ms would make the *first*
/// slow provider the thing that fails the latency contract, and a provider cut
/// off at its budget does not delay the frame at all — spec §3's rule is that
/// a budget miss "logs and isolates, never blocks the frame", and the other
/// providers' items have already streamed by then.
///
/// 50 ms is also what every manifest in the tree already declares, so this
/// ceiling clamps nothing that exists today and only bites a provider asking
/// for materially more. A provider that genuinely needs longer than this is a
/// provider doing I/O on the query path, which spec §3 forbids outright: the
/// network providers return "a cached-or-pending row synchronously and push an
/// update frame when the fetch lands", so their slow half is not a query at
/// all.
///
/// It is a constant rather than a knob because a per-provider override is
/// exactly the provider-authored policy issue #32 exists to remove.
/// [`HostPolicy::max_budget`] lets the *host* lower it, never a provider raise
/// it.
pub const MAX_PROVIDER_BUDGET: Duration = Duration::from_millis(50);

/// The host's own policy, applied to every manifest at registration.
///
/// This is the layer issue #32 found missing: before it, every input to a
/// scheduling decision came from the provider's own manifest, so the
/// declarative pre-filter spec §6 describes as the host's protection was
/// really a provider self-declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPolicy {
    /// Ceiling on a provider's budget. A manifest asking for more is clamped
    /// to this. Defaults to [`MAX_PROVIDER_BUDGET`]; a host may set it lower
    /// but nothing lets a provider raise it.
    pub max_budget: Duration,
    /// Floor under a provider's `min_term_len`. A manifest declaring less is
    /// raised to this, so the host can keep providers off short terms
    /// regardless of what they asked for. Defaults to `0`, which changes
    /// nothing.
    ///
    /// One direction only: a provider that declares a *higher* minimum keeps
    /// it. The floor exists to make providers cheaper to run, not to make a
    /// cautious provider run more often than it wanted.
    pub min_term_len_floor: usize,
}

impl Default for HostPolicy {
    fn default() -> Self {
        HostPolicy {
            max_budget: MAX_PROVIDER_BUDGET,
            min_term_len_floor: 0,
        }
    }
}

/// Why [`ProviderHost::register`] refused a provider.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    /// Another provider is already registered under this
    /// [`ProviderManifest::id`].
    ///
    /// Refusing is load-bearing for boost correctness rather than registry
    /// hygiene: [`APPS_PROVIDER_ID`](crate::provider::APPS_PROVIDER_ID)'s docs
    /// spell out that two providers sharing an id both pass
    /// [`CheckedItems::check`](crate::pipeline::CheckedItems::check) and both
    /// collect every alias boost tagged with that id — issue #31's boost theft,
    /// moved up one level from "which item" to "which provider" — and name
    /// enforcing uniqueness here as the M2 registry's job.
    #[error("a provider is already registered under the id `{0}`")]
    DuplicateId(String),
}

/// One registered provider: the manifest captured at registration, the
/// host-clamped copy scheduling reads, and the provider itself.
struct Registration {
    /// Exactly what [`Provider::manifest`] answered at registration, before
    /// any clamp. Kept so the host can compare it against a later call and
    /// catch a provider whose manifest shifts — clamping deliberately makes
    /// `effective` differ, so `effective` cannot serve as that baseline.
    // Task 5 (query execution) is the caller; this allow goes with it.
    #[allow(dead_code)]
    declared: ProviderManifest,
    /// `declared` with [`HostPolicy`] applied. Every scheduling decision and
    /// the enforced budget read this, and nothing re-reads
    /// [`Provider::manifest`] to make one.
    effective: ProviderManifest,
    // Task 5 (query execution) is the caller; this allow goes with it.
    #[allow(dead_code)]
    provider: Arc<dyn ErasedProvider>,
}

/// Owns registered providers and runs their queries.
///
/// See the module docs for what is enforced here without a provider's
/// cooperation.
pub struct ProviderHost {
    providers: Vec<Registration>,
    policy: HostPolicy,
    log: Arc<dyn ProviderLog>,
}

impl ProviderHost {
    /// A host with an explicit policy and log seam.
    pub fn new(policy: HostPolicy, log: Arc<dyn ProviderLog>) -> Self {
        ProviderHost {
            providers: Vec::new(),
            policy,
            log,
        }
    }

    /// A host with the default policy — every ceiling and floor at its
    /// documented value.
    pub fn with_log(log: Arc<dyn ProviderLog>) -> Self {
        ProviderHost::new(HostPolicy::default(), log)
    }

    /// Registers `provider`, reading its manifest **once** and keeping the
    /// value.
    ///
    /// From here on nothing this host does consults
    /// [`Provider::manifest`] to make a decision, so a provider that answers
    /// differently later changes neither whether it is asked to run nor what
    /// budget it gets. That is issue #32's criterion, and the reason the
    /// capture happens here rather than per query.
    ///
    /// Refuses a provider whose id is already registered — see
    /// [`RegistrationError::DuplicateId`].
    pub fn register<P: Provider>(&mut self, provider: P) -> Result<(), RegistrationError> {
        self.register_arc(Arc::new(provider))
    }

    /// [`ProviderHost::register`] for a provider the caller already holds
    /// behind an `Arc` — the same capture, the same refusals.
    pub fn register_arc<P: Provider>(&mut self, provider: Arc<P>) -> Result<(), RegistrationError> {
        // `provider.manifest()` is ambiguous here: `P` implements `Provider`
        // (which declares `manifest`) and, via the blanket `impl<P: Provider>
        // ErasedProvider for P` below, `ErasedProvider` too (which also
        // declares `manifest`). Fully qualifying the call picks the trait
        // whose contract this capture is actually about.
        let declared = Provider::manifest(&*provider);
        if self.providers.iter().any(|r| r.effective.id == declared.id) {
            return Err(RegistrationError::DuplicateId(declared.id.to_string()));
        }

        let effective = ProviderManifest {
            budget: declared.budget.min(self.policy.max_budget),
            min_term_len: declared.min_term_len.max(self.policy.min_term_len_floor),
            ..declared.clone()
        };

        self.providers.push(Registration {
            declared,
            effective,
            provider,
        });
        Ok(())
    }

    /// The captured, clamped manifests, in registration order. What scheduling
    /// reads, exposed so a caller can see what the host actually accepted
    /// rather than what a provider asked for.
    pub fn manifests(&self) -> Vec<ProviderManifest> {
        self.providers.iter().map(|r| r.effective.clone()).collect()
    }

    /// How many providers are registered.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether no providers are registered. A host in this state answers every
    /// query with nothing at all, which is a real state during M2: providers
    /// arrive in later slices.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The registrations this routed query reaches, by captured manifest, and
    /// a [`ProviderEvent::Skipped`] on the seam for each one it does not.
    ///
    /// This is [`should_query`]'s caller — the thing issue #32 found it did
    /// not have outside tests, which is what left the codebase with no worked
    /// example of the intended enforcement point.
    // Task 5 (query execution) is the caller; this allow goes with it.
    #[allow(dead_code)]
    fn selected(&self, q: &RoutedQuery) -> Vec<&Registration> {
        self.providers
            .iter()
            .filter(|r| {
                let run = should_query(&r.effective, q);
                if !run {
                    self.log.record(ProviderEvent::Skipped {
                        provider: r.effective.id,
                    });
                }
                run
            })
            .collect()
    }

    /// The ids [`ProviderHost::selected`] would run for `q`, in registration
    /// order. Test-only: production callers want the providers, not their
    /// names.
    #[cfg(test)]
    fn selected_ids(&self, q: &RoutedQuery) -> Vec<&str> {
        self.selected(q).iter().map(|r| r.effective.id).collect()
    }
}

/// A dyn-compatible view of a [`Provider`], so a host can hold providers of
/// different concrete types in one collection.
///
/// # Why erasure is needed, and why this trait stays private
///
/// [`Provider`] is dyn-incompatible by construction — its RPITIT methods make
/// it so — and
/// [`ProviderOutput`](crate::pipeline::ProviderOutput)'s docs rely on exactly
/// that: "not something `dyn Provider` can launder either". So the host cannot
/// hold `Arc<dyn Provider>`, and needs this.
///
/// What keeps erasure from reopening the hole is where
/// [`ErasedProvider::output`] is implemented: inside a blanket
/// `impl<P: Provider>`, where the concrete `P` is in hand, so
/// [`ProviderOutput::from_provider`](crate::pipeline::ProviderOutput::from_provider)
/// still receives the object that was asked rather than a manifest a caller
/// chose. Nothing an item claims about itself can select the manifest it is
/// checked against, before or after erasure.
///
/// It is private to this crate for the same reason: a public
/// dyn-compatible provider trait would be a second route to supplying a
/// manifest, and the blanket impl means every [`Provider`] already has one.
trait ErasedProvider: Send + Sync + 'static {
    // Task 5 (query execution) is the caller; this allow goes with it.
    #[allow(dead_code)]
    fn manifest(&self) -> ProviderManifest;

    /// [`Provider::query`] with its future boxed, which is what makes the
    /// method dyn-compatible.
    // Task 5 (query execution) is the caller; this allow goes with it.
    #[allow(dead_code)]
    fn query_erased(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        ctx: QueryCtx,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static>>;

    /// Pairs `items` with this provider's manifest the only way
    /// [`ProviderOutput`](crate::pipeline::ProviderOutput) allows — see the
    /// trait docs for why this method exists here rather than at the call site.
    // Task 5 (query execution) is the caller; this allow goes with it.
    #[allow(dead_code)]
    fn output(&self, items: Vec<Item>) -> ProviderOutput;
}

impl<P: Provider> ErasedProvider for P {
    fn manifest(&self) -> ProviderManifest {
        Provider::manifest(self)
    }

    fn query_erased(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        ctx: QueryCtx,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static>> {
        Box::pin(Provider::query(self, q, ctx))
    }

    fn output(&self, items: Vec<Item>) -> ProviderOutput {
        ProviderOutput::from_provider(self, items)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::sanitize::MAX_PROVIDER_MESSAGE;
    use std::sync::Mutex;

    /// A [`ProviderLog`] that keeps what it was told, as owned strings — the
    /// recording impl every test below and in Task 5 asserts against.
    ///
    /// It formats each event into a short line rather than storing the borrowed
    /// event, because [`ProviderEvent`] borrows and a recorder has to outlive
    /// the call. The lines are what the assertions read.
    #[derive(Default)]
    pub(crate) struct RecordingLog {
        pub(crate) lines: Mutex<Vec<String>>,
    }

    impl RecordingLog {
        pub(crate) fn lines(&self) -> Vec<String> {
            self.lines
                .lock()
                .expect("no test panics holding this")
                .clone()
        }
    }

    impl ProviderLog for RecordingLog {
        fn record(&self, event: ProviderEvent<'_>) {
            let line = match event {
                ProviderEvent::Answered {
                    provider, items, ..
                } => format!("answered {provider} {items}"),
                ProviderEvent::Failed(failure) => format!(
                    "failed {} {:?} {}",
                    failure.provider(),
                    failure.kind(),
                    failure.message()
                ),
                ProviderEvent::BudgetMiss { provider, .. } => format!("budget-miss {provider}"),
                ProviderEvent::Rejected {
                    provider,
                    rejections,
                } => format!("rejected {provider} {}", rejections.len()),
                ProviderEvent::Skipped { provider } => format!("skipped {provider}"),
            };
            self.lines
                .lock()
                .expect("no test panics holding this")
                .push(line);
        }
    }

    #[test]
    fn a_provider_failure_is_attributed_to_the_captured_id_not_the_error_text() {
        // The provider's text names another provider; attribution must ignore
        // it entirely.
        let failure = ProviderFailure::from_error(
            "calculator",
            ProviderError::Failed("apps: index corrupt".into()),
            Duration::from_millis(3),
        );
        assert_eq!(failure.provider(), "calculator");
        assert_eq!(failure.kind(), FailureKind::Failed);
        assert_eq!(failure.message(), "apps: index corrupt");
    }

    #[test]
    fn provider_error_text_is_sanitized_when_the_failure_is_built() {
        let raw = format!("\u{1b}[31m{}", "x".repeat(MAX_PROVIDER_MESSAGE * 2));
        let failure = ProviderFailure::from_error(
            "apps",
            ProviderError::Failed(raw),
            Duration::from_millis(1),
        );
        assert_eq!(failure.message().len(), MAX_PROVIDER_MESSAGE);
        assert!(!failure.message().contains('\u{1b}'));
    }

    #[test]
    fn the_kinds_that_carry_no_provider_text_have_an_empty_message() {
        for error in [ProviderError::Timeout, ProviderError::Cancelled] {
            let failure = ProviderFailure::from_error("apps", error, Duration::ZERO);
            assert_eq!(failure.message(), "");
        }
    }

    #[test]
    fn a_panic_failure_names_the_provider_and_carries_the_hosts_own_words() {
        let failure = ProviderFailure::panicked("apps", Duration::from_millis(2));
        assert_eq!(failure.provider(), "apps");
        assert_eq!(failure.kind(), FailureKind::Panicked);
        assert_eq!(failure.message(), "the provider panicked");
    }

    #[test]
    fn a_budget_miss_reports_as_a_timeout_to_the_client() {
        let failure = ProviderFailure::budget_miss("slow", Duration::from_millis(50));
        assert_eq!(failure.kind(), FailureKind::Timeout);
        assert_eq!(failure.provider(), "slow");
    }

    #[test]
    fn a_provider_volunteered_timeout_and_a_host_cut_off_are_the_same_kind() {
        // Deliberate: the client learns "it timed out" either way. What tells
        // them apart is the log seam's `BudgetMiss`, asserted in Task 5.
        let volunteered =
            ProviderFailure::from_error("slow", ProviderError::Timeout, Duration::ZERO);
        let enforced = ProviderFailure::budget_miss("slow", Duration::ZERO);
        assert_eq!(volunteered.kind(), enforced.kind());
        assert_ne!(
            volunteered.message(),
            enforced.message(),
            "and the message is what distinguishes them for a reader"
        );
    }

    #[test]
    fn a_providers_message_is_sanitized_as_observed_through_the_accessor() {
        // Pins the enforcement, not just the constructor: `message()` is the
        // only way any caller outside this module can read a
        // `ProviderFailure`'s text at all, since the field itself is
        // private. If the field were still `pub`, this test would pass either
        // way and prove nothing about enforcement — reading through the
        // accessor is what makes it exercise the one path a consumer has.
        let raw = format!("\u{1b}[31m{}", "x".repeat(MAX_PROVIDER_MESSAGE * 2));
        let failure =
            ProviderFailure::from_error("apps", ProviderError::Failed(raw), Duration::ZERO);
        let observed = failure.message();
        assert_eq!(observed.len(), MAX_PROVIDER_MESSAGE);
        assert!(!observed.contains('\u{1b}'));
    }

    #[test]
    fn the_noop_log_accepts_every_event_shape() {
        // Compile-and-run coverage that `ProviderEvent`'s borrows work for a
        // real implementation, which is what Task 5's call sites depend on.
        let failure = ProviderFailure::panicked("apps", Duration::ZERO);
        NoopLog.record(ProviderEvent::Answered {
            provider: "apps",
            items: 3,
            elapsed: Duration::ZERO,
        });
        NoopLog.record(ProviderEvent::Failed(&failure));
        NoopLog.record(ProviderEvent::BudgetMiss {
            provider: "apps",
            budget: Duration::ZERO,
            elapsed: Duration::ZERO,
        });
        NoopLog.record(ProviderEvent::Rejected {
            provider: "apps",
            rejections: &[],
        });
        NoopLog.record(ProviderEvent::Skipped { provider: "apps" });
    }

    #[test]
    fn the_recording_log_captures_what_it_is_told() {
        let log = RecordingLog::default();
        log.record(ProviderEvent::Answered {
            provider: "apps",
            items: 2,
            elapsed: Duration::ZERO,
        });
        log.record(ProviderEvent::Skipped { provider: "calc" });
        assert_eq!(log.lines(), vec!["answered apps 2", "skipped calc"]);
    }

    use crate::provider::{Provider, ProviderManifest, QueryCtx};
    use crate::router::{Mode, RoutedQuery, route};
    use hop_protocol::{ActionId, ExecOutcome, Item, ItemId, Kind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider whose manifest is whatever the test says it is, and whose
    /// `query` answers with a fixed list. Task 5's tests extend this file with
    /// providers that hang and panic; this one is the well-behaved baseline.
    pub(crate) struct ScriptedProvider {
        pub(crate) manifest: ProviderManifest,
        pub(crate) items: Vec<Item>,
        /// How many times `manifest()` has been called — the counter that
        /// proves capture happens once.
        pub(crate) manifest_calls: AtomicUsize,
    }

    impl ScriptedProvider {
        pub(crate) fn new(id: &'static str, kinds: Vec<Kind>, items: Vec<Item>) -> Self {
            ScriptedProvider {
                manifest: ProviderManifest {
                    id,
                    kinds,
                    modes: vec![Mode::All],
                    min_term_len: 0,
                    budget: Duration::from_millis(10),
                },
                items,
                manifest_calls: AtomicUsize::new(0),
            }
        }
    }

    impl Provider for ScriptedProvider {
        fn manifest(&self) -> ProviderManifest {
            self.manifest_calls.fetch_add(1, Ordering::Relaxed);
            self.manifest.clone()
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
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

    /// A provider whose `manifest()` answers one way the first time and
    /// another way afterwards — issue #32's interior-mutability abuse, built
    /// from honest-looking parts.
    pub(crate) struct ShiftyProvider {
        calls: AtomicUsize,
    }

    impl ShiftyProvider {
        pub(crate) fn new() -> Self {
            ShiftyProvider {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Provider for ShiftyProvider {
        fn manifest(&self) -> ProviderManifest {
            let first = self.calls.fetch_add(1, Ordering::Relaxed) == 0;
            ProviderManifest {
                id: "shifty",
                kinds: vec![Kind::App],
                modes: vec![Mode::Apps],
                // Declares a 3-character minimum while the host is looking,
                // then zero forever after — so an unscheduled provider would
                // start being dispatched on every keystroke.
                min_term_len: if first { 3 } else { 0 },
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Ok(Vec::new())
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    // Task 5 (query execution) is the caller; this allow goes with it.
    #[allow(dead_code)]
    pub(crate) fn item(provider: &str, kind: Kind, id: &str, title: &str) -> Item {
        Item {
            id: ItemId::new(id).unwrap(),
            kind,
            title: title.to_string(),
            subtitle: None,
            icon: None,
            actions: vec![],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: provider.to_string(),
        }
    }

    fn host() -> ProviderHost {
        ProviderHost::with_log(Arc::new(NoopLog))
    }

    #[test]
    fn a_registered_providers_manifest_is_read_exactly_once() {
        let provider = Arc::new(ScriptedProvider::new("apps", vec![Kind::App], vec![]));
        let mut host = host();
        // Registration takes ownership, so the counter is read through a clone
        // of the Arc the test kept.
        host.register_arc(provider.clone()).unwrap();
        assert_eq!(
            provider.manifest_calls.load(Ordering::Relaxed),
            1,
            "registration reads the manifest once and captures it"
        );
        assert_eq!(host.len(), 1);
    }

    #[test]
    fn a_second_registration_under_an_id_already_in_use_is_refused() {
        // Load-bearing for boost correctness, not registry hygiene: two
        // providers sharing an id both pass `CheckedItems::check` and both
        // collect every alias boost tagged with it — see `APPS_PROVIDER_ID`'s
        // docs, which name rejecting this as the host's job.
        let mut host = host();
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        let err = host
            .register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .expect_err("a duplicate id must be refused");
        assert!(matches!(err, RegistrationError::DuplicateId(id) if id == "apps"));
        assert_eq!(host.len(), 1, "the duplicate must not be registered");
    }

    #[test]
    fn a_manifest_budget_over_the_ceiling_is_clamped() {
        let mut provider = ScriptedProvider::new("greedy", vec![Kind::App], vec![]);
        provider.manifest.budget = Duration::from_secs(3600);
        let mut host = host();
        host.register(provider).unwrap();
        assert_eq!(
            host.manifests()[0].budget,
            MAX_PROVIDER_BUDGET,
            "an hour-long budget is clamped to the host's ceiling"
        );
    }

    #[test]
    fn a_manifest_budget_under_the_ceiling_is_left_alone() {
        let mut host = host();
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        assert_eq!(host.manifests()[0].budget, Duration::from_millis(10));
    }

    #[test]
    fn the_host_can_raise_a_minimum_term_length_above_what_a_provider_declared() {
        let mut host = ProviderHost::new(
            HostPolicy {
                min_term_len_floor: 2,
                ..HostPolicy::default()
            },
            Arc::new(NoopLog),
        );
        // Declares 0 — "always run, including for the empty term".
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        assert_eq!(host.manifests()[0].min_term_len, 2);
    }

    #[test]
    fn the_floor_never_lowers_a_providers_own_minimum() {
        let mut provider = ScriptedProvider::new("apps", vec![Kind::App], vec![]);
        provider.manifest.min_term_len = 5;
        let mut host = ProviderHost::new(
            HostPolicy {
                min_term_len_floor: 2,
                ..HostPolicy::default()
            },
            Arc::new(NoopLog),
        );
        host.register(provider).unwrap();
        assert_eq!(
            host.manifests()[0].min_term_len,
            5,
            "the floor raises, it never relaxes a provider's own stricter rule"
        );
    }

    #[test]
    fn scheduling_reads_the_captured_manifest_so_a_shifty_provider_changes_nothing() {
        // The whole of issue #32: `min_term_len: 3` at registration, `0`
        // afterwards. Scheduling must still refuse a 2-character term.
        let mut host = host();
        host.register(ShiftyProvider::new()).unwrap();

        let short = route("a hi");
        assert_eq!(short.term, "hi");
        assert!(
            host.selected_ids(&short).is_empty(),
            "the captured minimum of 3 governs, not the 0 it now answers with"
        );

        let long = route("a firefox");
        assert_eq!(
            host.selected_ids(&long),
            vec!["shifty"],
            "and a term over the captured minimum still reaches it"
        );
    }

    #[test]
    fn the_prefilter_declines_a_provider_whose_captured_modes_exclude_the_route() {
        let mut host = host();
        // `ScriptedProvider` declares `modes: [Mode::All]`, so an exclusive
        // windows route reaches nothing.
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        assert!(host.selected_ids(&route("w terminal")).is_empty());
        assert_eq!(host.selected_ids(&route("terminal")), vec!["apps"]);
    }

    #[test]
    fn a_skipped_provider_is_recorded_on_the_log_seam() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ScriptedProvider::new("apps", vec![Kind::App], vec![]))
            .unwrap();
        host.selected_ids(&route("w terminal"));
        assert_eq!(log.lines(), vec!["skipped apps"]);
    }
}
