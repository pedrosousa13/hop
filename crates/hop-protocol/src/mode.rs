//! The search-mode vocabulary.
//!
//! [`Mode`] lives here rather than in `hop-core`, where routing itself lives,
//! for the same reason [`Kind`](crate::Kind) does: it crosses a process
//! boundary, and every type that does is this crate's business. The daemon
//! decides a query's mode and the client needs to know which one answered —
//! see [`DaemonMsg::QueryRouted`](crate::DaemonMsg::QueryRouted).
//!
//! `hop_core::router` re-exports this type, carrying the routing-side
//! documentation with it: what each variant does and does not promise about a
//! term is a property of `route()`, not of the wire, so that reasoning belongs
//! next to the function that establishes it.

use serde::{Deserialize, Serialize};

/// Which search mode a query was interpreted as.
///
/// A **closed set**, the same treatment [`Kind`](crate::Kind) gets and for the
/// same reason: both are vocabularies a peer must already understand, so a
/// value outside the set is a protocol error rather than something to pass
/// through. Adding a variant is a wire-contract change like any other.
///
/// # What this type does not tell a client
///
/// Two warnings, both established by `hop_core::router` and repeated here
/// because a client reading a frame sees this type without that context:
///
/// - **A mode is not a sink.** It says which providers were asked, never how
///   the term must be escaped — that is the answering provider's property.
/// - **A mode is not a shape check.** [`Mode::Currency`] says a conversion was
///   asked for, not that the term carries a number; [`Mode::Calculator`] does
///   not promise an evaluable expression.
///
/// A client may therefore use this to *label* what happened, which is what
/// [`DaemonMsg::QueryRouted`](crate::DaemonMsg::QueryRouted) exists for. It
/// may not use it to infer anything about the items in the same exchange
/// beyond which modes could have produced them.
///
/// # `exclusive` is the load-bearing half, and it is not on this type
///
/// The mode alone does not say whether results were *filtered*. An exclusive
/// route filters to its mode's kinds and nothing else shows; an inferred route
/// filters nothing and merely promotes. Those are different things to tell a
/// user — one lost them results they cannot see — and the distinction rides
/// beside this value as `QueryRouted`'s `exclusive` field rather than inside
/// it, because the same variant is reachable both ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    All,
    Windows,
    Apps,
    Files,
    Emoji,
    Timezone,
    Currency,
    Calculator,
    Weather,
    Actions,
    /// Part of the vocabulary, but `hop_core::router::route` never returns it
    /// yet — no explicit prefix or inference rule targets it in this
    /// milestone.
    WebSearch,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// The wire spelling is snake_case, and `WebSearch` is the only variant
    /// where that is more than lowercasing — pinned so a rename cannot
    /// silently change the contract.
    #[test]
    fn modes_serialize_as_snake_case() {
        assert_eq!(serde_json::to_string(&Mode::All).unwrap(), r#""all""#);
        assert_eq!(
            serde_json::to_string(&Mode::WebSearch).unwrap(),
            r#""web_search""#
        );
        assert_eq!(
            serde_json::to_string(&Mode::Calculator).unwrap(),
            r#""calculator""#
        );
    }

    #[test]
    fn modes_round_trip() {
        for mode in [
            Mode::All,
            Mode::Windows,
            Mode::Apps,
            Mode::Files,
            Mode::Emoji,
            Mode::Timezone,
            Mode::Currency,
            Mode::Calculator,
            Mode::Weather,
            Mode::Actions,
            Mode::WebSearch,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(serde_json::from_str::<Mode>(&json).unwrap(), mode);
        }
    }

    /// A value outside the closed set is a refusal, not a fallback to `All`.
    /// `All` is the *routing* fallback and would be a plausible-looking wrong
    /// answer here, so this pins that deserialization does not reach for it.
    #[test]
    fn an_unknown_mode_is_refused_rather_than_defaulted() {
        assert!(serde_json::from_str::<Mode>(r#""not_a_mode""#).is_err());
        assert!(serde_json::from_str::<Mode>(r#""All""#).is_err());
    }
}
