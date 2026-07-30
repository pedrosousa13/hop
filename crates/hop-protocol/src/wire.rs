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
    fn daemon_results_round_trips() {
        let msg = DaemonMsg::Results {
            query_id: 7,
            partial: true,
            items: vec![sample_item()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<DaemonMsg>(&json).unwrap(), msg);
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
