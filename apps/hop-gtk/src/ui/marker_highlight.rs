//! The consumed-marker highlight inside the query field — issue #184's
//! second signal, distinguishing the byte range `DaemonMsg::QueryRouted`
//! reports as consumed from the rest of what the user typed.
//!
//! # The risk this module exists to close: a stale span against fresh text
//!
//! A [`MarkerSpan`] is computed by the router against the text of **one
//! specific query**. The user keeps typing while a `QueryRouted` frame is in
//! flight, so by the time it arrives the query entry may already hold
//! different text than the one the span was computed against. `gtk::Entry`'s
//! own `set_attributes` interprets a `pango::AttrList`'s byte offsets against
//! *whatever the entry currently displays* — it has no notion of "the text
//! this list was meant for". Applying a structurally valid span computed for
//! query A to query B's now-current text would silently slice the wrong
//! substring: both offsets can land on real character boundaries of the
//! *new* text and still name the wrong bytes, so nothing errors and nothing
//! looks broken — exactly the failure mode the task brief calls out.
//!
//! [`apply`] closes this with a plain string comparison, immediately before
//! the one call that would apply the span: it takes `event_query_text` (the
//! text `ipc::client::run` bound to this exact frame's `query_id` — see that
//! module's `QueryRouted` match arm for how that binding is established and
//! why it is trustworthy) and only proceeds to highlight if it is *still*
//! equal to `entry.text()`, the live value GTK will actually interpret the
//! offsets against. If the user has typed further in the meantime, the two
//! differ, and this clears the highlight instead of risking a wrong one —
//! safe by construction, since the next keystroke's own query sends its own
//! `QueryRouted` frame, carrying its own freshly bound text, which
//! re-establishes a highlight for whatever is on screen *then*.
//!
//! This is a second, independent guard on top of the wire's own
//! superseded-query-id rule (`ipc::client`'s `Some(query_id) == current_id`
//! check, which is what stops a frame for an old `query_id` from reaching
//! this module at all): that check protects the *IPC thread's* bookkeeping,
//! but the entry lives on the GTK thread and can advance to newer text in
//! the gap between a query being sent and its `QueryRouted` frame being
//! delivered back across the channel, even for what was, at send time, the
//! most current query. Trusting `query_id` alone would still leave that gap
//! open; comparing the bound text against the live text right at the point
//! of application closes it regardless of how the race unfolds upstream.
//!
//! # `MarkerSpan::slice`, not raw indexing
//!
//! [`attributes_for`] never reads `event_query_text[span.start()..span.end()]`
//! directly. It calls [`MarkerSpan::slice`] and only trusts the offsets if
//! that call succeeds — the same panic-safety [`MarkerSpan`]'s own docs
//! describe (`text.get(..)`, not `&text[..]`), applied here rather than
//! re-implemented. The substring itself is discarded (`gtk::pango::Attribute`
//! only needs the byte offsets, which are already known), but the *call* is
//! what proves the offsets are valid against `event_query_text` — landing on
//! real character boundaries and within its bounds — before this module
//! trusts them for anything.

use gtk::prelude::*;

use hop_protocol::MarkerSpan;

use crate::tokens;

/// Builds the `pango::AttrList` [`apply`] should hand to the entry: one
/// foreground-colour attribute over `span`'s byte range, in
/// [`tokens::ACCENT_RGB`] — or `None` if there is nothing to highlight,
/// either because `span` is `None` (the route consumed no marker) or because
/// `span` does not describe a real range of `text` (caught via
/// [`MarkerSpan::slice`]; see this module's doc comment).
fn attributes_for(text: &str, span: Option<MarkerSpan>) -> Option<gtk::pango::AttrList> {
    let span = span?;
    // The substring itself is unused below — only its existence matters,
    // as the proof that `span`'s offsets are valid against `text`. See this
    // module's doc comment, "`MarkerSpan::slice`, not raw indexing".
    span.slice(text)?;

    let list = gtk::pango::AttrList::new();
    let (r, g, b) = *tokens::ACCENT_RGB;
    let mut attr: gtk::pango::Attribute = gtk::pango::AttrColor::new_foreground(
        tokens::widen_channel(r),
        tokens::widen_channel(g),
        tokens::widen_channel(b),
    )
    .into();
    attr.set_start_index(u32::try_from(span.start()).unwrap_or(u32::MAX));
    attr.set_end_index(u32::try_from(span.end()).unwrap_or(u32::MAX));
    list.insert(attr);
    Some(list)
}

/// Applies (or clears) the consumed-marker highlight on `entry`.
///
/// `event_query_text` is the text the `QueryRouted` frame's `marker_span` was
/// computed against — bound to the frame by `ipc::client::run`, carried on
/// [`crate::ipc::IpcEvent::Routed`], and passed straight through by
/// `ui::window::HopWindow::apply_event` — never re-derived from `entry`
/// itself. Whether that text still matches what `entry` is showing *right
/// now* is exactly what this function checks before doing anything: see this
/// module's doc comment for why that check, not the wire's own
/// superseded-query-id rule alone, is what actually keeps a stale span from
/// reaching the wrong text.
///
/// `gtk::Entry::set_attributes` takes a bare `&AttrList`, not an `Option` —
/// unlike `gtk::Label`'s equivalent — so "no highlight" is expressed as an
/// empty list rather than `None`, which is exactly what both the "text moved
/// on" and the "nothing to highlight" cases below collapse to.
pub fn apply(entry: &gtk::Entry, event_query_text: &str, span: Option<MarkerSpan>) {
    let list = if entry.text() == event_query_text {
        attributes_for(event_query_text, span)
    } else {
        None
    }
    .unwrap_or_else(gtk::pango::AttrList::new);
    entry.set_attributes(&list);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// No span at all (the `Mode::All` fallback, or a shape-matched inferred
    /// route) — nothing to highlight, and no attribute is ever built to
    /// begin with.
    #[test]
    fn no_span_produces_no_attributes() {
        assert!(attributes_for("w firefox", None).is_none());
    }

    /// A span that actually describes `text` produces exactly one attribute,
    /// over exactly the reported byte range — not the whole string, not a
    /// shifted range.
    #[test]
    fn a_valid_span_produces_one_attribute_over_exactly_that_range() {
        let text = "w firefox";
        let span = MarkerSpan::new(0, 2).unwrap();
        let list = attributes_for(text, Some(span)).expect("a valid span must produce a list");
        // `AttrList` has no direct "give me every attribute" accessor in
        // this pango-rs version — `iterator().attrs()` is its own documented
        // route to the attributes active at a byte position, and position 0
        // is inside the one range this list holds.
        let attrs: Vec<_> = list.iterator().attrs().iter().cloned().collect();
        assert_eq!(attrs.len(), 1, "expected exactly one attribute");
        assert_eq!(attrs[0].start_index(), 0);
        assert_eq!(attrs[0].end_index(), 2);
    }

    /// The stale-text risk this module's doc comment names directly, at the
    /// level `MarkerSpan::slice` alone can catch: a span computed against a
    /// *longer* string reaches past a shorter one it is handed instead —
    /// caught by `slice` returning `None`, not by a panic.
    #[test]
    fn a_span_past_the_end_of_this_text_produces_no_attributes() {
        let span = MarkerSpan::new(0, 9).unwrap(); // valid for "w firefox", not for "w "
        assert!(attributes_for("w ", Some(span)).is_none());
    }

    /// A range that does not land on a character boundary of *this* text —
    /// exactly the shape a span computed against different text (which
    /// happened to be the same length) could produce — is refused the same
    /// way, never sliced.
    #[test]
    fn a_span_off_this_texts_character_boundaries_produces_no_attributes() {
        let text = "café"; // 5 bytes: c-a-f- then a 2-byte é
        assert!(!text.is_char_boundary(4));
        let span = MarkerSpan::new(3, 4).unwrap();
        assert!(attributes_for(text, Some(span)).is_none());
    }

    // [`apply`]'s own guard — that a perfectly valid span is still not
    // applied once the entry's live text has moved past the text it was
    // computed against — needs a real `gtk::Entry`, which needs a real
    // display to construct. That check runs under `ui::window`'s broadway
    // harness instead: see
    // `ui::window::tests::assert_stale_marker_span_is_never_applied_to_newer_text`.
}
