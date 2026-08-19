//! Client/daemon message frames exchanged over the (future) IPC transport.
//!
//! # Where peer trust comes from
//!
//! Not from anything below. [`ClientMsg::Hello`] and [`DaemonMsg::HelloAck`]
//! carry a version number and nothing that identifies who is holding the
//! other end of the socket — no credential, no token, no peer id. Completing
//! the handshake proves only that a peer speaks the same `api_version`, not
//! that it is anyone in particular.
//!
//! What actually gates who can open the socket in the first place is
//! filesystem permissions, set by `hopd` and invisible from this crate: the
//! socket file is narrowed to mode 0600 right after `bind`
//! (`crates/hopd/src/server.rs`), and the runtime directory holding it is
//! created at mode 0700 (`crates/hopd/src/runtime_dir.rs`). A Linux peer
//! credential check — `SO_PEERCRED` — would corroborate that a connecting
//! process really is who the socket's ownership implies, but nothing in this
//! workspace consults one today; there is no connection-handling code that
//! asks. Reaching the socket is the only bar there is.
//!
//! The consequence: any process that can open the socket is fully
//! authorized. The bounds and content rules this crate enforces — documented
//! on [`ClientMsg`], [`DaemonMsg`] and throughout [`limits`] — constrain a
//! confused or careless peer and bound how much memory and work one
//! connection can cost. They are not an access-control layer over a hostile
//! peer, because a hostile peer that reached the socket is already inside
//! whatever boundary those rules draw. See
//! `docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, "Where peer
//! trust comes from", for the fuller argument this section summarizes.

use serde::{Deserialize, Serialize};

use crate::content::{CopyText, OpenUrl};
use crate::item::{ActionId, Item, ItemId};
use crate::limits;
use crate::marker_span::MarkerSpan;
use crate::mode::Mode;
use crate::redaction::QueryText;

/// Messages sent from a client to the daemon.
///
/// Every variable-length field is bounded at the deserialization boundary; the
/// bounds and their reasoning live in [`limits`].
///
/// This enum derives `Debug`, and one of its variants holds text the user
/// typed. That field is a [`QueryText`], whose `Debug` prints a marker and a
/// byte count in place of the text, so formatting a frame does not reproduce
/// the keystrokes — see [`redaction`](crate::redaction).
///
/// # The tag buffers before these bounds apply
///
/// This enum is internally tagged, so serde must read the whole JSON value into
/// an in-memory buffer before it can dispatch on `type` and hand the fields to
/// the deserializers that enforce the bounds. A 200 MB `query` frame is
/// therefore *rejected*, but only after 200 MB has been buffered. Closing that
/// gap needs a cap on the frame length applied by the transport before serde
/// sees a byte — issue #21. The representation is deliberately left as it is:
/// the tagged form is the wire contract, and changing it to dodge the buffering
/// would be a breaking change that still would not bound the frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// The handshake. A client must send this as the first frame on every
    /// connection, naming the `api_version` it speaks; only after a matching
    /// [`DaemonMsg::HelloAck`] does a daemon accept anything else.
    ///
    /// # This crate states the rule; nothing here enforces it
    ///
    /// `Hello` is an ordinary variant of this enum, sitting beside `Query`,
    /// `Cancel` and `Execute` with nothing marking it as a pre-session
    /// message — there is no session type encoding "handshake completed", so
    /// nothing in these types refuses an `Execute` sent first. What actually
    /// refuses it is `hopd`'s connection driver
    /// (`crates/hopd/src/connection.rs`), which tracks handshake state itself
    /// and answers a first frame that is not `Hello` with
    /// [`ErrorCode::HandshakeRequired`] before closing the connection. That
    /// is this daemon's behavior, not a guarantee the contract makes — an
    /// implementer of a second daemon has to track the same state and refuse
    /// the same way itself; reading these types alone will not tell it to.
    Hello { api_version: u32 },
    /// A query from the client.
    ///
    /// A `Query` on a connection with a query already active cancels that query
    /// server-side; the daemon sends no further frames for the superseded id,
    /// not even `QueryDone`.
    ///
    /// # `id` must be unique for the life of the connection
    ///
    /// The client chooses `id`, and it must not reuse one it has already sent
    /// on the same connection. A counter incremented per query is the natural
    /// implementation, and monotonic ids also make "later" readable off the
    /// number, which nothing else on this connection supplies.
    ///
    /// Reuse is not *refused*, because by the time the second frame arrives
    /// the daemon no longer holds the state that would let it recognise one:
    /// the retained set is keyed by the id and there is no history beside it.
    /// What the daemon does instead is treat `Query { id }` as an ordinary new
    /// exchange — a second one that happens to carry the same id — which
    /// supersedes the first and replaces its retained items whole. The
    /// consequences land on the client:
    ///
    /// - Items delivered in the first round are still on the client's screen
    ///   and still labelled with this `query_id`, but the daemon no longer
    ///   holds them, so an [`Execute`](ClientMsg::Execute) frame naming one is
    ///   refused as unknown (issue #59) even though this daemon is what sent
    ///   it. That is
    ///   the exact failure the retained set's never-evict rule exists to
    ///   prevent, reintroduced by the client rather than by the daemon.
    /// - Frames of the two rounds are indistinguishable on the wire, so the
    ///   client cannot tell a late frame of the first from a frame of the
    ///   second, and the stale-frame drop it relies on has nothing to key on.
    Query {
        id: u64,
        /// What the user typed, held as a [`QueryText`] rather than a `String`
        /// so that formatting this frame does not print it — see
        /// [`redaction`](crate::redaction). The type also carries the
        /// [`MAX_QUERY_TEXT`](crate::limits::MAX_QUERY_TEXT) bound applied on
        /// the way in.
        ///
        /// This is the string that flows into the query path and that the
        /// learning store keeps resident as an in-memory key. It never reaches
        /// disk — only the item-id-keyed frequency table is persisted.
        text: QueryText,
    },
    /// Cancels the active query if `id` names it (the daemon answers
    /// `QueryDone { query_id: id }`); dropped silently otherwise, because a
    /// cancel racing a natural `QueryDone` is ordinary traffic.
    Cancel { id: u64 },
    Execute {
        query_id: u64,
        item_id: ItemId,
        action_id: ActionId,
    },
}

/// Messages sent from the daemon to a client.
///
/// A client trusts its daemon no more than the daemon trusts its clients, so
/// these are bounded in the same way and for the same reason: see
/// [`limits`], and the buffering caveat on [`ClientMsg`], which
/// applies identically here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMsg {
    /// Answers [`ClientMsg::Hello`], echoing back `api_version`. `hopd` sends
    /// this only after accepting a `Hello` whose version already matched
    /// [`API_VERSION`](crate::API_VERSION), so in this daemon's behavior the
    /// value here is always that same constant reflected back, never a
    /// counter-offer.
    ///
    /// # No capability set, today
    ///
    /// This is everything the ack carries. There is no feature list, no
    /// negotiated option, no set of capabilities beyond the bare version
    /// number — a peer learns compatibility from `api_version` alone and has
    /// no way to ask what else this daemon supports. Adding one, a
    /// `capabilities` field or similar, is a wire-contract change like any
    /// other: existing peers do not expect it and have to be updated to read
    /// it, the same distinction [`ErrorCode`]'s docs draw for a new variant.
    HelloAck {
        api_version: u32,
    },
    /// How the daemon routed an accepted [`ClientMsg::Query`]: which
    /// [`Mode`] it was interpreted as, and whether that route **filtered**
    /// results to that mode's kinds.
    ///
    /// # Why this is its own frame rather than a field on `Results`
    ///
    /// Because a query that matches nothing sends no `Results` frame at all.
    /// The daemon's terminal frame is [`DaemonMsg::QueryDone`], and it is sent
    /// alone when a source finishes empty — so a mode carried on `Results`
    /// would be absent precisely when it matters most. An exclusive route that
    /// filtered everything away is the case a user most needs explained: the
    /// difference between "Windows — no matches" and a bare "no results" is
    /// the whole reason a client is told the mode.
    ///
    /// Putting it on `QueryDone` instead was rejected for the opposite
    /// reason — it arrives after results have rendered, so a mode label would
    /// appear late, while the user is still typing.
    ///
    /// # Ordering
    ///
    /// Exactly one `QueryRouted` per accepted query, sent **before** any
    /// `Results` or `QueryDone` bearing the same `query_id`. A client may
    /// therefore render a mode label before the first item arrives, and the
    /// stale-frame rule governs this frame exactly as it governs `Results`: a
    /// `QueryRouted` for a superseded id is dropped like any other stale
    /// frame.
    ///
    /// # `exclusive` is not derivable from `mode`
    ///
    /// The same [`Mode`] is reachable both ways — `route("$100 usd")` and
    /// `route("100 usd to eur")` are both [`Mode::Currency`], one exclusive
    /// and one inferred — so the flag is a separate field rather than
    /// something a client could infer. It is also the half that carries the
    /// user-facing meaning: `exclusive` is true exactly when results the user
    /// cannot see were withheld.
    ///
    /// # `marker_span` (issue #184)
    ///
    /// The byte range within the query's raw text that routing consumed as a
    /// marker — a prefix, a sigil, a trailing phrase, or (on an
    /// alias-matched timezone route) the whole typed token. `None` where
    /// nothing was consumed as a marker: the [`Mode::All`] fallback, and an
    /// inferred route that matched a *shape* (a bare sum, a bare currency
    /// amount) rather than a marker.
    ///
    /// This exists so a client can highlight the consumed marker inside the
    /// query field it is already rendering without re-parsing the query text
    /// to find it — see [`MarkerSpan`]'s own docs for why it travels as
    /// offsets into text the client already holds rather than as the
    /// marker's characters, and for what those offsets do and do not
    /// guarantee about landing on a real character boundary.
    QueryRouted {
        query_id: u64,
        mode: Mode,
        exclusive: bool,
        marker_span: Option<MarkerSpan>,
    },
    /// One frame of a query's results.
    ///
    /// # The replace rule
    ///
    /// `items` is the **complete current result list** for `query_id`, never
    /// an increment on the previous frame: a client receiving this frame
    /// replaces whatever it is holding for that id rather than appending to
    /// it. A daemon never splits one list across frames — see
    /// [`MAX_ITEMS_PER_RESULTS_FRAME`](crate::limits::MAX_ITEMS_PER_RESULTS_FRAME)
    /// for what makes that true on the wire — so a `results` frame is never
    /// "the rest of" the one before it, and a client that concatenates two
    /// frames' `items` produces a list this daemon never sent.
    ///
    /// # Why several frames still arrive for one query
    ///
    /// A daemon sends one `results` frame per *provider arrival*, not one per
    /// query: each frame is a fresh, re-ranked list over everything received
    /// for the query so far. That is what lets a fast provider's results
    /// reach the screen while a slow one is still running — no frame waits on
    /// the slowest provider to be assembled ("No slowest-provider gate", the
    /// design spec's §3). A query that reaches three providers ordinarily
    /// produces three `results` frames before its `QueryDone`, each replacing
    /// the last in full.
    Results {
        query_id: u64,
        /// Advisory, and unchanged in that respect: the terminal signal is
        /// still [`DaemonMsg::QueryDone`], never a `partial: false` frame, and
        /// clients must key on that rather than on this field.
        ///
        /// What changed is what `true` *means*. Under the replace rule above
        /// there is no partial list left to complete — only a series of
        /// wholesale replacements — so `true` no longer says "more items
        /// follow"; it says "a later frame may replace this list". Reading it
        /// the old way, as a promise that this same list will grow, is
        /// exactly the mistake this paragraph exists to head off.
        partial: bool,
        /// The complete current result list for `query_id` — see the replace
        /// rule above. Bounded at
        /// [`MAX_ITEMS_PER_RESULTS_FRAME`](crate::limits::MAX_ITEMS_PER_RESULTS_FRAME)
        /// items and refused at the parse if it holds more. Under
        /// replacement that bound is not only this field's: because a client
        /// holds exactly the last frame it received rather than a sum of
        /// frames, it is also the effective ceiling on what a client holds
        /// for this query at any moment.
        ///
        /// [`MAX_ITEMS_PER_QUERY`](crate::limits::MAX_ITEMS_PER_QUERY) bounds
        /// something else — not this field, and not what a client holds,
        /// since a client accumulates nothing under replacement, but what the
        /// daemon accumulates from providers across every arrival in order to
        /// build each frame. See that constant's docs for where that
        /// accumulation happens and how it is enforced.
        #[serde(deserialize_with = "limits::de_results_items")]
        items: Vec<Item>,
    },
    /// The one terminal frame of a query exchange; sent when the source finishes,
    /// when the exchange ends at a cap — the result source's accumulator at
    /// [`MAX_ITEMS_PER_QUERY`](crate::limits::MAX_ITEMS_PER_QUERY), or the
    /// connection's defensive [`MAX_ITEMS_PER_RESULTS_FRAME`](crate::limits::MAX_ITEMS_PER_RESULTS_FRAME)
    /// bound truncating one over-long list — or in answer to a matching `Cancel`.
    ///
    /// # When an exchange ends without one
    ///
    /// A client waiting on this frame must be prepared for each of these
    /// instead, because none of them produces it:
    ///
    /// - A query **superseded** by a new `Query` on the same connection. The
    ///   client that superseded it has moved on, and a `QueryDone` for the old
    ///   id would be dropped as stale anyway.
    /// - A [`DaemonMsg::Error`] naming the query id, which is terminal for
    ///   that exchange in this frame's place — see that variant's contract.
    /// - The connection ending, whether at EOF or behind a connection-scoped
    ///   `DaemonMsg::Error` the daemon closes after. Every exchange on it ends
    ///   with it, in flight or not.
    QueryDone {
        query_id: u64,
    },
    Executed {
        query_id: u64,
        outcome: ExecOutcome,
    },
    /// A protocol-level error.
    ///
    /// # What `query_id` scopes, and what it does not
    ///
    /// `query_id` says what the error is *about*. It does not say whether the
    /// connection survives it, and a client that reads it as "fatal" or
    /// "recoverable" is reading something this field does not carry.
    ///
    /// - **`Some(id)`** scopes the error to that exchange, and it is terminal
    ///   for it: no [`DaemonMsg::QueryDone`] follows for `id`, and the two
    ///   never both arrive for one id. The connection stays usable and every
    ///   other query id on it is untouched, so a client drops an error naming
    ///   an id it is not waiting on exactly as it drops a stale `results`
    ///   frame. `ErrorCode::UnknownItem` and `ErrorCode::UnknownAction` are
    ///   query-scoped by construction and belong in this form.
    /// - **`None`** scopes the error to the connection, or to a frame that
    ///   named no query: a version mismatch, a frame before the handshake, a
    ///   frame refused at its length prefix or at its parse, or a frame the
    ///   daemon does not implement yet. Whether the connection continues is
    ///   *not* in the frame — `hopd` closes it behind the first four and keeps
    ///   it open behind the last, and a peer learns which by whether EOF
    ///   follows. A client with an exchange in flight should treat this form
    ///   as ending that exchange: nothing promises it a `QueryDone`, and it
    ///   cannot tell from the frame that one is still coming.
    Error {
        query_id: Option<u64>,
        error: ProtoError,
    },
}

/// The result of executing an action.
///
/// [`CopyText`] and [`OpenUrl`] are the two variants that tell a client to act
/// rather than describing what happened, so both carry a validating type rather
/// than a `String`: what may be in one is decided once, in
/// [`content`](crate::content), and cannot be sidestepped by sending a frame.
/// The wire form is a bare JSON string in both cases, as it was before those
/// types existed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutcome {
    Done,
    /// Text for the client to put on the clipboard.
    CopyText(CopyText),
    /// A URL for the client to open.
    OpenUrl(OpenUrl),
}

/// A protocol-level error reported by the daemon.
///
/// # What `message` may contain (issue #84)
///
/// #74 bounded this field's *length* (see [`MAX_ERROR_MESSAGE`]); nothing
/// bounded its *kind*. `message` was a bare `String`, so any `hopd` error
/// site could `format!` a filesystem path, a provider's own words, or a raw
/// `serde_json::Error`'s `Display` straight into a frame this daemon sends
/// to every client that asks — the same disclosure #27 closed for the
/// client-to-daemon direction (`QueryText`) but explicitly scoped out for
/// this one, because a redacting newtype fixes the wrong half of the
/// problem here: see "Rejected: a redacting newtype" below.
///
/// The decision: `message` is never typed in at a `hopd` error site. It is
/// **derived**, by [`ProtoError::new`], from an [`ErrorCode`] and an
/// [`ErrorDetail`] — a closed set of already-bounded, already-typed values
/// ([`ItemId`], [`ActionId`], a provider id, a `u32` version, a `usize`
/// length) or a `&'static str` chosen at the call site. A `String` computed
/// at runtime — a path, a caught error's `Display`, anything a future error
/// site might be tempted to interpolate — has no route into `message` at
/// all: there is no `ErrorDetail` variant that takes one. That is what
/// "enforced by something other than a reviewer's memory" (criterion 1)
/// means concretely: the daemon does not merely avoid disclosure by
/// discipline, it cannot express it, because `message` itself — the private
/// field below — is not constructible outside [`ProtoError::new`].
///
/// # What structuring the message costs
///
/// Three costs, all accepted:
///
/// - **`ErrorDetail::Item` visibly shortens an oversized id.**
///   [`ProtoError::new`] applies no length check of its own — see
///   [`MAX_ERROR_MESSAGE`]'s own docs for where the bound is actually
///   enforced (the receiving peer's parse, nowhere else) — and remains
///   infallible by design. `Item`'s renderer budgets the complete message,
///   retaining the longest UTF-8-safe prefix of an in-bound
///   [`ItemId`] (bounded by [`MAX_ITEM_ID`](crate::limits::MAX_ITEM_ID)) that
///   fits before appending `… [truncated]`. This keeps the diagnostic visible
///   and within the peer's parse bound without changing either wire limit. The
///   behavior is pinned by
///   `tests::unknown_item_message_at_max_item_id_stays_within_max_error_message`,
///   its threshold and multi-byte boundary tests, and
///   `tests::every_error_detail_message_stays_within_max_error_message`.
/// - **`ErrorDetail::Fixed` still takes a string.** A `&'static str` is a
///   compile-time literal, not a runtime value, so nothing this daemon
///   *computes* — a path it opened, a peer's own bytes, an error it
///   caught — can reach it without either being typed into this crate's
///   source (reviewable, and immediately visible in a diff) or being
///   deliberately leaked to `'static` first (`Box::leak`, `String::leak`),
///   which is conspicuous safe-Rust ceremony no accidental call site
///   reaches for. This closes the *accidental* path — `format!("{err}")`,
///   `.to_string()` on a caught error, a `PathBuf` interpolated without a
///   thought — which is the path every real call site in this daemon
///   actually took before this change. It is not an absolute guarantee
///   against a determined author, the same honest limit `RoutedText` and
///   `QueryText` accept about their own callers.
/// - **`ErrorDetail::Provider` carries a plain `String`, not a validating
///   newtype.** This crate has no `ProviderId` type — `Item.provider` is a
///   plain, wire-bounded `String` everywhere else in this protocol too (see
///   [`limits::MAX_PROVIDER_ID`]) — so `ErrorDetail::Provider` matches that
///   existing shape rather than inventing a stronger one this issue did not
///   ask for. The guarantee that a `hopd` call site passes a real,
///   manifest-checked provider id and not arbitrary text is therefore a
///   caller invariant, the same trust this codebase already places in
///   `Item.provider` at the pipeline's checked-items boundary — not
///   something the type of this one field enforces. It is the weakest of
///   `ErrorDetail`'s variants against criterion 1, named here rather than
///   left to look like an oversight.
///
/// # Rejected: a redacting newtype
///
/// A `QueryText`/`RoutedText`-shaped type — its own `Debug` printing a
/// marker instead of the text — was considered and declined. That pattern
/// fixes disclosure into a *log*: a value with a redacting `Debug` still
/// carries the real text everywhere else, `Display`, `as_str`,
/// serialization, and gets to the party that logs a whole frame only
/// because logging is the one path its `Debug` intercepts. `ProtoError`
/// does not have that shape of problem — it is not logged and then
/// separately serialized, it *is* serialized, straight to the client that
/// is the one asking to see it — so a redacting `Debug` would protect
/// nothing: the internals it hid from a log line would still be sitting in
/// `message` when this struct is serialized onto the wire moments later.
/// Structuring what `message` may contain closes the disclosure at its
/// only real crossing; redacting `Debug` would have been theater over it.
///
/// # Rejected: discipline at construction sites
///
/// Leaving `message: String` public and relying on each of `hopd`'s error
/// sites to compose safe text by convention was the weakest option against
/// criterion 1 by construction: a reviewer's memory is exactly what it
/// would have rested on, and the issue's own motivation — the count was
/// nine sites and climbing — is the argument that a discipline which has to
/// be re-remembered at every new one does not scale. See this struct's
/// `message` field for the mechanism that replaces it.
///
/// # The asymmetry with a client's `Deserialize`
///
/// [`ProtoError::new`] is the *daemon's* construction path — the only one,
/// once `message` is private — but [`Deserialize`] on this struct stays as
/// permissive as before: any string up to [`MAX_ERROR_MESSAGE`] bytes
/// parses, including one no call to [`ProtoError::new`] could ever have
/// produced. A client reading a frame it *received* cannot check who built
/// it or retroactively restrict what shipped on the wire — the wire form is
/// unchanged, still a bare JSON string — so the parse stays permissive by
/// necessity. This is not an oversight; it is the identical split #83 draws
/// between `QueryText::new` (fallible, a client's own construction path)
/// and `RoutedText`'s infallible constructor (a value built from data this
/// crate does not control the shape of). Pinned by
/// `tests::a_client_deserializes_an_error_message_no_local_constructor_could_produce`.
///
/// # A gap this decision does not close (criterion 3)
///
/// `message` is not the only free-form text a `DaemonMsg` frame carries.
/// [`Item`]'s `copy_text`, and — walking one level further, into `items`' own
/// `actions` —
/// [`Action`](crate::item::Action)'s `label`
/// ([`limits::MAX_COPY_TEXT`], [`limits::MAX_ACTION_LABEL`]) travel inside
/// every [`DaemonMsg::Results`] frame as provider-authored strings. Titles
/// and subtitles are different: [`ItemTitle`](crate::ItemTitle) and
/// [`ItemSubtitle`](crate::ItemSubtitle) are validating newtypes, so their
/// byte bounds and single-line control-character rule apply on every
/// construction path while their wire form remains a bare string. The
/// action label remains a bounded plain string and is checked at the
/// `hop-core` checked-items boundary.
///
/// `Item`'s other string-shaped fields — `id`, `default_action`, `provider`,
/// and each action's own `id` — are deliberately not on this list: they are
/// opaque identifiers, not display prose, and `provider` in particular is
/// already checked against its producer's manifest id at the pipeline's
/// checked-items boundary (`hop-core`'s `CheckedItems::check`) rather than
/// carried verbatim from whatever a provider claims. `icon` is not on this
/// list either: both its arms, `IconName` and `IconPath`, are already
/// validating newtypes, not bare `String`s.
///
/// [`DaemonMsg::Executed`]'s [`ExecOutcome::CopyText`] and
/// [`ExecOutcome::OpenUrl`] are, by contrast, *not* a gap: both already
/// carry a validating newtype (see [`ExecOutcome`]'s own docs) rather than
/// a bare `String`, landed before this issue. Nor is the provider-authored
/// text behind [`ErrorCode::ProviderFailed`] — a `ProviderError::Failed`'s
/// own message — a gap in this field: `hopd`'s connection driver already
/// declines to forward it verbatim (see `crates/hopd/src/connection.rs`),
/// which is exactly why [`ErrorDetail::Provider`] carries only a provider
/// *id*, never a provider's own words. `HelloAck`, `QueryRouted`,
/// `QueryDone` and `Executed`'s own scalar/enum fields carry no free text at
/// all — `api_version`, `query_id`, [`Mode`], and `exclusive`/`partial` are
/// all closed or numeric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtoError {
    pub code: ErrorCode,
    /// Bounded at [`MAX_ERROR_MESSAGE`](crate::limits::MAX_ERROR_MESSAGE) bytes
    /// on the way in — an error headed for a UI is not a payload channel.
    ///
    /// Private on the construction side by design — see this struct's own
    /// docs. [`ProtoError::new`] is the only way to build one outside this
    /// module; [`ProtoError::message`] is how any caller, in any crate,
    /// reads one back.
    #[serde(deserialize_with = "limits::de_error_message")]
    message: String,
}

impl ProtoError {
    /// Builds a protocol error whose `message` is derived entirely from
    /// `code` and `detail` — see this struct's own docs for why that is the
    /// only construction path.
    pub fn new(code: ErrorCode, detail: ErrorDetail) -> Self {
        Self {
            code,
            message: detail.render(),
        }
    }

    /// The message text, however it was constructed — derived by
    /// [`ProtoError::new`] on the daemon's side, or whatever a peer's frame
    /// carried on a client's side. See this struct's "The asymmetry with a
    /// client's `Deserialize`" for why those are not the same guarantee.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// The bounded, typed data a [`ProtoError`] message may be derived from.
///
/// Never serialized: this type exists purely on the daemon's construction
/// path, discarded the moment [`ProtoError::new`] renders it to a `String`.
/// It carries no `code` of its own — `ProtoError::new` takes `code`
/// separately, since [`ErrorCode`] is the wire-stable classification a
/// client dispatches on and a detail is only ever the human text alongside
/// it — so pairing a variant here with an unrelated `code` is a call-site
/// mistake this type does not prevent, the same way passing the wrong
/// `ErrorCode` to `hopd`'s pre-#84 `send_error` never was prevented either.
/// What this type *does* prevent is the disclosure this issue is about: no
/// variant below accepts a `String` a `hopd` error site did not already
/// have bounded and typed for an unrelated reason.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorDetail {
    /// A message with no interpolated data at all: `message` becomes
    /// exactly `text`. `text` must be `&'static str` — see this struct's own
    /// "What structuring the message costs" for what that buys and what it
    /// does not.
    Fixed(&'static str),
    /// `message` names the item that was not found. Pairs with
    /// [`ErrorCode::UnknownItem`].
    Item(ItemId),
    /// `message` names the action that was not found or not offered.
    /// Pairs with [`ErrorCode::UnknownAction`].
    Action(ActionId),
    /// `message` names the provider whose execute failed. See this type's
    /// own docs for why this is a plain `String` rather than a validating
    /// newtype. Pairs with [`ErrorCode::ProviderFailed`].
    Provider(String),
    /// `message` reports a handshake's mismatched `api_version`s. Pairs
    /// with [`ErrorCode::VersionMismatch`].
    Version { expected: u32, actual: u32 },
    /// `message` reports a frame's length prefix having named more than
    /// [`MAX_FRAME_BYTES`](crate::limits::MAX_FRAME_BYTES) bytes. Pairs with
    /// [`ErrorCode::FrameTooLarge`].
    FrameTooLarge { len: usize },
}

impl ErrorDetail {
    fn render(&self) -> String {
        match self {
            ErrorDetail::Fixed(text) => (*text).to_string(),
            ErrorDetail::Item(id) => render_unknown_item(id),
            ErrorDetail::Action(id) => format!("unknown action {id}"),
            ErrorDetail::Provider(id) => format!("provider `{id}` failed to execute the action"),
            ErrorDetail::Version { expected, actual } => {
                format!("hopd speaks api_version {expected}, client sent {actual}")
            }
            ErrorDetail::FrameTooLarge { len } => {
                format!(
                    "frame of {len} bytes is over the {}-byte cap",
                    crate::limits::MAX_FRAME_BYTES
                )
            }
        }
    }
}

// `hop-core` and `hopd` each have a byte-boundary truncation helper, but this
// protocol crate cannot depend on either downstream crate without inverting
// the dependency graph (or creating a cycle). A public protocol utility would
// unnecessarily widen the API for this one private message render.
fn render_unknown_item(id: &ItemId) -> String {
    const PREFIX: &str = "unknown item ";
    const MARKER: &str = "… [truncated]";

    let id = id.as_str();
    if PREFIX.len() + id.len() <= limits::MAX_ERROR_MESSAGE {
        return format!("{PREFIX}{id}");
    }

    let id_budget = limits::MAX_ERROR_MESSAGE - PREFIX.len() - MARKER.len();
    let mut end = id_budget.min(id.len());
    while end > 0 && !id.is_char_boundary(end) {
        end -= 1;
    }
    format!("{PREFIX}{}{MARKER}", &id[..end])
}

/// The category of a protocol-level error.
///
/// Adding a variant here is a wire-contract change, the same way a third
/// [`IconSpec`](crate::item::IconSpec) arm would be: an older client that does
/// not recognize a new code cannot render it, so a new variant is something a
/// client has to be updated to handle, not something it can safely ignore the
/// way an unknown JSON field is ignored elsewhere in this crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    VersionMismatch,
    UnknownItem,
    UnknownAction,
    ProviderFailed,
    Internal,
    /// The daemon refused a frame because its length prefix, decoded by
    /// [`payload_len`](crate::framing::payload_len), named a payload over
    /// [`MAX_FRAME_BYTES`](crate::limits::MAX_FRAME_BYTES).
    ///
    /// The refusal happens at the prefix, before the payload behind it is
    /// read — that is the point of `payload_len` being a pre-allocation gate
    /// — so this code reports a peer that claimed to be sending too much, not
    /// one whose oversized payload this process actually held in memory.
    FrameTooLarge,
    /// The daemon refused a frame because it arrived before
    /// [`ClientMsg::Hello`](crate::wire::ClientMsg::Hello) on that connection.
    ///
    /// Every connection must open with a handshake before anything else is
    /// accepted — folded in from issue #26's criterion — so a client that
    /// sends `Query` or `Execute` first gets this instead of a response to a
    /// connection the daemon never agreed was version-compatible.
    HandshakeRequired,
    /// The daemon refused a frame because its payload, once read in full,
    /// did not decode as a [`ClientMsg`] — malformed JSON, an unrecognized
    /// `type` tag, or a value that fails one of [`limits`](crate::limits)'s
    /// bounds.
    ///
    /// This is [`FrameError::Decode`](crate::framing::FrameError::Decode)
    /// surfaced to the peer: the payload came off the wire from that peer,
    /// so a failure to parse it is peer-fault, the same attribution
    /// `framing`'s `Encode`/`Decode` split makes for the daemon's own side of
    /// the same failure mode. That is what keeps this code distinct from
    /// [`ErrorCode::Internal`] — `Internal` names a bug in this daemon,
    /// `MalformedFrame` names bytes this daemon was never obligated to make
    /// sense of.
    MalformedFrame,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::content::{IconName, ItemSubtitle, ItemTitle};
    use crate::item::*;

    fn sample_item() -> Item {
        Item {
            id: ItemId::new("app:firefox").unwrap(),
            kind: Kind::App,
            title: ItemTitle::new("Firefox").unwrap(),
            subtitle: Some(ItemSubtitle::new("Web Browser").unwrap()),
            icon: Some(IconSpec::Name(IconName::new("firefox").unwrap())),
            actions: vec![Action {
                id: ActionId::new("open").unwrap(),
                kind: ActionKind::Open,
                label: "Open".into(),
            }],
            default_action: ActionId::new("open").unwrap(),
            copy_text: None,
            append_to_end: false,
            provider: "apps".into(),
        }
    }

    #[test]
    fn client_msg_round_trips() {
        let msg = ClientMsg::Query {
            id: 7,
            text: QueryText::new("fire").unwrap(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"query""#));
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn formatting_a_query_frame_does_not_disclose_its_text() {
        let typed = "correct horse battery staple";
        let msg = ClientMsg::Query {
            id: 7,
            text: QueryText::new(typed).unwrap(),
        };
        let formatted = format!("{msg:?}");
        assert!(
            !formatted.contains(typed),
            "a formatted query frame must not carry what was typed, got: {formatted}"
        );
        // The field is still there to be read about, and the frame's other
        // fields are still diagnostic: what a log line loses is the value.
        assert!(formatted.contains("text"), "got: {formatted}");
        assert!(formatted.contains("id: 7"), "got: {formatted}");
    }

    #[test]
    fn client_msg_hello_round_trips() {
        let msg = ClientMsg::Hello { api_version: 1 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"hello","api_version":1}"#);
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn client_msg_cancel_round_trips() {
        let msg = ClientMsg::Cancel { id: 7 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"cancel","id":7}"#);
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn client_msg_execute_round_trips() {
        let msg = ClientMsg::Execute {
            query_id: 7,
            item_id: ItemId::new("app:firefox").unwrap(),
            action_id: ActionId::new("open").unwrap(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"execute","query_id":7,"item_id":"app:firefox","action_id":"open"}"#
        );
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn daemon_msg_query_routed_round_trips_with_a_marker_span() {
        let msg = DaemonMsg::QueryRouted {
            query_id: 7,
            mode: Mode::Weather,
            exclusive: true,
            marker_span: Some(MarkerSpan::new(0, 3).unwrap()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"query_routed","query_id":7,"mode":"weather","exclusive":true,"marker_span":{"start":0,"end":3}}"#
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn daemon_msg_query_routed_round_trips_with_no_marker_span() {
        let msg = DaemonMsg::QueryRouted {
            query_id: 7,
            mode: Mode::All,
            exclusive: false,
            marker_span: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"query_routed","query_id":7,"mode":"all","exclusive":false,"marker_span":null}"#
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn a_query_routed_frame_with_an_inverted_marker_span_is_refused() {
        let json = r#"{"type":"query_routed","query_id":7,"mode":"all","exclusive":false,"marker_span":{"start":5,"end":2}}"#;
        assert!(serde_json::from_str::<DaemonMsg>(json).is_err());
    }

    #[test]
    fn a_query_routed_frame_with_an_out_of_bounds_marker_span_is_refused() {
        let json = format!(
            r#"{{"type":"query_routed","query_id":7,"mode":"all","exclusive":false,"marker_span":{{"start":0,"end":{}}}}}"#,
            limits::MAX_QUERY_TEXT + 1
        );
        assert!(serde_json::from_str::<DaemonMsg>(&json).is_err());
    }

    #[test]
    fn a_query_routed_frame_missing_marker_span_parses_as_none() {
        // A plain `Option<T>` field gets serde's ordinary missing-field
        // default, even inside this crate's internally-tagged, buffered
        // parse — no `#[serde(default)]` needed, unlike the fields in
        // `crate::item` that pair a `deserialize_with` with one (see that
        // module's docs for why those need it and this field does not).
        //
        // This is worth pinning precisely because it means the JSON shape
        // itself is lenient: a frame missing `marker_span` is not, on its
        // own, what the `API_VERSION` bump in this crate's docs protects
        // against. What it protects against is a client built from a crate
        // that does not compile against this shape at all — `DaemonMsg` is a
        // Rust type a stale binary embeds at compile time, so the risk the
        // bump closes is a stale *binary*, not a stale *frame*; this test is
        // about the latter, and the two must not be conflated.
        let json = r#"{"type":"query_routed","query_id":7,"mode":"all","exclusive":false}"#;
        assert_eq!(
            serde_json::from_str::<DaemonMsg>(json).unwrap(),
            DaemonMsg::QueryRouted {
                query_id: 7,
                mode: Mode::All,
                exclusive: false,
                marker_span: None,
            }
        );
    }

    #[test]
    fn daemon_results_round_trips() {
        let msg = DaemonMsg::Results {
            query_id: 7,
            partial: true,
            items: vec![sample_item()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            concat!(
                r#"{"type":"results","query_id":7,"partial":true,"items":["#,
                r#"{"id":"app:firefox","kind":"app","title":"Firefox","#,
                r#""subtitle":"Web Browser","icon":{"name":"firefox"},"#,
                r#""actions":[{"id":"open","kind":"open","label":"Open"}],"#,
                r#""default_action":"open","copy_text":null,"append_to_end":false,"provider":"apps"}"#,
                r#"]}"#
            )
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn daemon_msg_hello_ack_round_trips() {
        let msg = DaemonMsg::HelloAck { api_version: 1 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"hello_ack","api_version":1}"#);
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn daemon_msg_query_done_round_trips() {
        let msg = DaemonMsg::QueryDone { query_id: 7 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"query_done","query_id":7}"#);
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn daemon_msg_executed_round_trips_with_non_done_outcome() {
        let msg = DaemonMsg::Executed {
            query_id: 7,
            outcome: ExecOutcome::CopyText(CopyText::new("hello").unwrap()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"executed","query_id":7,"outcome":{"copy_text":"hello"}}"#
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn daemon_msg_error_round_trips_with_query_id() {
        let msg = DaemonMsg::Error {
            query_id: Some(7),
            error: ProtoError {
                code: ErrorCode::UnknownItem,
                message: "no such item".into(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"error","query_id":7,"error":{"code":"unknown_item","message":"no such item"}}"#
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn daemon_msg_error_round_trips_without_query_id() {
        let msg = DaemonMsg::Error {
            query_id: None,
            error: ProtoError {
                code: ErrorCode::Internal,
                message: "boom".into(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"error","query_id":null,"error":{"code":"internal","message":"boom"}}"#
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
    }

    #[test]
    fn exec_outcome_variants_round_trip() {
        let done = ExecOutcome::Done;
        let json = serde_json::to_string(&done).unwrap();
        assert_eq!(json, r#""done""#);
        assert_eq!(serde_json::from_str::<ExecOutcome>(&json).unwrap(), done);

        let copy = ExecOutcome::CopyText(CopyText::new("hello").unwrap());
        let json = serde_json::to_string(&copy).unwrap();
        assert_eq!(json, r#"{"copy_text":"hello"}"#);
        assert_eq!(serde_json::from_str::<ExecOutcome>(&json).unwrap(), copy);

        let open = ExecOutcome::OpenUrl(OpenUrl::new("https://example.com").unwrap());
        let json = serde_json::to_string(&open).unwrap();
        assert_eq!(json, r#"{"open_url":"https://example.com"}"#);
        assert_eq!(serde_json::from_str::<ExecOutcome>(&json).unwrap(), open);
    }

    #[test]
    fn proto_error_round_trips_for_each_error_code() {
        let cases = [
            (ErrorCode::VersionMismatch, r#""version_mismatch""#),
            (ErrorCode::UnknownItem, r#""unknown_item""#),
            (ErrorCode::UnknownAction, r#""unknown_action""#),
            (ErrorCode::ProviderFailed, r#""provider_failed""#),
            (ErrorCode::Internal, r#""internal""#),
            (ErrorCode::FrameTooLarge, r#""frame_too_large""#),
            (ErrorCode::HandshakeRequired, r#""handshake_required""#),
            (ErrorCode::MalformedFrame, r#""malformed_frame""#),
        ];
        for (code, expected_json) in cases {
            assert_eq!(serde_json::to_string(&code).unwrap(), expected_json);

            let err = ProtoError {
                code: code.clone(),
                message: "boom".into(),
            };
            let json = serde_json::to_string(&err).unwrap();
            assert_eq!(serde_json::from_str::<ProtoError>(&json).unwrap(), err);
        }
    }

    #[test]
    fn proto_error_new_derives_a_fixed_message_from_a_static_str() {
        let err = ProtoError::new(
            ErrorCode::HandshakeRequired,
            ErrorDetail::Fixed("the first frame on a connection must be hello"),
        );
        assert_eq!(err.code, ErrorCode::HandshakeRequired);
        assert_eq!(
            err.message(),
            "the first frame on a connection must be hello"
        );
    }

    #[test]
    fn proto_error_new_derives_message_from_each_typed_detail() {
        let item = ProtoError::new(
            ErrorCode::UnknownItem,
            ErrorDetail::Item(ItemId::new("app:1").unwrap()),
        );
        assert_eq!(item.message(), "unknown item app:1");

        let action = ProtoError::new(
            ErrorCode::UnknownAction,
            ErrorDetail::Action(ActionId::new("open").unwrap()),
        );
        assert_eq!(action.message(), "unknown action open");

        let provider = ProtoError::new(
            ErrorCode::ProviderFailed,
            ErrorDetail::Provider("apps".to_string()),
        );
        assert_eq!(
            provider.message(),
            "provider `apps` failed to execute the action"
        );

        let version = ProtoError::new(
            ErrorCode::VersionMismatch,
            ErrorDetail::Version {
                expected: 1,
                actual: 999,
            },
        );
        assert_eq!(
            version.message(),
            "hopd speaks api_version 1, client sent 999"
        );

        let too_large = ProtoError::new(
            ErrorCode::FrameTooLarge,
            ErrorDetail::FrameTooLarge { len: 99_999_999 },
        );
        assert_eq!(
            too_large.message(),
            format!(
                "frame of 99999999 bytes is over the {}-byte cap",
                crate::limits::MAX_FRAME_BYTES
            )
        );
    }

    /// Pins the claim `MAX_ERROR_MESSAGE`'s doc makes: every `ErrorDetail`
    /// variant renders a message that stays within
    /// [`crate::limits::MAX_ERROR_MESSAGE`] even at their own maximum
    /// plausible input — `ActionId` and the provider string at their own
    /// wire bounds, the numeric fields at their type's maximum, and the
    /// longest `Fixed` literal any real `hopd` call site uses today. This is
    /// not a guarantee `ProtoError::new` enforces — it applies no length
    /// check of its own, see `MAX_ERROR_MESSAGE`'s docs — so this test is
    /// what stands behind the claim instead: add a variant, or grow one of
    /// these bounds, in a way that pushes a rendered message over the limit,
    /// and this fails rather than the overflow going unnoticed until a
    /// client refuses to parse it.
    #[test]
    fn every_error_detail_message_stays_within_max_error_message() {
        use crate::limits::{MAX_ACTION_ID, MAX_ERROR_MESSAGE, MAX_PROVIDER_ID};

        let cases = [
            ProtoError::new(
                ErrorCode::UnknownItem,
                ErrorDetail::Item(ItemId::new("a".repeat(crate::limits::MAX_ITEM_ID)).unwrap()),
            ),
            ProtoError::new(
                ErrorCode::UnknownAction,
                ErrorDetail::Action(ActionId::new("a".repeat(MAX_ACTION_ID)).unwrap()),
            ),
            ProtoError::new(
                ErrorCode::ProviderFailed,
                ErrorDetail::Provider("p".repeat(MAX_PROVIDER_ID)),
            ),
            ProtoError::new(
                ErrorCode::VersionMismatch,
                ErrorDetail::Version {
                    expected: u32::MAX,
                    actual: u32::MAX,
                },
            ),
            ProtoError::new(
                ErrorCode::FrameTooLarge,
                ErrorDetail::FrameTooLarge { len: usize::MAX },
            ),
            // The longest `ErrorDetail::Fixed` literal any real `hopd` call
            // site passes today (see `crates/hopd/src/connection.rs`).
            ProtoError::new(
                ErrorCode::Internal,
                ErrorDetail::Fixed("a connection may complete its handshake only once"),
            ),
        ];

        for err in cases {
            assert!(
                err.message().len() <= MAX_ERROR_MESSAGE,
                "{:?}'s message is {} bytes, over MAX_ERROR_MESSAGE ({MAX_ERROR_MESSAGE}): {:?}",
                err.code,
                err.message().len(),
                err.message(),
            );
        }
    }

    /// A maximal legal `ItemId` is visibly shortened so its rendered error
    /// remains a message a receiving peer can parse.
    #[test]
    fn unknown_item_message_at_max_item_id_stays_within_max_error_message() {
        let max_id = ItemId::new("a".repeat(crate::limits::MAX_ITEM_ID)).unwrap();
        let err = ProtoError::new(ErrorCode::UnknownItem, ErrorDetail::Item(max_id));
        assert!(
            err.message().len() <= crate::limits::MAX_ERROR_MESSAGE,
            "a max-length ItemId must stay within MAX_ERROR_MESSAGE, got {} bytes",
            err.message().len(),
        );
        assert!(err.message().ends_with("… [truncated]"));
    }

    #[test]
    fn unknown_item_message_shortens_only_after_the_full_id_threshold() {
        const PREFIX: &str = "unknown item ";
        const MARKER: &str = "… [truncated]";
        let full_id_bytes = crate::limits::MAX_ERROR_MESSAGE - PREFIX.len();

        let below = "a".repeat(full_id_bytes - 1);
        let below_err = ProtoError::new(
            ErrorCode::UnknownItem,
            ErrorDetail::Item(ItemId::new(below.clone()).unwrap()),
        );
        assert_eq!(below_err.message(), format!("{PREFIX}{below}"));

        let exact = "a".repeat(full_id_bytes);
        let exact_err = ProtoError::new(
            ErrorCode::UnknownItem,
            ErrorDetail::Item(ItemId::new(exact.clone()).unwrap()),
        );
        assert_eq!(exact_err.message(), format!("{PREFIX}{exact}"));

        let over = "a".repeat(full_id_bytes + 1);
        let over_err = ProtoError::new(
            ErrorCode::UnknownItem,
            ErrorDetail::Item(ItemId::new(over).unwrap()),
        );
        let retained_id_bytes = crate::limits::MAX_ERROR_MESSAGE - PREFIX.len() - MARKER.len();
        assert_eq!(
            over_err.message(),
            format!("{PREFIX}{}{MARKER}", "a".repeat(retained_id_bytes))
        );
        assert!(over_err.message().len() <= crate::limits::MAX_ERROR_MESSAGE);
    }

    #[test]
    fn unknown_item_message_shortening_never_splits_a_multi_byte_id() {
        const PREFIX: &str = "unknown item ";
        const MARKER: &str = "… [truncated]";
        let id = format!("{}語{}", "a".repeat(995), "a".repeat(20),);
        let err = ProtoError::new(
            ErrorCode::UnknownItem,
            ErrorDetail::Item(ItemId::new(id).unwrap()),
        );

        assert_eq!(
            err.message(),
            format!("{PREFIX}{}{MARKER}", "a".repeat(995))
        );
        assert!(err.message().ends_with(MARKER));
        assert!(err.message().len() <= crate::limits::MAX_ERROR_MESSAGE);
        assert!(std::str::from_utf8(err.message().as_bytes()).is_ok());
    }

    /// The asymmetry constraint #84 settles: [`ProtoError::new`] can only ever
    /// produce a message shaped by [`ErrorDetail`], but a client deserializing
    /// a frame it *received* has no way to check who built it, and must still
    /// accept any bounded string — the same split #83 draws between
    /// `QueryText::new` and `RoutedText`'s infallible constructor. This parses
    /// a message no call to [`ProtoError::new`] could ever produce (free text
    /// with punctuation `ProtoError::new`'s templates never emit) and asserts
    /// it survives the parse unchanged.
    #[test]
    fn a_client_deserializes_an_error_message_no_local_constructor_could_produce() {
        let json =
            r#"{"code":"internal","message":"free-form: whatever a daemon wrote, /any/path, 🔥"}"#;
        let err: ProtoError = serde_json::from_str(json).unwrap();
        assert_eq!(
            err.message(),
            "free-form: whatever a daemon wrote, /any/path, 🔥"
        );
    }

    #[test]
    fn unknown_fields_tolerated_for_forward_compat() {
        let json = r#"{"type":"hello","api_version":1,"future_field":true}"#;
        assert_eq!(
            serde_json::from_str::<ClientMsg>(json).unwrap(),
            ClientMsg::Hello { api_version: 1 }
        );
    }
}
