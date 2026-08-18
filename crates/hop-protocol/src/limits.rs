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
//! over. What actually prevents the allocation is [`MAX_FRAME_BYTES`], the cap
//! on frame length that [`framing::payload_len`](crate::framing::payload_len)
//! applies before a byte reaches serde — issue #21. This crate's framing layer
//! is IO-free and offers that check; a transport still has to call it before
//! it allocates, which is a property of the daemon and CLI built on top of
//! this crate, not of this module. These bounds complement that cap; they do
//! not replace it.
//!
//! # What the bounds compose to
//!
//! Each constant below is justified on its own, which makes it easy to read the
//! module and conclude that the set of them puts a usable ceiling on a frame. It
//! does not. One item sitting on every one of its bounds is:
//!
//! ```text
//!   id                4 096   (MAX_ITEM_ID)
//!   title             1 024   (MAX_TITLE)
//!   subtitle          1 024   (MAX_SUBTITLE)
//!   icon              4 096   (the larger of MAX_ICON_NAME and MAX_ICON_PATH:
//!                              an icon is a name or a path, never both)
//!   copy_text        65 536   (MAX_COPY_TEXT)
//!   provider             64   (MAX_PROVIDER_ID)
//!   default_action      128   (MAX_ACTION_ID)
//!   32 actions        8 192   (MAX_ACTIONS_PER_ITEM × (MAX_ACTION_ID + MAX_ACTION_LABEL))
//!                   -------
//!                    84 160 bytes
//! ```
//!
//! At [`MAX_ITEMS_PER_RESULTS_FRAME`] that is roughly **84 MB in a single
//! `results` frame, entirely within every bound in this module** — before JSON
//! syntax, before escaping, and before counting the several partial frames a
//! daemon may send for one query. Read together with the buffering caveat
//! above: a frame like that is accepted, and a frame of the same size that
//! breaks one field bound is still buffered whole before it is refused. The
//! frame-level cap, [`MAX_FRAME_BYTES`], is therefore load-bearing, not
//! belt-and-braces, and this arithmetic is the number it is set against.
//!
//! Both totals are recomputed from the constants themselves by the test
//! `the_documented_worst_case_is_what_the_constants_compose_to`, so retuning any
//! bound above fails a test rather than quietly rotting this table.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Error as _, SeqAccess, Visitor};
use thiserror::Error;

/// Maximum bytes of a query's text ([`ClientMsg::Query`](crate::wire::ClientMsg::Query)).
///
/// A launcher query is a few words typed against a keystroke-latency budget.
/// 1 KiB still admits a generous accidental paste while keeping the string that
/// flows into the query path — and that `hop-core`'s learning store then holds
/// resident as an in-memory key — small enough that a hostile client cannot
/// grow either.
///
/// This is a query-path and resident-memory bound, not a disk one. Query text
/// never reaches disk: `hop-core`'s learning store persists only its global
/// launch-frequency table, which is keyed by item id (see [`MAX_ITEM_ID`]), and
/// deliberately leaves the query-keyed half of its state unserialized.
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

/// Maximum bytes of an [`IconName`](crate::content::IconName), the name arm of
/// an [`IconSpec`](crate::item::IconSpec).
///
/// A name looked up in an icon theme, such as `firefox` or
/// `application-x-executable`. 256 bytes covers the longest names any theme
/// ships.
///
/// This is the bound; what a name may *contain* is a content rule, and it lives
/// on the type in [`content`](crate::content) along with the bound's application.
pub const MAX_ICON_NAME: usize = 256;

/// Maximum bytes of an [`IconPath`](crate::content::IconPath), the path arm of
/// an [`IconSpec`](crate::item::IconSpec).
///
/// An absolute path to an icon file, so `PATH_MAX` = 4096, as for
/// [`MAX_ITEM_ID`].
///
/// The two bounds never both apply to one item: an icon is a name or a path, so
/// this module's budget table counts the larger of them once rather than the sum.
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
/// 8 KiB is past what browsers accept in practice, so a URL longer than this
/// would fail to open anyway; the bound refuses it at the parse instead of
/// carrying it as far as the launch.
pub const MAX_OPEN_URL: usize = 8_192;

/// Maximum bytes of a [`ProtoError`](crate::wire::ProtoError)'s message.
///
/// An error string headed for a UI. 1 KiB matches [`MAX_TITLE`]: enough for a
/// diagnostic sentence, not enough to be a payload.
///
/// This bound is enforced at exactly one place: [`de_error_message`], the
/// receiving peer's parse. [`ProtoError::new`](crate::wire::ProtoError::new)
/// applies **no** length check of its own — it is not a gate, it is a
/// deterministic render over an [`ErrorDetail`](crate::wire::ErrorDetail).
/// The sending side stays under this bound only because every
/// `ErrorDetail` variant interpolates a value that is *itself* bounded well
/// under it — [`MAX_ACTION_ID`], [`MAX_PROVIDER_ID`], a fixed-width integer,
/// or a `&'static str` chosen at a call site — with one documented
/// exception: `ErrorDetail::Item`'s bound is [`MAX_ITEM_ID`], which is
/// nearly 4× this constant, so a legitimate, in-bound `ItemId` can make
/// `ProtoError::new` build a message a receiving peer's own
/// [`de_error_message`] would refuse. See `ProtoError`'s "A gap this
/// decision does not close" for that case, named rather than fixed here, and
/// `wire::tests::unknown_item_message_can_exceed_max_error_message_at_max_item_id`,
/// which pins the current, overflowing behavior directly. What `message`
/// may *contain* — as opposed to how long it is — is
/// [`ErrorDetail`](crate::wire::ErrorDetail)'s decision (#84).
pub const MAX_ERROR_MESSAGE: usize = 1_024;

/// Maximum number of actions on a single [`Item`](crate::item::Item).
///
/// An item's actions are a context menu, not a database. 32 is well past the
/// half-dozen any provider produces today.
pub const MAX_ACTIONS_PER_ITEM: usize = 32;

/// Maximum number of items in one [`DaemonMsg::Results`](crate::wire::DaemonMsg::Results) frame.
///
/// This is the ceiling a *hostile* daemon cannot exceed, not the number an
/// honest one sends: `hop-core`'s pipeline truncates to a `max_results` its
/// caller passes in, and every caller today passes something far smaller — but
/// that is a caller's choice, not a constant, and nothing in this crate can
/// enforce it, so this bound stands on its own rather than on that habit.
///
/// It bounds one frame — but under the replace rule
/// ([`DaemonMsg::Results`](crate::wire::DaemonMsg::Results)'s docs), a client
/// holds only the most recently received frame's list for a query, never a
/// sum of frames, so this is also the effective bound on what a client holds
/// for one query at any moment. It is still not a bound on how many `results`
/// frames a daemon may send for one query, nor on what the daemon accumulates
/// from providers to build each one — that growth is bounded separately, by
/// [`MAX_ITEMS_PER_QUERY`], applied where the daemon does that accumulating.
/// See also this module's docs for what this constant multiplies out to
/// against the per-item bounds.
///
/// Under replacement it is also what bounds the daemon's own retained state —
/// not [`MAX_ITEMS_PER_QUERY`], which no longer touches it. `hopd`'s
/// `connection.rs` keeps `Exchange::delivered`, the last list sent, truncated
/// to this constant before being retained; `delivered` is the retained set an
/// `execute` frame resolves against (issue #59), and the threat model's
/// Decision 1 — settling issue #25 — depends on that retained state staying
/// bounded for its "rides on state the daemon must keep anyway" argument to
/// hold. This constant is now what keeps that true.
pub const MAX_ITEMS_PER_RESULTS_FRAME: usize = 1_000;

/// Maximum items the daemon may accumulate from providers for one query id,
/// across every provider arrival of the exchange.
///
/// Under the replace rule
/// ([`DaemonMsg::Results`](crate::wire::DaemonMsg::Results)'s docs), a
/// `results` frame carries the complete current list rather than an
/// increment, so nothing about what a client holds sums across frames — a
/// client replaces, never accumulates, and its only guard against an
/// oversized frame is [`MAX_ITEMS_PER_RESULTS_FRAME`], applied at the parse
/// by [`de_results_items`]. This constant bounds something else, entirely on
/// the daemon's side: the checked items a query has received from providers
/// so far, which the daemon re-assembles into a fresh list on every arrival
/// and which therefore keeps growing for as long as the query stays open.
/// [`MAX_ITEMS_PER_RESULTS_FRAME`] does not bound that growth — it bounds one
/// assembled list, and an honest assembly is far smaller besides — so
/// without a separate cap a query reaching enough providers would grow the
/// daemon's per-query accumulator without limit even while what reaches the
/// client each time stays small. This is that cap, and it is enforced where
/// the accumulating happens: `hopd`'s `source.rs`, inside the accumulator
/// `HostSource::start` spawns for the query.
///
/// Unlike its neighbours this bound is not enforced at the parse — no single
/// `results` frame can break it, since what it bounds is built before any
/// frame is assembled. When a provider's arrival would push the accumulator
/// past this cap, the accumulator truncates that arrival to what fits before
/// absorbing it, sends the frame assembled from the now-full accumulator, and
/// stops — closing its channel, which is what causes the connection to answer
/// with the exchange's terminal frame. **Truncate-and-terminate**: what was
/// already assembled and delivered stays exactly that, and the remainder that
/// did not fit is dropped with nothing on the wire naming it, the rule this
/// daemon applies at every other bound of this shape.
///
/// 5 000 is five times [`MAX_ITEMS_PER_RESULTS_FRAME`] — five single frames'
/// worth of accumulated provider input. Honest traffic is two orders of
/// magnitude smaller — a launcher renders tens of items, and what one
/// arrival actually assembles is bounded far below even one frame's maximum
/// — so this is a memory guard on the accumulator, not a display guard: at
/// the composed per-item worst case (84 160 bytes, see the module docs) what
/// one query's accumulator holds while it is live stays under ~421 MB
/// hostile-shaped, ~1 MB honest-shaped — assuming the items being counted
/// respect the per-item bounds, which is a narrower assumption than it
/// sounds.
///
/// # Why those byte figures are conditional
///
/// This constant bounds a **count**. The byte figures above are that count
/// multiplied by bounds this module applies *at the parse*
/// (`#[serde(deserialize_with = …)]`), so they hold for every item that
/// arrived over a socket and for no item that did not. [`Item`](crate::item::Item)'s
/// `title`, `subtitle`, `provider` and `actions` fields are plain `String`s
/// and `Vec`s with no bound outside the parse — `id` and `default_action`
/// are validated newtypes (`ItemId`/`ActionId`) bounded at construction
/// regardless of origin, and `copy_text` joins them as of issue #78 (see
/// below), but that leaves every other variable-length field uncovered. An
/// item a daemon builds in-process — or takes from a result source
/// in-process — has passed no *length* check on those: 5 000 items with a
/// 100 MB title each are 5 000 items, and this cap admits them. The only
/// backstop below that is [`MAX_FRAME_BYTES`]
/// at encode time, which refuses the frame as an error rather than reporting
/// an over-sized item.
///
/// The obligation is therefore on whatever produces items in-process, and it
/// is documented where such a thing is written — `hopd`'s `ResultSource`
/// seam, and `hop-core`'s provider host (issue #56), the first code that
/// accepts items from a provider without parsing them. Landing the host
/// closed the scheduling gap this comment used to describe as future work,
/// but at the time did not add this enforcement: what the host checked an
/// item against was its producer's declared `kind` and `provider` string,
/// never the length of a field.
///
/// Issue #61 closed the field-length half of that gap, at the one seam every
/// provider's answer must cross: `hop-core`'s
/// `pipeline::CheckedItems::check`, called once per provider by
/// `ProviderHost::run_one` before an answer reaches assembly. It now rejects
/// an item whose `title`, `subtitle`, an action's `label`, or action count is
/// over the same bound this module already applies to that same field on the
/// wire (see `pipeline::FailedCheck::FieldTooLong`) — so the specific claim
/// above, "documented, not enforced... wherever an item is built in-process,"
/// is no longer true of a provider's answer, which is where the overwhelming
/// majority of in-process items originate.
///
/// `copy_text` used to be on that list and no longer is, not because its gap
/// reopened but because issue #78 closed it a different way: `Item.copy_text`
/// is now `Option<content::CopyText>`, and `CopyText`'s own constructor
/// enforces `MAX_COPY_TEXT` — and its content rules — on every value that
/// exists, in-process or off the wire. There is no longer a state
/// `CheckedItems::check` could catch that construction had not already
/// refused, so checking it there again would be the second gate this crate's
/// docs on [`validated`] argue against.
///
/// It narrows, though — it does not disappear. `CheckedItems::check` is a
/// choke point only for callers that go through it: `hop-core`'s
/// `Ranker::rank` is a public function taking a bare `Vec<Item>`, and a
/// caller that hand-builds items and calls it directly (or otherwise
/// constructs and consumes items without ever reaching
/// `CheckedItems::check`) still gets no field-length enforcement at all,
/// same as before #61. The obligation this paragraph describes is enforced
/// at the seam a provider's answer is required to cross to reach assembly;
/// it is still exactly "documented, not enforced" for an item built and
/// consumed entirely outside that seam.
pub const MAX_ITEMS_PER_QUERY: usize = 5_000;

/// Maximum bytes of a client-to-daemon frame payload that `hopd` admits before
/// allocating its inbound buffer.
///
/// This is deliberately narrower than [`MAX_FRAME_BYTES`], which remains the
/// shared frame ceiling and the daemon-to-client results ceiling. The daemon
/// checks this value after [`framing::payload_len`](crate::framing::payload_len)
/// decodes the shared 4-byte prefix and before it allocates the payload, so a
/// buggy or runaway same-uid local client can hold at most one 64 KiB inbound
/// payload buffer per admitted connection. It is robustness against that
/// local-client failure mode, not a security boundary against a hostile peer.
/// With 64 admitted connections, this composes with each connection's
/// retained set of at most [`MAX_ITEMS_PER_RESULTS_FRAME`] items to at most
/// 4 MiB of inbound payload buffers plus 64,000 retained bounded items.
pub const MAX_INBOUND_FRAME_BYTES: usize = 65_536;

/// Maximum bytes of one shared frame's JSON payload, exclusive of the 4-byte
/// length prefix [`framing`](crate::framing) puts in front of it.
///
/// This is not one more field bound alongside [`MAX_TITLE`] and its
/// neighbours above: it is the shared frame-level cap that "What these bounds do not
/// close" promises, enforced by
/// [`framing::payload_len`](crate::framing::payload_len) **before** a payload
/// is allocated, closing issue #21 once a transport calls it — see the
/// buffering caveat on [`ClientMsg`](crate::wire::ClientMsg), which this
/// composes with rather than replaces.
///
/// # Why 268 435 456 (256 MiB)
///
/// This module's "What the bounds compose to" table prices the worst-case
/// in-bounds `results` frame — one item sitting on every bound, repeated
/// [`MAX_ITEMS_PER_RESULTS_FRAME`] times — at 84 160 000 bytes of field
/// content, before JSON syntax and escaping. 256 MiB admits that frame with
/// roughly 3× headroom for syntax and realistic escaping;
/// `the_documented_worst_case_is_what_the_constants_compose_to` asserts
/// `MAX_FRAME_BYTES >= 3 * per_frame` so that retuning an item bound above
/// cannot silently outgrow this cap without that test noticing.
///
/// What 256 MiB deliberately does *not* admit is the pathological version of
/// that same frame: every field escaped to its most expensive form —
/// `\uXXXX`, six JSON bytes for one byte of content — which prices out to
/// roughly 505 MB. A frame only reachable by encoding every field that way is
/// exactly what this cap exists to refuse: it bounds what a peer can make the
/// process allocate, not what an honest daemon would ever send. `hopd` layers
/// [`MAX_INBOUND_FRAME_BYTES`] on client payloads only; it does not narrow this
/// shared/outbound ceiling.
pub const MAX_FRAME_BYTES: usize = 268_435_456;

/// A value that broke the size budget in [`limits`](self).
///
/// Deserialization turns this into a serde error, so a transport reports an
/// over-long field as a protocol error instead of proceeding with a truncated
/// or oversized value. Nothing in this crate truncates: a value that does not
/// fit is refused, because silently shortening an id would produce a different
/// id and silently dropping items would produce a wrong item list.
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
///
/// Public, unlike the rest of this module's parse-time machinery
/// (`validated`, `BoundedString`, and friends, all `pub(crate)`), because
/// bound-checking a value is a real need even for a caller that never
/// deserializes a wire type at all. `hop-core`'s alias loader is the case
/// that forced the question: a `window` alias's `app_id` and
/// `title_contains` are bounded against [`MAX_ITEM_ID`] and [`MAX_TITLE`] at
/// load time, long before either string becomes an
/// [`ItemId`](crate::item::ItemId) or reaches the wire, if it ever does. That
/// caller could have reimplemented this function's three-line body instead —
/// nothing stopped it, since [`BoundError::TooLong`]'s fields are as public
/// as the enum they're on — but a hand-copied bound check is a second
/// definition of "what counts as exceeding a bound, and how that is
/// reported," one a future change to either copy would not reach. Exporting
/// this function keeps that definition singular; `validated` and
/// `BoundedString` stay `pub(crate)` because nothing outside this crate's
/// deserialization path has ever needed them.
pub fn check_len(field: &'static str, max: usize, actual: usize) -> Result<(), BoundError> {
    if actual > max {
        return Err(BoundError::TooLong { field, max, actual });
    }
    Ok(())
}

/// Deserializes a `String`, refusing one over `max` bytes.
///
/// The check runs inside the visitor rather than after it, so that where the
/// value is still borrowed it is refused before being copied. Which of
/// [`BoundedString`]'s two arms a given parse reaches is not obvious, and a
/// wrong guess in either direction is dangerous, so it is written out rather
/// than reasoned about. Measured with a probe visitor over both message shapes
/// this crate has:
///
/// ```text
///   plain struct field, from_str,    no escape  ->  visit_str
///   plain struct field, from_str,    escaped    ->  visit_str
///   plain struct field, from_reader, either     ->  visit_str
///   internally tagged,  from_str,    no escape  ->  visit_str
///   internally tagged,  from_str,    escaped    ->  visit_string
///   internally tagged,  from_reader, either     ->  visit_string
/// ```
///
/// So there is exactly one way into [`BoundedString::visit_string`]: the
/// internally-tagged `Content` buffer, holding an owned `Content::String`.
/// `ContentVisitor` produces that whenever it cannot borrow from the input —
/// any string carrying an escape, and every string read through `from_reader` —
/// and `ContentDeserializer::deserialize_string` hands it straight to
/// `visit_string`. [`ClientMsg`](crate::wire::ClientMsg) and
/// [`DaemonMsg`](crate::wire::DaemonMsg) are both `#[serde(tag = "type")]`, so
/// **every escaped string in every real frame takes that path**. Its
/// `check_len` is load-bearing, not a defensive duplicate of the other arm: a
/// 5 KiB window title containing a single `\n` or `\"` reaches it and nothing
/// else, and dropping the check there would let that title past [`MAX_TITLE`]
/// untested. [`tests::an_over_long_escaped_title_is_refused_inside_a_tagged_frame`]
/// pins it.
///
/// The rows that land in [`BoundedString::visit_str`] do so for two different
/// reasons, and neither is "the value is always borrowed". A `from_reader`
/// parse cannot borrow at all; serde_json hands the visitor a slice of its own
/// scratch buffer. And the buffered row that *does* borrow arrives as
/// `visit_borrowed_str`, reaching `visit_str` only because this visitor does
/// not override that method and serde's default forwards it there.
///
/// What the placement buys is therefore narrow: one copy avoided, on the rows
/// above that reach `visit_str` with a genuinely borrowed slice. It buys
/// nothing on `visit_string`, where the `String` is already allocated, and the
/// buffering caveat in this module's docs sits above every row regardless. The
/// check is placed here because the parse is the right *place* to refuse, not
/// because it makes the refusal free.
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

/// Deserializes a validating newtype by handing the parsed value to `build` —
/// the type's own constructor.
///
/// The point is that there is **one** gate, not two that happen to agree: a
/// rule added to the constructor later (rejecting the empty string, say, or
/// normalising Unicode so learning-store keys cannot split on encoding form)
/// applies to values off the socket without anybody remembering to add it here
/// too. The `max` passed in is only a pre-filter; it uses the same constant the
/// constructor does, so it can only ever reject what the constructor would also
/// reject. The constructor's answer is what counts. Its error type is only
/// required to be `Display`, so a newtype whose rules go beyond length — see
/// [`content`](crate::content) — reports them through the same path.
///
/// What the pre-filter buys is as narrow as it is for [`BoundedString`], and
/// for the same reason. On [`Validated::visit_str`] it refuses an over-long
/// value before `to_owned` copies it into an owned `String`. On
/// [`Validated::visit_string`] it buys nothing at all: the `String` is already
/// allocated before the visitor is entered. The routing table on [`string`]
/// says which parses reach which arm — for a value inside a tagged frame, an
/// escape is enough to make it the allocating one. The check sits at the parse
/// because that is the right *place* to refuse, not because it makes the
/// refusal free.
pub(crate) fn validated<'de, D, T, B, F>(
    deserializer: D,
    field: &'static str,
    max: usize,
    build: F,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    B: fmt::Display,
    F: FnOnce(String) -> Result<T, B>,
{
    deserializer.deserialize_string(Validated {
        field,
        max,
        build,
        marker: std::marker::PhantomData,
    })
}

/// The `Option` counterpart of [`validated`]: deserializes `Option<T>` for a
/// validating newtype, refusing a `Some` that breaks `build`'s rules exactly
/// as [`validated`] would, and mapping absence and explicit `null` to `None`
/// exactly as [`opt_string`] does for a plain bounded string.
pub(crate) fn validated_opt<'de, D, T, B, F>(
    deserializer: D,
    field: &'static str,
    max: usize,
    build: F,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    B: fmt::Display,
    F: FnOnce(String) -> Result<T, B>,
{
    deserializer.deserialize_option(ValidatedOpt {
        field,
        max,
        build,
        marker: std::marker::PhantomData,
    })
}

/// The visitor `validated_opt` drives to deserialize an `Option<T>` for a
/// validating newtype.
///
/// # `expecting`'s field name is currently unreachable
///
/// `expecting` below writes `self.field`, matching every other visitor in
/// this module, but no parse this crate exercises today can actually reach
/// it, for the same reason [`BoundedOptString`]'s cannot: `deserialize_option`
/// only ever calls one of this visitor's other three methods, never falls
/// through to the default `invalid_type` that would format a message from
/// `expecting` at all. `visit_none` and `visit_unit` answer `null` and
/// absence, and `visit_some` hands anything else straight to [`validated`],
/// which drives [`Validated`] over the same `field` instead — so a present,
/// wrong-typed value is judged (and named) there, not here. Both
/// deserializers this crate drives an `Option<T>` field through — serde_json's
/// own, and the internally-tagged `ContentDeserializer` that
/// [`ClientMsg`](crate::wire::ClientMsg) and
/// [`DaemonMsg`](crate::wire::DaemonMsg) buffer into — agree on that
/// null-or-`visit_some` split, so there is no parse in this codebase today
/// that would make `deserialize_option` reach for a fourth arm and fall back
/// to `expecting`.
///
/// Leaving `expecting` fieldless anyway was considered, on the strength of
/// that unreachability, and rejected, for the same reason it was rejected on
/// [`BoundedOptString`]: `field` is already in scope on this struct, matching
/// it costs nothing here, and a future `Deserializer` — or a future serde
/// version — is free to route `deserialize_option` differently for a type it
/// cannot special-case. An `expecting` that stayed fieldless would then
/// silently reopen the exact gap issue #82 closed elsewhere, discovered only
/// if somebody thought to check this one arm again. Naming the field costs
/// one comparison against a constant; leaving it unnamed bets against every
/// future deserializer keeping today's shape.
struct ValidatedOpt<T, F> {
    field: &'static str,
    max: usize,
    build: F,
    marker: std::marker::PhantomData<T>,
}

impl<'de, T, B, F> Visitor<'de> for ValidatedOpt<T, F>
where
    B: fmt::Display,
    F: FnOnce(String) -> Result<T, B>,
{
    type Value = Option<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} to be null or a string of at most {} bytes that its type accepts",
            self.field, self.max
        )
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        validated(deserializer, self.field, self.max, self.build).map(Some)
    }
}

struct Validated<T, F> {
    field: &'static str,
    max: usize,
    build: F,
    marker: std::marker::PhantomData<T>,
}

impl<T, B, F> Visitor<'_> for Validated<T, F>
where
    B: fmt::Display,
    F: FnOnce(String) -> Result<T, B>,
{
    type Value = T;

    /// Leads with the field, as [`BoundError::TooLong`] does, so an operator
    /// scanning a refusal finds the name first rather than reading to the end
    /// of a byte count to learn what broke. That is also why this writes a
    /// sentence (`{field} to be …`) rather than an appositive (`{field}, …`):
    /// serde's own `invalid_type` wraps whatever `expecting` returns in
    /// `expected {…}`, and `expected Foo, a string of…` reads as two things
    /// being expected — a literal `Foo` and a string — rather than one thing
    /// said about `Foo`. `to be` is the connector that resolves correctly
    /// under that prefix; the exact word matters less than the sentence
    /// shape it produces.
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} to be a string of at most {} bytes that its type accepts",
            self.field, self.max
        )
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        check_len(self.field, self.max, v.len()).map_err(E::custom)?;
        (self.build)(v.to_owned()).map_err(E::custom)
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        check_len(self.field, self.max, v.len()).map_err(E::custom)?;
        (self.build)(v).map_err(E::custom)
    }
}

struct BoundedString {
    field: &'static str,
    max: usize,
}

impl Visitor<'_> for BoundedString {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} to be a string of at most {} bytes",
            self.field, self.max
        )
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

/// The visitor `opt_string` drives to deserialize an `Option<String>`.
///
/// # `expecting`'s field name is currently unreachable
///
/// `expecting` below writes `self.field`, matching every other visitor in
/// this module, but no parse this crate exercises today can actually reach
/// it: `deserialize_option` only ever calls one of this visitor's other three
/// methods, never falls through to the default `invalid_type` that would
/// format a message from `expecting` at all. `visit_none` and `visit_unit`
/// answer `null` and absence, and `visit_some` hands anything else straight
/// to [`string`], which drives `BoundedString` over the same `field` instead
/// — so a present, wrong-typed value is judged (and named) there, not here.
/// Both deserializers this crate drives an `Option<String>` field through —
/// serde_json's own, and the internally-tagged `ContentDeserializer` that
/// [`ClientMsg`](crate::wire::ClientMsg) and
/// [`DaemonMsg`](crate::wire::DaemonMsg) buffer into — agree on that
/// null-or-`visit_some` split, so there is no parse in this codebase today
/// that would make `deserialize_option` reach for a fourth arm and fall back
/// to `expecting`.
///
/// Leaving `expecting` fieldless anyway was considered, on the strength of
/// that unreachability, and rejected: `field` is already in scope on this
/// struct, matching it costs nothing here, and a future `Deserializer` — or a
/// future serde version — is free to route `deserialize_option` differently
/// for a type it cannot special-case. An `expecting` that stayed fieldless
/// would then silently reopen the exact gap issue #82 closed elsewhere,
/// discovered only if somebody thought to check this one arm again. Naming
/// the field costs one comparison against a constant; leaving it unnamed bets
/// against every future deserializer keeping today's shape.
struct BoundedOptString {
    field: &'static str,
    max: usize,
}

impl<'de> Visitor<'de> for BoundedOptString {
    type Value = Option<String>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} to be null or a string of at most {} bytes",
            self.field, self.max
        )
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
        write!(
            f,
            "{} to be a sequence of at most {} elements",
            self.field, self.max
        )
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
//
// Not every bounded field is here. A field whose type is a validating newtype
// carries its bound in that type's own `Deserialize`, through `validated`
// above, so that the bound and whatever else the type promises are one gate
// rather than two: `MAX_ITEM_ID` and `MAX_ACTION_ID` are applied by
// `crate::item`, `MAX_OPEN_URL`, `MAX_ICON_NAME`, `MAX_ICON_PATH` and the
// outcome half of `MAX_COPY_TEXT` by `crate::content`, and `MAX_QUERY_TEXT` by
// `crate::redaction`. Grepping a constant still finds every field it governs; it
// just finds some of them in the module that owns the type.
//
// `Item.copy_text` is the item half of `MAX_COPY_TEXT`, and it is a
// validating newtype too — `Option<content::CopyText>` — so by that same rule
// its bound is not here either. What is different about it is *where* its
// `deserialize_with` lives: not on `CopyText`'s own `Deserialize`, which
// `crate::content` owns and which names every refusal `CopyText::FIELD`, but
// as `crate::item::de_item_copy_text`, which calls `validated_opt` above with
// `content::CopyText::new_named` so the refusal names `Item.copy_text`
// instead. See `crate::content`'s module docs for why that field name has to
// differ from `CopyText::FIELD`'s.

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
    use crate::content::{ALLOWED_URL_SCHEMES, CopyText, OpenUrl};
    use crate::item::Item;
    use crate::redaction::QueryText;
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

    /// An opening that carries a candidate URL past
    /// [`OpenUrl`]'s scheme rule, so that its length is what a
    /// boundary test is left measuring.
    ///
    /// Built from [`ALLOWED_URL_SCHEMES`] rather than spelled out: which
    /// schemes are allowed, and why, is [`crate::content`]'s to say, and a
    /// literal here would be a second copy of that decision in a module with no
    /// view of it. Any member of the list will do — nothing in these tests
    /// depends on which — so the first is taken.
    fn allowed_url_opening() -> String {
        let scheme = ALLOWED_URL_SCHEMES
            .first()
            .expect("the scheme allow-list is never empty");
        format!("{scheme}:")
    }

    /// Asserts a bounded string field is tested on **both** sides of its bound:
    /// exactly `max` bytes parses and survives whole, `max + 1` bytes does not
    /// parse at all. An off-by-one that only rejects far-over values fails here.
    ///
    /// Each side is checked twice, once with an ASCII candidate and once with a
    /// multi-byte one, because ASCII alone cannot tell bytes from characters:
    /// every bound here is a byte count, and a parse path that counted
    /// `chars()` instead would pass an ASCII-only suite while admitting four
    /// times the documented budget in emoji.
    ///
    /// `build` places a candidate value inside an otherwise-valid message of
    /// type `T`, so each field is exercised through the real message it travels
    /// in rather than in isolation.
    fn assert_string_boundary<T>(max: usize, build: impl Fn(&str) -> Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + fmt::Debug,
    {
        assert_string_boundary_with_prefix::<T>(max, "", build);
    }

    /// [`assert_string_boundary`] for a field that also has to satisfy a
    /// content rule to parse at all: `prefix` opens every candidate, and counts
    /// against the bound like any other bytes, so what is being tested either
    /// side of the bound is still the length and only the length.
    fn assert_string_boundary_with_prefix<T>(
        max: usize,
        prefix: &str,
        build: impl Fn(&str) -> Value,
    ) where
        T: Serialize + for<'de> Deserialize<'de> + fmt::Debug,
    {
        // "é" is two bytes, so `filler / 2` of them sit exactly on the bound; a
        // trailing ASCII byte then puts the value one byte — not one character —
        // over it. An odd filler takes one ASCII byte of padding so the
        // candidate still lands exactly on the bound: the prefix's length is
        // whatever the field's content rules require, not something this helper
        // gets to choose.
        let filler = max - prefix.len();
        let multi_byte = format!(
            "{prefix}{}{}",
            "é".repeat(filler / 2),
            "a".repeat(filler % 2)
        );
        assert_eq!(multi_byte.len(), max);

        for at_bound in [format!("{prefix}{}", "a".repeat(filler)), multi_byte] {
            let parsed: T = serde_json::from_str(&build(&at_bound).to_string())
                .unwrap_or_else(|e| panic!("a value of exactly {max} bytes must parse, got: {e}"));
            assert!(
                serde_json::to_string(&parsed).unwrap().contains(&at_bound),
                "the value must survive the parse whole; nothing here truncates to fit"
            );

            let over = format!("{at_bound}a");
            assert_eq!(over.len(), max + 1);
            let err = serde_json::from_str::<T>(&build(&over).to_string())
                .expect_err("a value one byte over the bound must be refused");
            assert!(
                err.to_string().contains("over its maximum of"),
                "the refusal must name the bound it broke, got: {err}"
            );
        }
    }

    /// The sequence counterpart of [`assert_string_boundary`]: `max` elements
    /// parse and arrive complete, `max + 1` do not parse. `build` returns a
    /// message holding `n` elements, and `count` reads the length back out of
    /// the parsed message — truncating to fit is the failure mode a count bound
    /// is most likely to get wrong, so the length is asserted, not assumed.
    fn assert_count_boundary<T>(
        max: usize,
        build: impl Fn(usize) -> Value,
        count: impl Fn(&T) -> usize,
    ) where
        T: for<'de> Deserialize<'de> + fmt::Debug,
    {
        let parsed: T = serde_json::from_str(&build(max).to_string())
            .unwrap_or_else(|e| panic!("exactly {max} elements must parse, got: {e}"));
        assert_eq!(
            count(&parsed),
            max,
            "all {max} elements must survive the parse; nothing here truncates to fit"
        );

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
            item["icon"] = json!({ "name": v });
            item
        });
    }

    #[test]
    fn icon_path_bound_holds_on_both_sides() {
        // A path has to be absolute to get as far as its length being the
        // reason it is refused, so the leading `/` is the prefix — and it counts
        // against the bound like any other byte.
        assert_string_boundary_with_prefix::<Item>(MAX_ICON_PATH, "/", |v| {
            let mut item = item_json();
            item["icon"] = json!({ "path": v });
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
        // A URL has to open with an allowed scheme to get as far as its length
        // being the reason it is refused.
        assert_string_boundary_with_prefix::<ExecOutcome>(
            MAX_OPEN_URL,
            &allowed_url_opening(),
            |v| json!({ "open_url": v }),
        );
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
        assert_count_boundary::<Item>(
            MAX_ACTIONS_PER_ITEM,
            |n| {
                let mut item = item_json();
                item["actions"] = json!(vec![action_json(); n]);
                item
            },
            |item| item.actions.len(),
        );
    }

    #[test]
    fn items_per_results_frame_bound_holds_on_both_sides() {
        assert_count_boundary::<DaemonMsg>(
            MAX_ITEMS_PER_RESULTS_FRAME,
            |n| {
                json!({
                    "type": "results",
                    "query_id": 1,
                    "partial": false,
                    "items": vec![item_json(); n],
                })
            },
            |msg| match msg {
                DaemonMsg::Results { items, .. } => items.len(),
                other => panic!("expected a results frame, got {other:?}"),
            },
        );
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
        assert!(text.contains(QueryText::FIELD), "got: {text}");
        assert!(text.contains(&MAX_QUERY_TEXT.to_string()), "got: {text}");
    }

    // --- A wrong-typed value names its field too (issue #82) ----------------
    //
    // The length refusal just above goes through `BoundError::TooLong`, which
    // carries `field`. A value of the wrong JSON type never reaches that: it
    // is refused earlier, by serde's own `invalid_type`, formatted from
    // whichever `Visitor::expecting` was in play — `Validated::expecting` for
    // a validating newtype, `BoundedString::expecting` for a plain bounded
    // `String` field, `BoundedVec::expecting` for a bounded sequence. All
    // three held `field` in their struct and never wrote it, so all three
    // named nothing. These pin the fix across every shape this module hands
    // to `deserialize_with`, not only the validating newtypes issue #82 named.

    #[test]
    fn a_type_mismatch_on_a_validated_newtype_names_its_field() {
        // `Validated::expecting` backs every validating newtype that goes
        // through `limits::validated`, not only the four `FIELD`-carrying
        // types in `content` — `QueryText` is one too. A wrong-typed `text`
        // and a `null` one both have to be refused by the same `expecting`,
        // naming the field either way.
        let wrong_type = json!({ "type": "query", "id": 1, "text": 42 });
        let err = serde_json::from_str::<ClientMsg>(&wrong_type.to_string()).unwrap_err();
        assert!(err.to_string().contains(QueryText::FIELD), "got: {err}");

        let null = json!({ "type": "query", "id": 1, "text": null });
        let err = serde_json::from_str::<ClientMsg>(&null.to_string()).unwrap_err();
        assert!(err.to_string().contains(QueryText::FIELD), "got: {err}");
    }

    #[test]
    fn a_type_mismatch_on_a_bounded_string_field_names_its_field() {
        // `Item.title` has no `FIELD` constant of its own — `de_title` passes
        // a literal to `string` — but `BoundedString::expecting` has the same
        // defect `Validated::expecting` does, for the same reason: the
        // struct holds `field`, and the old `expecting` never wrote it.
        let mut item = item_json();
        item["title"] = json!(42);
        let err = serde_json::from_str::<Item>(&item.to_string()).unwrap_err();
        assert!(err.to_string().contains("Item.title"), "got: {err}");
    }

    #[test]
    fn a_type_mismatch_on_an_optional_bounded_string_field_names_its_field() {
        // `subtitle` is `Option<String>`, deserialized through
        // `BoundedOptString`. A JSON value that is present and non-null still
        // routes through `visit_some` into the very same `BoundedString`
        // `de_subtitle` hands off to for the inner value — so this exercises
        // `BoundedString::expecting` again, under a different field name, not
        // `BoundedOptString::expecting`. See that struct's own doc comment,
        // above in this file, for why its `expecting` cannot be reached this
        // way at all.
        let mut item = item_json();
        item["subtitle"] = json!(true);
        let err = serde_json::from_str::<Item>(&item.to_string()).unwrap_err();
        assert!(err.to_string().contains("Item.subtitle"), "got: {err}");
    }

    #[test]
    fn a_type_mismatch_on_a_bounded_vec_field_names_its_field() {
        // `Item.actions` is a bounded sequence, deserialized through
        // `BoundedVec`. A number where an array is expected is refused by
        // `deserialize_seq` before any element is read, through
        // `BoundedVec::expecting`.
        let mut item = item_json();
        item["actions"] = json!(42);
        let err = serde_json::from_str::<Item>(&item.to_string()).unwrap_err();
        assert!(err.to_string().contains("Item.actions"), "got: {err}");
    }

    // The bounds on `Item`, `IconSpec`, `Action`, `ExecOutcome` and `ProtoError`
    // are mostly reached above by parsing those types directly, where
    // serde_json's own `Deserializer` drives the visitor. Inside a real frame
    // the driver is different: the tagged enums buffer into `Content` first, so
    // the visitors are fed by `ContentDeserializer` instead. It enforces the
    // same bounds today — but that is a property of serde's buffering, not
    // something this crate controls, so it is pinned here rather than assumed.
    // A serde change that, say, short-circuited `Content::Null` in
    // `deserialize_option` would otherwise silently unbound `subtitle` inside a
    // `results` frame while every direct-parse test stayed green.

    fn results_frame(item: Value) -> String {
        json!({ "type": "results", "query_id": 1, "partial": false, "items": [item] }).to_string()
    }

    #[test]
    fn every_item_bound_fires_through_the_tagged_results_frame() {
        let over_by_one = |max: usize| json!("a".repeat(max + 1));

        let mut over_id = item_json();
        over_id["id"] = over_by_one(MAX_ITEM_ID);
        let mut over_default_action = item_json();
        over_default_action["default_action"] = over_by_one(MAX_ACTION_ID);
        let mut over_title = item_json();
        over_title["title"] = over_by_one(MAX_TITLE);
        let mut over_subtitle = item_json();
        over_subtitle["subtitle"] = over_by_one(MAX_SUBTITLE);
        let mut over_provider = item_json();
        over_provider["provider"] = over_by_one(MAX_PROVIDER_ID);
        let mut over_copy_text = item_json();
        over_copy_text["copy_text"] = over_by_one(MAX_COPY_TEXT);
        let mut over_icon_name = item_json();
        over_icon_name["icon"] = json!({ "name": over_by_one(MAX_ICON_NAME) });
        let mut over_icon_path = item_json();
        over_icon_path["icon"] = json!({ "path": format!("/{}", "a".repeat(MAX_ICON_PATH)) });
        let mut over_label = item_json();
        let mut long_label = action_json();
        long_label["label"] = over_by_one(MAX_ACTION_LABEL);
        over_label["actions"] = json!([long_label]);
        let mut over_action_id = item_json();
        let mut long_action_id = action_json();
        long_action_id["id"] = over_by_one(MAX_ACTION_ID);
        over_action_id["actions"] = json!([long_action_id]);
        let mut over_action_count = item_json();
        over_action_count["actions"] = json!(vec![action_json(); MAX_ACTIONS_PER_ITEM + 1]);

        let cases = [
            ("ItemId", over_id),
            ("ActionId", over_default_action),
            ("ActionId", over_action_id),
            ("Item.title", over_title),
            ("Item.subtitle", over_subtitle),
            ("Item.provider", over_provider),
            ("Item.copy_text", over_copy_text),
            ("IconSpec::Name", over_icon_name),
            ("IconSpec::Path", over_icon_path),
            ("Action.label", over_label),
            ("Item.actions", over_action_count),
        ];

        for (field, item) in cases {
            let Err(err) = serde_json::from_str::<DaemonMsg>(&results_frame(item)) else {
                panic!("an over-long {field} must be refused inside a results frame");
            };
            assert!(
                err.to_string().contains(field),
                "the refusal must name {field}, got: {err}"
            );
        }
    }

    #[test]
    fn outcome_and_error_bounds_fire_through_their_tagged_frames() {
        let opening = allowed_url_opening();
        let over_long_url = format!("{opening}{}", "a".repeat(MAX_OPEN_URL + 1 - opening.len()));

        let cases = [
            (
                CopyText::FIELD,
                json!({
                    "type": "executed",
                    "query_id": 1,
                    "outcome": { "copy_text": "a".repeat(MAX_COPY_TEXT + 1) },
                }),
            ),
            (
                OpenUrl::FIELD,
                json!({
                    "type": "executed",
                    "query_id": 1,
                    "outcome": { "open_url": over_long_url },
                }),
            ),
            (
                "ProtoError.message",
                json!({
                    "type": "error",
                    "query_id": null,
                    "error": { "code": "internal", "message": "a".repeat(MAX_ERROR_MESSAGE + 1) },
                }),
            ),
        ];

        for (field, frame) in cases {
            let err = serde_json::from_str::<DaemonMsg>(&frame.to_string())
                .expect_err("the over-long value must be refused inside a frame");
            assert!(err.to_string().contains(field), "got: {err}");
        }
    }

    // Every other string test in this module uses escape-free values, and per
    // the routing table on `string` all of those reach `BoundedString` through
    // `visit_str` — including the ones that go through a tagged frame. That
    // leaves the `visit_string` arm, and the only way in is an escaped string
    // inside a tagged frame. This is not an exotic input: a window title
    // holding a quote or a newline is ordinary traffic, and without these two
    // cases the check on that arm could be deleted with every test still
    // green.

    #[test]
    fn an_over_long_escaped_title_is_refused_inside_a_tagged_frame() {
        let mut item = item_json();
        // The trailing newline is what serde escapes on the way out, which is
        // what forces the buffered content to be owned rather than borrowed.
        item["title"] = json!(format!("{}\n", "a".repeat(MAX_TITLE)));

        let err = serde_json::from_str::<DaemonMsg>(&results_frame(item))
            .expect_err("an over-long title must be refused however it is encoded");
        assert!(err.to_string().contains("Item.title"), "got: {err}");
    }

    #[test]
    fn an_escaped_title_on_the_bound_still_parses_whole_inside_a_tagged_frame() {
        let title = format!("{}\n", "a".repeat(MAX_TITLE - 1));
        assert_eq!(title.len(), MAX_TITLE);
        let mut item = item_json();
        item["title"] = json!(title);

        let msg: DaemonMsg = serde_json::from_str(&results_frame(item))
            .unwrap_or_else(|e| panic!("a value of exactly {MAX_TITLE} bytes must parse: {e}"));
        let DaemonMsg::Results { items, .. } = msg else {
            panic!("expected a results frame");
        };
        assert_eq!(
            items[0].title, title,
            "the escape must be decoded and the value must survive whole"
        );
    }

    #[test]
    fn optional_item_fields_are_still_optional_inside_the_tagged_results_frame() {
        // The buffered path has its own `deserialize_option`, so absence has to
        // be pinned here too, not only against a bare `Item`.
        let mut item = item_json();
        let object = item.as_object_mut().unwrap();
        object.remove("subtitle");
        object.remove("copy_text");
        object.remove("icon");

        let msg: DaemonMsg = serde_json::from_str(&results_frame(item))
            .unwrap_or_else(|e| panic!("omitted optional fields must still parse, got: {e}"));
        let DaemonMsg::Results { items, .. } = msg else {
            panic!("expected a results frame");
        };
        assert_eq!(items[0].subtitle, None);
        assert_eq!(items[0].copy_text, None);
        assert_eq!(items[0].icon, None);
    }

    // The budget table in this module's docs is the only place the composed
    // worst case is stated, and prose does not recompute itself when a constant
    // is retuned. This is the same arithmetic read off the constants, so a
    // changed bound fails here instead of leaving that table quietly wrong.
    // Both totals are literals on purpose: they are what the docs claim, and a
    // test that recomputed them on both sides would assert nothing.
    #[test]
    fn the_documented_worst_case_is_what_the_constants_compose_to() {
        // An icon contributes one of its two bounds and not the sum of them:
        // `IconSpec` is an enum, so a name and a path cannot both be present.
        let per_item = MAX_ITEM_ID
            + MAX_TITLE
            + MAX_SUBTITLE
            + MAX_ICON_NAME.max(MAX_ICON_PATH)
            + MAX_COPY_TEXT
            + MAX_PROVIDER_ID
            + MAX_ACTION_ID
            + MAX_ACTIONS_PER_ITEM * (MAX_ACTION_ID + MAX_ACTION_LABEL);
        assert_eq!(
            per_item, 84_160,
            "the per-item total in this module's budget table no longer matches the constants"
        );

        let per_frame = per_item * MAX_ITEMS_PER_RESULTS_FRAME;
        assert_eq!(
            per_frame, 84_160_000,
            "the ~84 MB per-frame figure in this module's budget table no longer holds"
        );

        // MAX_FRAME_BYTES is set against this same figure, with headroom for
        // JSON syntax and realistic escaping — see its doc comment. Asserted
        // here, alongside the arithmetic it is set against, so that retuning
        // an item bound cannot silently outgrow the frame cap without this
        // test noticing.
        assert!(
            MAX_FRAME_BYTES >= 3 * per_frame,
            "MAX_FRAME_BYTES no longer has 3x headroom over the documented worst-case frame"
        );
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

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn the_per_query_cap_admits_at_least_one_full_frame() {
        // A cap below one frame's bound would make a single maximal `results`
        // frame unrepresentable: the daemon could accept the frame's items and
        // immediately have to truncate them. The relation, not either number,
        // is the invariant.
        assert!(MAX_ITEMS_PER_QUERY >= MAX_ITEMS_PER_RESULTS_FRAME);
    }
}
