//! Client/daemon message frames exchanged over the (future) IPC transport.

use serde::{Deserialize, Serialize};

use crate::item::{ActionId, Item, ItemId};

/// Messages sent from a client to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        api_version: u32,
    },
    Query {
        id: u64,
        text: String,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMsg {
    HelloAck {
        api_version: u32,
    },
    Results {
        query_id: u64,
        partial: bool,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutcome {
    Done,
    CopyText(String),
    OpenUrl(String),
}

/// A protocol-level error reported by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtoError {
    pub code: ErrorCode,
    pub message: String,
}

/// The category of a protocol-level error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    VersionMismatch,
    UnknownItem,
    UnknownAction,
    ProviderFailed,
    Internal,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::item::*;

    fn sample_item() -> Item {
        Item {
            id: ItemId("app:firefox".into()),
            kind: Kind::App,
            title: "Firefox".into(),
            subtitle: Some("Web Browser".into()),
            icon: Some(IconSpec {
                name: Some("firefox".into()),
                path: None,
            }),
            actions: vec![Action {
                id: ActionId("open".into()),
                kind: ActionKind::Open,
                label: "Open".into(),
            }],
            default_action: ActionId("open".into()),
            copy_text: None,
            append_to_end: false,
            provider: "apps".into(),
        }
    }

    #[test]
    fn client_msg_round_trips() {
        let msg = ClientMsg::Query {
            id: 7,
            text: "fire".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"query""#));
        assert_eq!(serde_json::from_str::<ClientMsg>(&json).unwrap(), msg);
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
            item_id: ItemId("app:firefox".into()),
            action_id: ActionId("open".into()),
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
                r#""subtitle":"Web Browser","icon":{"name":"firefox","path":null},"#,
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
            outcome: ExecOutcome::CopyText("hello".into()),
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

        let copy = ExecOutcome::CopyText("hello".into());
        let json = serde_json::to_string(&copy).unwrap();
        assert_eq!(json, r#"{"copy_text":"hello"}"#);
        assert_eq!(serde_json::from_str::<ExecOutcome>(&json).unwrap(), copy);

        let open = ExecOutcome::OpenUrl("https://example.com".into());
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
