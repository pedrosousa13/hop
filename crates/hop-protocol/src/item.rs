//! The item/action model: what a query result looks like and what can be done with it.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::limits::{self, BoundError, MAX_ACTION_ID, MAX_ITEM_ID, check_len};

/// The stable identifier of an [`Item`], opaque to clients.
///
/// The inner string is private and the only way in is [`ItemId::new`], which
/// refuses anything over [`MAX_ITEM_ID`] bytes. That is the whole point of the
/// newtype: an `ItemId` that exists is an `ItemId` within its bound, whether it
/// was built by a provider or parsed off the socket, and no later caller has to
/// remember to check. Deserialization runs a length pre-filter against that
/// same constant and then hands the string to that same constructor, whose
/// answer is the one that decides; the pre-filter can only ever reject what
/// `new` would reject too. So a rule added to `new` later governs ids off the
/// socket without being repeated at the parse, and a hostile peer cannot
/// produce an out-of-bound id by parsing.
///
/// The wire form is unchanged by any of this: an id is still a bare JSON
/// string, never an object or a wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ItemId(String);

impl<'de> Deserialize<'de> for ItemId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(deserializer, "ItemId", MAX_ITEM_ID, ItemId::new)
    }
}

impl ItemId {
    /// Builds an id, refusing a value over [`MAX_ITEM_ID`] bytes.
    ///
    /// # Errors
    ///
    /// [`BoundError::TooLong`] if the value is over the bound. It is refused
    /// rather than truncated: a shortened id is a *different* id, and would
    /// silently point at the wrong item.
    pub fn new(value: impl Into<String>) -> Result<Self, BoundError> {
        let value = value.into();
        check_len("ItemId", MAX_ITEM_ID, value.len())?;
        Ok(Self(value))
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the id, yielding the string inside.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The stable identifier of an [`Action`], opaque to clients.
///
/// Bounded and constructed exactly as [`ItemId`] is, at [`MAX_ACTION_ID`] bytes,
/// and deserialized through [`ActionId::new`] for the same reason.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ActionId(String);

impl<'de> Deserialize<'de> for ActionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(deserializer, "ActionId", MAX_ACTION_ID, ActionId::new)
    }
}

impl ActionId {
    /// Builds an id, refusing a value over [`MAX_ACTION_ID`] bytes.
    ///
    /// # Errors
    ///
    /// [`BoundError::TooLong`] if the value is over the bound.
    pub fn new(value: impl Into<String>) -> Result<Self, BoundError> {
        let value = value.into();
        check_len("ActionId", MAX_ACTION_ID, value.len())?;
        Ok(Self(value))
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the id, yielding the string inside.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The category of an [`Item`], used for display and ranking hints.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Bounded at [`MAX_ACTION_LABEL`](crate::limits::MAX_ACTION_LABEL) bytes on the way in.
    #[serde(deserialize_with = "limits::de_action_label")]
    pub label: String,
}

/// How to render an [`Item`]'s icon.
///
/// Both fields are optional by *absence* as well as by explicit null. That is
/// what the `default` in each attribute buys: serde's derive gives an
/// `Option<T>` field its missing-field fallback only while the field has no
/// `deserialize_with`, so adding one without `default` would quietly turn an
/// optional field into a mandatory one and refuse `{"name":"firefox"}` for a
/// missing `path`. Nothing in this crate omits a field when serializing, so no
/// round-trip test would notice; a future client written against the documented
/// shape would.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconSpec {
    /// Bounded at [`MAX_ICON_NAME`](crate::limits::MAX_ICON_NAME) bytes on the way in.
    #[serde(default, deserialize_with = "limits::de_icon_name")]
    pub name: Option<String>,
    /// Bounded at [`MAX_ICON_PATH`](crate::limits::MAX_ICON_PATH) bytes on the way in.
    #[serde(default, deserialize_with = "limits::de_icon_path")]
    pub path: Option<String>,
}

/// A single query result: something a user can act on.
///
/// Every variable-length field here is bounded at the deserialization boundary,
/// so an `Item` that was parsed is an `Item` within budget. The bounds are in
/// [`limits`], which also records what they do *not* guarantee.
///
/// The three `Option` fields are all optional by absence as well as by explicit
/// null, but they arrive there two different ways. `subtitle` and `copy_text`
/// each carry a `deserialize_with`, which suppresses the missing-field fallback
/// serde's derive would otherwise give an `Option`, so each pairs it with an
/// explicit `default` to put that fallback back — see [`IconSpec`] for why the
/// pairing is load-bearing. `icon` carries neither attribute, so it still has
/// that implicit fallback, which is precisely the mechanism the other two are
/// restoring. Of the three, `icon` is the one with no "Bounded at…" line,
/// because it holds no bytes of its own: its two strings are bounded on
/// [`IconSpec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub kind: Kind,
    /// Bounded at [`MAX_TITLE`](crate::limits::MAX_TITLE) bytes on the way in.
    #[serde(deserialize_with = "limits::de_title")]
    pub title: String,
    /// Bounded at [`MAX_SUBTITLE`](crate::limits::MAX_SUBTITLE) bytes on the way in.
    #[serde(default, deserialize_with = "limits::de_subtitle")]
    pub subtitle: Option<String>,
    pub icon: Option<IconSpec>,
    /// Bounded at [`MAX_ACTIONS_PER_ITEM`](crate::limits::MAX_ACTIONS_PER_ITEM)
    /// actions on the way in.
    #[serde(deserialize_with = "limits::de_item_actions")]
    pub actions: Vec<Action>,
    pub default_action: ActionId,
    /// Bounded at [`MAX_COPY_TEXT`](crate::limits::MAX_COPY_TEXT) bytes on the way in.
    #[serde(default, deserialize_with = "limits::de_item_copy_text")]
    pub copy_text: Option<String>,
    /// Pinned after ranked results (web search actions), rather than ranked among them.
    pub append_to_end: bool,
    /// Bounded at [`MAX_PROVIDER_ID`](crate::limits::MAX_PROVIDER_ID) bytes on the way in.
    #[serde(deserialize_with = "limits::de_provider")]
    pub provider: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn sample_item() -> Item {
        Item {
            id: ItemId::new("app:firefox").unwrap(),
            kind: Kind::App,
            title: "Firefox".into(),
            subtitle: Some("Web Browser".into()),
            icon: Some(IconSpec {
                name: Some("firefox".into()),
                path: None,
            }),
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
    fn item_id_encodes_as_bare_string() {
        let id = ItemId::new("app:firefox").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""app:firefox""#);
        assert_eq!(
            serde_json::from_str::<ItemId>(r#""app:firefox""#).unwrap(),
            id
        );
    }

    #[test]
    fn action_id_encodes_as_bare_string() {
        let id = ActionId::new("open").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""open""#);
        assert_eq!(serde_json::from_str::<ActionId>(r#""open""#).unwrap(), id);
    }

    #[test]
    fn item_id_rejects_over_long_value_at_construction() {
        let ok = "a".repeat(MAX_ITEM_ID);
        assert_eq!(ItemId::new(&ok).unwrap().as_str(), ok);

        let over = "a".repeat(MAX_ITEM_ID + 1);
        assert!(ItemId::new(&over).is_err());
    }

    #[test]
    fn action_id_rejects_over_long_value_at_construction() {
        let ok = "a".repeat(MAX_ACTION_ID);
        assert_eq!(ActionId::new(&ok).unwrap().as_str(), ok);

        let over = "a".repeat(MAX_ACTION_ID + 1);
        assert!(ActionId::new(&over).is_err());
    }

    /// An item as JSON with every field present, for a test to remove one from.
    fn full_item_json() -> serde_json::Value {
        serde_json::json!({
            "id": "app:firefox",
            "kind": "app",
            "title": "Firefox",
            "subtitle": "Web Browser",
            "icon": { "name": "firefox", "path": "/usr/share/icons/firefox.png" },
            "actions": [{ "id": "open", "kind": "open", "label": "Open" }],
            "default_action": "open",
            "copy_text": "https://example.com",
            "append_to_end": false,
            "provider": "apps"
        })
    }

    fn item_without(field: &str) -> Item {
        let mut json = full_item_json();
        let object = json.as_object_mut().unwrap();
        object
            .remove(field)
            .unwrap_or_else(|| panic!("no field {field} to remove"));
        serde_json::from_str(&json.to_string())
            .unwrap_or_else(|e| panic!("omitting {field} must still parse, got: {e}"))
    }

    fn icon_without(field: &str) -> IconSpec {
        let mut json = serde_json::json!({ "name": "firefox", "path": "/icons/firefox.png" });
        let object = json.as_object_mut().unwrap();
        object
            .remove(field)
            .unwrap_or_else(|| panic!("no field {field} to remove"));
        serde_json::from_str(&json.to_string())
            .unwrap_or_else(|e| panic!("omitting {field} must still parse, got: {e}"))
    }

    // An optional field is optional by *absence*, not only by explicit null. A
    // client that omits one is the motivating case: it parsed before these
    // bounds existed, and adding a `deserialize_with` must not have quietly
    // made it mandatory. hop's own serializer never omits a field, so no
    // round-trip test can catch this — these four have to say it directly.

    #[test]
    fn item_subtitle_is_none_when_omitted() {
        assert_eq!(item_without("subtitle").subtitle, None);
    }

    #[test]
    fn item_copy_text_is_none_when_omitted() {
        assert_eq!(item_without("copy_text").copy_text, None);
    }

    #[test]
    fn item_icon_is_none_when_omitted() {
        assert_eq!(item_without("icon").icon, None);
    }

    #[test]
    fn icon_name_is_none_when_omitted() {
        assert_eq!(icon_without("name").name, None);
    }

    #[test]
    fn icon_path_is_none_when_omitted() {
        assert_eq!(icon_without("path").path, None);
    }

    #[test]
    fn item_id_bound_is_counted_in_bytes_not_characters() {
        // "é" is two bytes in UTF-8, so a value of MAX_ITEM_ID / 2 characters is
        // exactly at the byte bound and one more character is over it.
        let at_bound = "é".repeat(MAX_ITEM_ID / 2);
        assert_eq!(at_bound.chars().count(), MAX_ITEM_ID / 2);
        assert!(ItemId::new(&at_bound).is_ok());
        assert!(ItemId::new(format!("{at_bound}é")).is_err());
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
        assert_eq!(item.id, ItemId::new("app:firefox").unwrap());
        assert_eq!(item.kind, Kind::App);
    }
}
