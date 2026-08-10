//! Exact-match aliases: a user types a short token and gets the thing they
//! meant, either by rewriting the query text the ranker sees, or by boosting
//! a specific target item.
//!
//! Ported from the previous GNOME extension's `lib/aliases.js`
//! (`parseAliasRecord` / `parseAliasesConfig` / `buildAliasContext`).
//! Matching is **exact only** — no prefix matching, no fuzzy matching. An
//! alias must never surprise a user mid-word; that is a deliberate design
//! decision, not an omission.
//!
//! ## Deliberate divergence from the JS
//!
//! The JS `parseAliasesConfig` swallows a JSON parse error and returns an
//! empty list, silently. [`Aliases::from_json`] does not: invalid JSON is a
//! configuration mistake worth surfacing, so it returns [`AliasError`]
//! instead. Everything else about tolerant parsing is preserved — a single
//! malformed *entry* inside an otherwise-valid array is skipped, not fatal.
//!
//! There is one more fatal case the JS had no equivalent of, because it had no
//! bounded id type: an `app` record whose synthesized item id breaks
//! [`hop_protocol::MAX_ITEM_ID`] (issue #22). That record is well-formed, so
//! skipping it would leave an alias that looks configured and never fires;
//! [`Aliases::from_json`] refuses the config instead.
//!
//! A `window` record's `app_id` and `title_contains` are fatal on their own
//! bounds for the same reason, even though neither is resolved into anything
//! yet (issue #76): `app_id` is the exact string a future window [`ItemId`]
//! would be synthesized from, bounded at [`hop_protocol::MAX_ITEM_ID`]; and
//! `title_contains` is matched against a window title, so a needle longer
//! than [`hop_protocol::MAX_TITLE`] — the longest representable haystack —
//! can never match anything real. Both are refused at load rather than
//! skipped, by [`AliasError::WindowFieldTooLong`]; "Why an over-long id is not
//! treated like a typo", below, is the argument for that and is not repeated
//! here.
//!
//! ## Why an over-long id is not treated like a typo
//!
//! Those two paragraphs pull in opposite directions, and the tension is real:
//! one entry sinking every other alias is exactly what skipping malformed
//! entries exists to prevent. The difference is what the entry *says*. A typo
//! is ambiguous — `"typ": "app"` may be a misspelled `type` or a key a later
//! version will define — and the only honest reading of it is that nothing was
//! asked for, so skipping it loses nothing.
//!
//! An over-long app id is not ambiguous. It names exactly which app to boost,
//! and that boost cannot be built, because [`ItemId::new`] is fallible and
//! [`Aliases::apply`] runs on every keystroke with no `Result` to report a
//! failure through. So the choice is not "tolerate or refuse" but "fail once,
//! loudly, at load" against "produce no boost for that alias, silently,
//! forever" — an alias that quietly stopped working is the harder bug of the
//! two, and the only one the user cannot act on. Load-time rejection is the
//! deliberate answer; it is not tolerance being forgotten here.
//!
//! ## Window aliases — the one thing `apply` cannot do
//!
//! See the doc comment on [`Aliases::apply`].

use std::collections::HashMap;

use hop_protocol::{BoundError, ItemId, MAX_ITEM_ID, MAX_TITLE, check_len};
use serde_json::Value;

use crate::provider::APPS_PROVIDER_ID;

/// The boost [`Aliases::apply`] contributes for each matching `AppBoost`
/// record. Must sit strictly above [`crate::learning::LEARNING_BOOST_CAP`]
/// (85.0) — an explicit alias is a direct user instruction and must always
/// beat learned behavior. See
/// [`tests::alias_boost_constant_beats_learning_cap`].
pub const ALIAS_BOOST: f32 = 180.0;

/// What an alias record resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasTarget {
    /// Replace the ranking term with this query text.
    Rewrite(String),
    /// Boost the app item `app:<appId>`.
    ///
    /// Carries the synthesized [`ItemId`] rather than the raw app id, because
    /// [`ItemId::new`] is fallible ([`hop_protocol::MAX_ITEM_ID`], issue #22)
    /// and [`Aliases::apply`] — which builds this boost on every keystroke —
    /// returns no `Result`. Building the id here, at parse time, is what lets
    /// `apply` stay infallible: an `AppBoost` that exists names an id that was
    /// already accepted, so the only two bad outcomes available at query time,
    /// a panic or a boost that silently never fires, are both off the table.
    AppBoost(ItemId),
    /// Boost windows matching an app id and/or a substring of their title.
    /// Faithfully parsed and stored, but never resolved by
    /// [`Aliases::apply`] — see that method's doc comment.
    ///
    /// Both fields are checked at load, in [`Aliases::from_json`] (issue
    /// #76).
    ///
    /// `title_contains` is checked against [`hop_protocol::MAX_TITLE`]
    /// because it is matched against an item's `title`, itself a wire value
    /// already bounded at that ceiling — a needle longer than the longest
    /// representable title can never match anything, so rejecting it now
    /// loses nothing a future resolver could have used. This check is
    /// complete on its own terms; nothing about resolving window aliases
    /// later changes it.
    ///
    /// `app_id` is checked against [`hop_protocol::MAX_ITEM_ID`], but this is
    /// a **weaker** check than [`AliasTarget::AppBoost`]'s, not a repeat of
    /// it. `AppBoost` bounds the *synthesized* string
    /// `ItemId::new(format!("app:{app_id}"))` actually builds, so a fallible
    /// construction has already happened and succeeded by the time that value
    /// exists. Nothing here can do that: no resolver builds a window
    /// [`ItemId`] yet, so there is no synthesized string to construct and
    /// check — only the raw `app_id` a future resolver would start from.
    /// Bounding the raw string at [`hop_protocol::MAX_ITEM_ID`] guarantees it
    /// is not already hopeless: an id already over the ceiling could never
    /// have fit into *any* synthesized id, whatever a future resolver builds
    /// on top of it. It does **not** guarantee that whatever gets
    /// synthesized later will fit — an `app_id` near the top of the range
    /// passes this check and can still overflow once a resolver builds
    /// something longer from it, exactly as `AppBoost` shows is possible for
    /// its own four-byte `"app:"` prefix. Whoever closes the resolution gap
    /// still has to synthesize an [`ItemId`] from `app_id` and from whatever
    /// identifies the matched window, and still has to decide where *that*
    /// construction is checked — at load, in [`Aliases::from_json`], or
    /// `apply` stops being infallible. This check narrows that future work;
    /// it does not finish it, and does not pretend to.
    ///
    /// Neither check is a **Bound** in the sense `CONTEXT.md`'s glossary
    /// reserves for that word — a maximum on a *wire* value, declared once in
    /// `hop-protocol`'s `limits` for both peers. These are locally-parsed
    /// config-file strings, and `title_contains` in particular never crosses
    /// the wire at all (see the glossary's **Pin budget** entry for the same
    /// acknowledgment made about a different value that isn't one either).
    /// [`hop_protocol::MAX_ITEM_ID`] and [`hop_protocol::MAX_TITLE`] are still
    /// the right ceilings to reuse, for the reasons given above — each field
    /// either already is, or is headed for, a value the wire does bound — but
    /// reusing a wire ceiling for a config-file string is a deliberate
    /// stretch of the term, not an instance of it.
    ///
    /// An over-long value on either field is fatal for the whole config —
    /// [`AliasError::WindowFieldTooLong`] — not skipped like a malformed
    /// entry; see this crate's module docs, "Why an over-long id is not
    /// treated like a typo", for the argument, which applies here unchanged.
    WindowBoost {
        app_id: Option<String>,
        title_contains: Option<String>,
    },
}

/// A tolerant, exact-match alias table: alias string -> every target
/// registered under it. Kept as a `Vec` per key (not a single target)
/// because the JS filters the whole list by key and applies *every* match —
/// two records sharing a key are both meant to fire, not to shadow one
/// another.
#[derive(Debug, Default, Clone)]
pub struct Aliases {
    by_key: HashMap<String, Vec<AliasTarget>>,
}

/// Why an alias config could not be loaded at all.
///
/// Every variant is fatal for the whole config, which is what separates them
/// from the malformed *entries* [`Aliases::from_json`] skips one by one: a
/// skipped entry is one the JS skipped too, whereas each of these is a config
/// the user meant and that cannot be honoured.
///
/// `#[non_exhaustive]` because the list is still not finished: resolving a
/// window alias against a live candidate list — the gap
/// [`AliasTarget::WindowBoost`]'s doc comment describes — will still need to
/// synthesize and check an `ItemId` for the matched window, which is a new
/// fallible step this enum has no variant for yet, bounding `app_id` and
/// `title_contains` themselves (issue #76) notwithstanding. There are no
/// consumers outside this crate yet, so paying for that now costs nothing and
/// makes the next variant a non-breaking addition.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AliasError {
    /// The config was not valid JSON at all. Carries the underlying parse
    /// message.
    #[error("aliases config is not valid JSON: {0}")]
    InvalidJson(String),
    /// An `app` alias names an app id whose synthesized item id
    /// (`app:<appId>`) breaks [`hop_protocol::MAX_ITEM_ID`].
    ///
    /// Names the alias **as the user wrote it** — not the normalized lookup
    /// key — because the app id is by definition thousands of bytes long, so
    /// the alias is what the user has to search their config for, and
    /// `"Slack"` is not findable by searching for `"slack"`.
    ///
    /// The bound itself is carried as a `#[source]` rather than interpolated
    /// into the message: reporters that walk the chain (`{:#}` under `anyhow`,
    /// say) would otherwise print it twice.
    #[error("alias {alias:?} names an app whose item id is over the id bound")]
    AppItemIdTooLong {
        /// The offending record's alias, exactly as it appears in the config.
        alias: String,
        /// The bound that was broken.
        #[source]
        source: BoundError,
    },
    /// A `window` alias names an `app_id` or `title_contains` value over its
    /// own load-time bound: [`hop_protocol::MAX_ITEM_ID`] for `app_id`,
    /// [`hop_protocol::MAX_TITLE`] for `title_contains`. See
    /// [`AliasTarget::WindowBoost`]'s doc comment for why each field carries
    /// the bound it does (issue #76).
    ///
    /// One variant covers both fields rather than two, because the two
    /// failures have the same shape and differ only in which field and which
    /// constant applied — a distinction [`BoundError::TooLong`]'s own `field`
    /// already carries (`"AliasTarget::WindowBoost.app_id"` or
    /// `"AliasTarget::WindowBoost.title_contains"`). A second variant would
    /// duplicate that distinction in this enum instead of reading it off the
    /// source error that already names it.
    ///
    /// Names the alias **as the user wrote it**, and carries the bound as a
    /// `#[source]` rather than interpolated into the message, for the same
    /// reasons [`AliasError::AppItemIdTooLong`] does — see that variant's doc
    /// comment.
    #[error("alias {alias:?} names a window field over its bound")]
    WindowFieldTooLong {
        /// The offending record's alias, exactly as it appears in the config.
        alias: String,
        /// The bound that was broken; its own `field` says which one.
        #[source]
        source: BoundError,
    },
}

/// The result of applying aliases to a raw search term.
#[derive(Debug, Clone, PartialEq)]
pub struct AliasEffect {
    /// The term the ranker should actually use. Equal to the raw `term`
    /// passed to [`Aliases::apply`], byte-for-byte, unless a `Rewrite`
    /// alias matched.
    pub effective_term: String,
    /// Additional per-item score contributions, keyed by `(provider,
    /// ItemId)` rather than a bare [`ItemId`]. Empty when no alias matched,
    /// or when only a `WindowBoost` matched (see [`Aliases::apply`]).
    ///
    /// The provider half of the key is [`APPS_PROVIDER_ID`] for every entry
    /// here today — `AppBoost` is the only [`AliasTarget`] variant `apply`
    /// resolves into a boost, and it always means "the apps provider's item
    /// for this app id" (see [`AliasTarget::AppBoost`]'s doc comment). A
    /// bare `ItemId` key would let an item from a *different*, honestly
    /// self-declared provider collect this boost merely by sharing the id
    /// string — an id-namespace collision the maintainer's issue #31 scope
    /// decision calls out explicitly, distinct from (and not caught by) the
    /// impersonation [`crate::pipeline::CheckedItems::check`] already rejects.
    /// Consumed by [`crate::rank::Boosts::by_provider_item`], which
    /// applies each entry only to the item whose own producer matches.
    pub boosts: HashMap<(String, ItemId), f32>,
}

/// Trim, then lowercase — the normalization `parseAliasRecord` and
/// `buildAliasContext` both apply to alias keys and to `titleContains`.
fn normalize_token(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Reads `obj[key]` as a JSON string, if present and actually a string.
/// A missing key or a wrong-typed value (a number, an object, ...) both
/// come back `None` here rather than failing the whole entry outright —
/// callers decide what "missing" means for that field.
fn str_field<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

/// Parses one alias record out of a `serde_json::Value`, per
/// `parseAliasRecord` in the JS. Returns `Ok(None)` for anything malformed:
/// not an object, an empty or whitespace-containing alias, an unrecognized
/// `type`, or a target missing its required field(s). That tolerance is the
/// point — one typo must not disable every other alias.
///
/// # Errors
///
/// Two things it will not skip, both fatal for the whole config: an `app`
/// record whose synthesized item id breaks [`hop_protocol::MAX_ITEM_ID`]
/// returns [`AliasError::AppItemIdTooLong`], and a `window` record whose
/// `app_id` or `title_contains` breaks its own bound returns
/// [`AliasError::WindowFieldTooLong`]. Each such record is well-formed and
/// unambiguous about what the user wanted; it simply cannot be honoured, and
/// the alternative — skipping it — would leave an alias that looks configured
/// and never fires. Failing once at load beats failing invisibly on every
/// keystroke.
fn parse_record(value: &Value) -> Result<Option<(String, AliasTarget)>, AliasError> {
    // A bare number, string, or array element that isn't an object at all
    // is a malformed entry: skip it rather than failing the whole parse.
    let Some(obj) = value.as_object() else {
        return Ok(None);
    };

    // Both spellings are kept: `alias` is the normalized lookup key, and
    // `raw_alias` is what the user actually typed, which is the only version
    // that can be found by searching their config file. Only the error below
    // uses the raw one.
    let Some(raw_alias) = str_field(obj, "alias") else {
        return Ok(None);
    };
    let alias = normalize_token(raw_alias);
    if alias.is_empty() || alias.chars().any(char::is_whitespace) {
        return Ok(None);
    }

    let Some(kind) = str_field(obj, "type").map(normalize_token) else {
        return Ok(None);
    };
    let target_obj = obj.get("target").and_then(Value::as_object);

    let target = match kind.as_str() {
        "rewrite" => {
            let query = target_obj
                .and_then(|t| str_field(t, "query"))
                .unwrap_or("")
                .trim()
                .to_string();
            if query.is_empty() {
                return Ok(None);
            }
            AliasTarget::Rewrite(query)
        }
        "app" => {
            let app_id = target_obj
                .and_then(|t| str_field(t, "appId"))
                .unwrap_or("")
                .trim()
                .to_string();
            if app_id.is_empty() {
                return Ok(None);
            }
            // The id `apply` will boost, built and checked here so `apply`
            // cannot fail — see `AliasTarget::AppBoost`'s doc comment.
            let item_id = ItemId::new(format!("app:{app_id}")).map_err(|source| {
                AliasError::AppItemIdTooLong {
                    alias: raw_alias.to_string(),
                    source,
                }
            })?;
            AliasTarget::AppBoost(item_id)
        }
        "window" => {
            let app_id = target_obj
                .and_then(|t| str_field(t, "appId"))
                .unwrap_or("")
                .trim()
                .to_string();
            // Bounds the trimmed value that is actually stored on
            // `WindowBoost::app_id` — see that field's doc comment for why
            // `MAX_ITEM_ID` is the right bound for it. Trimming only ever
            // removes bytes, so checking the trimmed value can only ever be
            // *more* permissive than checking the raw field, never less: an
            // id rejected here would have been rejected raw too.
            //
            // Calls `hop_protocol::check_len` — the same function
            // `ItemId::new` itself calls — rather than reimplementing the
            // check locally: this is a value that never becomes an `ItemId`,
            // but "what counts as exceeding a bound" is `hop-protocol`'s
            // decision to own once, not this crate's to duplicate.
            check_len("AliasTarget::WindowBoost.app_id", MAX_ITEM_ID, app_id.len()).map_err(
                |source| AliasError::WindowFieldTooLong {
                    alias: raw_alias.to_string(),
                    source,
                },
            )?;

            let title_contains = target_obj
                .and_then(|t| str_field(t, "titleContains"))
                .map(normalize_token)
                .unwrap_or_default();
            // Bounds the *normalized* value, deliberately, not the raw field.
            // `normalize_token` lowercases, and lowercasing can grow a
            // string's byte length for some Unicode input (Turkish dotted
            // İ is the case this crate's tests pin), so the raw field and the
            // value actually stored on `WindowBoost::title_contains` are not
            // always the same length. Bounding the stored value is the
            // contract that cannot be invalidated later by a config whose raw
            // field fits but whose normalized form does not: that field would
            // otherwise load as a `title_contains` over `MAX_TITLE` — an
            // alias that can never match anything, precisely what this bound
            // exists to refuse. See
            // `tests::window_title_contains_bound_is_checked_after_normalization_not_before`.
            //
            // Same `hop_protocol::check_len` call as `app_id` above, for the
            // same reason: one definition of a bound violation, not a copy of
            // it in this crate.
            check_len(
                "AliasTarget::WindowBoost.title_contains",
                MAX_TITLE,
                title_contains.len(),
            )
            .map_err(|source| AliasError::WindowFieldTooLong {
                alias: raw_alias.to_string(),
                source,
            })?;

            if app_id.is_empty() && title_contains.is_empty() {
                return Ok(None);
            }
            AliasTarget::WindowBoost {
                app_id: (!app_id.is_empty()).then_some(app_id),
                title_contains: (!title_contains.is_empty()).then_some(title_contains),
            }
        }
        _ => return Ok(None),
    };

    Ok(Some((alias, target)))
}

impl Aliases {
    /// Parses an alias configuration, ported from `parseAliasesConfig`.
    ///
    /// - Input that is not valid JSON at all returns [`AliasError`] — a
    ///   **deliberate divergence** from the JS, which swallowed this case
    ///   and returned an empty list. See the module docs.
    /// - Valid JSON that is not an array (an object, a number, a string, a
    ///   bool, `null`) returns `Ok` with no aliases: it parsed, it just has
    ///   nothing in it.
    /// - A valid array parses entry by entry; a malformed entry (wrong
    ///   shape, missing required fields, an unrecognized `type`, a
    ///   non-object element) is skipped rather than failing the whole
    ///   config, so one typo can't silently disable every other alias.
    /// - Two entry-level problems are *not* skipped, both fatal for the whole
    ///   config: an `app` record whose item id would break
    ///   [`hop_protocol::MAX_ITEM_ID`] returns
    ///   [`AliasError::AppItemIdTooLong`], and a `window` record whose
    ///   `app_id` or `title_contains` breaks its own bound
    ///   ([`hop_protocol::MAX_ITEM_ID`] and [`hop_protocol::MAX_TITLE`]
    ///   respectively) returns [`AliasError::WindowFieldTooLong`] — both name
    ///   the alias. Every string this crate could later need to build an id
    ///   or match a title from is therefore checked here, once at load,
    ///   rather than on every keystroke inside [`Aliases::apply`] — see
    ///   [`AliasTarget::AppBoost`] and [`AliasTarget::WindowBoost`] for why
    ///   that is where each check has to live.
    ///
    /// # Errors
    ///
    /// [`AliasError::InvalidJson`] if `json` does not parse at all, or
    /// [`AliasError::AppItemIdTooLong`] / [`AliasError::WindowFieldTooLong`]
    /// per the bullets above.
    pub fn from_json(json: &str) -> Result<Aliases, AliasError> {
        let value: Value =
            serde_json::from_str(json).map_err(|err| AliasError::InvalidJson(err.to_string()))?;

        let mut by_key: HashMap<String, Vec<AliasTarget>> = HashMap::new();
        if let Value::Array(items) = value {
            for item in &items {
                if let Some((alias, target)) = parse_record(item)? {
                    by_key.entry(alias).or_default().push(target);
                }
            }
        }

        Ok(Aliases { by_key })
    }

    /// Applies aliases to a raw search term, ported from
    /// `buildAliasContext` minus the candidate item list.
    ///
    /// 1. Normalizes `term` (trim, lowercase) and looks up records whose
    ///    alias equals it **exactly** — no prefix or fuzzy matching. No
    ///    match returns `term` unchanged, verbatim, with no boosts.
    /// 2. If any matching record is a [`AliasTarget::Rewrite`], its query
    ///    becomes `effective_term` (the first one, if several match — same
    ///    as the JS `Array.prototype.find`).
    /// 3. Otherwise `effective_term` is `term` **exactly as passed in**,
    ///    not the normalized lookup key — original case and surrounding
    ///    whitespace intact. This is what the ranker receives.
    /// 4. Every matching [`AliasTarget::AppBoost`] adds [`ALIAS_BOOST`] to
    ///    item id `app:<appId>`, tagged with [`APPS_PROVIDER_ID`] — that
    ///    provider's item for this app id, not any item that happens to
    ///    share the id string; see the doc comment on [`AliasEffect::boosts`].
    ///    Two records boosting the same id sum.
    ///
    /// Infallible and pure, deliberately: this runs on every keystroke, and
    /// the one fallible step it would otherwise have to take — synthesizing
    /// the boosted [`ItemId`] — already happened in [`Aliases::from_json`].
    ///
    /// ### Window aliases are not resolved here
    ///
    /// The JS `buildAliasContext` matches window aliases against the live
    /// candidate item list (by the window's app id and title). This method
    /// never sees items — it cannot synthesize an [`ItemId`] for a window,
    /// because that id depends on which windows happen to be open right
    /// now. So a matching [`AliasTarget::WindowBoost`] parses and is stored,
    /// but contributes **no boost** here. Resolving it needs a candidate
    /// item list to match against, which `apply` doesn't have — that is
    /// still open work, not yet scheduled against a specific milestone. This
    /// is a known boundary, not a bug — see
    /// [`tests::window_alias_matches_but_apply_emits_no_boost_by_design`].
    pub fn apply(&self, term: &str) -> AliasEffect {
        let key = normalize_token(term);
        let mut boosts: HashMap<(String, ItemId), f32> = HashMap::new();

        let Some(targets) = self.by_key.get(&key) else {
            return AliasEffect {
                effective_term: term.to_string(),
                boosts,
            };
        };

        let rewrite = targets.iter().find_map(|target| match target {
            AliasTarget::Rewrite(query) => Some(query.clone()),
            _ => None,
        });
        let effective_term = rewrite.unwrap_or_else(|| term.to_string());

        for target in targets {
            if let AliasTarget::AppBoost(item_id) = target {
                *boosts
                    .entry((APPS_PROVIDER_ID.to_string(), item_id.clone()))
                    .or_insert(0.0) += ALIAS_BOOST;
            }
            // AliasTarget::WindowBoost: deliberately not resolved here —
            // see the doc comment above.
        }

        AliasEffect {
            effective_term,
            boosts,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use hop_protocol::{MAX_ITEM_ID, MAX_TITLE};

    use super::*;

    // --- Ported from aliases.test.mjs ---

    // Inverted from the JS's "parseAliasesConfig falls back to empty
    // aliases for invalid JSON": this crate's contract is the opposite on
    // purpose (see the module docs) — invalid JSON is an error, not a
    // silent empty config. The JS swallowed this case; we don't.
    #[test]
    fn invalid_json_returns_an_error_rather_than_silently_yielding_empty_aliases() {
        let result = Aliases::from_json("{broken");
        assert!(result.is_err());
    }

    // Ports "buildAliasContext rewrites query from rewrite alias".
    #[test]
    fn rewrite_alias_rewrites_the_query() {
        let aliases =
            Aliases::from_json(r#"[{"alias":"gh","type":"rewrite","target":{"query":"github"}}]"#)
                .unwrap();
        let effect = aliases.apply("gh");
        assert_eq!(effect.effective_term, "github");
        assert!(effect.boosts.is_empty());
    }

    // Pins the other half of the rewrite-target rule: `target.query` is
    // trimmed but deliberately *not* lowercased, unlike `alias` and
    // `titleContains`. Nothing else in this suite exercises a mixed-case
    // rewrite query, so a future "helpful" normalization of this field
    // would slip through unnoticed without this test.
    #[test]
    fn rewrite_target_query_is_trimmed_but_not_lowercased() {
        let aliases = Aliases::from_json(
            r#"[{"alias":"gh","type":"rewrite","target":{"query":"  GitHub Pull Requests  "}}]"#,
        )
        .unwrap();
        let effect = aliases.apply("gh");
        assert_eq!(effect.effective_term, "GitHub Pull Requests");
    }

    // Ports "buildAliasContext boosts app alias targets on exact alias
    // query".
    #[test]
    fn app_alias_boosts_its_target() {
        let aliases = Aliases::from_json(
            r#"[{"alias":"term","type":"app","target":{"appId":"org.gnome.Terminal.desktop"}}]"#,
        )
        .unwrap();
        let effect = aliases.apply("term");
        assert_eq!(effect.effective_term, "term");
        assert_eq!(
            effect.boosts.get(&(
                APPS_PROVIDER_ID.to_string(),
                ItemId::new("app:org.gnome.Terminal.desktop").unwrap()
            )),
            Some(&ALIAS_BOOST)
        );
        assert_eq!(effect.boosts.len(), 1);
    }

    // Pins the provider dimension itself: the key an `AppBoost` registers
    // under is *tagged* with `APPS_PROVIDER_ID`, not a bare `ItemId`. An
    // item sharing the same id but produced by a different, honestly
    // self-declared provider must not satisfy this key — see
    // `crate::rank::Boosts` and
    // `pipeline::tests::alias_boost_does_not_land_on_an_identically_id_item_from_a_different_provider`
    // for why that distinction is the whole point of this issue's boost
    // half.
    #[test]
    fn app_alias_boost_is_tagged_with_the_apps_provider_not_a_bare_item_id() {
        let aliases =
            Aliases::from_json(r#"[{"alias":"term","type":"app","target":{"appId":"terminal"}}]"#)
                .unwrap();
        let effect = aliases.apply("term");
        // The positive half: the boost really is there, under the tag this
        // test is named for.
        assert_eq!(
            effect.boosts.get(&(
                APPS_PROVIDER_ID.to_string(),
                ItemId::new("app:terminal").unwrap()
            )),
            Some(&ALIAS_BOOST),
            "the boost must be present, tagged with the apps provider"
        );
        // The negative half: it is *not* also reachable under some other
        // provider's tag — an implementation that emitted no boost at all
        // would satisfy this assertion alone without earning the test name,
        // which is why both halves must live here together.
        assert_eq!(
            effect
                .boosts
                .get(&("not-apps".to_string(), ItemId::new("app:terminal").unwrap())),
            None,
            "the boost must be keyed to the apps provider specifically, not \
             to any provider that happens to answer with this id"
        );
    }

    // Cannot port "buildAliasContext boosts only matching open window
    // aliases" as-is: that JS test resolves the alias against a live item
    // list, and `apply(&self, term: &str)` never sees items (see the doc
    // comment on `apply`). This asserts the documented boundary instead:
    // a window alias that matches the term parses and is stored, but
    // contributes no boost from `apply` alone. Resolving it against real
    // windows needs a candidate item list `apply` doesn't have; that
    // remains open work, not yet scheduled against a specific milestone.
    #[test]
    fn window_alias_matches_but_apply_emits_no_boost_by_design() {
        let aliases = Aliases::from_json(
            r#"[{"alias":"stand","type":"window","target":{"appId":"org.gnome.Calendar","titleContains":"standup"}}]"#,
        )
        .unwrap();
        let effect = aliases.apply("stand");
        assert_eq!(
            effect.effective_term, "stand",
            "no rewrite matched, so the raw term survives unchanged"
        );
        assert!(
            effect.boosts.is_empty(),
            "window aliases are a known boundary: apply() has no item list to \
             resolve them against, so it must not fabricate a boost here"
        );
    }

    // --- From the acceptance criteria and parsing rules ---

    #[test]
    fn exact_match_only_rejects_prefixes_in_both_directions() {
        let aliases =
            Aliases::from_json(r#"[{"alias":"gh","type":"rewrite","target":{"query":"github"}}]"#)
                .unwrap();
        // "g": alias is longer than the term (term is a prefix of alias).
        // "ghi", "gh1": alias is shorter than the term (alias is a prefix
        // of term). "1gh": alias appears as a suffix, not the whole term.
        // None of these should match; each must fall through as an
        // identity effect, term untouched.
        for term in ["g", "ghi", "gh1", "1gh"] {
            let effect = aliases.apply(term);
            assert_eq!(
                effect.effective_term, term,
                "prefix/suffix variant {term:?} must not match the alias"
            );
            assert!(
                effect.boosts.is_empty(),
                "{term:?} must not match the alias"
            );
        }
    }

    #[test]
    fn case_and_whitespace_are_normalized_before_lookup() {
        let aliases =
            Aliases::from_json(r#"[{"alias":"gh","type":"rewrite","target":{"query":"github"}}]"#)
                .unwrap();
        assert_eq!(aliases.apply("GH").effective_term, "github");
        assert_eq!(aliases.apply("  gh  ").effective_term, "github");
    }

    #[test]
    fn unknown_term_is_an_identity_effect() {
        let aliases =
            Aliases::from_json(r#"[{"alias":"gh","type":"rewrite","target":{"query":"github"}}]"#)
                .unwrap();
        let effect = aliases.apply("nothing-registered");
        assert_eq!(effect.effective_term, "nothing-registered");
        assert!(effect.boosts.is_empty());
    }

    // The point of rule 3 in `apply`'s doc comment, and easy to get wrong:
    // when no rewrite matches, the effective term is the raw input
    // *exactly as passed in* — not the normalized lookup key.
    #[test]
    fn no_rewrite_means_the_raw_term_survives_unnormalized() {
        let aliases = Aliases::from_json(
            r#"[{"alias":"gh","type":"app","target":{"appId":"org.example.Github"}}]"#,
        )
        .unwrap();
        let effect = aliases.apply("  GH  ");
        assert_eq!(
            effect.effective_term, "  GH  ",
            "no rewrite matched, so the term must survive with its original \
             case and whitespace, not the normalized \"gh\""
        );
        assert_eq!(
            effect.boosts.get(&(
                APPS_PROVIDER_ID.to_string(),
                ItemId::new("app:org.example.Github").unwrap()
            )),
            Some(&ALIAS_BOOST)
        );
    }

    // One malformed entry per rejection reason worth covering, interleaved
    // with two valid entries, must not sink the valid ones.
    #[test]
    fn one_malformed_entry_does_not_sink_the_rest() {
        let json = r#"[
            {"alias":"good1","type":"rewrite","target":{"query":"one"}},
            {"alias":"gh hub","type":"rewrite","target":{"query":"x"}},
            {"alias":"bad-type","type":"unknown","target":{}},
            {"alias":"bad-rewrite","type":"rewrite","target":{"query":"   "}},
            {"alias":"bad-app","type":"app","target":{}},
            {"alias":"bad-window","type":"window","target":{}},
            42,
            {"alias":"good2","type":"app","target":{"appId":"org.example.App"}}
        ]"#;
        let aliases = Aliases::from_json(json).unwrap();

        assert_eq!(aliases.apply("good1").effective_term, "one");
        assert_eq!(
            aliases.apply("good2").boosts.get(&(
                APPS_PROVIDER_ID.to_string(),
                ItemId::new("app:org.example.App").unwrap()
            )),
            Some(&ALIAS_BOOST)
        );

        // None of the malformed keys registered anything.
        for term in ["gh hub", "bad-type", "bad-rewrite", "bad-app", "bad-window"] {
            let effect = aliases.apply(term);
            assert_eq!(effect.effective_term, term);
            assert!(effect.boosts.is_empty());
        }
    }

    #[test]
    fn valid_json_that_is_not_an_array_yields_ok_with_no_aliases() {
        for json in [
            r#"{"alias":"gh"}"#,
            "42",
            r#""just a string""#,
            "null",
            "true",
        ] {
            let aliases = Aliases::from_json(json).unwrap();
            assert_eq!(aliases.apply("gh").effective_term, "gh");
            assert!(aliases.apply("gh").boosts.is_empty());
        }
    }

    #[test]
    fn two_app_aliases_under_the_same_key_sum() {
        let json = r#"[
            {"alias":"gh","type":"app","target":{"appId":"org.example.App"}},
            {"alias":"gh","type":"app","target":{"appId":"org.example.App"}}
        ]"#;
        let aliases = Aliases::from_json(json).unwrap();
        let effect = aliases.apply("gh");
        assert_eq!(
            effect.boosts.get(&(
                APPS_PROVIDER_ID.to_string(),
                ItemId::new("app:org.example.App").unwrap()
            )),
            Some(&(2.0 * ALIAS_BOOST))
        );
    }

    #[test]
    fn a_rewrite_and_an_app_boost_under_the_same_key_both_apply() {
        let json = r#"[
            {"alias":"gh","type":"rewrite","target":{"query":"github"}},
            {"alias":"gh","type":"app","target":{"appId":"org.example.App"}}
        ]"#;
        let aliases = Aliases::from_json(json).unwrap();
        let effect = aliases.apply("gh");
        assert_eq!(effect.effective_term, "github");
        assert_eq!(
            effect.boosts.get(&(
                APPS_PROVIDER_ID.to_string(),
                ItemId::new("app:org.example.App").unwrap()
            )),
            Some(&ALIAS_BOOST)
        );
    }

    // --- The item-id bound on a synthesized boost target (issue #22). ---
    //
    // `apply` runs on every keystroke and returns no `Result`, so the item id
    // it boosts — `app:<appId>` — has to be known-good before it gets there.
    // These pin that the check happens at load, loudly and once, rather than
    // at query time where the only options would be a panic or a boost that
    // silently never fires.

    // The alias is spelled `"Slack"` here, with a capital, because the error
    // has to quote it the way the user wrote it. Lookup keys are normalized,
    // so an error built from the key would say `"slack"` — which is not what
    // is in the file, and not what a search of the file will find.
    #[test]
    fn an_app_alias_whose_item_id_would_exceed_the_bound_is_rejected_at_load() {
        // `app:` + this is one byte over MAX_ITEM_ID.
        let app_id = "a".repeat(MAX_ITEM_ID - "app:".len() + 1);
        let json = format!(r#"[{{"alias":"Slack","type":"app","target":{{"appId":"{app_id}"}}}}]"#);

        let err = Aliases::from_json(&json)
            .expect_err("an app id whose item id breaks the bound must fail the load");
        assert!(
            err.to_string().contains("\"Slack\""),
            "the error must name the alias as written, so the user can find it \
             in their config, got: {err}"
        );
    }

    // The bound's own message is carried as a `#[source]` and not interpolated
    // into this error's `Display`, so a reporter that walks the chain prints it
    // once rather than twice. Both halves are asserted: the message does not
    // restate it, and the chain really does carry it.
    #[test]
    fn the_load_error_carries_the_bound_as_a_source_not_in_its_own_message() {
        let app_id = "a".repeat(MAX_ITEM_ID);
        let json = format!(r#"[{{"alias":"big","type":"app","target":{{"appId":"{app_id}"}}}}]"#);

        let err = Aliases::from_json(&json).unwrap_err();
        assert!(
            !err.to_string().contains("maximum"),
            "the message must not restate what the source already says, got: {err}"
        );
        let source = std::error::Error::source(&err)
            .expect("the broken bound must be reachable as a source");
        assert!(
            source.to_string().contains("over its maximum of"),
            "got: {source}"
        );
    }

    // The other side of the bound: an app id that sits exactly on it loads
    // and boosts as usual. Without this, rejecting every app alias would pass
    // the test above.
    #[test]
    fn an_app_alias_exactly_on_the_item_id_bound_still_loads_and_boosts() {
        let app_id = "a".repeat(MAX_ITEM_ID - "app:".len());
        let json =
            format!(r#"[{{"alias":"ontheline","type":"app","target":{{"appId":"{app_id}"}}}}]"#);

        let aliases = Aliases::from_json(&json).unwrap();
        let effect = aliases.apply("ontheline");
        assert_eq!(
            effect.boosts.get(&(
                APPS_PROVIDER_ID.to_string(),
                ItemId::new(format!("app:{app_id}")).unwrap()
            )),
            Some(&ALIAS_BOOST)
        );
    }

    // DIVERGENCE: the JS had no bounded id type, so it had no equivalent of
    // this case — every `app` record it parsed produced a usable boost, and
    // nothing an entry could contain was fatal to the config.
    //
    // An over-long app id is a *fatal* config error, unlike the malformed
    // entries `one_malformed_entry_does_not_sink_the_rest` skips. The
    // distinction is deliberate: a skipped entry is one the JS also skipped,
    // whereas this one parsed fine and would have produced a boost that could
    // never land — a failure the user would otherwise only notice as an alias
    // that quietly stopped working.
    #[test]
    fn an_over_long_app_id_sinks_the_whole_config_rather_than_being_skipped() {
        let app_id = "a".repeat(MAX_ITEM_ID);
        let json = format!(
            r#"[
                {{"alias":"good","type":"rewrite","target":{{"query":"github"}}}},
                {{"alias":"toolong","type":"app","target":{{"appId":"{app_id}"}}}}
            ]"#
        );

        assert!(Aliases::from_json(&json).is_err());
    }

    // --- Window alias field bounds (issue #76). ---
    //
    // `app_id` and `title_contains` are raw config strings `apply` never
    // resolves into an id today, but the issue's own open question — whether
    // they should carry load-time bounds anyway — is settled yes, by the same
    // convention `AppItemIdTooLong` set: validate everything validatable at
    // load, so a future resolver inherits an already-checked string instead of
    // a fallible construction with nowhere to report failure.

    // The alias is spelled with a capital, same reason as the `app` version of
    // this test: the error has to quote the alias as written, not the
    // normalized lookup key, or a user searching their config for `"Stand"`
    // would not find what the message says.
    #[test]
    fn a_window_alias_whose_app_id_exceeds_the_bound_is_rejected_at_load_naming_the_alias_as_written()
     {
        let app_id = "a".repeat(MAX_ITEM_ID + 1);
        let json =
            format!(r#"[{{"alias":"Stand","type":"window","target":{{"appId":"{app_id}"}}}}]"#);

        let err = Aliases::from_json(&json)
            .expect_err("a window app_id over the bound must fail the whole load");
        assert!(
            err.to_string().contains("\"Stand\""),
            "the error must name the alias as written, got: {err}"
        );
    }

    // Analogous to `the_load_error_carries_the_bound_as_a_source_not_in_its_own_message`
    // (the `app` version), for `WindowFieldTooLong`. That convention — the
    // message must not restate the bound, and the chain must genuinely carry
    // it as a `#[source]` — is a claim `WindowFieldTooLong`'s own doc comment
    // makes about itself, so it gets its own test rather than relying on the
    // `app` variant's test to stand in for both.
    #[test]
    fn the_window_field_error_carries_the_bound_as_a_source_not_in_its_own_message() {
        let app_id = "a".repeat(MAX_ITEM_ID + 1);
        let json =
            format!(r#"[{{"alias":"big","type":"window","target":{{"appId":"{app_id}"}}}}]"#);

        let err = Aliases::from_json(&json).unwrap_err();
        assert!(
            !err.to_string().contains("maximum"),
            "the message must not restate what the source already says, got: {err}"
        );
        let source = std::error::Error::source(&err)
            .expect("the broken bound must be reachable as a source");
        assert!(
            source.to_string().contains("over its maximum of"),
            "got: {source}"
        );
    }

    // The other side of the bound: exactly on it must still load and behave
    // like any other window alias (parsed and stored, no boost from `apply`
    // alone — see `window_alias_matches_but_apply_emits_no_boost_by_design`).
    // Without this, rejecting every window alias would still pass the test
    // above.
    #[test]
    fn a_window_alias_app_id_exactly_on_the_bound_still_loads() {
        let app_id = "a".repeat(MAX_ITEM_ID);
        let json =
            format!(r#"[{{"alias":"stand","type":"window","target":{{"appId":"{app_id}"}}}}]"#);

        let aliases = Aliases::from_json(&json).unwrap();
        let effect = aliases.apply("stand");
        assert_eq!(effect.effective_term, "stand");
        assert!(
            effect.boosts.is_empty(),
            "a window alias never contributes a boost from apply alone"
        );
    }

    #[test]
    fn a_window_alias_whose_title_contains_exceeds_the_bound_is_rejected_at_load_naming_the_alias_as_written()
     {
        let title = "a".repeat(MAX_TITLE + 1);
        let json = format!(
            r#"[{{"alias":"Stand","type":"window","target":{{"titleContains":"{title}"}}}}]"#
        );

        let err = Aliases::from_json(&json)
            .expect_err("a window title_contains over the bound must fail the whole load");
        assert!(
            err.to_string().contains("\"Stand\""),
            "the error must name the alias as written, got: {err}"
        );
    }

    #[test]
    fn a_window_alias_title_contains_exactly_on_the_bound_still_loads() {
        let title = "a".repeat(MAX_TITLE);
        let json = format!(
            r#"[{{"alias":"stand","type":"window","target":{{"titleContains":"{title}"}}}}]"#
        );

        let aliases = Aliases::from_json(&json).unwrap();
        let effect = aliases.apply("stand");
        assert_eq!(effect.effective_term, "stand");
        assert!(effect.boosts.is_empty());
    }

    // An over-long window field is fatal for the *whole config*, exactly like
    // `AppItemIdTooLong`, not skipped like the malformed `bad-window` entry in
    // `one_malformed_entry_does_not_sink_the_rest`. Proven by asserting a
    // separate, otherwise-valid alias in the same config never loads either —
    // the whole call returns `Err`, so nothing in it is reachable.
    #[test]
    fn an_over_long_window_field_sinks_the_whole_config_rather_than_being_skipped() {
        let app_id = "a".repeat(MAX_ITEM_ID + 1);
        let json = format!(
            r#"[
                {{"alias":"good","type":"rewrite","target":{{"query":"github"}}}},
                {{"alias":"toolong","type":"window","target":{{"appId":"{app_id}"}}}}
            ]"#
        );

        assert!(Aliases::from_json(&json).is_err());
    }

    // Unchanged: a window record naming neither field is still malformed, not
    // an over-long one, and stays skipped rather than fatal.
    #[test]
    fn a_window_record_with_neither_field_is_still_skipped() {
        let json = r#"[{"alias":"empty-window","type":"window","target":{}}]"#;
        let aliases = Aliases::from_json(json).unwrap();
        let effect = aliases.apply("empty-window");
        assert_eq!(effect.effective_term, "empty-window");
        assert!(effect.boosts.is_empty());
    }

    // Mirrors `item.rs`'s `item_id_bound_is_counted_in_bytes_not_characters`:
    // "é" is two bytes in UTF-8, so a value of MAX_ITEM_ID / 2 characters sits
    // exactly at the byte bound, far short of it in characters. Proves the
    // bound is counted in bytes, not chars, for this field too.
    #[test]
    fn a_window_app_id_at_the_byte_bound_in_far_fewer_chars_is_accepted() {
        let app_id = "é".repeat(MAX_ITEM_ID / 2);
        assert_eq!(app_id.len(), MAX_ITEM_ID);
        assert_eq!(app_id.chars().count(), MAX_ITEM_ID / 2);
        let json =
            format!(r#"[{{"alias":"stand","type":"window","target":{{"appId":"{app_id}"}}}}]"#);

        assert!(Aliases::from_json(&json).is_ok());
    }

    // Pins the deliberate choice to bound the *stored* (normalized) value of
    // `title_contains`, not the raw field: `normalize_token` lowercases, and
    // lowercasing can grow a string's byte length. 'İ' (U+0130, LATIN CAPITAL
    // LETTER I WITH DOT ABOVE) is 2 bytes raw but lowercases to "i" plus a
    // combining dot above (U+0307), 3 bytes — so 400 of them are 800 bytes raw
    // (under MAX_TITLE) but 1200 bytes once normalized (over it). If this bound
    // were checked against the raw field instead, this config would load and
    // store a title_contains over MAX_TITLE — exactly the "accepts an alias
    // that can never fire" failure mode this bound exists to close.
    #[test]
    fn window_title_contains_bound_is_checked_after_normalization_not_before() {
        let title = "İ".repeat(400);
        assert_eq!(title.len(), 800, "raw value must sit under MAX_TITLE");
        assert!(
            title.to_lowercase().len() > MAX_TITLE,
            "normalized value must sit over MAX_TITLE, or this test proves nothing"
        );
        let json = format!(
            r#"[{{"alias":"stand","type":"window","target":{{"titleContains":"{title}"}}}}]"#
        );

        let err = Aliases::from_json(&json).expect_err(
            "a title_contains whose *normalized* form breaks the bound must fail the load, \
             even though the raw field does not",
        );
        assert!(matches!(err, AliasError::WindowFieldTooLong { .. }));
    }

    // --- The precedence constant ---

    // Must reference both constants rather than repeating their values: a
    // test that hardcodes `180.0 > 85.0` would keep passing after either
    // constant was retuned and prove nothing about the code. Clippy flags
    // this as an "assertion has a constant value" since both operands
    // happen to be `const`s resolvable at compile time — true, but that's
    // exactly the point being asserted, so it's allowed here rather than
    // rewritten to hide the comparison from clippy's analysis.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn alias_boost_constant_beats_learning_cap() {
        assert!(ALIAS_BOOST > crate::learning::LEARNING_BOOST_CAP);
    }
}
