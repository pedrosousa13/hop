//! The size budget of the wire contract: one maximum per variable-length field.
//!
//! Every variable-length field in this crate — every `String`, every `Vec` —
//! carries a maximum here, and the maximum is enforced where the value is
//! *parsed*, not where it is later read. A peer on either side of the socket is
//! untrusted: the daemon must survive a client that sends a 200 MB query, and a
//! client must survive a daemon that answers with ten million items. Checking
//! after the fact would mean the allocation already happened, and would mean
//! every future caller has to remember to check.
//!
//! # Bounds are counted in bytes
//!
//! Every constant here is a count of **bytes**, never of characters. What is
//! being protected is memory and disk, both of which are byte-denominated, and
//! a byte cap is what a future frame-level cap composes with. A consequence
//! worth stating plainly: a bound of 1 024 admits 1 024 ASCII characters but
//! only 256 four-byte emoji. That is the intended behaviour, not an oversight.
//!
//! # The values are deliberately generous
//!
//! A bound that breaks legitimate use is worse than one set loosely, because
//! the first thing a too-tight bound does is silently drop real items. Each
//! constant's doc says what it protects and why its number is what it is.
//! Changing one is a one-line edit here, and the whole budget is readable in
//! one screen on purpose.
//!
//! # What these bounds do not close
//!
//! [`ClientMsg`](crate::wire::ClientMsg) and [`DaemonMsg`](crate::wire::DaemonMsg)
//! are internally tagged enums (`#[serde(tag = "type")]`). serde cannot dispatch
//! on `type` until it has seen it, so it buffers the *entire* JSON value into an
//! in-memory representation first and only then hands the buffered fields to the
//! field deserializers that check these bounds. The bounds therefore apply
//! **after** buffering: a 200 MB `query` frame is rejected, but not before
//! 200 MB has been held in memory. That is a narrower guarantee than "these
//! bounds prevent the allocation", and it is stated here rather than glossed
//! over. What actually prevents the allocation is a cap on the frame length
//! applied by the transport before a byte reaches serde — issue #21. These
//! bounds complement that cap; they do not replace it.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Error as _, SeqAccess, Visitor};
use thiserror::Error;

/// Maximum bytes of a query's text ([`ClientMsg::Query`](crate::wire::ClientMsg::Query)).
///
/// A launcher query is a few words typed against a keystroke-latency budget.
/// 1 KiB still admits a generous accidental paste while keeping the string that
/// flows into the search path — and, via `hop-core`'s learning store, onto disk
/// as a persisted key — small enough that a hostile client cannot grow either.
pub const MAX_QUERY_TEXT: usize = 1_024;

/// Maximum bytes of an [`ItemId`](crate::item::ItemId).
///
/// A file item's id embeds an absolute path, and Linux caps a path at
/// `PATH_MAX` = 4096 bytes, so anything an honest file provider can produce
/// fits. Ids also travel back in `execute` frames and become learning-store
/// keys, so this is the ceiling on both.
pub const MAX_ITEM_ID: usize = 4_096;

/// Maximum bytes of an [`ActionId`](crate::item::ActionId).
///
/// Action ids are short verbs — `open`, `focus`, `copy`, `move_to_workspace`.
/// 128 bytes leaves room for a provider-namespaced id and nothing resembling
/// free text.
pub const MAX_ACTION_ID: usize = 128;

/// Maximum bytes of an [`Item`](crate::item::Item)'s title.
///
/// Window titles get long — a browser puts the whole page title there. 1 KiB is
/// well past anything a single line of a list view can display, so this bound
/// is a memory guard rather than a display guard.
pub const MAX_TITLE: usize = 1_024;

/// Maximum bytes of an [`Item`](crate::item::Item)'s subtitle.
///
/// Same reasoning as [`MAX_TITLE`]: a subtitle is one display line, and 1 KiB is
/// far past what fits on one.
pub const MAX_SUBTITLE: usize = 1_024;

/// Maximum bytes of an [`Action`](crate::item::Action)'s label.
///
/// A label is button text. 128 bytes is a display-sanity bound as much as a
/// memory one — nothing this long is renderable in a context menu.
pub const MAX_ACTION_LABEL: usize = 128;

/// Maximum bytes of an [`Item`](crate::item::Item)'s provider id.
///
/// A provider id is an identifier that matches a manifest id (`apps`,
/// `windows`, `files`), not free text. 64 bytes is already generous for one.
pub const MAX_PROVIDER_ID: usize = 64;

/// Maximum bytes of an [`IconSpec`](crate::item::IconSpec)'s icon-theme name.
///
/// A name looked up in an icon theme, such as `firefox` or
/// `application-x-executable`. 256 bytes covers the longest names any theme
/// ships.
pub const MAX_ICON_NAME: usize = 256;

/// Maximum bytes of an [`IconSpec`](crate::item::IconSpec)'s icon path.
///
/// An absolute path to an icon file, so `PATH_MAX` = 4096, as for
/// [`MAX_ITEM_ID`].
pub const MAX_ICON_PATH: usize = 4_096;

/// Maximum bytes of copy text — an [`Item`](crate::item::Item)'s `copy_text` and
/// [`ExecOutcome::CopyText`](crate::wire::ExecOutcome::CopyText).
///
/// Copy text can legitimately be a chunk of text rather than a label: a
/// calculator's full answer, a snippet, a URL with a long query string. 64 KiB
/// is the most generous bound in this module for that reason.
pub const MAX_COPY_TEXT: usize = 65_536;

/// Maximum bytes of a URL in [`ExecOutcome::OpenUrl`](crate::wire::ExecOutcome::OpenUrl).
///
/// 8 KiB is past what browsers accept in practice, so a URL this long would
/// fail to open anyway; the bound stops it from being allocated first.
pub const MAX_OPEN_URL: usize = 8_192;

/// Maximum bytes of a [`ProtoError`](crate::wire::ProtoError)'s message.
///
/// An error string headed for a UI. 1 KiB matches [`MAX_TITLE`]: enough for a
/// diagnostic sentence, not enough to be a payload.
pub const MAX_ERROR_MESSAGE: usize = 1_024;

/// Maximum number of actions on a single [`Item`](crate::item::Item).
///
/// An item's actions are a context menu, not a database. 32 is well past the
/// half-dozen any provider produces today.
pub const MAX_ACTIONS_PER_ITEM: usize = 32;

/// Maximum number of items in one [`DaemonMsg::Results`](crate::wire::DaemonMsg::Results) frame.
///
/// The pipeline's own result cap is far smaller than this; this is the ceiling
/// a *hostile* daemon cannot exceed, not the number an honest one sends. It
/// bounds one frame, not a query: a daemon may legitimately stream several
/// partial `results` frames for the same query, so this is not a bound on the
/// total a client accumulates.
pub const MAX_ITEMS_PER_RESULTS_FRAME: usize = 1_000;

/// A value that broke the size budget in [`limits`](self).
///
/// Deserialization turns this into a serde error, so a transport reports an
/// over-long field as a protocol error instead of proceeding with a truncated
/// or oversized value. Nothing in this crate truncates: a value that does not
/// fit is refused, because silently shortening an id would produce a different
/// id and silently dropping items would produce a wrong result set.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BoundError {
    /// A string field longer than its byte maximum.
    #[error("{field} is {actual} bytes, over its maximum of {max} bytes")]
    TooLong {
        /// The field that broke its bound, as `Type::Variant.field`.
        field: &'static str,
        /// The maximum, in bytes.
        max: usize,
        /// The length of the offending value, in bytes.
        actual: usize,
    },
    /// A sequence field holding more elements than its maximum.
    ///
    /// Carries no actual count: the check fires as soon as the maximum is
    /// passed, so the rest of the sequence is never counted or allocated.
    #[error("{field} holds more than its maximum of {max}")]
    TooMany {
        /// The field that broke its bound, as `Type::Variant.field`.
        field: &'static str,
        /// The maximum number of elements.
        max: usize,
    },
}

/// Checks a byte length against a maximum, naming the field in the error.
pub(crate) fn check_len(field: &'static str, max: usize, actual: usize) -> Result<(), BoundError> {
    if actual > max {
        return Err(BoundError::TooLong { field, max, actual });
    }
    Ok(())
}

/// Deserializes a `String`, refusing one over `max` bytes.
///
/// The check runs inside the visitor, before the borrowed input is copied into
/// an owned `String`, so an over-long value costs no allocation of its own —
/// subject to the buffering caveat in this module's docs.
fn string<'de, D>(deserializer: D, field: &'static str, max: usize) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_string(BoundedString { field, max })
}

/// Deserializes an `Option<String>`, refusing a `Some` over `max` bytes.
fn opt_string<'de, D>(
    deserializer: D,
    field: &'static str,
    max: usize,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_option(BoundedOptString { field, max })
}

/// Deserializes a `Vec<T>`, refusing one holding more than `max` elements.
///
/// Fails on the element *past* the maximum rather than after collecting the
/// whole sequence, and never reserves capacity beyond `max` however large the
/// sequence claims to be, so a hostile length cannot be turned into a hostile
/// allocation here.
fn vec<'de, D, T>(deserializer: D, field: &'static str, max: usize) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    deserializer.deserialize_seq(BoundedVec {
        field,
        max,
        marker: std::marker::PhantomData,
    })
}

struct BoundedString {
    field: &'static str,
    max: usize,
}

impl Visitor<'_> for BoundedString {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a string of at most {} bytes", self.max)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        check_len(self.field, self.max, v.len()).map_err(E::custom)?;
        Ok(v.to_owned())
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        check_len(self.field, self.max, v.len()).map_err(E::custom)?;
        Ok(v)
    }
}

struct BoundedOptString {
    field: &'static str,
    max: usize,
}

impl<'de> Visitor<'de> for BoundedOptString {
    type Value = Option<String>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "null or a string of at most {} bytes", self.max)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        string(deserializer, self.field, self.max).map(Some)
    }
}

struct BoundedVec<T> {
    field: &'static str,
    max: usize,
    marker: std::marker::PhantomData<T>,
}

impl<'de, T: Deserialize<'de>> Visitor<'de> for BoundedVec<T> {
    type Value = Vec<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a sequence of at most {} elements", self.max)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        // A self-reported size hint is peer-controlled, so it may only ever
        // shrink the reservation, never grow it past the bound.
        let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(self.max));
        while let Some(element) = seq.next_element()? {
            if out.len() == self.max {
                return Err(A::Error::custom(BoundError::TooMany {
                    field: self.field,
                    max: self.max,
                }));
            }
            out.push(element);
        }
        Ok(out)
    }
}

// One `deserialize_with` target per bounded field. They are spelled out rather
// than generated so that `grep`ping a constant finds every field it governs,
// and so that each error names the field it came from.

pub(crate) fn de_item_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "ItemId", MAX_ITEM_ID)
}

pub(crate) fn de_action_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "ActionId", MAX_ACTION_ID)
}

pub(crate) fn de_query_text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "ClientMsg::Query.text", MAX_QUERY_TEXT)
}

pub(crate) fn de_title<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "Item.title", MAX_TITLE)
}

pub(crate) fn de_subtitle<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    opt_string(d, "Item.subtitle", MAX_SUBTITLE)
}

pub(crate) fn de_provider<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "Item.provider", MAX_PROVIDER_ID)
}

pub(crate) fn de_action_label<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "Action.label", MAX_ACTION_LABEL)
}

pub(crate) fn de_icon_name<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    opt_string(d, "IconSpec.name", MAX_ICON_NAME)
}

pub(crate) fn de_icon_path<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    opt_string(d, "IconSpec.path", MAX_ICON_PATH)
}

pub(crate) fn de_item_copy_text<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    opt_string(d, "Item.copy_text", MAX_COPY_TEXT)
}

pub(crate) fn de_outcome_copy_text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "ExecOutcome::CopyText", MAX_COPY_TEXT)
}

pub(crate) fn de_outcome_open_url<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "ExecOutcome::OpenUrl", MAX_OPEN_URL)
}

pub(crate) fn de_error_message<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    string(d, "ProtoError.message", MAX_ERROR_MESSAGE)
}

pub(crate) fn de_item_actions<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<crate::item::Action>, D::Error> {
    vec(d, "Item.actions", MAX_ACTIONS_PER_ITEM)
}

pub(crate) fn de_results_items<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<crate::item::Item>, D::Error> {
    vec(d, "DaemonMsg::Results.items", MAX_ITEMS_PER_RESULTS_FRAME)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde::Serialize;
    use serde_json::{Value, json};

    use super::*;
    use crate::item::Item;
    use crate::wire::{ClientMsg, DaemonMsg, ExecOutcome, ProtoError};

    /// An otherwise-valid item, as JSON, for a test to overwrite one field of.
    fn item_json() -> Value {
        json!({
            "id": "app:firefox",
            "kind": "app",
            "title": "Firefox",
            "subtitle": null,
            "icon": null,
            "actions": [],
            "default_action": "open",
            "copy_text": null,
            "append_to_end": false,
            "provider": "apps"
        })
    }

    fn action_json() -> Value {
        json!({ "id": "open", "kind": "open", "label": "Open" })
    }

    /// Asserts a bounded string field is tested on **both** sides of its bound:
    /// exactly `max` bytes parses and survives whole, `max + 1` bytes does not
    /// parse at all. An off-by-one that only rejects far-over values fails here.
    ///
    /// `build` places a candidate value inside an otherwise-valid message of
    /// type `T`, so each field is exercised through the real message it travels
    /// in rather than in isolation.
    fn assert_string_boundary<T>(max: usize, build: impl Fn(&str) -> Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + fmt::Debug,
    {
        let at_bound = "a".repeat(max);
        let parsed: T = serde_json::from_str(&build(&at_bound).to_string())
            .unwrap_or_else(|e| panic!("a value of exactly {max} bytes must parse, got: {e}"));
        assert!(
            serde_json::to_string(&parsed).unwrap().contains(&at_bound),
            "the value must survive the parse whole; nothing here truncates to fit"
        );

        let over = "a".repeat(max + 1);
        let err = serde_json::from_str::<T>(&build(&over).to_string())
            .expect_err("a value one byte over the bound must be refused");
        assert!(
            err.to_string().contains("over its maximum of"),
            "the refusal must name the bound it broke, got: {err}"
        );
    }

    /// The sequence counterpart of [`assert_string_boundary`]: `max` elements
    /// parse, `max + 1` do not. `build` returns a message holding `n` elements.
    fn assert_count_boundary<T>(max: usize, build: impl Fn(usize) -> Value)
    where
        T: for<'de> Deserialize<'de> + fmt::Debug,
    {
        serde_json::from_str::<T>(&build(max).to_string())
            .unwrap_or_else(|e| panic!("exactly {max} elements must parse, got: {e}"));

        let err = serde_json::from_str::<T>(&build(max + 1).to_string())
            .expect_err("one element over the bound must be refused");
        assert!(
            err.to_string().contains("holds more than its maximum of"),
            "the refusal must name the bound it broke, got: {err}"
        );
    }

    #[test]
    fn item_id_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_ITEM_ID, |v| {
            let mut item = item_json();
            item["id"] = json!(v);
            item
        });
    }

    #[test]
    fn action_id_bound_holds_on_both_sides() {
        assert_string_boundary::<ClientMsg>(
            MAX_ACTION_ID,
            |v| json!({ "type": "execute", "query_id": 1, "item_id": "app:firefox", "action_id": v }),
        );
    }

    #[test]
    fn query_text_bound_holds_on_both_sides() {
        assert_string_boundary::<ClientMsg>(
            MAX_QUERY_TEXT,
            |v| json!({ "type": "query", "id": 1, "text": v }),
        );
    }

    #[test]
    fn title_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_TITLE, |v| {
            let mut item = item_json();
            item["title"] = json!(v);
            item
        });
    }

    #[test]
    fn subtitle_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_SUBTITLE, |v| {
            let mut item = item_json();
            item["subtitle"] = json!(v);
            item
        });
    }

    #[test]
    fn action_label_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_ACTION_LABEL, |v| {
            let mut item = item_json();
            let mut action = action_json();
            action["label"] = json!(v);
            item["actions"] = json!([action]);
            item
        });
    }

    #[test]
    fn provider_id_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_PROVIDER_ID, |v| {
            let mut item = item_json();
            item["provider"] = json!(v);
            item
        });
    }

    #[test]
    fn icon_name_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_ICON_NAME, |v| {
            let mut item = item_json();
            item["icon"] = json!({ "name": v, "path": null });
            item
        });
    }

    #[test]
    fn icon_path_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_ICON_PATH, |v| {
            let mut item = item_json();
            item["icon"] = json!({ "name": null, "path": v });
            item
        });
    }

    #[test]
    fn item_copy_text_bound_holds_on_both_sides() {
        assert_string_boundary::<Item>(MAX_COPY_TEXT, |v| {
            let mut item = item_json();
            item["copy_text"] = json!(v);
            item
        });
    }

    #[test]
    fn outcome_copy_text_bound_holds_on_both_sides() {
        assert_string_boundary::<ExecOutcome>(MAX_COPY_TEXT, |v| json!({ "copy_text": v }));
    }

    #[test]
    fn outcome_open_url_bound_holds_on_both_sides() {
        assert_string_boundary::<ExecOutcome>(MAX_OPEN_URL, |v| json!({ "open_url": v }));
    }

    #[test]
    fn error_message_bound_holds_on_both_sides() {
        assert_string_boundary::<ProtoError>(
            MAX_ERROR_MESSAGE,
            |v| json!({ "code": "internal", "message": v }),
        );
    }

    #[test]
    fn actions_per_item_bound_holds_on_both_sides() {
        assert_count_boundary::<Item>(MAX_ACTIONS_PER_ITEM, |n| {
            let mut item = item_json();
            item["actions"] = json!(vec![action_json(); n]);
            item
        });
    }

    #[test]
    fn items_per_results_frame_bound_holds_on_both_sides() {
        assert_count_boundary::<DaemonMsg>(MAX_ITEMS_PER_RESULTS_FRAME, |n| {
            json!({
                "type": "results",
                "query_id": 1,
                "partial": false,
                "items": vec![item_json(); n],
            })
        });
    }

    #[test]
    fn results_frame_fails_on_one_item_with_an_over_long_string() {
        // The companion of the count bound: a frame of *one* item still fails if
        // that item breaks a string bound, so a hostile daemon cannot get bytes
        // through by sending few items instead of many.
        let mut item = item_json();
        item["title"] = json!("a".repeat(MAX_TITLE + 1));
        let frame = json!({
            "type": "results",
            "query_id": 1,
            "partial": false,
            "items": [item],
        });
        let err = serde_json::from_str::<DaemonMsg>(&frame.to_string())
            .expect_err("an over-long title must sink the whole frame");
        assert!(err.to_string().contains("Item.title"), "got: {err}");
    }

    #[test]
    fn refusal_names_the_field_that_broke_its_bound() {
        // The error is what a transport reports as a protocol error, so it has to
        // say which field failed rather than just that something did.
        let msg = json!({ "type": "query", "id": 1, "text": "a".repeat(MAX_QUERY_TEXT + 1) });
        let err = serde_json::from_str::<ClientMsg>(&msg.to_string()).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("ClientMsg::Query.text"), "got: {text}");
        assert!(text.contains(&MAX_QUERY_TEXT.to_string()), "got: {text}");
    }

    #[test]
    fn a_hostile_sequence_length_cannot_be_turned_into_a_hostile_allocation() {
        // serde_json reports no size hint, but a self-describing format could.
        // The bound caps the reservation either way, so the guard is that the
        // parse refuses rather than reserving first and refusing after.
        let frame = json!({
            "type": "results",
            "query_id": 1,
            "partial": false,
            "items": vec![item_json(); MAX_ITEMS_PER_RESULTS_FRAME + 1],
        });
        assert!(serde_json::from_str::<DaemonMsg>(&frame.to_string()).is_err());
    }
}
