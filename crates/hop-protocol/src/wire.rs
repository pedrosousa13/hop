//! Client/daemon message frames exchanged over the (future) IPC transport.

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
    Hello {
        api_version: u32,
    },
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
    Cancel {
        id: u64,
    },
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
    HelloAck {
        api_version: u32,
    },
    Results {
        query_id: u64,
        partial: bool,
        /// Bounded at
        /// [`MAX_ITEMS_PER_RESULTS_FRAME`](crate::limits::MAX_ITEMS_PER_RESULTS_FRAME)
        /// items on the way in. This bounds one frame, not one query: a daemon
        /// may send several partial `results` frames for the same `query_id`.
        #[serde(deserialize_with = "limits::de_results_items")]
        items: Vec<Item>,
    },
    QueryDone {
        query_id: u64,
    },
    Executed {
        query_id: u64,
        outcome: ExecOutcome,
    },
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
