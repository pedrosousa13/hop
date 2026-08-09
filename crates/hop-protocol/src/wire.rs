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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtoError {
    pub code: ErrorCode,
    /// Bounded at [`MAX_ERROR_MESSAGE`](crate::limits::MAX_ERROR_MESSAGE) bytes
    /// on the way in — an error headed for a UI is not a payload channel.
    #[serde(deserialize_with = "limits::de_error_message")]
    pub message: String,
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
    use crate::content::IconName;
    use crate::item::*;

    fn sample_item() -> Item {
        Item {
            id: ItemId::new("app:firefox").unwrap(),
            kind: Kind::App,
            title: "Firefox".into(),
            subtitle: Some("Web Browser".into()),
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
    fn unknown_fields_tolerated_for_forward_compat() {
        let json = r#"{"type":"hello","api_version":1,"future_field":true}"#;
        assert_eq!(
            serde_json::from_str::<ClientMsg>(json).unwrap(),
            ClientMsg::Hello { api_version: 1 }
        );
    }
}
