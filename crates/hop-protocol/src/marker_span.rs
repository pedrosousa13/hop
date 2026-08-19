//! The consumed-marker span on
//! [`DaemonMsg::QueryRouted`](crate::wire::DaemonMsg::QueryRouted): the byte
//! range within a query's raw text that routing consumed as a marker — a
//! prefix, a sigil, a trailing phrase, or (on an alias-matched timezone route)
//! the whole typed token — while deciding which mode to route the query
//! under.
//!
//! Issue #184 wants a client to highlight the consumed marker inside the
//! query field without re-parsing the query text itself (criterion 3): the
//! router already knows exactly what it consumed while it is deciding the
//! route, so it reports the span instead of a client re-deriving it from
//! `mode` and the text it typed. See `hop_core::router::RoutedQuery` for the
//! routing-side half of this story — which branch sets the span to what, and
//! why it is never computed by diffing the routed term against the raw query
//! after the fact.
//!
//! # Why a range and not the marker's text
//!
//! The client that receives this frame is the same client that sent the
//! query, so it already holds the text it typed. Echoing the marker's
//! characters back would put a second copy of the user's own input on the
//! wire, and a second place that input could be read off by whatever is
//! watching the socket or a log of it — a disclosure surface two integers do
//! not open. `start` and `end` let the client slice its own copy instead of
//! being handed one.

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::limits::MAX_QUERY_TEXT;

/// A byte range within a query's raw text, naming the span routing consumed
/// as a marker. See [the module docs](self) for what this is and why it
/// travels as offsets rather than text.
///
/// # What this range does and does not guarantee
///
/// [`MarkerSpan::new`] — which `Deserialize` also calls, so every value that
/// survives a wire round trip passed the same gate — refuses an inverted
/// range (`start` after `end`) and one reaching past [`MAX_QUERY_TEXT`], the
/// same bound a query's raw text is held to before it is ever routed. That is
/// everything this type can check on its own: it is deserialized without the
/// text it is a range *into*, so there is no string here to test `start` and
/// `end` against for landing on a real character boundary. A daemon sending
/// `{"start":1,"end":2}` passes both checks above even against a query whose
/// first character is three bytes long, and nothing at deserialization time
/// can catch that — the check needs a string this type is never handed.
///
/// Closing that gap does not need the text to travel a second time, though.
/// It needs the one way a client reads the marker out to be one that cannot
/// panic on a bad range, however the range came to be bad:
/// [`MarkerSpan::slice`] hands back `text.get(start..end)` rather than
/// `&text[start..end]`, so a range that passed both checks above but still
/// splits a character comes back `None`, exactly as an out-of-bounds one
/// would. A client that reads the marker only through [`MarkerSpan::slice`]
/// — never by indexing its own query text with [`MarkerSpan::start`] and
/// [`MarkerSpan::end`] directly — cannot be made to panic by anything a peer
/// sends on this field, whatever `hopd` did or did not check before sending
/// it.
///
/// # What reporting this span costs
///
/// `start` and `end` are two plain integers, not text, so there is no
/// character content here for a `Debug` to hide the way
/// [`QueryText`](crate::redaction::QueryText)'s hides what the user typed —
/// `MarkerSpan` derives `Debug` and prints both fields as they are. Printing
/// them anyway is a choice, not an oversight, and it is worth pricing rather
/// than filed under "just two numbers".
///
/// On every route decided by a fixed prefix, sigil or suffix (`w `, `$`,
/// ` weather`, and the rest `hop_core::router` enumerates), `end - start` is
/// public before this field is ever read: it is the byte length of a literal
/// spelled out in that module's own source, and which literal matched is
/// already implied by `mode` and `exclusive` on the same frame. What this
/// field adds on those routes is `start` alone — the count of leading
/// whitespace bytes the raw query carried before the marker, which nothing
/// else on the frame reports.
///
/// The one route where the span's length is not public that way is
/// `infer_timezone`'s bare-alias-token branch (`route("pst")`,
/// `route("sao paulo")`): there the marker is the user's own typed token, not
/// a fixed literal, and its length sits close to — on that branch, exactly
/// at — the raw query's own byte length, a figure the routing side already
/// discloses once, in `hop_core::router::RoutedText`'s redacted `Debug`. This
/// field does not open a new disclosure on that route so much as let the same
/// figure be read a second way.
///
/// Redacting this field regardless — printing a marker instead of `start` and
/// `end`, the way `QueryText` redacts what it holds — was considered and
/// rejected. There is no text underneath to protect by doing so; the only
/// effect would be to hide, from whoever is debugging a routing defect, which
/// branch fired and how far into the query it reached, in exchange for
/// closing a disclosure the paragraphs above show is already narrow. That is
/// not the trade `QueryText` makes, and copying its redaction here anyway
/// would be following its shape past the reason for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MarkerSpan {
    start: usize,
    end: usize,
}

/// Why [`MarkerSpan::new`] refused a `(start, end)` pair — including one
/// arriving off the wire, since `Deserialize` calls the same constructor and
/// turns this into a serde error rather than proceeding with a bad range.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MarkerSpanError {
    /// `start` is after `end`, so the pair does not describe a span at all.
    #[error("{field} start ({start}) comes after its end ({end})")]
    Inverted {
        /// The wire field that broke the rule.
        field: &'static str,
        /// The offending `start` value.
        start: usize,
        /// The offending `end` value.
        end: usize,
    },
    /// `end` reaches past the longest raw query the wire admits. Every
    /// `marker_span` is a range into a query's raw text, and that text is
    /// itself held to [`MAX_QUERY_TEXT`] before it ever reaches routing, so a
    /// span claiming to reach further than that cannot describe a real one.
    #[error("{field} end ({end}) is past the maximum query length of {max} bytes")]
    OutOfBounds {
        /// The wire field that broke the rule.
        field: &'static str,
        /// The offending `end` value.
        end: usize,
        /// The maximum, in bytes — [`MAX_QUERY_TEXT`].
        max: usize,
    },
}

impl MarkerSpan {
    /// The wire field this value travels in, named by every refusal of one.
    pub(crate) const FIELD: &'static str = "DaemonMsg::QueryRouted.marker_span";

    /// Builds a marker span, refusing an inverted range or one reaching past
    /// [`MAX_QUERY_TEXT`]. See this type's own docs for what that does and
    /// does not guarantee about `start` and `end` landing on real character
    /// boundaries of any particular string.
    ///
    /// # Errors
    ///
    /// [`MarkerSpanError::Inverted`] if `start > end`.
    /// [`MarkerSpanError::OutOfBounds`] if `end > MAX_QUERY_TEXT`.
    pub fn new(start: usize, end: usize) -> Result<Self, MarkerSpanError> {
        if start > end {
            return Err(MarkerSpanError::Inverted {
                field: Self::FIELD,
                start,
                end,
            });
        }
        if end > MAX_QUERY_TEXT {
            return Err(MarkerSpanError::OutOfBounds {
                field: Self::FIELD,
                end,
                max: MAX_QUERY_TEXT,
            });
        }
        Ok(Self { start, end })
    }

    /// The byte offset the marker started at.
    pub fn start(&self) -> usize {
        self.start
    }

    /// The byte offset just past the marker's end.
    pub fn end(&self) -> usize {
        self.end
    }

    /// Reads the marker text out of `text` — the query string this span was
    /// computed against — without ever panicking, whatever `text` is and
    /// whatever this span holds. Returns `None` if the range falls outside
    /// `text`'s bounds or does not land on a character boundary at either
    /// end, exactly as [`str::get`] would; this is [`str::get`], not
    /// `&text[..]`, for exactly that reason. See this type's own docs, "What
    /// this range does and does not guarantee", for why this method — and not
    /// a check at parse time — is what actually keeps a bad range from
    /// panicking a client.
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.get(self.start..self.end)
    }
}

impl<'de> Deserialize<'de> for MarkerSpan {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            start: usize,
            end: usize,
        }
        let raw = Raw::deserialize(deserializer)?;
        MarkerSpan::new(raw.start, raw.end).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn accessors_return_what_was_built() {
        let span = MarkerSpan::new(3, 7).unwrap();
        assert_eq!(span.start(), 3);
        assert_eq!(span.end(), 7);
    }

    #[test]
    fn an_inverted_range_is_refused() {
        let err = MarkerSpan::new(7, 3).unwrap_err();
        assert!(matches!(
            err,
            MarkerSpanError::Inverted {
                start: 7,
                end: 3,
                ..
            }
        ));
        assert!(err.to_string().contains(MarkerSpan::FIELD), "got: {err}");
    }

    #[test]
    fn an_equal_start_and_end_is_the_empty_span_and_is_accepted() {
        // Not inverted: `start == end` is a legitimate zero-length span, the
        // shape an empty explicit-prefix term reports (`route("w ")`'s marker
        // is `"w "` in full, but a route whose marker itself is empty is not
        // reachable today; the boundary case is still worth pinning so a
        // later change to `new` cannot silently start refusing it).
        let span = MarkerSpan::new(4, 4).unwrap();
        assert_eq!((span.start(), span.end()), (4, 4));
    }

    #[test]
    fn a_range_past_max_query_text_is_refused() {
        let err = MarkerSpan::new(0, MAX_QUERY_TEXT + 1).unwrap_err();
        assert!(matches!(
            err,
            MarkerSpanError::OutOfBounds {
                end,
                max: MAX_QUERY_TEXT,
                ..
            } if end == MAX_QUERY_TEXT + 1
        ));
        assert!(err.to_string().contains(MarkerSpan::FIELD), "got: {err}");
    }

    #[test]
    fn a_range_at_exactly_max_query_text_is_accepted() {
        assert!(MarkerSpan::new(0, MAX_QUERY_TEXT).is_ok());
    }

    #[test]
    fn round_trips_through_json() {
        let span = MarkerSpan::new(2, 9).unwrap();
        let json = serde_json::to_string(&span).unwrap();
        assert_eq!(json, r#"{"start":2,"end":9}"#);
        assert_eq!(serde_json::from_str::<MarkerSpan>(&json).unwrap(), span);
    }

    #[test]
    fn deserializing_an_inverted_range_off_the_wire_is_refused() {
        let json = r#"{"start":9,"end":2}"#;
        let err = serde_json::from_str::<MarkerSpan>(json).unwrap_err();
        assert!(err.to_string().contains(MarkerSpan::FIELD), "got: {err}");
    }

    #[test]
    fn deserializing_an_out_of_bounds_range_off_the_wire_is_refused() {
        let json = format!(r#"{{"start":0,"end":{}}}"#, MAX_QUERY_TEXT + 1);
        let err = serde_json::from_str::<MarkerSpan>(&json).unwrap_err();
        assert!(err.to_string().contains(MarkerSpan::FIELD), "got: {err}");
    }

    // --- `slice` is what actually keeps a bad range from panicking a client;
    // see the type's own "What this range does and does not guarantee".

    #[test]
    fn slice_reads_the_exact_marker_text() {
        let span = MarkerSpan::new(0, 2).unwrap();
        assert_eq!(span.slice("w firefox"), Some("w "));
    }

    #[test]
    fn slice_of_a_range_that_splits_a_character_is_none_not_a_panic() {
        // "café" is 5 bytes: c-a-f- then é as the two bytes 0xC3 0xA9. Byte
        // offset 4 sits between those two bytes, so it is not a character
        // boundary — this is exactly the shape a hostile or buggy peer could
        // send that passes `MarkerSpan::new`'s checks (0 <= 4 <= 5 <=
        // MAX_QUERY_TEXT) while still being unsafe to index into `"café"`
        // directly.
        let text = "café";
        assert_eq!(text.len(), 5);
        assert!(!text.is_char_boundary(4));
        let span = MarkerSpan::new(3, 4).unwrap();
        assert_eq!(span.slice(text), None);
    }

    #[test]
    fn slice_of_a_range_past_the_given_text_is_none_not_a_panic() {
        // `MarkerSpan::new` only knows about `MAX_QUERY_TEXT`, not about any
        // particular query's actual length, so a span built against a long
        // query and then handed a short one must not panic either.
        let span = MarkerSpan::new(0, 50).unwrap();
        assert_eq!(span.slice("short"), None);
    }

    #[test]
    fn slice_of_the_empty_span_is_the_empty_string() {
        let span = MarkerSpan::new(3, 3).unwrap();
        assert_eq!(span.slice("w firefox"), Some(""));
    }
}
