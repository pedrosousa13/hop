//! The item/action model: what a query result looks like and what can be done with it.

use serde::{Deserialize, Serialize};

/// The stable identifier of an [`Item`], opaque to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

/// The stable identifier of an [`Action`], opaque to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(pub String);

/// The category of an [`Item`], used for display and ranking hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    App,
    Window,
    File,
    Calculator,
    Currency,
    Timezone,
    Weather,
    Emoji,
    WebSearch,
    Action,
}

/// What kind of effect an [`Action`] has when executed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Open,
    Focus,
    Copy,
    Run,
    CloseWindow,
    MoveToWorkspace,
    OpenUrl,
}

/// A single thing that can be done with an [`Item`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: ActionId,
    pub kind: ActionKind,
    pub label: String,
}

/// How to render an [`Item`]'s icon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconSpec {
    pub name: Option<String>,
    pub path: Option<String>,
}

/// A single query result: something a user can act on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub kind: Kind,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<IconSpec>,
    pub actions: Vec<Action>,
    pub default_action: ActionId,
    pub copy_text: Option<String>,
    /// Pinned after ranked results (web search actions), rather than ranked among them.
    pub append_to_end: bool,
    pub provider: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

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
    fn item_id_encodes_as_bare_string() {
        let id = ItemId("app:firefox".into());
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""app:firefox""#);
        assert_eq!(
            serde_json::from_str::<ItemId>(r#""app:firefox""#).unwrap(),
            id
        );
    }

    #[test]
    fn action_id_encodes_as_bare_string() {
        let id = ActionId("open".into());
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""open""#);
        assert_eq!(serde_json::from_str::<ActionId>(r#""open""#).unwrap(), id);
    }

    #[test]
    fn kind_snake_case_multi_word_variant() {
        assert_eq!(
            serde_json::to_string(&Kind::WebSearch).unwrap(),
            r#""web_search""#
        );
        assert_eq!(
            serde_json::from_str::<Kind>(r#""web_search""#).unwrap(),
            Kind::WebSearch
        );
    }

    #[test]
    fn action_kind_snake_case_multi_word_variants() {
        assert_eq!(
            serde_json::to_string(&ActionKind::CloseWindow).unwrap(),
            r#""close_window""#
        );
        assert_eq!(
            serde_json::from_str::<ActionKind>(r#""close_window""#).unwrap(),
            ActionKind::CloseWindow
        );

        assert_eq!(
            serde_json::to_string(&ActionKind::MoveToWorkspace).unwrap(),
            r#""move_to_workspace""#
        );
        assert_eq!(
            serde_json::from_str::<ActionKind>(r#""move_to_workspace""#).unwrap(),
            ActionKind::MoveToWorkspace
        );

        assert_eq!(
            serde_json::to_string(&ActionKind::OpenUrl).unwrap(),
            r#""open_url""#
        );
        assert_eq!(
            serde_json::from_str::<ActionKind>(r#""open_url""#).unwrap(),
            ActionKind::OpenUrl
        );
    }

    #[test]
    fn item_round_trips_with_none_fields() {
        let item = Item {
            subtitle: None,
            icon: None,
            copy_text: None,
            ..sample_item()
        };
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(
            json,
            concat!(
                r#"{"id":"app:firefox","kind":"app","title":"Firefox","#,
                r#""subtitle":null,"icon":null,"#,
                r#""actions":[{"id":"open","kind":"open","label":"Open"}],"#,
                r#""default_action":"open","copy_text":null,"append_to_end":false,"provider":"apps"}"#
            )
        );
        assert_eq!(serde_json::from_str::<Item>(&json).unwrap(), item);
    }

    #[test]
    fn item_round_trips_with_copy_text_and_append_to_end() {
        let item = Item {
            copy_text: Some("https://example.com".into()),
            append_to_end: true,
            ..sample_item()
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""copy_text":"https://example.com""#));
        assert!(json.contains(r#""append_to_end":true"#));
        assert_eq!(serde_json::from_str::<Item>(&json).unwrap(), item);
    }

    #[test]
    fn item_tolerates_unknown_fields_for_forward_compat() {
        let json = r#"{
            "id": "app:firefox",
            "kind": "app",
            "title": "Firefox",
            "subtitle": null,
            "icon": null,
            "actions": [],
            "default_action": "open",
            "copy_text": null,
            "append_to_end": false,
            "provider": "apps",
            "future_field": "ignored"
        }"#;
        let item: Item = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, ItemId("app:firefox".into()));
        assert_eq!(item.kind, Kind::App);
    }
}
