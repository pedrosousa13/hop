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
use std::time::{Duration, Instant};

use hop_protocol::Item;
use tokio::sync::mpsc;

use crate::pipeline::{CheckedItems, ProviderOutput, Rejection};
use crate::provider::{
    CancellationFlag, Provider, ProviderError, ProviderManifest, QueryCtx, should_query,
};
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
    declared: ProviderManifest,
    /// `declared` with [`HostPolicy`] applied. Every scheduling decision and
    /// the enforced budget read this, and nothing re-reads
    /// [`Provider::manifest`] to make one.
    effective: ProviderManifest,
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

    /// Runs every provider this routed query reaches, each as its own task,
    /// each under the budget captured for it, streaming what each one answers
    /// as its own batch.
    ///
    /// Returns the [`CancellationFlag`] shared by every provider's
    /// [`QueryCtx`], so a caller that wants to cancel cooperatively can — but
    /// the flag is not how cancellation normally arrives. Dropping `results`
    /// is: a provider's send then fails, and that failure sets this flag for
    /// every sibling still running. That makes cancellation a property of the
    /// channel, matching `hopd`'s `ResultSource` contract, rather than a second
    /// mechanism a caller has to remember.
    ///
    /// # Why one task per provider
    ///
    /// "One task" names the unit of isolation, not a literal count:
    /// [`ProviderHost::run_one`] itself spawns a second, inner task to run
    /// the provider's query future under, so each selected provider actually
    /// gets two — an outer supervisor task (the one this function spawns,
    /// running `run_one`) and the inner, abortable query task `run_one`
    /// spawns within it. Both are necessary and neither is redundant: the
    /// supervisor is what has to outlive the panic it reports, so it cannot
    /// be the same task the panic happens in, and the inner task is what
    /// gives `run_one` a [`JoinHandle`](tokio::task::JoinHandle) it can time
    /// out and then abort. It is what makes three separate guarantees hold at
    /// once, and no shape with fewer tasks delivers all three:
    ///
    /// - **Panic containment.** A panic inside a spawned task surfaces as
    ///   [`JoinError::is_panic`](tokio::task::JoinError::is_panic) rather than
    ///   unwinding into whatever polled it. Polling providers in one task, the
    ///   only option before issue #29 made the future `'static`, means one
    ///   provider's panic takes the query — and, on the connection driver's
    ///   task, the connection.
    /// - **A cut-off that needs no cooperation.** The task is timed out and
    ///   then aborted, so a provider that never polls
    ///   [`QueryCtx::cancel`] is still abandoned at its budget.
    /// - **No slowest-provider gate** (spec §3). Each task sends as soon as its
    ///   provider answers, so a fast provider's items are on the wire while a
    ///   slow one is still running.
    ///
    /// # What abort does and does not stop
    ///
    /// [`JoinHandle::abort`](tokio::task::JoinHandle::abort) takes effect at
    /// the task's next yield point. A provider awaiting anything is dropped
    /// promptly; a provider in a loop that never yields keeps a worker thread
    /// until it does. What the host guarantees regardless is its own
    /// behaviour: it stops waiting at the budget, reports the miss, and the
    /// frame is never blocked. Bounding a non-yielding provider needs
    /// process-level isolation, which issue #29 puts explicitly out of scope
    /// and the v3 sandbox tier (spec §6) is the answer to.
    pub fn spawn_query(
        self: &Arc<Self>,
        q: Arc<RoutedQuery>,
        results: mpsc::Sender<Vec<Item>>,
    ) -> CancellationFlag {
        let cancel = CancellationFlag::default();

        for registration in self.selected(&q) {
            let host = Arc::clone(self);
            let provider = Arc::clone(&registration.provider);
            let declared = registration.declared.clone();
            let effective = registration.effective.clone();
            let q = Arc::clone(&q);
            let results = results.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                host.run_one(provider, declared, effective, q, results, cancel)
                    .await;
            });
        }

        // Every task holds its own clone of `results`; this function's copy
        // going out of scope is what lets the last task's drop close the
        // channel. A host with no selected providers therefore closes it here,
        // which is how "nothing answered" reaches the driver as a clean
        // `QueryDone` rather than a hang.
        cancel
    }

    /// One provider's whole turn: run it under its budget, classify what came
    /// back, check its items against its own manifest, and send what survived.
    async fn run_one(
        &self,
        provider: Arc<dyn ErasedProvider>,
        declared: ProviderManifest,
        effective: ProviderManifest,
        q: Arc<RoutedQuery>,
        results: mpsc::Sender<Vec<Item>>,
        cancel: CancellationFlag,
    ) {
        let id = effective.id;
        let budget = effective.budget;
        let started = Instant::now();

        let ctx = QueryCtx {
            cancel: cancel.clone(),
            deadline: started + budget,
        };

        // The handle is kept rather than moved into `timeout`, so the task can
        // still be aborted after the budget expires. `JoinHandle` is `Unpin`,
        // which is what makes `&mut handle` a future.
        let mut handle = tokio::spawn(Arc::clone(&provider).query_erased(q, ctx));
        let outcome = match tokio::time::timeout(budget, &mut handle).await {
            Err(_elapsed) => {
                handle.abort();
                let elapsed = started.elapsed();
                self.log.record(ProviderEvent::BudgetMiss {
                    provider: id,
                    budget,
                    elapsed,
                });
                Err(ProviderFailure::budget_miss(id, elapsed))
            }
            Ok(Err(join_error)) if join_error.is_panic() => {
                Err(ProviderFailure::panicked(id, started.elapsed()))
            }
            Ok(Err(_cancelled_task)) => Err(ProviderFailure::from_error(
                id,
                ProviderError::Cancelled,
                started.elapsed(),
            )),
            Ok(Ok(Err(error))) => Err(ProviderFailure::from_error(id, error, started.elapsed())),
            Ok(Ok(Ok(items))) => Ok(items),
        };

        let items = match outcome {
            Ok(items) => items,
            Err(failure) => {
                self.log.record(ProviderEvent::Failed(&failure));
                return;
            }
        };

        // The comparison `ProviderOutput::from_provider`'s docs ask a host to
        // make, and which only a host can: a captured manifest cannot be
        // re-minted in response to what a provider decided to return, so a
        // provider whose `manifest()` now answers differently is caught here
        // and its whole answer refused. `declared` rather than `effective` is
        // the baseline, because clamping deliberately changes fields.
        //
        // `provider.output(items)` is the *only* remaining `Provider::manifest`
        // call this function makes, and the comparison below reads it back
        // through `ProviderOutput::manifest` rather than minting a second,
        // separate call to compare against `declared`. That is deliberate:
        // `CheckedItems::check` below checks `items` against whatever manifest
        // `output` was actually built with, so a call made — and checked —
        // here, then discarded in favour of a *third* call for the pairing,
        // would leave the pairing checked against a manifest nothing verified.
        // A provider can answer differently on adjacent calls (see
        // `ShiftyProvider`), so "the manifest was checked" is only true of the
        // manifest that was actually checked.
        let output = provider.output(items);
        if output.manifest() != &declared {
            let failure = ProviderFailure::from_error(
                id,
                ProviderError::Failed(
                    "the provider's manifest changed after registration".to_string(),
                ),
                started.elapsed(),
            );
            self.log.record(ProviderEvent::Failed(&failure));
            return;
        }

        // One `ProviderOutput`, from this provider alone, so each item is
        // checked against its own producer and nothing else — the property
        // `CheckedItems::check`'s loop comment warns against hoisting away.
        let checked = CheckedItems::check(vec![output]);
        if !checked.rejections().is_empty() {
            self.log.record(ProviderEvent::Rejected {
                provider: id,
                rejections: checked.rejections(),
            });
        }

        let items = checked.items().to_vec();
        self.log.record(ProviderEvent::Answered {
            provider: id,
            items: items.len(),
            elapsed: started.elapsed(),
        });

        if items.is_empty() {
            // No send to fail here, so nothing would otherwise notice a
            // dropped receiver on this path — check directly, so a provider
            // that answers empty still relays cancellation to its siblings.
            if results.is_closed() {
                cancel.cancel();
            }
            return;
        }

        // A failed send means the receiver is gone, which is this seam's
        // cancellation. Setting the flag is what carries that to the siblings
        // still running: they learn it from the flag rather than waiting to
        // discover their own send failing.
        if results.send(items).await.is_err() {
            cancel.cancel();
        }
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
    /// [`Provider::query`] with its future boxed, which is what makes the
    /// method dyn-compatible.
    fn query_erased(
        self: Arc<Self>,
        q: Arc<RoutedQuery>,
        ctx: QueryCtx,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Item>, ProviderError>> + Send + 'static>>;

    /// Pairs `items` with this provider's manifest the only way
    /// [`ProviderOutput`](crate::pipeline::ProviderOutput) allows — see the
    /// trait docs for why this method exists here rather than at the call
    /// site. `run_one` reads the manifest this mints back through
    /// [`ProviderOutput::manifest`](crate::pipeline::ProviderOutput::manifest)
    /// rather than through a separate erased `manifest()` call, so this is
    /// the only [`Provider::manifest`] call this trait makes.
    fn output(&self, items: Vec<Item>) -> ProviderOutput;
}

impl<P: Provider> ErasedProvider for P {
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

    /// A provider whose declared `kinds` widen starting on its *third* call —
    /// registration is the first, and `run_one`'s single post-registration
    /// manifest call (made inside `provider.output(items)`) is the second.
    ///
    /// Built to pin that `run_one` makes exactly one manifest call after
    /// registration, and that the call it makes is the one
    /// [`CheckedItems::check`] actually checks items against — not an earlier
    /// or later one. Before that was true, `run_one` re-checked a *separate*
    /// call (`provider.manifest()`) against `declared`, then minted a *third*
    /// call inside `provider.output(items)` to build the checked
    /// `ProviderOutput`. A provider like this one — `declared` on its first
    /// two calls, widened from then on — would pass that re-check on call two
    /// while its widened call three governed the actual item check, letting a
    /// forged `Kind::Window` item through. Under the fixed, single-call
    /// shape, this provider's widening is never reached: call two is both the
    /// re-check and the mint, so it still answers `declared`, and its forged
    /// item is refused like any other kind mismatch.
    pub(crate) struct DelayedWideningProvider {
        calls: AtomicUsize,
    }

    impl DelayedWideningProvider {
        pub(crate) fn new() -> Self {
            DelayedWideningProvider {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Provider for DelayedWideningProvider {
        fn manifest(&self) -> ProviderManifest {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            let kinds = if call <= 1 {
                vec![Kind::App]
            } else {
                vec![Kind::App, Kind::Window]
            };
            ProviderManifest {
                id: "drifting",
                kinds,
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Ok(vec![
                item("drifting", Kind::App, "app:ok", "Fine"),
                item("drifting", Kind::Window, "window:forged", "Forged"),
            ])
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

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

    use tokio::sync::mpsc;

    /// A provider that never returns — the non-cooperating case #28 is about.
    /// It does not poll `ctx.cancel` at all, so nothing but the host can end
    /// it.
    pub(crate) struct HangingProvider;

    impl Provider for HangingProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "hanging",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            // A yielding hang rather than a busy loop: `abort` takes effect at
            // a yield point, and a busy loop would pin a worker thread for the
            // whole test run. The provider still never checks cancellation,
            // which is the property under test.
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// Behaviourally identical to [`HangingProvider`] — never completes, never
    /// polls `ctx.cancel` — but with a budget the caller chooses rather than a
    /// fixed 10 ms.
    ///
    /// Used only by
    /// `a_hanging_provider_does_not_delay_a_fast_providers_batch` below, which
    /// needs a wide budget to give its wall-clock assertion room under CI
    /// contention. A separate type rather than a configurable field on
    /// `HangingProvider` itself, which other tests already depend on staying
    /// exactly as it is.
    pub(crate) struct SlowHangingProvider {
        budget: Duration,
    }

    impl SlowHangingProvider {
        pub(crate) fn new(budget: Duration) -> Self {
            SlowHangingProvider { budget }
        }
    }

    impl Provider for SlowHangingProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "hanging",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: self.budget,
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// Records, via `Drop`, that the value holding it was dropped. Used only
    /// by [`AbandonedOnDropProvider`] — see that type's docs for why this is
    /// the one observable difference between a task that was actually
    /// aborted and a host that merely stopped waiting on it.
    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Behaviourally identical to [`HangingProvider`] — never completes,
    /// never polls `ctx.cancel` — but its future also holds a [`DropSignal`]
    /// across its one await point, so the flag it shares with the test tells
    /// the test whether the future was ever actually dropped.
    ///
    /// A separate type rather than an addition to `HangingProvider`, which
    /// other tests already depend on staying exactly as it is.
    ///
    /// # Why this exists
    ///
    /// `a_provider_that_never_completes_is_cut_off_at_its_budget_without_cooperating`
    /// asserts that the host stopped waiting and logged a budget miss.
    /// Neither observation distinguishes "the host stopped *waiting* and left
    /// the task running forever" from "the host *abandoned* the task":
    /// removing `run_one`'s `handle.abort()` call and keeping only the
    /// `tokio::time::timeout` around it produces the identical "stopped
    /// waiting, logged a miss" outcome, because `timeout` alone only stops
    /// awaiting the handle — it does not touch the task. `abort` is the whole
    /// difference between a provider that is *cut off* (issue #28's
    /// criterion) and one that merely stops being listened to.
    ///
    /// The guard closes that gap: it is constructed inside the `query`
    /// future's body and held across the `loop`'s await point, so it lives in
    /// the future's own suspended state. Dropping the future — which is what
    /// `abort` does — drops the guard with it. A task that is still running,
    /// just no longer awaited by anything, never reaches a point where this
    /// value goes out of scope, so the guard stays alive and the flag stays
    /// `false`. Because this provider never completes on its own, a `true`
    /// flag can only mean the future was dropped, never that it ran to
    /// completion.
    pub(crate) struct AbandonedOnDropProvider {
        dropped: Arc<AtomicBool>,
    }

    impl AbandonedOnDropProvider {
        /// A provider paired with the flag its guard will set, so a test can
        /// hold the flag after registration consumes the provider.
        pub(crate) fn new() -> (Self, Arc<AtomicBool>) {
            let dropped = Arc::new(AtomicBool::new(false));
            (
                AbandonedOnDropProvider {
                    dropped: dropped.clone(),
                },
                dropped,
            )
        }
    }

    impl Provider for AbandonedOnDropProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "abandoned",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            // Constructed here, inside the future, and held across the
            // loop's await point — see the type's docs for why that placement
            // is what makes a drop of this future observable from outside it.
            let _guard = DropSignal(Arc::clone(&self.dropped));
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// The one provider in this file that actually reads its `QueryCtx`.
    /// Every other provider here ignores `_ctx` entirely, which is why
    /// nothing in this file otherwise notices whether `run_one` hands each
    /// provider the *shared* [`CancellationFlag`] and a `deadline` that
    /// actually reflects its budget, as opposed to a freshly-defaulted flag
    /// or an already-elapsed deadline — both compile, both leave every other
    /// test in this file green.
    ///
    /// On `query`, it records how much time remained under `ctx.deadline` the
    /// moment it started (`remaining_at_start`), then loops polling
    /// `ctx.cancel` until it is set and records that it saw cancellation
    /// (`saw_cancellation`) before giving up. It never completes on its own —
    /// the only way its `query` future returns is by observing cancellation —
    /// so a caller has to actually cancel the query for this provider's turn
    /// to end at all.
    pub(crate) struct CooperativeProvider {
        saw_cancellation: Arc<AtomicBool>,
        remaining_at_start: Arc<Mutex<Option<Duration>>>,
    }

    impl CooperativeProvider {
        /// The budget this provider declares — named so the test that
        /// registers it can assert `remaining_at_start` against the same
        /// value rather than a repeated literal.
        pub(crate) const BUDGET: Duration = Duration::from_millis(200);

        /// A provider paired with the two flags its `query` future will set,
        /// so a test can read them after registration consumes the provider.
        pub(crate) fn new() -> (Self, Arc<AtomicBool>, Arc<Mutex<Option<Duration>>>) {
            let saw_cancellation = Arc::new(AtomicBool::new(false));
            let remaining_at_start = Arc::new(Mutex::new(None));
            (
                CooperativeProvider {
                    saw_cancellation: saw_cancellation.clone(),
                    remaining_at_start: remaining_at_start.clone(),
                },
                saw_cancellation,
                remaining_at_start,
            )
        }
    }

    impl Provider for CooperativeProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "cooperative",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: CooperativeProvider::BUDGET,
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            let remaining = ctx.deadline.saturating_duration_since(Instant::now());
            *self
                .remaining_at_start
                .lock()
                .expect("no test panics holding this") = Some(remaining);

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

    /// A provider that panics inside its future.
    pub(crate) struct PanickingProvider;

    impl Provider for PanickingProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "panicking",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            panic!("a provider indexing an empty vec");
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// A provider that fails with attacker-shaped text: over the cap, opening
    /// with a terminal escape and a right-to-left override.
    pub(crate) struct NastyProvider;

    impl Provider for NastyProvider {
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: "nasty",
                kinds: vec![Kind::App],
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(10),
            }
        }

        async fn query(
            self: Arc<Self>,
            _q: Arc<RoutedQuery>,
            _ctx: QueryCtx,
        ) -> Result<Vec<Item>, ProviderError> {
            Err(ProviderError::Failed(format!(
                "\u{1b}[31m\u{202e}{}",
                "x".repeat(MAX_PROVIDER_MESSAGE * 10)
            )))
        }

        async fn execute(
            self: Arc<Self>,
            _item_id: ItemId,
            _action_id: ActionId,
        ) -> Result<ExecOutcome, ProviderError> {
            Ok(ExecOutcome::Done)
        }
    }

    /// Drains every batch a query produces, in arrival order.
    async fn drain(mut rx: mpsc::Receiver<Vec<Item>>) -> Vec<Item> {
        let mut all = Vec::new();
        while let Some(batch) = rx.recv().await {
            all.extend(batch);
        }
        all
    }

    /// Runs one query against `host` and returns everything it streamed.
    async fn run(host: Arc<ProviderHost>, raw: &str) -> Vec<Item> {
        let (tx, rx) = mpsc::channel(1);
        host.spawn_query(Arc::new(route(raw)), tx);
        drain(rx).await
    }

    #[tokio::test]
    async fn a_well_behaved_providers_items_are_streamed() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let items = run(Arc::new(host), "firefox").await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Firefox");
        assert_eq!(log.lines(), vec!["answered apps 1"]);
    }

    #[tokio::test]
    async fn the_channel_closes_once_every_selected_provider_has_finished() {
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(ScriptedProvider::new("a", vec![Kind::App], vec![]))
            .unwrap();
        host.register(ScriptedProvider::new("b", vec![Kind::App], vec![]))
            .unwrap();

        let (tx, mut rx) = mpsc::channel(1);
        Arc::new(host).spawn_query(Arc::new(route("x")), tx);
        // Both answer with no items, so nothing is sent and the only event is
        // the close — which is what `hopd`'s driver turns into `QueryDone`.
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_host_with_no_providers_closes_immediately() {
        let host = Arc::new(ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog)));
        let (tx, mut rx) = mpsc::channel(1);
        host.spawn_query(Arc::new(route("x")), tx);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_provider_that_never_completes_is_cut_off_at_its_budget_without_cooperating() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(HangingProvider).unwrap();

        let started = std::time::Instant::now();
        let items = run(Arc::new(host), "x").await;
        let waited = started.elapsed();

        assert!(items.is_empty());
        assert!(
            waited < Duration::from_secs(1),
            "the host stopped waiting on its own; it waited {waited:?}"
        );
        let lines = log.lines();
        assert!(
            lines.iter().any(|l| l == "budget-miss hanging"),
            "a budget miss must reach the seam: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "failed hanging Timeout the provider exceeded its budget"),
            "and be reported as a timeout: {lines:?}"
        );
    }

    /// What the test above does *not* pin, and this one does: that the
    /// abandoned task was actually `abort`-ed, not merely stopped-waiting-on.
    /// `a_provider_that_never_completes_is_cut_off_at_its_budget_without_cooperating`
    /// asserts only that the host stopped waiting and logged a budget miss —
    /// both true whether `run_one` calls `handle.abort()` or just lets
    /// `tokio::time::timeout` expire and walks away. Deleting the `abort()`
    /// call leaves that test green. It does not leave this one green:
    /// [`AbandonedOnDropProvider`]'s guard is dropped if and only if its
    /// future is dropped, which only `abort` causes, so a `false` flag here
    /// means the task was abandoned-but-still-running rather than cut off.
    /// See that type's docs for the full mechanism. Do not delete this test
    /// as redundant with the one above — the two assert different halves of
    /// issue #28's criterion.
    #[tokio::test]
    async fn a_hanging_providers_future_is_actually_dropped_when_the_host_aborts_it() {
        let (provider, dropped) = AbandonedOnDropProvider::new();
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(provider).unwrap();

        run(Arc::new(host), "x").await;

        // `abort` takes effect at the task's next yield point, not
        // synchronously with the host giving up on it, so the flag may not be
        // set the instant `run` returns. Poll for it, the same way
        // `dropping_the_receiver_cancels_the_providers_still_running` polls
        // for the cancellation flag, rather than asserting immediately or
        // sleeping a fixed amount.
        for _ in 0..100 {
            if dropped.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the provider's future was never dropped — the task was never actually aborted");
    }

    #[tokio::test]
    async fn a_panicking_provider_yields_a_panic_shaped_failure_naming_it() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(PanickingProvider).unwrap();

        let items = run(Arc::new(host), "x").await;
        assert!(items.is_empty());
        assert_eq!(
            log.lines(),
            vec!["failed panicking Panicked the provider panicked"]
        );
    }

    #[tokio::test]
    async fn one_providers_panic_does_not_cost_another_provider_its_results() {
        // Spec §9's per-provider isolation rule, and #29's second criterion.
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(PanickingProvider).unwrap();
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let items = run(Arc::new(host), "firefox").await;
        assert_eq!(
            items.len(),
            1,
            "the surviving provider's items still reach the client"
        );
        assert_eq!(items[0].title, "Firefox");
        assert!(
            log.lines()
                .iter()
                .any(|l| l.starts_with("failed panicking"))
        );
    }

    #[tokio::test]
    async fn a_hanging_provider_does_not_delay_a_fast_providers_batch() {
        // "No slowest-provider gate" (spec §3): the fast provider's items must
        // arrive well before the hanging one's budget expires.
        //
        // The hanging provider's budget (500 ms, via `SlowHangingProvider`)
        // and the `tokio::time::timeout` bound below (100 ms) are chosen for
        // margin under CI contention, not to measure anything: a tighter pair
        // (an earlier version used a 10 ms budget and a 5 ms timeout) leaves
        // only a couple of milliseconds of slack, and ordinary scheduling
        // jitter under a loaded test binary can eat that. A host that gated
        // the fast provider's batch on every provider finishing could not
        // beat 500 ms — the hanging provider never finishes on its own — so
        // 100 ms is a bound only a genuinely ungated host can meet, with
        // ample room below the 500 ms floor a gated one would need. The
        // `HostPolicy` here is widened past its 50 ms default
        // (`MAX_PROVIDER_BUDGET`) so the 500 ms manifest budget is not
        // silently clamped back down before it can do its job. Do not
        // tighten these numbers back down; they are chosen for margin.
        let policy = HostPolicy {
            max_budget: Duration::from_millis(500),
            ..HostPolicy::default()
        };
        let mut host = ProviderHost::new(policy, Arc::new(NoopLog));
        host.register(SlowHangingProvider::new(Duration::from_millis(500)))
            .unwrap();
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let (tx, mut rx) = mpsc::channel(1);
        Arc::new(host).spawn_query(Arc::new(route("firefox")), tx);

        let first = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("the fast provider's batch must not wait on the slow one")
            .expect("a batch, not a close");
        assert_eq!(first[0].title, "Firefox");
    }

    #[tokio::test]
    async fn provider_error_text_is_bounded_and_stripped_before_it_leaves() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(NastyProvider).unwrap();

        run(Arc::new(host), "x").await;
        let lines = log.lines();
        let line = lines.first().expect("one failure was recorded");
        assert!(line.starts_with("failed nasty Failed "));
        let message = line.trim_start_matches("failed nasty Failed ");
        assert_eq!(message.len(), MAX_PROVIDER_MESSAGE);
        assert!(!message.contains('\u{1b}'));
        assert!(!message.contains('\u{202e}'));
    }

    #[tokio::test]
    async fn items_that_fail_their_own_producers_manifest_are_refused_and_recorded() {
        // The manifest checks still run, and their rejections now have
        // somewhere to go. This provider declares `kinds: [App]` and returns a
        // Window item.
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![
                item("apps", Kind::App, "app:ok", "Fine"),
                item("apps", Kind::Window, "window:forged", "Forged"),
            ],
        ))
        .unwrap();

        let items = run(Arc::new(host), "x").await;
        assert_eq!(
            items.len(),
            1,
            "the forged-kind item never reaches a client"
        );
        assert_eq!(items[0].id.as_str(), "app:ok");
        assert!(log.lines().iter().any(|l| l == "rejected apps 1"));
    }

    #[tokio::test]
    async fn a_provider_whose_manifest_shifted_after_registration_has_its_answer_refused() {
        // The comparison `ProviderOutput::from_provider`'s docs ask a host to
        // make: captured versus fresh, refuse on mismatch. `ShiftyProvider`
        // answers `min_term_len: 3` once and `0` afterwards, so its second
        // call — the one the check makes — differs.
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(ShiftyProvider::new()).unwrap();

        let items = run(Arc::new(host), "a firefox").await;
        assert!(items.is_empty());
        let lines = log.lines();
        assert!(
            lines.iter().any(|l| l.starts_with("failed shifty Failed")),
            "the mismatch is reported as a failure attributed to the provider: {lines:?}"
        );
    }

    /// Pins that `run_one` checks items against the *same* manifest call it
    /// re-checks against `declared` — not an earlier or later one. See
    /// [`DelayedWideningProvider`]'s docs for the two-call trap this closes:
    /// under the old shape, a provider answering `declared` through the
    /// re-check and only widening on a *later*, separate mint call would have
    /// its forged item accepted, because the call that passed the re-check
    /// and the call the item check actually used were different calls. This
    /// test's provider is built with exactly that shape, and the assertions
    /// below are only true because the fix collapsed those into one call.
    #[tokio::test]
    async fn a_provider_that_only_widens_on_a_call_after_the_recheck_is_still_refused() {
        let log = Arc::new(RecordingLog::default());
        let mut host = ProviderHost::new(HostPolicy::default(), log.clone());
        host.register(DelayedWideningProvider::new()).unwrap();

        let items = run(Arc::new(host), "x").await;
        assert_eq!(
            items.len(),
            1,
            "the forged Kind::Window item must never reach a client, however late \
             the provider's manifest widens"
        );
        assert_eq!(items[0].id.as_str(), "app:ok");
        assert!(
            log.lines().iter().any(|l| l == "rejected drifting 1"),
            "the forged item's rejection must reach the seam: {:?}",
            log.lines()
        );
    }

    #[tokio::test]
    async fn dropping_the_receiver_cancels_the_providers_still_running() {
        // The `ResultSource` contract `hopd` relies on: dropping the receiver
        // is cancellation. This checks the flag `spawn_query` returns — set
        // once the lone provider's send fails — not whether a provider
        // observes it through its own `ctx.cancel`: `ScriptedProvider`
        // ignores `_ctx` entirely, same as every other provider in this file
        // but for `CooperativeProvider`. See
        // `the_ctx_a_provider_receives_is_the_shared_flag_and_a_deadline_matching_its_budget`
        // for the test that exercises a provider actually polling its ctx.
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let (tx, rx) = mpsc::channel(1);
        let cancel = Arc::new(host).spawn_query(Arc::new(route("firefox")), tx);
        drop(rx);

        // The provider's send fails, and that is what sets the flag for every
        // sibling still running.
        for _ in 0..100 {
            if cancel.is_cancelled() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("a failed send must set the shared cancellation flag");
    }

    /// Pins the two halves of the `QueryCtx` `run_one` builds:
    /// [`CooperativeProvider`] must observe the *shared*
    /// [`CancellationFlag`] — the same one `spawn_query` returns and every
    /// sibling's failed send sets — through its own `ctx.cancel`, and its
    /// `ctx.deadline` must sit somewhere between "now" and "now plus its
    /// budget", not already in the past.
    ///
    /// Two mutations to `run_one`'s `QueryCtx` construction leave every other
    /// test in this file green because no other provider reads its ctx:
    /// `cancel: CancellationFlag::default()` in place of `cancel.clone()`
    /// would make cooperative cancellation dead wiring — nothing this
    /// provider observes would ever become true — and `deadline:
    /// Instant::now()` in place of `started + budget` would make
    /// `remaining_at_start` zero. This test is the one that would fail under
    /// either.
    #[tokio::test]
    async fn the_ctx_a_provider_receives_is_the_shared_flag_and_a_deadline_matching_its_budget() {
        let (provider, saw_cancellation, remaining_at_start) = CooperativeProvider::new();
        let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(NoopLog));
        host.register(provider).unwrap();
        // A fast provider alongside it: this is the one whose failed send
        // actually sets the shared flag, once `rx` below is dropped. Without
        // it nothing would ever call `cancel.cancel()`, since
        // `CooperativeProvider` itself never sends.
        host.register(ScriptedProvider::new(
            "apps",
            vec![Kind::App],
            vec![item("apps", Kind::App, "app:firefox", "Firefox")],
        ))
        .unwrap();

        let (tx, rx) = mpsc::channel(1);
        Arc::new(host).spawn_query(Arc::new(route("firefox")), tx);
        drop(rx);

        // The deadline half: poll until `CooperativeProvider::query` has
        // recorded what it saw, which happens on its very first poll — well
        // before either provider's budget expires.
        let remaining = 'wait_for_deadline: {
            for _ in 0..100 {
                if let Some(remaining) = *remaining_at_start
                    .lock()
                    .expect("no test panics holding this")
                {
                    break 'wait_for_deadline remaining;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the provider never recorded a deadline reading");
        };
        assert!(
            remaining > Duration::ZERO,
            "an already-elapsed deadline (Instant::now() instead of started + budget) \
             would read as zero or saturate to zero here: {remaining:?}"
        );
        assert!(
            remaining <= CooperativeProvider::BUDGET,
            "the deadline must be no further out than the provider's own budget: {remaining:?}"
        );

        // The cancellation half: the fast provider's send has failed by now
        // (or will shortly), which is what sets the *shared* flag. Confirm
        // this provider's own `ctx.cancel` — not a fresh default — is what it
        // saw go true.
        for _ in 0..100 {
            if saw_cancellation.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the provider never observed cancellation through its own ctx.cancel");
    }
}
