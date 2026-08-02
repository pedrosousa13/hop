//! What a wire value may disclose when something formats it.
//!
//! [`limits`] says how *long* a value may be and [`content`](crate::content)
//! says what it may *contain*. This module asks a third question about a value
//! that is already accepted: what it prints as.
//!
//! The frames in [`wire`](crate::wire) derive `Debug`, and a derived `Debug`
//! prints each field through that field's own `Debug`, so a field held as a
//! `String` prints its characters. For the text of a
//! [`ClientMsg::Query`](crate::wire::ClientMsg::Query) that is a disclosure
//! rather than a diagnostic: the value is whatever was in a launcher overlay
//! when the keystroke landed, which can be a pasted credential or a search
//! nobody would attach to a bug report. A daemon that logged its received
//! frames at debug level would write those keystrokes to the system journal,
//! and a diagnostics bundle would then carry the journal to whoever is
//! helping. No attack is involved: the disclosure is on the default path, not
//! behind one.
//!
//! # The redaction travels with the value
//!
//! [`QueryText`] carries its own `Debug`, which is what makes it hold outside
//! the frame: a consumer that destructures the field and formats it alone gets
//! the same redacted output. A hand-written `Debug` on the frame enum would
//! protect the value only while it is still inside a frame, and it would make
//! the redaction a property of that one impl rather than of the value — so a
//! second field carrying typed text would need somebody to remember it, where
//! this type only has to be the field's type.
//!
//! # This is about formatting, not about transport
//!
//! `Serialize` is untouched and writes the text out in full. It has to: the
//! daemon is the thing that answers the query, and a query it cannot read is
//! not a query. What changes is that reading the text takes an accessor, so the
//! *incidental* paths — a formatted frame in a log line, a panic message, an
//! error built with `{:?}` — carry a marker and a byte count instead.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::limits::{self, BoundError, MAX_QUERY_TEXT, check_len};

/// The text of a [`ClientMsg::Query`](crate::wire::ClientMsg::Query): what a
/// user typed into the launcher overlay.
///
/// The inner string is private and the only way in is [`QueryText::new`], so a
/// `QueryText` that exists is within [`MAX_QUERY_TEXT`] bytes, whether a client
/// built it or it arrived off the socket. That is the arrangement
/// [`ItemId`](crate::item::ItemId) has and it is reached the same way:
/// `Deserialize` runs a pre-filter against that same constant and then hands
/// the string to that same constructor, whose answer decides.
///
/// The newtype does not change the wire form: query text is still a bare JSON
/// string, never an object or a wrapper. Pinned by the test
/// `tests::query_text_travels_as_a_bare_string`.
///
/// # What `Debug` prints
///
/// `QueryText(<redacted, N bytes>)`, where `N` is [`QueryText::len`] — the
/// length of the text in bytes, the unit [`MAX_QUERY_TEXT`] is counted in. The
/// text itself does not appear. `{:#?}` prints the same thing: this `Debug`
/// writes one line and does not vary on the alternate flag, so a frame
/// pretty-printed for a bug report is redacted as well. Pinned by the tests
/// `tests::debug_reports_a_marker_and_a_byte_count_instead_of_the_text` and
/// `tests::the_alternate_debug_flag_does_not_reveal_the_text`, and by
/// `tests::debug_output_turns_on_the_length_and_not_on_the_characters`, which
/// is what a prefix or a first character of the text would fail.
///
/// Length is what survives, and it survives on purpose: "the client sent an
/// empty query" and "the client sent 800 bytes" are answerable from a redacted
/// frame, and neither answer needs the characters. It is still something about
/// the value — a redacted frame says how much was typed — and that is the trade
/// this type makes rather than printing a bare marker.
///
/// # No `Display`
///
/// [`ItemId`](crate::item::ItemId) and [`ActionId`](crate::item::ActionId) both
/// implement `Display`; this type deliberately does not, and neither does it
/// get `ToString` from one. A `Display` writing the text would put it back
/// within reach of `{}`, which is the same disclosure through a second
/// formatting trait — and `{}` is reached for without thinking about `Debug` at
/// all. A `Display` writing the *redacted* form instead would trade the hole
/// for a different problem: `{}` is what code reaches for to show a value to a
/// user, and it would show a marker.
///
/// So the text is reached by name, through [`QueryText::as_str`] or
/// [`QueryText::into_string`], which is a visible act at the call site rather
/// than a formatting default. Pinned by the test
/// `tests::query_text_does_not_implement_display`, which asserts in a `const`
/// block, so adding the impl fails the build rather than silently reopening the
/// path.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct QueryText(String);

impl<'de> Deserialize<'de> for QueryText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(
            deserializer,
            QueryText::FIELD,
            MAX_QUERY_TEXT,
            QueryText::new,
        )
    }
}

impl QueryText {
    /// The wire field this value travels in, and what an over-long value's
    /// refusal names.
    ///
    /// Query text occupies one field of the contract, so naming that field
    /// locates a refusal better than naming this type would, and it makes the
    /// constructor's refusal and the parse's read identically.
    pub(crate) const FIELD: &'static str = "ClientMsg::Query.text";

    /// Builds query text, refusing a value over [`MAX_QUERY_TEXT`] bytes.
    ///
    /// # Errors
    ///
    /// [`BoundError::TooLong`] if the value is over the bound. It is refused
    /// rather than truncated: a shortened query is a different query, and would
    /// be searched for, and learned against, as something the user never typed.
    pub fn new(value: impl Into<String>) -> Result<Self, BoundError> {
        let value = value.into();
        check_len(Self::FIELD, MAX_QUERY_TEXT, value.len())?;
        Ok(Self(value))
    }

    /// The text as a string slice.
    ///
    /// This is the disclosing accessor: what it returns is a plain `&str` whose
    /// own `Debug` and `Display` print the characters, so formatting the result
    /// puts them wherever that formatting goes.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the text, yielding the string inside. Discloses as
    /// [`QueryText::as_str`] does.
    pub fn into_string(self) -> String {
        self.0
    }

    /// The length of the text in bytes, which is what `Debug` reports.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for QueryText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QueryText(<redacted, {} bytes>)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;
    use crate::item::ItemId;
    use crate::wire::ClientMsg;

    /// A value distinctive enough that finding it in formatted output is
    /// finding this value and not a coincidence.
    const TYPED: &str = "correct horse battery staple";

    /// Every test this file's docs name by hand must exist, so that renaming
    /// one fails here instead of leaving a doc pointing at nothing. The same
    /// check `crate::content` runs over its own docs, and for the same reason:
    /// a pointer to a `#[cfg(test)]` item cannot be an intra-doc link, because
    /// rustdoc has no `tests` module to resolve it against.
    ///
    /// A pointer is a backticked `tests::` followed by an identifier.
    #[test]
    fn every_test_this_file_names_in_its_docs_exists() {
        let source = include_str!("redaction.rs");
        let named: Vec<&str> = source
            .lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("///") || line.starts_with("//!"))
            // Odd-indexed pieces are what sat between a pair of backticks.
            .flat_map(|line| line.split('`').skip(1).step_by(2))
            .filter_map(|token| token.strip_prefix("tests::"))
            .filter(|name| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            })
            .collect();

        assert!(
            named.len() >= 5,
            "the docs name at least five tests by hand; finding {} means this \
             scan stopped matching rather than the docs stopping pointing",
            named.len()
        );

        for name in named {
            assert!(
                source.contains(&format!("fn {name}(")),
                "a doc comment names `tests::{name}`, which no test in this file defines"
            );
        }
    }

    // --- What formatting a value discloses ----------------------------------

    #[test]
    fn debug_reports_a_marker_and_a_byte_count_instead_of_the_text() {
        let text = QueryText::new(TYPED).unwrap();
        assert_eq!(
            format!("{text:?}"),
            format!("QueryText(<redacted, {} bytes>)", TYPED.len())
        );
    }

    #[test]
    fn the_alternate_debug_flag_does_not_reveal_the_text() {
        // `{:#?}` is what a pretty-printed frame in a bug report uses, and a
        // `Debug` written with `write!` ignores the flag rather than being
        // asked about it, so the two forms are asserted equal.
        let text = QueryText::new(TYPED).unwrap();
        assert_eq!(format!("{text:#?}"), format!("{text:?}"));
    }

    #[test]
    fn the_reported_byte_count_is_bytes_and_not_characters() {
        // The count is the unit MAX_QUERY_TEXT is denominated in, so a
        // multi-byte query must report more bytes than it has characters.
        let typed = "café ☕";
        let text = QueryText::new(typed).unwrap();
        assert!(typed.len() > typed.chars().count());
        assert_eq!(text.len(), typed.len());
        assert!(format!("{text:?}").contains(&format!("{} bytes", typed.len())));
    }

    #[test]
    fn debug_output_turns_on_the_length_and_not_on_the_characters() {
        // Searching the output for the value would pass while `Debug` printed a
        // prefix or a first character of it, and searching it character by
        // character cannot work — the marker has letters of its own. Two
        // different texts of the same byte length formatting identically is the
        // property that admits neither.
        let typed = QueryText::new(TYPED).unwrap();
        let filler = QueryText::new("x".repeat(TYPED.len())).unwrap();
        assert_ne!(typed, filler);
        assert_eq!(format!("{typed:?}"), format!("{filler:?}"));
    }

    #[test]
    fn debug_of_an_empty_query_is_still_a_redaction() {
        // The marker is unconditional, so an empty query reads as a redaction
        // reporting nothing rather than as a value that was not redacted.
        assert_eq!(
            format!("{:?}", QueryText::new("").unwrap()),
            "QueryText(<redacted, 0 bytes>)"
        );
    }

    /// Answers "does `T` implement [`fmt::Display`]?" as a value, by putting an
    /// inherent associated constant and a blanket trait one on the same probe:
    /// the inherent one exists only for a `T` that implements `Display`, and
    /// where it exists it is what the call site resolves to.
    struct DisplayProbe<T>(std::marker::PhantomData<T>);

    trait MaybeDisplay {
        const IMPLEMENTS_DISPLAY: bool = false;
    }

    impl<T> MaybeDisplay for DisplayProbe<T> {}

    impl<T: fmt::Display> DisplayProbe<T> {
        const IMPLEMENTS_DISPLAY: bool = true;
    }

    #[test]
    fn query_text_does_not_implement_display() {
        // Both are const blocks, so this fails at compile time rather than at
        // run time: adding the impl stops the crate's tests building.
        //
        // `ItemId` is the control: it does implement `Display`, so a probe that
        // answered `false` for everything would fail here rather than let the
        // assertion below pass for the wrong reason.
        const {
            assert!(
                DisplayProbe::<ItemId>::IMPLEMENTS_DISPLAY,
                "the probe reports no Display for a type that has one"
            );
        }
        const {
            assert!(
                !DisplayProbe::<QueryText>::IMPLEMENTS_DISPLAY,
                "QueryText must not implement Display; see its docs for why"
            );
        }
    }

    // --- The bound, and the one gate it is applied at -----------------------

    #[test]
    fn the_constructor_refuses_a_value_over_the_bound() {
        let at_bound = "a".repeat(MAX_QUERY_TEXT);
        assert_eq!(QueryText::new(&at_bound).unwrap().as_str(), at_bound);

        let err = QueryText::new(format!("{at_bound}a")).unwrap_err();
        assert!(matches!(err, BoundError::TooLong { .. }));
        assert!(err.to_string().contains(QueryText::FIELD), "got: {err}");
    }

    #[test]
    fn a_refusal_carries_the_field_and_not_the_value() {
        // The refusal becomes a serde error that a transport reports and logs,
        // so it is the second place the text could escape through formatting.
        let over = format!("{TYPED}{}", "a".repeat(MAX_QUERY_TEXT));
        let err = QueryText::new(over).unwrap_err();
        let message = err.to_string();
        assert!(!message.contains(TYPED), "got: {message}");
        assert!(message.contains(QueryText::FIELD), "got: {message}");

        let frame = json!({ "type": "query", "id": 1, "text": format!("{TYPED}{}", "a".repeat(MAX_QUERY_TEXT)) });
        let parse_error = serde_json::from_str::<ClientMsg>(&frame.to_string())
            .expect_err("a value over the bound must be refused at the parse too");
        assert!(
            !parse_error.to_string().contains(TYPED),
            "got: {parse_error}"
        );
    }

    #[test]
    fn parsing_cannot_produce_a_value_the_constructor_would_refuse() {
        let over = json!({ "type": "query", "id": 1, "text": "a".repeat(MAX_QUERY_TEXT + 1) });
        assert!(serde_json::from_str::<ClientMsg>(&over.to_string()).is_err());
    }

    #[test]
    fn query_text_travels_as_a_bare_string() {
        let msg = ClientMsg::Query {
            id: 7,
            text: QueryText::new(TYPED).unwrap(),
        };
        let encoded = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            encoded,
            format!(r#"{{"type":"query","id":7,"text":"{TYPED}"}}"#)
        );
        assert_eq!(serde_json::from_str::<ClientMsg>(&encoded).unwrap(), msg);
    }

    #[test]
    fn serialization_writes_the_text_out_whole() {
        // The redaction is about formatting, not about transport: the daemon
        // has to receive what was typed, so this is the property that must not
        // be "fixed" into a redaction as well.
        let text = QueryText::new(TYPED).unwrap();
        assert_eq!(
            serde_json::to_string(&text).unwrap(),
            format!("\"{TYPED}\"")
        );
    }

    #[test]
    fn the_accessors_return_what_was_built() {
        let text = QueryText::new(TYPED).unwrap();
        assert_eq!(text.as_str(), TYPED);
        assert_eq!(text.len(), TYPED.len());
        assert!(!text.is_empty());
        assert!(QueryText::new("").unwrap().is_empty());
        assert_eq!(text.into_string(), TYPED);
    }
}
