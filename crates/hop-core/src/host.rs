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

use std::time::Duration;

use crate::pipeline::Rejection;
use crate::provider::ProviderError;
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
}
