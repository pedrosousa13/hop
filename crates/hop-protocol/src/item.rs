//! The item/action model: what a query result looks like and what can be done with it.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::content::{CopyText, IconName, IconPath, ItemSubtitle, ItemTitle};
use crate::limits::{self, BoundError, MAX_ACTION_ID, MAX_COPY_TEXT, MAX_ITEM_ID, check_len};

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

/// How to render an [`Item`]'s icon: a theme name **or** a file, never both and
/// never neither.
///
/// # The wire form, and how it changed
///
/// An icon is a JSON object with exactly one key, naming which arm it is:
///
/// ```text
///   {"name": "firefox"}
///   {"path": "/usr/share/pixmaps/firefox.png"}
/// ```
///
/// This is an **externally tagged** enum, which is serde's default for one, and
/// the shape is doing the work rather than a validator — so an icon carrying
/// both a name and a path, and an icon carrying neither, are values no frame can
/// express. Pinned by the tests
/// `tests::an_icon_carrying_both_a_name_and_a_path_does_not_parse` and
/// `tests::an_icon_carrying_neither_a_name_nor_a_path_does_not_parse`. That is
/// also why there is no documented precedence between the two: there is no state
/// for a precedence rule to resolve.
///
/// Two layers produce that between them, and it is worth being exact about
/// which does what, because only one of them is this crate's derive. The derive
/// asks the deserializer for an enum and matches the single key it is handed to
/// a variant, so an unknown key is the derive's `unknown variant` error. But a
/// *second* key and *no* key are both `serde_json`'s: having deserialized the
/// variant it looks for the end of the map, and finding anything else there is
/// an error, as is a map that ended before a key could be read. Measured against
/// serde_json 1.0.151, both of those report `expected value` — the first where
/// the map should have ended, the second where a key should have been.
///
/// **This is a breaking change to the contract.** The previous form was an
/// object with two optional fields — `{"name": "firefox", "path": null}`, or
/// both set, or both null — and none of those parse now. A provider written
/// against the old shape must send one key and drop the other rather than
/// sending the other as null. An item with no icon is unaffected: that is still
/// said by leaving `icon` out or sending `null` for it, which is a property of
/// the `Option` on [`Item`] rather than of this type.
///
/// A third arm added later would be breaking in the same way, and in the
/// opposite direction from [`Item`] itself: an `Item` ignores a field it does
/// not know, for forward compatibility, while an icon arm this contract does not
/// have is refused. Pinned by
/// `tests::an_icon_naming_an_arm_this_contract_does_not_have_does_not_parse`.
///
/// # What each arm promises
///
/// Neither arm is a bare `String`, so the promise is the type rather than a note
/// a client has to read. [`IconName`] is bounded and carries a content rule that
/// keeps it from being a path in disguise. [`IconPath`] is bounded and must be
/// absolute, free of any `..` component, and free of NUL and every other control
/// character.
///
/// Two things an `IconSpec` does *not* promise about its path arm, because they
/// are not rules the constructor applies. The roots an icon is expected to live
/// under are **documented on [`IconPath`] and not enforced anywhere**, for
/// reasons that type prices under its own heading — so a path outside all of
/// them still parses. And the file is checked to be a regular file only by
/// [`IconPath::open_regular_file`], which a client calls when it is about to
/// read the icon; parsing an `IconSpec` makes no syscall and so says nothing
/// about what, or whether, the path names.
///
/// What is true of both arms is that their gate is their constructor, and
/// `Deserialize` routes through it: an `IconSpec` that exists carries a value
/// that passed every rule its type applies, whether a provider built it or it
/// arrived off the socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconSpec {
    /// A name to look up in the desktop's icon theme.
    Name(IconName),
    /// An absolute path to an icon file.
    Path(IconPath),
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
/// explicit `default` to put that fallback back. The pairing is load-bearing:
/// without the `default`, adding a `deserialize_with` would quietly turn an
/// optional field into a mandatory one, and nothing in this crate omits a field
/// when serializing, so no round-trip test would notice — a client written
/// against the documented shape would. `icon` carries neither attribute, so it
/// still has that implicit fallback, which is precisely the mechanism the other
/// two are restoring. Of the three, `icon` is the one with no "Bounded at…"
/// line, because it holds no bytes of its own: whichever arm of [`IconSpec`] it
/// carries is bounded by that arm's own type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub kind: Kind,
    /// A validated single-line display title, bounded at
    /// [`MAX_TITLE`](crate::limits::MAX_TITLE) bytes and free of control
    /// characters.
    pub title: ItemTitle,
    /// An optional validated single-line display subtitle, bounded at
    /// [`MAX_SUBTITLE`](crate::limits::MAX_SUBTITLE) bytes and free of control
    /// characters.
    #[serde(default)]
    pub subtitle: Option<ItemSubtitle>,
    pub icon: Option<IconSpec>,
    /// Bounded at [`MAX_ACTIONS_PER_ITEM`](crate::limits::MAX_ACTIONS_PER_ITEM)
    /// actions on the way in.
    #[serde(deserialize_with = "limits::de_item_actions")]
    pub actions: Vec<Action>,
    pub default_action: ActionId,
    /// Text a client may put on the clipboard for this item, carrying the
    /// same rules as [`CopyText`]: bounded at
    /// [`MAX_COPY_TEXT`](crate::limits::MAX_COPY_TEXT) bytes and free of every
    /// control character outside [`ALLOWED_COPY_TEXT_CONTROLS`](crate::content::ALLOWED_COPY_TEXT_CONTROLS),
    /// checked in that order. This is the same clipboard
    /// [`ExecOutcome::CopyText`](crate::wire::ExecOutcome::CopyText) reaches,
    /// by a different route — see [`crate::content`]'s module docs for why
    /// the two are gated identically but a refusal here names
    /// `Item.copy_text` rather than [`CopyText::FIELD`].
    #[serde(default, deserialize_with = "de_item_copy_text")]
    pub copy_text: Option<CopyText>,
    /// Asks for this item to be pinned after the ranked results, rather than
    /// ranked among them. The pinned web-search row is the motivating case.
    ///
    /// It is a request, and what it requests is an exception to every ordering
    /// rule the assembler has. `hop-core`'s pipeline splits pinned items out
    /// before it filters an exclusive query (`w firefox`) down to that mode's
    /// kinds and before it scores anything, so a pinned item is placed without
    /// having matched what was typed, without clearing the minimum-score
    /// floor, and whatever its own kind. That is deliberate: the web-search
    /// row has to show for a query nothing about it matches. Nothing after the
    /// split can drop such an item on relevance.
    ///
    /// So the exception is limited by *count* instead, and the limit is not
    /// here. What limits a frame in this crate is
    /// [`MAX_ITEMS_PER_RESULTS_FRAME`](crate::limits::MAX_ITEMS_PER_RESULTS_FRAME),
    /// which caps items in one frame without regard to this flag, and no
    /// per-query limit is parseable at all: a query's items arrive across
    /// several frames from several providers, and no single parse sees enough
    /// of one query to count its pinned items. The limit therefore lives where
    /// a whole query is in hand — as the **pin budget** in `hop-core`'s
    /// `pipeline` module, `MAX_PINNED_ITEMS_PER_PROVIDER` pinned items from
    /// any one producer and `MAX_PINNED_ITEMS_PER_QUERY` in all, honored in
    /// **provider-supplied order** — the order the outputs reached assembly,
    /// each provider's items in the order it returned them — with the rest
    /// returned as rejections. A provider that flags everything it returns
    /// gets its one row, not the flood: being verbose wins it nothing, and
    /// cannot cost another provider the row it asked for. Being *early* still
    /// can, though, and the difference is worth keeping straight — the
    /// per-producer share stops a provider taking a second row, while the
    /// per-query total is shared and spent in provider-supplied order, so a
    /// provider early in that order spends a slot a later one might have had.
    ///
    /// What the pin budget does not do is decide *who* may pin: the flag is a
    /// field on this type, so anything that can answer a query can set it, and
    /// the budget counts a first-party provider's rows and a hostile one's
    /// alike. A capability check would be the answer to that, and
    /// `MAX_PINNED_ITEMS_PER_QUERY`'s docs are where it is stated to belong.
    pub append_to_end: bool,
    /// Bounded at [`MAX_PROVIDER_ID`](crate::limits::MAX_PROVIDER_ID) bytes on the way in.
    #[serde(deserialize_with = "limits::de_provider")]
    pub provider: String,
}

/// The `deserialize_with` for [`Item::copy_text`].
///
/// Not [`CopyText`]'s own `Deserialize` — that would name a refusal
/// [`CopyText::FIELD`], `"ExecOutcome::CopyText"`, which is the wire field a
/// value travels in when it arrives through an outcome, and this field is a
/// different one. This calls [`limits::validated_opt`] with
/// [`CopyText::new_named`] instead, the same rules as [`CopyText::new`]
/// behind a `field` argument, so the length pre-filter and the content check
/// both name `Item.copy_text` — see [`crate::content`]'s module docs for the
/// full reasoning, and issue #82 for what happens when a refusal names a
/// field the value did not travel in.
fn de_item_copy_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<CopyText>, D::Error> {
    limits::validated_opt(d, "Item.copy_text", MAX_COPY_TEXT, |v| {
        CopyText::new_named("Item.copy_text", v)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use crate::limits::{MAX_SUBTITLE, MAX_TITLE};

    use super::*;

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
            "icon": { "name": "firefox" },
            "actions": [{ "id": "open", "kind": "open", "label": "Open" }],
            "default_action": "open",
            "copy_text": "https://example.com",
            "append_to_end": false,
            "provider": "apps"
        })
    }

    #[test]
    fn item_title_carrying_a_control_character_is_refused() {
        let mut json = full_item_json();
        json["title"] = json!("before\nafter");
        let err =
            serde_json::from_value::<Item>(json).expect_err("a multi-line title must not parse");
        assert!(err.to_string().contains("Item.title"), "got: {err}");
        assert!(err.to_string().contains("U+000A"), "got: {err}");
    }

    #[test]
    fn item_subtitle_carrying_a_control_character_is_refused() {
        let mut json = full_item_json();
        json["subtitle"] = json!(format!("before{}after", '\u{1b}'));
        let err = serde_json::from_value::<Item>(json)
            .expect_err("a subtitle carrying ESC must not parse");
        assert!(err.to_string().contains("Item.subtitle"), "got: {err}");
        assert!(err.to_string().contains("U+001B"), "got: {err}");
    }

    #[test]
    fn item_title_rejects_c1_controls() {
        let mut json = full_item_json();
        json["title"] = json!(format!("before{}after", '\u{85}'));
        let err = serde_json::from_value::<Item>(json).expect_err("U+0085 must not parse");
        assert!(err.to_string().contains("Item.title"), "got: {err}");
        assert!(err.to_string().contains("U+0085"), "got: {err}");
    }

    #[test]
    fn item_display_fields_accept_exact_byte_bounds_and_reject_one_byte_over() {
        let mut title_at_bound = full_item_json();
        title_at_bound["title"] = json!("t".repeat(MAX_TITLE));
        assert!(serde_json::from_value::<Item>(title_at_bound).is_ok());

        let mut title_over_bound = full_item_json();
        title_over_bound["title"] = json!("t".repeat(MAX_TITLE + 1));
        assert!(serde_json::from_value::<Item>(title_over_bound).is_err());

        let mut subtitle_at_bound = full_item_json();
        subtitle_at_bound["subtitle"] = json!("s".repeat(MAX_SUBTITLE));
        assert!(serde_json::from_value::<Item>(subtitle_at_bound).is_ok());

        let mut subtitle_over_bound = full_item_json();
        subtitle_over_bound["subtitle"] = json!("s".repeat(MAX_SUBTITLE + 1));
        assert!(serde_json::from_value::<Item>(subtitle_over_bound).is_err());
    }

    #[test]
    fn item_display_field_length_error_precedes_control_error() {
        let mut json = full_item_json();
        json["title"] = json!(format!("{}\u{1b}", "t".repeat(MAX_TITLE)));
        let err = serde_json::from_value::<Item>(json).expect_err("invalid title must be refused");
        let text = err.to_string();
        assert!(text.contains("Item.title"), "got: {text}");
        assert!(text.contains("over its maximum"), "got: {text}");
        assert!(
            !text.contains("U+001B"),
            "length must be reported first, got: {text}"
        );
    }

    #[test]
    fn item_display_fields_preserve_ordinary_and_empty_strings() {
        let mut json = full_item_json();
        json["title"] = json!("");
        json["subtitle"] = json!("普通");
        let item: Item = serde_json::from_value(json).expect("ordinary values must parse");
        assert_eq!(item.title.as_str(), "");
        assert_eq!(item.subtitle.as_ref().unwrap().as_str(), "普通");
    }

    #[test]
    fn item_title_serializes_as_a_bare_string() {
        let item = sample_item();
        let json = serde_json::to_value(item).unwrap();
        assert_eq!(json["title"], "Firefox");
        assert!(json["title"].is_string());
    }

    #[test]
    fn item_subtitle_is_none_when_explicitly_null() {
        let mut json = full_item_json();
        json["subtitle"] = serde_json::Value::Null;
        let item: Item = serde_json::from_value(json).expect("null subtitle must parse");
        assert_eq!(item.subtitle, None);
    }

    #[test]
    fn item_display_field_wrong_types_name_the_correct_field() {
        let mut title = full_item_json();
        title["title"] = json!(42);
        let err = serde_json::from_value::<Item>(title).expect_err("numeric title must fail");
        assert!(err.to_string().contains("Item.title"), "got: {err}");

        let mut subtitle = full_item_json();
        subtitle["subtitle"] = json!(42);
        let err = serde_json::from_value::<Item>(subtitle).expect_err("numeric subtitle must fail");
        assert!(err.to_string().contains("Item.subtitle"), "got: {err}");
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

    // An optional field is optional by *absence*, not only by explicit null. A
    // client that omits one is the motivating case: it parsed before these
    // bounds existed, and adding a `deserialize_with` must not have quietly
    // made it mandatory. hop's own serializer never omits a field, so no
    // round-trip test can catch this — these three have to say it directly.
    //
    // `IconSpec` has no such field any more: it is an enum whose one key is
    // mandatory by construction, which is what
    // `an_icon_carrying_neither_a_name_nor_a_path_does_not_parse` asserts from
    // the other side.

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

    /// Every test this file's docs name by hand must exist, so that renaming one
    /// fails here instead of leaving a doc pointing at nothing. The same check
    /// `crate::content` and `crate::redaction` run over their own docs, and for
    /// the same reason: a pointer to a `#[cfg(test)]` item cannot be an intra-doc
    /// link, because rustdoc has no `tests` module to resolve it against.
    ///
    /// A pointer is a backticked `tests::` followed by an identifier.
    #[test]
    fn every_test_this_file_names_in_its_docs_exists() {
        let source = include_str!("item.rs");
        let named: Vec<&str> = source
            .lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("///") || line.starts_with("//!"))
            // Odd-indexed pieces are what sat between a pair of backticks.
            .flat_map(|line| line.split('`').skip(1).step_by(2))
            .filter_map(|token| token.strip_prefix("tests::"))
            .filter(|name| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            })
            .collect();

        assert!(
            named.len() >= 3,
            "the docs name at least three tests by hand; finding {} means this \
             scan stopped matching rather than the docs stopping pointing",
            named.len()
        );

        for name in named {
            assert!(
                source.contains(&format!("fn {name}(")),
                "a doc comment names `tests::{name}`, which no test in this file defines"
            );
        }
    }

    // --- IconSpec: a name or a path, never both and never neither -----------

    #[test]
    fn an_icon_travels_as_the_single_key_naming_which_arm_it_is() {
        let name = IconSpec::Name(IconName::new("firefox").unwrap());
        assert_eq!(
            serde_json::to_string(&name).unwrap(),
            r#"{"name":"firefox"}"#
        );
        assert_eq!(
            serde_json::from_str::<IconSpec>(r#"{"name":"firefox"}"#).unwrap(),
            name
        );

        let path = IconSpec::Path(IconPath::new("/usr/share/pixmaps/firefox.png").unwrap());
        assert_eq!(
            serde_json::to_string(&path).unwrap(),
            r#"{"path":"/usr/share/pixmaps/firefox.png"}"#
        );
        assert_eq!(
            serde_json::from_str::<IconSpec>(r#"{"path":"/usr/share/pixmaps/firefox.png"}"#)
                .unwrap(),
            path
        );
    }

    #[test]
    fn an_icon_carrying_both_a_name_and_a_path_does_not_parse() {
        // The wire form is what makes the ambiguity unrepresentable rather than
        // undocumented: there is no precedence to state because there is no
        // frame that can ask for both.
        let both = r#"{"name":"firefox","path":"/usr/share/pixmaps/firefox.png"}"#;
        assert!(serde_json::from_str::<IconSpec>(both).is_err());
    }

    #[test]
    fn an_icon_carrying_neither_a_name_nor_a_path_does_not_parse() {
        // An item with no icon says so by leaving `icon` out or sending null,
        // which `item_icon_is_none_when_omitted` covers. An icon object with
        // nothing in it is a third state the type no longer has.
        assert!(serde_json::from_str::<IconSpec>("{}").is_err());
    }

    #[test]
    fn an_icon_whose_one_key_is_null_does_not_parse() {
        // A different case from the one above, and stopped by a different
        // thing. `{"name":null}` *is* a one-key map, so the enum's shape is
        // satisfied and the arm is chosen; what refuses it is `IconName`'s own
        // `Deserialize`, which wants a string. Worth its own test because the
        // old two-optional-field form accepted this exact document.
        let err = serde_json::from_str::<IconSpec>(r#"{"name":null}"#)
            .expect_err("an arm carrying null is not an arm carrying a value");
        assert!(
            err.to_string().contains("invalid type: null"),
            "the arm's own value type must be what objects, got: {err}"
        );
    }

    #[test]
    fn an_icon_naming_an_arm_this_contract_does_not_have_does_not_parse() {
        // Worth pinning beside `item_tolerates_unknown_fields_for_forward_compat`,
        // because it is the opposite answer: an `Item` ignores a field it does
        // not know, but an icon arm it does not know is refused, and so a later
        // third arm is a breaking change for an older client rather than
        // something it can skip.
        assert!(serde_json::from_str::<IconSpec>(r#"{"emoji":"🦊"}"#).is_err());
    }

    #[test]
    fn an_icon_refused_by_its_own_rules_cannot_be_produced_by_parsing() {
        // The rules are `IconName`'s and `IconPath`'s, and `crate::content`
        // tests each one against its constructor. What is asserted here is that
        // an item is the same gate: a refused value sinks the whole item rather
        // than arriving inside one.
        for icon in [
            json!({ "path": "icons/firefox.png" }),
            json!({ "path": "/usr/share/icons/../../../etc/shadow" }),
            json!({ "name": "hicolor/firefox" }),
            json!({ "name": "" }),
        ] {
            let mut item = full_item_json();
            item["icon"] = icon.clone();
            assert!(
                serde_json::from_str::<Item>(&item.to_string()).is_err(),
                "an item must not be able to carry a refused icon, accepted {icon}"
            );
        }
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
            copy_text: Some(CopyText::new("https://example.com").unwrap()),
            append_to_end: true,
            ..sample_item()
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""copy_text":"https://example.com""#));
        assert!(json.contains(r#""append_to_end":true"#));
        assert_eq!(serde_json::from_str::<Item>(&json).unwrap(), item);
    }

    // --- Item.copy_text: CopyText's rules, at Item's own field name --------

    #[test]
    fn item_copy_text_is_none_when_explicit_null() {
        let mut json = full_item_json();
        json["copy_text"] = serde_json::Value::Null;
        let item: Item = serde_json::from_str(&json.to_string()).unwrap();
        assert_eq!(item.copy_text, None);
    }

    #[test]
    fn item_copy_text_carrying_esc_is_refused() {
        let mut json = full_item_json();
        json["copy_text"] = json!(format!("before{}after", '\u{1B}'));
        let err = serde_json::from_str::<Item>(&json.to_string())
            .expect_err("an item copy_text carrying ESC must not parse");
        assert!(
            err.to_string().contains("U+001B"),
            "the refusal must name the offending code point, got: {err}"
        );
    }

    #[test]
    fn item_copy_text_carrying_tab_and_newline_is_accepted() {
        let mut json = full_item_json();
        json["copy_text"] = json!("line one\tindented\nline two");
        let item: Item = serde_json::from_str(&json.to_string())
            .expect("tab and newline are the allowed exceptions");
        assert_eq!(
            item.copy_text.unwrap().as_str(),
            "line one\tindented\nline two"
        );
    }

    #[test]
    fn item_copy_text_carrying_a_carriage_return_is_refused() {
        // CopyText deliberately refuses CR even though it is common in
        // Windows-origin and CRLF text — see CopyText's own doc comment,
        // "What refusing a carriage return costs", for what that costs and
        // why it is paid anyway.
        let mut json = full_item_json();
        json["copy_text"] = json!("line one\rline two");
        assert!(
            serde_json::from_str::<Item>(&json.to_string()).is_err(),
            "a carriage return must be refused in an item's copy_text, same as in CopyText's own rules"
        );
    }

    #[test]
    fn item_copy_text_refusal_names_the_item_field_not_the_outcome_field() {
        // The whole point of routing an item's copy_text through
        // `CopyText::new_named` rather than `CopyText`'s own `Deserialize`:
        // the value never travelled in `ExecOutcome::CopyText`'s wire field,
        // so a refusal must not claim that it did.
        let mut json = full_item_json();
        json["copy_text"] = json!(format!("{}", '\u{1B}'));
        let err = serde_json::from_str::<Item>(&json.to_string()).expect_err("ESC must be refused");
        let text = err.to_string();
        assert!(text.contains("Item.copy_text"), "got: {text}");
        assert!(
            !text.contains(CopyText::FIELD),
            "the refusal must not name the outcome's field {}, got: {text}",
            CopyText::FIELD
        );
    }

    #[test]
    fn item_copy_text_wrong_type_names_its_field() {
        // A number where a string is wanted is refused before
        // `CopyText::new_named` ever runs — by serde's own `invalid_type`,
        // formatted from `ValidatedOpt::expecting` or `Validated::expecting`
        // depending on which one actually judges it (see the doc comment on
        // `limits::ValidatedOpt` for why it is `Validated`'s `expecting` that
        // fires here, not `ValidatedOpt`'s own). This pins the observable
        // behavior, not which visitor produces it: either way, the refusal
        // must name `Item.copy_text`.
        let mut json = full_item_json();
        json["copy_text"] = json!(42);
        let err =
            serde_json::from_str::<Item>(&json.to_string()).expect_err("a number is not a string");
        assert!(err.to_string().contains("Item.copy_text"), "got: {err}");
    }

    #[test]
    fn item_copy_text_over_long_and_control_bearing_is_reported_as_over_long() {
        // Length is checked before content, so a value breaking both rules is
        // reported as over-long rather than as a forbidden character — the
        // same ordering CopyText's own doc comment documents and tests.
        let value = format!("{}{}", "a".repeat(MAX_COPY_TEXT), '\u{1B}');
        let mut json = full_item_json();
        json["copy_text"] = json!(value);
        let err = serde_json::from_str::<Item>(&json.to_string())
            .expect_err("a value breaking both rules must still be refused");
        assert!(
            err.to_string().contains("over its maximum of"),
            "got: {err}"
        );
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
