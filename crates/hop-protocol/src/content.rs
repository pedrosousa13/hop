//! What a provider-supplied value is allowed to contain.
//!
//! [`ExecOutcome::CopyText`](crate::wire::ExecOutcome::CopyText) and
//! [`ExecOutcome::OpenUrl`](crate::wire::ExecOutcome::OpenUrl) are the two
//! variants of [`ExecOutcome`](crate::wire::ExecOutcome) that tell a client to
//! *act* — put this on the clipboard, hand this to a URL launcher — rather than
//! reporting what happened. Both originate in a provider, and a provider is the
//! plugin seam every later extension tier adapts to, so "the daemon is trusted"
//! does not reach them: it degrades to "every installed provider is trusted".
//!
//! [`IconPath`] is the third value here a client acts on rather than displays,
//! and it reaches a different sink from those two: an
//! [`IconSpec`](crate::item::IconSpec) can carry a filesystem path, and a client
//! opens and decodes that path once per item while results stream. The sentence
//! about trust above reaches it unchanged — the path is a provider's and the
//! read is the client's, which is what makes a client the confused deputy if the
//! path is not constrained. [`IconName`] is the other arm of that spec, and it
//! is a lookup key rather than a sink; it is here because it needs a content
//! rule of its own, without which it would be a second way to send a path.
//!
//! [`Item`](crate::item::Item)'s `copy_text` reaches the same clipboard by a
//! different route, and it is still a plain bounded `String`. It wants
//! [`CopyText`] too, and giving it one changes the item model rather than the
//! outcome, so it is deliberately left for its own change.
//!
//! [`limits`] says how *long* these values may be; this module
//! says what they may *contain*. The two compose rather than replace one
//! another, and they compose in a fixed order: at the parse and in each
//! constructor the length is checked first, so a value that breaks both is
//! reported as over-long.
//!
//! # The one place this crate touches the filesystem
//!
//! Every rule in this module but one is decided from the value itself, which is
//! why `hop-protocol` otherwise makes no syscall at all.
//! [`IconPath::open_regular_file`] is the exception: an icon path's last rule —
//! that it names a regular file rather than a FIFO, a device or a directory — is
//! not a fact about the string, and no amount of reading the string settles it.
//!
//! The impurity is confined in three ways. It is in one method, not in a
//! `Deserialize`, so no parse makes a syscall and the query path stays clean.
//! It is explicitly called, so a caller opts into it at a point where it was
//! about to open the file anyway. And it inspects the descriptor it opened
//! rather than the path it was given, so it cannot be raced. That method's own
//! docs carry the reasoning, including which alternatives were rejected and what
//! the check still does not catch.
//!
//! # One gate, not two
//!
//! Each value is a newtype over a private `String` whose only constructor
//! applies every rule, and whose `Deserialize` hands the string it parsed to
//! that same constructor. A value of either type that exists has passed the
//! rules, whether a provider built it or it arrived off the socket — no frame
//! can carry a refused value into existence, and a rule added later cannot be
//! remembered at one gate and forgotten at the other.
//!
//! # How a refused character arrives
//!
//! Worth writing down because it decides which parse path has to hold.
//!
//! A C0 control character (U+0000–U+001F) cannot appear raw inside a JSON
//! string: the grammar forbids it and serde_json refuses the document before
//! any visitor runs. Copy text carrying one therefore always arrives
//! *escaped*. DEL (U+007F) and the C1 controls (U+0080–U+009F) carry no such
//! requirement and can arrive raw.
//!
//! Which visitor arm either of those reaches is a property of the parse, not of
//! the character. The routing table in [`limits`] states it qualified by input
//! source, and that qualification is load-bearing here. Re-measured with a
//! probe visitor for the shape this module's values travel in — a `copy_text`
//! outcome inside the internally-tagged `executed` frame:
//!
//! ```text
//!   from_str / from_slice, raw (a control or not)  ->  borrowed arm
//!   from_str / from_slice, escaped                 ->  owned arm
//!   from_reader,           raw or escaped          ->  owned arm
//! ```
//!
//! So no single arm sees everything: a `from_str` parse meets refusable input
//! on both, and a `from_reader` parse — the likelier shape of a socket
//! transport — hands every string to the owned one. What routes it there is
//! the tagged frame's `Content` buffer rather than the reader itself, as
//! [`limits`] sets out: the buffer owns any string it cannot borrow for `'de`,
//! and a reader can lend none. The same reader parsing a *bare* value, with no
//! buffer in between, reaches the borrowed arm with a slice of serde_json's own
//! scratch buffer — measured, and the reason the table above is scoped to the
//! frame. That is why the rules live in the constructor both arms call rather
//! than in either one. That conclusion is what the tests
//! `tests::an_escaped_control_character_is_refused_inside_a_tagged_frame`,
//! `tests::a_raw_control_character_is_refused_inside_a_tagged_frame`,
//! `tests::a_refused_value_is_refused_through_from_reader_too` and
//! `tests::a_raw_c0_control_never_reaches_the_content_check` hold: a refusal
//! fires whichever of these paths a value arrives by.
//!
//! The table itself is not held that way, and deliberately is not. Nothing here
//! observes which arm a parse reached — that is serde's internal routing, which
//! this crate neither controls nor should pin — so if that routing changed, the
//! table would go stale while every test above stayed green. It is a
//! measurement, taken against serde_json 1.0.151, and the conclusion drawn from
//! it is one that holds however the routing moves.
//!
//! # What these rules do not close
//!
//! They decide which *sink* a value can reach and whether it can be read as
//! something other than an operand. They do not make it safe once it gets
//! there. A URL with an allowed scheme is still an arbitrary web address a
//! browser will fetch, and accepted copy text is still arbitrary text a
//! terminal will paste.
//!
//! What is removed is narrower than "anything dangerous": a local file dressed
//! as a URL, a command-line option dressed as a URL, a terminal control
//! sequence dressed as text, a path dressed as a theme name. A value that is
//! exactly what its variant says it is can still be hostile — accepted copy text
//! may hold a newline and so a second line, which [`CopyText`]'s own section on
//! that says plainly; an accepted URL is unparsed past its scheme, which
//! [`OpenUrl`]'s does; and an accepted icon path names *somewhere*, not
//! somewhere an icon belongs. That last one is worth stating without the
//! flattering word: the path rules make it absolute and free of `..`, which is
//! what lets somebody else check it against a root — they do not make it
//! *contained*, because no root is enforced here and because a symlink under one
//! still leads out. [`IconPath`]'s section on the roots prices exactly that. The
//! client's own launching, clipboard handling and icon resolution is the other
//! half of this, and it is not in this crate.

use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::limits::{
    self, BoundError, MAX_COPY_TEXT, MAX_ICON_NAME, MAX_ICON_PATH, MAX_OPEN_URL, check_len,
};

/// The URL schemes an [`OpenUrl`] may carry.
///
/// An allow-list, not a deny-list, so a handler this contract has never heard
/// of cannot be reached by installing a provider that names it. Comparison is
/// ASCII-case-insensitive: RFC 3986 §3.1 defines a scheme's case as
/// insignificant, so `HTTPS` is the same scheme as `https` to whatever would
/// dispatch on it, and treating them differently here would only mean refusing
/// one spelling of a URL a browser opens either way.
///
/// `http` and `https` are what a web-search or weather item opens. `mailto` is
/// what a "write to this address" action opens. All three name something a
/// desktop resolves to a network locator or an address, which is what makes
/// them safe to hand on without knowing what a provider is pointing at.
/// Everything else is refused, and three families are the reason the list is
/// this short: `file:` (and anything else naming local content) would let a
/// provider have a client read a chosen path, which is exfiltration through a
/// variant that claims to be opening a web page; `javascript:` and `data:` name
/// content a browser *executes* rather than fetches, and are handled
/// inconsistently enough between browsers that what a given one does with them
/// is not a property this crate can reason about; and a scheme registered by
/// some other locally installed application is an arbitrary local handler with
/// an arbitrary argument, which is a launcher, not a link.
///
/// Extending this list is a deliberate act with a threat model attached, not a
/// convenience: everything in it is a sink a provider gets to aim.
pub const ALLOWED_URL_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// The control characters a [`CopyText`] may carry, by exception.
///
/// Tab and newline are the indentation and the line breaks of the snippet or
/// multi-line answer that [`MAX_COPY_TEXT`] is sized for, so refusing them
/// would refuse honest content — the one failure mode worse than a loose rule.
///
/// This is a trade and not a classification: carriage return occurs in ordinary
/// text too, as the CR of CRLF, and it is refused anyway. [`CopyText`] carries
/// what allowing these two costs, and what refusing that one costs.
pub const ALLOWED_COPY_TEXT_CONTROLS: &[char] = &['\t', '\n'];

/// A value refused by a content rule in [`content`](self).
///
/// Deserialization turns this into a serde error, so a transport reports a
/// refused value as a protocol error rather than proceeding with it. Nothing
/// here sanitises: a value that breaks a rule is refused whole, because a URL
/// with its scheme swapped or copy text with its control characters stripped is
/// a *different* value, and quietly acting on something the peer did not send
/// is the failure this type exists to prevent.
///
/// Every variant names the wire field it came from, and none carries the
/// offending text: these values are peer-controlled, an error made from one
/// travels into logs and diagnostics, and the field plus the rule is what a
/// reader needs. [`ContentError::ForbiddenChar`] carries a code point besides,
/// which is a number rather than a piece of the value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContentError {
    /// A value over its byte maximum, from the size budget in
    /// [`limits`].
    ///
    /// Content rules and the size budget are separate concerns with separate
    /// error types; this arm carries the budget's verdict through rather than
    /// restating it, so a length refusal reads the same whichever gate produced
    /// it.
    #[error(transparent)]
    TooLong(#[from] BoundError),
    /// A URL whose scheme is not one of [`ALLOWED_URL_SCHEMES`], including a
    /// value carrying no scheme at all.
    #[error("{field} does not carry one of the allowed URL schemes")]
    SchemeNotAllowed {
        /// The wire field that broke the rule.
        field: &'static str,
    },
    /// A URL beginning with `-`, which is an option and not an operand to
    /// anything that builds an argument vector.
    #[error("{field} begins with a dash, which reads as a command-line flag")]
    LeadingDash {
        /// The wire field that broke the rule.
        field: &'static str,
    },
    /// A value with nothing in it, from a field where absence is said by
    /// leaving the value out rather than by sending an empty one.
    ///
    /// [`IconName`] is the one such field: an [`IconSpec`](crate::item::IconSpec)
    /// carrying an empty name is the "no icon at all" state coming back as a
    /// value, and that state is said by omitting the item's `icon` instead.
    #[error("{field} is empty")]
    Empty {
        /// The wire field that broke the rule.
        field: &'static str,
    },
    /// A path that does not begin at the filesystem root.
    #[error("{field} is not an absolute path")]
    NotAbsolute {
        /// The wire field that broke the rule.
        field: &'static str,
    },
    /// A path holding a `..` component, which lets it name a file outside
    /// wherever it appears to be.
    #[error("{field} holds a `..` component")]
    ParentComponent {
        /// The wire field that broke the rule.
        field: &'static str,
    },
    /// A path holding a NUL, which terminates the C string the path becomes at
    /// a syscall and so makes it name a shorter path there than it reads as
    /// here.
    ///
    /// A NUL is a control character, so [`ContentError::ForbiddenChar`] would
    /// report it too. It is named separately for the reason the leading-dash
    /// rule on [`OpenUrl`] is kept: the refusal should name the hazard rather
    /// than report a strange code point.
    #[error("{field} holds a NUL, which would truncate the path at a syscall")]
    InteriorNul {
        /// The wire field that broke the rule.
        field: &'static str,
    },
    /// A value holding a character its type may not carry.
    #[error("{field} holds U+{codepoint:04X}, which it may not carry")]
    ForbiddenChar {
        /// The wire field that broke the rule.
        field: &'static str,
        /// The offending character's Unicode code point.
        codepoint: u32,
    },
}

/// A URL that an [`ExecOutcome::OpenUrl`](crate::wire::ExecOutcome::OpenUrl)
/// may carry: one a client can hand to a URL launcher.
///
/// The inner string is private and the only way in is [`OpenUrl::new`], so an
/// `OpenUrl` that exists has passed every rule below — see [this module's
/// docs](self) for why that matters more here than for a displayed field, and
/// for what these rules still do not close.
///
/// # The rules, in the order they are applied
///
/// 1. **At most [`MAX_OPEN_URL`] bytes.** Delegated to the size budget, and
///    applied first, so a value that is both too long and otherwise refusable
///    is reported as too long and the rules below never scan it.
/// 2. **No leading `-`.** A client hands this value to a URL launcher; one
///    that builds an argument vector would read a leading `-` as an option
///    rather than as the URL. Rule 3 already excludes every such value, because
///    no scheme in [`ALLOWED_URL_SCHEMES`] starts with anything but a letter —
///    pinned by the test `tests::no_allowed_scheme_could_be_read_as_a_flag`.
///    It is kept as a rule of its own regardless: it is what has to survive a
///    later edit to the allow-list, and it names the actual hazard in the
///    refusal instead of reporting a strange scheme.
/// 3. **A scheme from [`ALLOWED_URL_SCHEMES`]**, compared
///    ASCII-case-insensitively. The scheme is what precedes the *first* `:`, so
///    an allowed scheme appearing later in the value does not rescue a refused
///    one.
/// 4. **No ASCII space and no control character.** These two are what let a
///    value be split, truncated or re-read by whatever it is passed to: a space
///    is where an argument ends for anything that builds a command line by
///    splitting, and a control character ends a line or steers a terminal. That
///    is the whole of what this rule targets. It is *not* a rule that a URL
///    must hold only characters RFC 3986 allows unencoded — plenty that the RFC
///    also requires percent-encoded are accepted here: every non-ASCII
///    character outside `Cc`, so `https://example.com/café` passes, and `<`,
///    `>`, `"`, `{`, `}`, `|`, `\`, `^` and a backtick besides, because
///    refusing those would refuse URLs that browsers open every day. The
///    `Cc` exception is not a nicety: "control character" here is
///    [`char::is_control`], Unicode's `Cc` category, which reaches past ASCII
///    to the C1 controls at U+0080–U+009F, and those this rule does refuse.
///    Pinned by the tests
///    `tests::open_url_accepts_characters_rfc_3986_would_percent_encode` on
///    one side and `tests::open_url_refuses_a_space_or_a_control_character` on
///    the other.
///
/// Nothing here normalises, trims or percent-encodes. An accepted URL is passed
/// on exactly as it arrived, so what a client opens is what the provider sent,
/// and a rule that reads the value cannot disagree with a later reader of it.
///
/// # What is not checked
///
/// The rules above decide which handler a value can reach and whether it can be
/// read as something other than an operand. They do not parse the URL, and
/// three consequences are worth naming here rather than leaving to be
/// discovered:
///
/// - **No authority is required.** A bare `https:` is accepted, as are
///   `https://` and `http:example.com`, whose path is opaque. What refuses
///   those is whatever tries to open them, not this type.
/// - **Userinfo is not inspected.** `https://evil.com@good.com/` is accepted.
///   Its host is `good.com`, but a user reading it sees `evil.com` first.
/// - **Hosts are neither compared nor normalised**, so a punycode or homograph
///   host that renders like a familiar one passes unremarked.
///
/// All three are display-and-phishing problems belonging to whatever shows or
/// opens the URL, and closing them needs a URL parser this crate deliberately
/// does not carry. Pinned by the test
/// `tests::open_url_does_not_check_what_follows_the_scheme`, so a later
/// widening of the rules updates this list rather than silently contradicting
/// it.
///
/// The newtype does not change the wire form: a URL is still a bare JSON
/// string, never an object or a wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct OpenUrl(String);

impl<'de> Deserialize<'de> for OpenUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(deserializer, OpenUrl::FIELD, MAX_OPEN_URL, OpenUrl::new)
    }
}

impl OpenUrl {
    /// The wire field this value travels in, named by every refusal of one.
    ///
    /// A URL occupies exactly one field of the contract, so naming that field
    /// is more locating than naming this type would be, and it makes the
    /// refusals from the constructor and from the parse read identically. An
    /// [`ItemId`](crate::item::ItemId) is named for its type instead, because
    /// it travels in several fields and its type is the only stable name it
    /// has.
    ///
    /// "Every refusal" includes a value of the wrong JSON type. A number or a
    /// `null` where a string is wanted never reaches [`OpenUrl::new`] at
    /// all — it is refused earlier, by serde's own `invalid_type` error, whose
    /// message is built from `limits`'s shared `expecting` for a validating
    /// newtype. That `expecting` used to write only the byte maximum, naming
    /// no field, which made this claim false for exactly that refusal (issue
    /// #82); it now writes this constant too. Pinned by
    /// `tests::a_wrong_typed_value_names_the_field_for_every_field_carrying_type`
    /// and `tests::a_null_value_names_the_field_for_every_field_carrying_type`,
    /// which cover all four `FIELD`-carrying types in this module, not only
    /// this one and [`CopyText::FIELD`].
    pub(crate) const FIELD: &'static str = "ExecOutcome::OpenUrl";

    /// Builds a URL, refusing one that breaks any rule on [`OpenUrl`].
    ///
    /// # Errors
    ///
    /// [`ContentError`], naming the first rule the value broke, in the order
    /// documented on [`OpenUrl`].
    pub fn new(value: impl Into<String>) -> Result<Self, ContentError> {
        let value = value.into();
        check_len(Self::FIELD, MAX_OPEN_URL, value.len())?;
        if value.starts_with('-') {
            return Err(ContentError::LeadingDash { field: Self::FIELD });
        }
        if !has_allowed_scheme(&value) {
            return Err(ContentError::SchemeNotAllowed { field: Self::FIELD });
        }
        if let Some(refused) = value.chars().find(|c| c.is_control() || *c == ' ') {
            return Err(ContentError::ForbiddenChar {
                field: Self::FIELD,
                codepoint: refused as u32,
            });
        }
        Ok(Self(value))
    }

    /// The URL as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the URL, yielding the string inside.
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Whether a value's scheme — everything before its first `:` — is allowed.
///
/// A value with no `:` has no scheme and is refused by the same answer.
fn has_allowed_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    ALLOWED_URL_SCHEMES
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

/// Text that an [`ExecOutcome::CopyText`](crate::wire::ExecOutcome::CopyText)
/// may carry: text a client can put on the clipboard.
///
/// The inner string is private and the only way in is [`CopyText::new`], so a
/// `CopyText` that exists is within [`MAX_COPY_TEXT`] bytes and carries no
/// control character outside [`ALLOWED_COPY_TEXT_CONTROLS`]. The length is
/// checked first, so a value that is both over-long and otherwise refusable is
/// reported as over-long.
///
/// # Which control characters are allowed, and why
///
/// The threat is a paste into a shell, so the rule sorts control characters by
/// what they do to the *device* the text lands in, not by whether they occur in
/// text — several do occur in text and are refused anyway, and the section on
/// carriage return below is the one that costs something.
///
/// What is kept is [`ALLOWED_COPY_TEXT_CONTROLS`], and it is kept for honest
/// content: a snippet has indentation and lines, and [`MAX_COPY_TEXT`] is the
/// most generous bound in the size budget precisely because copy text is
/// allowed to be a chunk of prose rather than a label. Refusing a line break
/// would mean an honest multi-line answer could not be copied at all, which is
/// a bigger, likelier failure than the one it would avert.
///
/// Everything else in Unicode's `Cc` category — the rest of C0, DEL, and the C1
/// controls — is refused. Most of it is device control with no meaning in a
/// clipboard at all, and refusing the category wholesale is both simpler to
/// state and safer than enumerating the members known to be dangerous today.
/// `ESC` is the one that matters most: it opens a terminal control sequence, so
/// it is what turns pasted text into instructions to the terminal rather than
/// input to whatever is reading.
///
/// # What refusing a carriage return costs
///
/// One member of that category occurs in ordinary text as well, and the rule
/// charges for it. As the CR of CRLF, a carriage return is common in text of
/// Windows origin, in HTTP bodies, and in `\r\n`-terminated files — so a
/// provider that copies such text through unchanged has the value refused, and
/// because the refusal happens at the parse, the whole frame refused with it.
/// That is the same "silently break honest providers" failure that argues for
/// allowing a newline two paragraphs up, and here it is accepted rather than
/// avoided: a carriage return returns a terminal to column zero, which makes it
/// the one character that can overwrite what was already shown of a line, and a
/// paste able to hide what it contains is worth more than CRLF convenience.
///
/// It is a judgement and not a boundary, and it should not be read as one. A
/// pasted newline submits a line to a line-oriented reader much as a carriage
/// return would, so refusing CR buys the column-zero overwrite and little
/// besides. A provider holding CRLF content should translate it to LF before
/// sending rather than expect this type to take it.
///
/// # What allowing a newline does not close
///
/// It leaves copy text able to hold more than one line, and a consumer that
/// treats a line ending as "run this" would run both. This type does not claim
/// to make a paste safe, and no rule available to it could while a newline is
/// allowed at all — which is the trade the two sections above set out, not a
/// property of the characters. Making a paste safe is the client's clipboard
/// handling, which is not in this crate.
///
/// # What is not refused
///
/// The rule is [`char::is_control`], so it covers `Cc` and nothing else. It
/// does not refuse U+2028 and U+2029, which are line separators in some
/// consumers, nor the bidirectional format characters such as U+202E, which can
/// make text *display* as something other than what it is. Those are a
/// display-spoofing concern rather than a terminal-control one; this type does
/// not address display spoofing, and saying so is better than implying a
/// guarantee it does not make. Pinned by the test
/// `tests::copy_text_does_not_refuse_the_non_cc_separators_and_format_characters`,
/// so a later widening of the rule updates this paragraph rather than silently
/// contradicting it.
///
/// The newtype does not change the wire form: copy text is still a bare JSON
/// string, never an object or a wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CopyText(String);

impl<'de> Deserialize<'de> for CopyText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(deserializer, CopyText::FIELD, MAX_COPY_TEXT, CopyText::new)
    }
}

impl CopyText {
    /// The wire field this value travels in, named by every refusal of one, for
    /// the reason given on [`OpenUrl`]'s constant of the same name.
    pub(crate) const FIELD: &'static str = "ExecOutcome::CopyText";

    /// Builds copy text, refusing a value that breaks any rule on [`CopyText`].
    ///
    /// # Errors
    ///
    /// [`ContentError::TooLong`] over [`MAX_COPY_TEXT`] bytes, or
    /// [`ContentError::ForbiddenChar`] naming the first refused control
    /// character in the value.
    pub fn new(value: impl Into<String>) -> Result<Self, ContentError> {
        let value = value.into();
        check_len(Self::FIELD, MAX_COPY_TEXT, value.len())?;
        if let Some(refused) = value
            .chars()
            .find(|c| c.is_control() && !ALLOWED_COPY_TEXT_CONTROLS.contains(c))
        {
            return Err(ContentError::ForbiddenChar {
                field: Self::FIELD,
                codepoint: refused as u32,
            });
        }
        Ok(Self(value))
    }

    /// The text as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the text, yielding the string inside.
    pub fn into_string(self) -> String {
        self.0
    }
}

/// The name arm of an [`IconSpec`](crate::item::IconSpec): a name to look up in
/// the desktop's icon theme, such as `firefox` or `application-x-executable`.
///
/// The inner string is private and the only way in is [`IconName::new`], so an
/// `IconName` that exists has passed every rule below, whether a provider built
/// it or it arrived off the socket.
///
/// # The rules, in the order they are applied
///
/// 1. **At most [`MAX_ICON_NAME`] bytes.** Delegated to the size budget and
///    applied first, so a value that is both too long and otherwise refusable is
///    reported as too long.
/// 2. **Not empty.** An empty name looks up nothing. The state it is trying to
///    express — this item has no icon — is said by leaving the item's `icon` out
///    altogether, and the whole point of making [`IconSpec`](crate::item::IconSpec)
///    an enum is that "neither a name nor a path" is unrepresentable; an empty
///    name would put that state back as a value.
/// 3. **No `/`.** This is the rule that keeps the two arms apart, and it is the
///    reason a name needs a content rule at all rather than only a bound. A
///    theme name is a key into a lookup, not a location. Whether a particular
///    icon loader would take a name holding a separator as a relative filename
///    is a property of that loader, and not one this crate can settle for every
///    client that will ever exist — so the rule refuses the shape instead of
///    betting on the behaviour. Without it, `name` would be a second channel for
///    a path-shaped value, one that arrives having passed none of the rules on
///    [`IconPath`]; that would make those rules optional, which is the same as
///    not having them. No icon theme names a file this way, so nothing honest is
///    refused.
/// 4. **No control character.** [`char::is_control`], Unicode's `Cc` category,
///    which reaches past ASCII to the C1 controls at U+0080–U+009F. None of them
///    means anything in a lookup key, and a name that resolves to nothing is
///    exactly the value that ends up quoted in a diagnostic — where an `ESC` is
///    a terminal control sequence and a newline is a second line. Nothing an
///    icon theme ships holds either.
///
/// Nothing here normalises, trims or case-folds. An accepted name is passed on
/// exactly as it arrived, so what a client looks up is what the provider sent.
///
/// # What is not checked
///
/// The rules decide that this is a name rather than a location. They do not
/// decide that the name *exists*: a name naming nothing in the installed theme
/// is accepted here and answered by whatever does the lookup, which falls back
/// to a generic icon. That is the right place for it — which names a theme
/// carries is a property of the machine, and this crate holds a contract, not an
/// inventory. The freedesktop icon naming specification's standard names are
/// where a provider should start, but conformance to it is not a rule here for
/// the same reason.
///
/// The newtype does not change the wire form: a name is still a bare JSON
/// string. What *did* change is the shape of the spec around it — see
/// [`IconSpec`](crate::item::IconSpec).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct IconName(String);

impl<'de> Deserialize<'de> for IconName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(deserializer, IconName::FIELD, MAX_ICON_NAME, IconName::new)
    }
}

impl IconName {
    /// The wire field this value travels in, named by every refusal of one, for
    /// the reason given on [`OpenUrl`]'s constant of the same name.
    pub(crate) const FIELD: &'static str = "IconSpec::Name";

    /// Builds a theme name, refusing one that breaks any rule on [`IconName`].
    ///
    /// # Errors
    ///
    /// [`ContentError`], naming the first rule the value broke, in the order
    /// documented on [`IconName`].
    pub fn new(value: impl Into<String>) -> Result<Self, ContentError> {
        let value = value.into();
        check_len(Self::FIELD, MAX_ICON_NAME, value.len())?;
        if value.is_empty() {
            return Err(ContentError::Empty { field: Self::FIELD });
        }
        if let Some(refused) = value.chars().find(|c| *c == '/' || c.is_control()) {
            return Err(ContentError::ForbiddenChar {
                field: Self::FIELD,
                codepoint: refused as u32,
            });
        }
        Ok(Self(value))
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the name, yielding the string inside.
    pub fn into_string(self) -> String {
        self.0
    }
}

/// The path arm of an [`IconSpec`](crate::item::IconSpec): an absolute path to
/// an icon file a client opens and decodes.
///
/// The inner string is private and the only way in is [`IconPath::new`], so an
/// `IconPath` that exists has passed every rule below however it was made. The
/// rules are what can be decided from the value alone; the one rule that needs
/// the filesystem is [`IconPath::open_regular_file`], which a caller runs
/// explicitly and which is the only place this crate makes a syscall.
///
/// That method is `#[cfg(unix)]`, because the flags it needs are, so the split
/// is not even across platforms: everywhere else it simply does not exist, and
/// an `IconPath` there carries the value rules and nothing more. No caller
/// silently loses the check — the method is absent, so code calling it does not
/// build — but a client for such a platform would have to supply its own
/// equivalent rather than inherit this one.
///
/// # The rules, in the order they are applied
///
/// 1. **At most [`MAX_ICON_PATH`] bytes.** Delegated to the size budget and
///    applied first, so a value that is both too long and otherwise refusable is
///    reported as too long.
/// 2. **Absolute.** A relative path is resolved against the *current working
///    directory of whoever opens it*, and that is the client's, which the
///    provider never knew and this contract cannot name. The same frame would
///    name a different file in every client, and a different one again after a
///    client changed directory — a wire value whose meaning depends on the
///    reader's state is not a wire value. This also disposes of the empty
///    string, which is refused as not absolute rather than by a rule of its own.
/// 3. **No `..` component.** Absolute is not the same as contained.
///    `/usr/share/icons/../../../etc/shadow` begins under a documented root and
///    ends outside every one of them, so a `..` turns the prefix check that
///    whoever resolves this path will do into a check that answers the wrong
///    question. Refusing the component is what makes that prefix check mean
///    something. It is the whole component `..` that is refused, not the two
///    characters wherever they fall: `..hidden.png` and `a..b.png` are ordinary
///    filenames and refusing them would refuse icons that exist. A `.` component
///    is left alone — it names the directory it sits in, so it cannot move a
///    path out of anything. Note what this rule does and does not buy: it makes
///    a textual prefix check answer the question it looks like it is answering.
///    It does not make the path *contained*, because the roots are not enforced
///    here at all and because a symlink under a root can still point outside it.
/// 4. **No NUL.** A NUL is where a C string ends, and the path becomes one
///    somewhere: at the syscall, or inside an image library reached through FFI,
///    or in whatever writes a cache key. So a value holding one is two different
///    paths depending on which side of that conversion reads it —
///    `/usr/share/icons/ok.png\0/etc/shadow` in Rust and
///    `/usr/share/icons/ok.png` past the conversion — and two readers
///    disagreeing about which file a value names is what a validated path exists
///    to prevent. Rust's own `File::open` refuses such a path rather than
///    truncating it, so this crate's own open is not the hazard; the rule is
///    here because a client's other uses of the string are, and because a
///    refusal at the parse names the hazard where an `InvalidInput` from an open
///    would not. A NUL is also a control character, so rule 5 would refuse it
///    anyway; this rule exists to name it, as the leading-dash rule on
///    [`OpenUrl`] does.
/// 5. **No control character.** [`char::is_control`], as on [`IconName`] and for
///    the same reason: a refused path is reported, and a reported path goes into
///    a log line and onto somebody's terminal.
///
/// Nothing here normalises, canonicalises or trims. An accepted path is passed
/// on exactly as it arrived, so what a client opens is what the provider sent,
/// and a later reader of the value cannot disagree with the rules that accepted
/// it.
///
/// # Where an icon is expected to live
///
/// These roots are the contract's *documented* obligation on whoever resolves an
/// icon path. They are **not** enforced by [`IconPath::new`] — a path outside all
/// of them is accepted here, which the test
/// `tests::icon_path_does_not_enforce_the_documented_roots` pins:
///
/// - `$XDG_DATA_DIRS/icons` — the freedesktop icon theme specification's search
///   path, `/usr/share/icons` and `/usr/local/share/icons` by default, plus
///   whatever Flatpak and Snap add to `XDG_DATA_DIRS` for exported applications.
/// - `/usr/share/pixmaps` — the legacy flat directory the same specification
///   keeps as a fallback, and where a good deal of packaged software still puts
///   its icon.
/// - `~/.icons` and `$XDG_DATA_HOME/icons` — the per-user themes.
///
/// # What documenting the roots instead of enforcing them costs
///
/// It costs the guarantee the issue behind this type asked for. A provider may
/// send `/home/user/.ssh/id_ed25519` or `/proc/self/mem`; both pass every rule
/// above, and — this is the half worth stating explicitly — both are *regular
/// files*, so [`IconPath::open_regular_file`] does not refuse them either. A
/// client that opens what it is told to open is then the confused deputy
/// performing a read the provider could not: it holds the user's session, and
/// the provider may be a sandboxed plugin. That is the whole of what is left
/// open, and it is left open deliberately.
///
/// The alternative — a root allow-list in the constructor, checked the way
/// [`ALLOWED_URL_SCHEMES`] is checked — was rejected because the list is not
/// knowable here. It is `$XDG_DATA_DIRS` at the moment of the check, plus
/// `$XDG_DATA_HOME`, plus whatever Flatpak and Snap contribute if they happen to
/// be installed. So the same frame would be valid on the machine that sent it
/// and refused on the machine beside it, and a contract whose validity is a
/// property of the reader's environment is not a contract: it could not be
/// tested against a fixture, a provider could not tell in advance whether its
/// output would parse, and the refusal a client saw would name a rule the
/// provider's machine does not have. Worse, the check would have to run at the
/// parse, where reading the environment is a side effect on a path that must
/// have none.
///
/// So the check belongs where the environment is known and where the file is
/// about to be read — the client's icon resolution — and what this type
/// contributes to it is rules 2 and 3, which are what make a prefix check
/// against a resolved root answerable at all.
///
/// The newtype does not change the wire form: a path is still a bare JSON
/// string. What *did* change is the shape of the spec around it — see
/// [`IconSpec`](crate::item::IconSpec).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct IconPath(String);

impl<'de> Deserialize<'de> for IconPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        limits::validated(deserializer, IconPath::FIELD, MAX_ICON_PATH, IconPath::new)
    }
}

impl IconPath {
    /// The wire field this value travels in, named by every refusal of one, for
    /// the reason given on [`OpenUrl`]'s constant of the same name.
    pub(crate) const FIELD: &'static str = "IconSpec::Path";

    /// Builds an icon path, refusing one that breaks any rule on [`IconPath`].
    ///
    /// This applies the rules that can be decided from the value. It makes no
    /// syscall and touches no filesystem, so it says nothing about whether the
    /// path exists or what kind of file it names — that is
    /// [`IconPath::open_regular_file`], and the split is deliberate: see that
    /// method for why.
    ///
    /// # Errors
    ///
    /// [`ContentError`], naming the first rule the value broke, in the order
    /// documented on [`IconPath`].
    pub fn new(value: impl Into<String>) -> Result<Self, ContentError> {
        let value = value.into();
        check_len(Self::FIELD, MAX_ICON_PATH, value.len())?;
        if !value.starts_with('/') {
            return Err(ContentError::NotAbsolute { field: Self::FIELD });
        }
        if value.split('/').any(|component| component == "..") {
            return Err(ContentError::ParentComponent { field: Self::FIELD });
        }
        if value.contains('\0') {
            return Err(ContentError::InteriorNul { field: Self::FIELD });
        }
        if let Some(refused) = value.chars().find(|c| c.is_control()) {
            return Err(ContentError::ForbiddenChar {
                field: Self::FIELD,
                codepoint: refused as u32,
            });
        }
        Ok(Self(value))
    }

    /// The path as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path as a [`Path`].
    ///
    /// Borrowed rather than built, so this allocates nothing and the value it
    /// returns is the same bytes the rules above accepted.
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// Consumes the path, yielding the string inside.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Opens the path, and hands back the file only if what was opened is a
    /// regular file.
    ///
    /// This is the rule on [`IconPath`] that cannot be decided from the value,
    /// and it is the only place `hop-protocol` makes a syscall. A caller runs it
    /// explicitly, before it reads or decodes anything.
    ///
    /// # This does not refuse anything, in this crate's sense of the word
    ///
    /// A **refusal** here is a value a gate would not build: a constructor
    /// returns one, and off the parse it sinks the whole frame. Nothing of the
    /// sort happens here. The frame parsed long ago, the [`IconPath`] is a
    /// perfectly valid value that passed every rule it could be judged by, and
    /// this method reports a fact about the machine rather than about the frame.
    /// So the vocabulary is deliberately plainer: a directory, a device or a
    /// FIFO is **not opened** — the descriptor is dropped unread and the caller
    /// gets an [`IconOpenError`] instead of a file. Nothing is refused and
    /// nothing is rejected, both of which mean something else in this codebase.
    ///
    /// # Why this is a method and not part of `Deserialize`
    ///
    /// Every other rule in this crate is applied at the parse, and this one
    /// deliberately is not. Putting the check inside `Deserialize` was
    /// considered and rejected on two counts. It would be a syscall per item per
    /// `results` frame — up to
    /// [`MAX_ITEMS_PER_RESULTS_FRAME`](crate::limits::MAX_ITEMS_PER_RESULTS_FRAME)
    /// opens to parse one frame, on the query path, which `CONTEXT.md` says may
    /// not touch disk at all. And it would not even be correct: the file a parse
    /// stats and the file a client later opens are two different opens, so a
    /// path could be a regular file at the parse and a FIFO by the time the icon
    /// is loaded. A check at the parse would buy latency and a false assurance.
    ///
    /// Leaving the rule to the client entirely was the other alternative, and it
    /// is rejected because the contract is the one place the rule can be stated
    /// once for every client that will ever exist. Stating it here as a method
    /// means the next client inherits it rather than rediscovering it.
    ///
    /// # How the check avoids racing itself
    ///
    /// It opens first and inspects the **descriptor**, never the path. Stat-then-
    /// open is a race — the path can be replaced between the two calls, and the
    /// thing checked is then not the thing opened. Open-then-fstat cannot race,
    /// because the descriptor *is* the file: whatever the path pointed at, the
    /// file this call returns is the file it inspected.
    ///
    /// The open carries `O_NONBLOCK`. Opening a FIFO for reading otherwise waits
    /// for a writer, which for a FIFO nobody writes to means waiting forever, on
    /// whatever thread the client renders from — and it happens *before* any
    /// fstat could run, so no amount of checking afterwards would help. On a
    /// regular file the flag has no effect at all, so the descriptor handed back
    /// behaves normally and a caller need not clear it.
    ///
    /// # Why not `O_NOFOLLOW`
    ///
    /// Declining to follow a symlink is the obvious next flag and it is
    /// deliberately not set. Icon themes are built out of symlinks —
    /// `/usr/share/icons/hicolor` is largely links between sizes and themes — so
    /// `O_NOFOLLOW` would leave ordinary icons unopened on an ordinary desktop,
    /// which is the one failure worse than a loose rule.
    ///
    /// What it would buy is close to nothing here, for two reasons. The fstat
    /// already sees what was *actually* opened, so a symlink pointing at
    /// `/dev/zero` or at a FIFO is not opened, exactly as the device or the FIFO
    /// itself is not — pinned by
    /// `tests::a_symlink_to_a_character_device_is_not_opened`. And
    /// `O_NOFOLLOW` only ever governs the final component: a symlink among the
    /// parent directories is followed regardless, so it could not deliver "this
    /// file really is under the root its path names" even if that were the
    /// question being asked, and it is not — the roots are documented rather
    /// than enforced.
    ///
    /// # What allowing a symlink costs
    ///
    /// It leaves a link under an allowed root able to point at any regular file
    /// the client can read: an icon path that reads as `~/.icons/x.png` can
    /// deliver `~/.ssh/id_ed25519` to the decoder. That is a real gap and it is
    /// accepted, because closing it here would close nothing — a provider that
    /// wanted the same read can simply send `/home/user/.ssh/id_ed25519`
    /// directly, and that path passes every rule on [`IconPath`] too. The
    /// exposure is the roots being unenforced, not symlinks being followed, and
    /// it is priced under [`IconPath`]'s own heading on that. Whoever does
    /// enforce the roots and wants "the bytes came from under the root" has to
    /// resolve the path rather than compare its prefix; `O_NOFOLLOW` would not
    /// have given it that either.
    ///
    /// # What this does not check
    ///
    /// - **Not the roots.** A regular file anywhere is opened. `/proc/self/mem`
    ///   is a regular file by its mode and passes here — pinned by
    ///   `tests::a_procfs_file_is_opened_because_it_is_a_regular_file`. The
    ///   regular-file rule and the root rule are different rules, and only the
    ///   first of them is applied anywhere in this crate.
    /// - **Not the size, and not the contents.** A 4 GB PNG and a decompression
    ///   bomb are both regular files. Decode limits belong to the image library
    ///   and to whatever calls it.
    /// - **Not who may read it.** The open runs with the client's credentials,
    ///   so a file the provider could not read and the client can is read
    ///   successfully. That is the confused-deputy shape the roots exist to
    ///   contain.
    ///
    /// # Errors
    ///
    /// [`IconOpenError::Open`] if the open failed, [`IconOpenError::Stat`] if
    /// the descriptor could not be inspected, and
    /// [`IconOpenError::NotARegularFile`] if what was opened is a directory, a
    /// device, a socket or a FIFO.
    #[cfg(unix)]
    pub fn open_regular_file(&self) -> Result<std::fs::File, IconOpenError> {
        use std::os::unix::fs::OpenOptionsExt;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.0)
            .map_err(|source| IconOpenError::Open {
                field: Self::FIELD,
                source,
            })?;

        // `File::metadata` is fstat on this descriptor, which is the whole
        // point: it reports the file that was opened rather than whatever the
        // path names now.
        let metadata = file.metadata().map_err(|source| IconOpenError::Stat {
            field: Self::FIELD,
            source,
        })?;
        if !metadata.is_file() {
            return Err(IconOpenError::NotARegularFile { field: Self::FIELD });
        }
        Ok(file)
    }
}

/// A failure of [`IconPath::open_regular_file`].
///
/// Separate from [`ContentError`], which is what a *value* is refused with, and
/// separate on purpose. This is not a refusal: the path passed every rule it
/// could be judged by, no frame is sunk, and what this reports is a fact about
/// the machine rather than about the frame. Nor is it a rejection, which in this
/// codebase is an item that assembly declined. Keeping the two error types apart
/// also keeps [`ContentError`] `Clone`, `PartialEq` and `Eq`, which
/// `std::io::Error` is none of.
///
/// Every variant names the wire field the path came from, as [`ContentError`]'s
/// do, and none carries the path. The path is peer-controlled, and an error is a
/// value a caller may format wherever it formats errors — what that is has not
/// been decided: no seam in this codebase covers the point where a *client*
/// formats a value like this one. `hop-core`'s `ProviderLog` seam exists now,
/// but it is scoped to provider events on the daemon side of the wire, not to
/// a client rendering an icon-open failure — so the field plus the rule is
/// what a reader gets.
#[cfg(unix)]
#[derive(Debug, Error)]
pub enum IconOpenError {
    /// The open itself failed — no such file, no permission, too many symlinks.
    #[error("{field} could not be opened")]
    Open {
        /// The wire field the path came from.
        field: &'static str,
        /// What the open failed with.
        #[source]
        source: std::io::Error,
    },
    /// The descriptor was opened but could not be inspected.
    #[error("{field} could not be inspected once open")]
    Stat {
        /// The wire field the path came from.
        field: &'static str,
        /// What the inspection failed with.
        #[source]
        source: std::io::Error,
    },
    /// What was opened is a directory, a device, a socket or a FIFO.
    ///
    /// Reported of the descriptor, so this is what the file *is*, not what the
    /// path looked like.
    #[error("{field} is not a regular file")]
    NotARegularFile {
        /// The wire field the path came from.
        field: &'static str,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;
    use std::path::Path;
    #[cfg(unix)]
    use std::{
        ffi::CString, io::Read, os::unix::ffi::OsStrExt, os::unix::fs::symlink, sync::mpsc,
        time::Duration,
    };

    use crate::item::IconSpec;
    use crate::limits::{MAX_ICON_NAME, MAX_ICON_PATH};
    use crate::wire::{DaemonMsg, ExecOutcome};

    // --- IconName -----------------------------------------------------------

    #[test]
    fn icon_name_accepts_an_ordinary_theme_name() {
        for name in ["firefox", "application-x-executable", "utilities-terminal"] {
            assert_eq!(IconName::new(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn icon_name_refuses_a_slash_because_a_name_is_not_a_path() {
        for name in ["/usr/share/pixmaps/firefox.png", "hicolor/firefox", "a/b"] {
            assert_eq!(
                IconName::new(name).unwrap_err(),
                ContentError::ForbiddenChar {
                    field: IconName::FIELD,
                    codepoint: u32::from(b'/'),
                },
                "a name carrying a separator is a path in disguise, accepted {name:?}"
            );
        }
    }

    #[test]
    fn icon_name_refuses_a_control_character() {
        for name in [
            "fire\u{1b}fox",
            "firefox\n",
            "fire\u{7f}fox",
            "fire\u{85}fox",
        ] {
            let err = IconName::new(name).unwrap_err();
            assert!(
                matches!(err, ContentError::ForbiddenChar { .. }),
                "a theme name carries no control character, got {err} for {name:?}"
            );
        }
    }

    #[test]
    fn icon_name_refuses_an_empty_value() {
        assert_eq!(
            IconName::new("").unwrap_err(),
            ContentError::Empty {
                field: IconName::FIELD
            }
        );
    }

    #[test]
    fn icon_name_refuses_a_value_over_its_byte_bound() {
        assert!(IconName::new("a".repeat(MAX_ICON_NAME)).is_ok());
        assert!(matches!(
            IconName::new("a".repeat(MAX_ICON_NAME + 1)).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    #[test]
    fn icon_name_checks_its_length_before_its_content() {
        let over = format!("/{}", "a".repeat(MAX_ICON_NAME));
        assert!(matches!(
            IconName::new(over).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    // --- IconPath: what it accepts ------------------------------------------

    #[test]
    fn icon_path_accepts_an_absolute_path_under_a_standard_root() {
        for path in [
            "/usr/share/icons/hicolor/48x48/apps/firefox.png",
            "/usr/share/pixmaps/firefox.png",
            "/home/user/.icons/custom/thing.svg",
        ] {
            assert_eq!(IconPath::new(path).unwrap().as_str(), path);
        }
    }

    #[test]
    fn icon_path_accepts_a_dotted_name_that_is_not_a_parent_component() {
        // The rule refuses the component `..`, not the two characters wherever
        // they fall: honest files are named like this and refusing them would
        // refuse icons that exist.
        for path in [
            "/usr/share/icons/..hidden.png",
            "/usr/share/icons/a..b.png",
            "/usr/share/icons/...",
            "/usr/share/./icons/firefox.png",
        ] {
            assert!(
                IconPath::new(path).is_ok(),
                "only a whole `..` component is refused, refused {path:?}"
            );
        }
    }

    #[test]
    fn icon_path_does_not_enforce_the_documented_roots() {
        // Pins the documented gap rather than an intended feature: the roots are
        // stated on the type as an obligation on whoever resolves the path, and
        // are deliberately not a rule here. See `IconPath`'s own section for
        // what that choice costs.
        for path in ["/tmp/anything.png", "/proc/self/mem", "/etc/shadow"] {
            assert!(
                IconPath::new(path).is_ok(),
                "the roots are documented, not enforced, refused {path:?}"
            );
        }
    }

    // --- IconPath: what it refuses ------------------------------------------

    #[test]
    fn icon_path_refuses_a_relative_path() {
        for path in ["icons/firefox.png", "./firefox.png", "../firefox.png", ""] {
            assert_eq!(
                IconPath::new(path).unwrap_err(),
                ContentError::NotAbsolute {
                    field: IconPath::FIELD
                },
                "a relative path names a different file in every client, accepted {path:?}"
            );
        }
    }

    #[test]
    fn icon_path_refuses_a_parent_component() {
        for path in [
            "/usr/share/icons/../../../etc/shadow",
            "/..",
            "/usr/../etc/shadow",
            "/usr/share/icons/..",
        ] {
            assert_eq!(
                IconPath::new(path).unwrap_err(),
                ContentError::ParentComponent {
                    field: IconPath::FIELD
                },
                "a `..` makes a prefix check a lie, accepted {path:?}"
            );
        }
    }

    #[test]
    fn icon_path_refuses_an_interior_nul() {
        // Named as its own refusal rather than reported as a control character:
        // a NUL terminates the C string the path becomes at the syscall, so a
        // checker reading the whole value and the kernel opening a prefix of it
        // disagree about which file this is.
        let err = IconPath::new("/usr/share/icons/ok.png\u{0}/etc/shadow").unwrap_err();
        assert_eq!(
            err,
            ContentError::InteriorNul {
                field: IconPath::FIELD
            }
        );
    }

    #[test]
    fn icon_path_refuses_a_control_character() {
        for path in [
            "/usr/share/icons/a\u{1b}b.png",
            "/usr/share/icons/a\nb.png",
            "/usr/share/icons/a\u{7f}b.png",
            "/usr/share/icons/a\u{9f}b.png",
        ] {
            let err = IconPath::new(path).unwrap_err();
            assert!(
                matches!(err, ContentError::ForbiddenChar { .. }),
                "a path carries no control character, got {err} for {path:?}"
            );
        }
    }

    #[test]
    fn icon_path_refuses_a_value_over_its_byte_bound() {
        let at_bound = format!("/{}", "a".repeat(MAX_ICON_PATH - 1));
        assert_eq!(at_bound.len(), MAX_ICON_PATH);
        assert!(IconPath::new(&at_bound).is_ok());

        assert!(matches!(
            IconPath::new(format!("{at_bound}a")).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    #[test]
    fn icon_path_checks_its_length_before_its_content() {
        // A value breaking both gates is reported as over-long, because the
        // length check runs first — the same order the parse applies.
        let over = format!("relative/{}", "a".repeat(MAX_ICON_PATH));
        assert!(matches!(
            IconPath::new(over).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    #[test]
    fn the_icon_accessors_return_what_was_built() {
        // Nothing here normalises or trims: an accepted value is handed on
        // exactly as it arrived, so what a client looks up or opens is what the
        // provider sent.
        let name = IconName::new("firefox").unwrap();
        assert_eq!(name.as_str(), "firefox");
        assert_eq!(name.into_string(), "firefox");

        let path = IconPath::new("/usr/share/pixmaps/firefox.png").unwrap();
        assert_eq!(path.as_str(), "/usr/share/pixmaps/firefox.png");
        assert_eq!(path.as_path(), Path::new("/usr/share/pixmaps/firefox.png"));
        assert_eq!(path.into_string(), "/usr/share/pixmaps/firefox.png");
    }

    // --- The docs' pointers into this module --------------------------------

    /// Every test this file's docs name by hand must exist, so that renaming one
    /// fails here instead of leaving a doc pointing at nothing.
    ///
    /// A pointer is a backticked `tests::` followed by the test's name. The
    /// qualifier is what makes it findable: it marks a backticked token as a
    /// pointer into this module rather than one of the API names the same docs
    /// mention in backticks, so nothing this file says about `append_to_end`,
    /// `de_item_copy_text` or any other multi-word identifier can be mistaken
    /// for one. It also reads as the path it is.
    ///
    /// The pointers are prose rather than intra-doc links on purpose. A link to
    /// a `#[cfg(test)]` item cannot resolve in a doc build — there is no `tests`
    /// module for rustdoc to find — so `cargo doc` answers each one with
    /// `unresolved link`. On a *private* item that passes unnoticed, rustdoc
    /// having no reason to process its docs, which is why the one link of this
    /// kind in [`limits`] is silent; every pointer in this file is on a public
    /// item, where the same link is a warning apiece and still never verified
    /// against anything. This test is the verification that link would not have
    /// been.
    #[test]
    fn every_test_this_file_names_in_its_docs_exists() {
        let source = include_str!("content.rs");
        let named: Vec<&str> = source
            .lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("///") || line.starts_with("//!"))
            // Odd-indexed pieces are what sat between a pair of backticks.
            .flat_map(|line| line.split('`').skip(1).step_by(2))
            .filter_map(|token| token.strip_prefix("tests::"))
            // What follows the qualifier has to be an identifier to be a
            // pointer; this is what keeps the marker's own mention above, and
            // any prose placeholder, out of the scan.
            .filter(|name| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            })
            .collect();

        assert!(
            named.len() >= 8,
            "the docs name at least eight tests by hand; finding {} means this \
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

    // --- OpenUrl: what it accepts -------------------------------------------

    #[test]
    fn open_url_accepts_an_ordinary_url() {
        let url = "https://example.com/search?q=hop&lang=en#top";
        assert_eq!(OpenUrl::new(url).unwrap().as_str(), url);
    }

    #[test]
    fn open_url_accepts_every_allowed_scheme() {
        for scheme in ALLOWED_URL_SCHEMES {
            // An opaque path rather than an authority, so the one gate under
            // test is the scheme: nothing here parses what follows the colon.
            let url = format!("{scheme}:example.com");
            assert!(
                OpenUrl::new(&url).is_ok(),
                "an allowed scheme must be accepted, refused {url}"
            );
        }
    }

    #[test]
    fn open_url_accepts_an_allowed_scheme_in_any_ascii_case() {
        for scheme in ALLOWED_URL_SCHEMES {
            let url = format!("{}:example.com", scheme.to_uppercase());
            assert!(
                OpenUrl::new(&url).is_ok(),
                "scheme comparison is ASCII-case-insensitive, refused {url}"
            );
        }
    }

    #[test]
    fn open_url_accepts_characters_rfc_3986_would_percent_encode() {
        // Rule 4 is about the two characters that let a value be split or
        // re-read, not about RFC 3986 legality. These all require encoding to
        // appear in a URL and are all accepted, which is what keeps the rule
        // from being read as "RFC-legal characters only".
        for url in [
            "https://example.com/café",
            "https://example.com/a<b>c",
            "https://example.com/a\"b",
            "https://example.com/{a}|b",
            "https://example.com/a\\b^c`d",
            // Non-ASCII is accepted up to, and only up to, `Cc`: a no-break
            // space, a line separator, a bidi override, and an emoji above the
            // BMP beside an ideograph inside it all pass, while the C1 controls
            // beside them do not — see the companion test below.
            "https://example.com/a\u{a0}b",
            "https://example.com/a\u{2028}b",
            "https://example.com/a\u{202e}b",
            "https://example.com/a\u{1f600}\u{4e2d}b",
        ] {
            assert!(
                OpenUrl::new(url).is_ok(),
                "this rule refuses only space and control characters, refused {url:?}"
            );
        }
    }

    #[test]
    fn open_url_does_not_check_what_follows_the_scheme() {
        // Pins the documented gap rather than an intended feature: nothing here
        // parses the URL, so an absent authority, a misleading userinfo and a
        // homograph host all pass. Each belongs to whatever shows or opens the
        // URL, and closing them needs a parser this crate does not carry.
        for url in [
            "https:",
            "https://",
            "http:example.com",
            "https://evil.com@good.com/",
            "https://xn--80ak6aa92e.com/",
            "https://ехample.com/",
        ] {
            assert!(OpenUrl::new(url).is_ok(), "unexpectedly refused {url:?}");
        }
    }

    #[test]
    fn open_url_keeps_the_value_it_was_given_whole() {
        // Nothing here normalises, trims or percent-encodes: an accepted URL is
        // handed on exactly as it arrived, so what a client opens is what the
        // provider sent.
        let url = "HTTPS://Example.COM/A%20b?q=%2F";
        assert_eq!(OpenUrl::new(url).unwrap().into_string(), url);
    }

    // --- OpenUrl: what it refuses -------------------------------------------

    #[test]
    fn open_url_refuses_a_local_file_url() {
        let err = OpenUrl::new("file:///home/user/.ssh/id_ed25519").unwrap_err();
        assert_eq!(
            err,
            ContentError::SchemeNotAllowed {
                field: OpenUrl::FIELD
            }
        );
    }

    #[test]
    fn open_url_refuses_script_ish_schemes() {
        for url in [
            "javascript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "vbscript:msgbox(1)",
        ] {
            assert_eq!(
                OpenUrl::new(url).unwrap_err(),
                ContentError::SchemeNotAllowed {
                    field: OpenUrl::FIELD
                },
                "the scheme must be what is named as refused, for {url}"
            );
        }
    }

    #[test]
    fn open_url_refuses_a_scheme_that_merely_resembles_an_allowed_one() {
        for url in [
            "httpx://example.com",
            "https-evil://example.com",
            "xhttps://example.com",
            // The scheme is what precedes the *first* colon, so an allowed
            // scheme later in the value does not rescue a refused one.
            "file:https://example.com",
        ] {
            assert!(
                OpenUrl::new(url).is_err(),
                "only an exact allowed scheme may pass, accepted {url}"
            );
        }
    }

    #[test]
    fn open_url_refuses_a_value_beginning_with_a_dash() {
        for url in ["-o", "--output=/tmp/x", "-https://example.com"] {
            assert_eq!(
                OpenUrl::new(url).unwrap_err(),
                ContentError::LeadingDash {
                    field: OpenUrl::FIELD
                },
                "a leading dash must be named as such, for {url}"
            );
        }
    }

    #[test]
    fn open_url_refuses_a_value_with_no_scheme_at_all() {
        for url in ["", "example.com", "//example.com", "/etc/passwd"] {
            assert!(
                OpenUrl::new(url).is_err(),
                "a value without an allowed scheme must be refused, accepted {url:?}"
            );
        }
    }

    #[test]
    fn open_url_refuses_a_space_or_a_control_character() {
        for url in [
            "https://example.com/a b",
            "https://example.com/a\nb",
            "https://example.com/a\rb",
            "https://example.com/a\u{0}b",
            "https://example.com/a\u{7f}b",
            // The C1 controls are non-ASCII and are refused all the same, at
            // both ends of the range. This is the half of rule 4 that the
            // accepting test above must not be read as contradicting.
            "https://example.com/a\u{80}b",
            "https://example.com/a\u{85}b",
            "https://example.com/a\u{9f}b",
        ] {
            let err = OpenUrl::new(url).unwrap_err();
            assert!(
                matches!(err, ContentError::ForbiddenChar { .. }),
                "a URL must carry no space and no control character, got {err} for {url:?}"
            );
        }
    }

    #[test]
    fn open_url_refuses_a_value_over_its_byte_bound() {
        let opening = "https://example.com/";
        let at_bound = format!("{opening}{}", "a".repeat(MAX_OPEN_URL - opening.len()));
        assert_eq!(at_bound.len(), MAX_OPEN_URL);
        assert!(OpenUrl::new(&at_bound).is_ok());

        assert!(matches!(
            OpenUrl::new(format!("{at_bound}a")).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    #[test]
    fn open_url_checks_its_length_before_its_content() {
        // A value that breaks both rules is reported as over-long, because the
        // length check runs first — the same order the parse applies.
        let over = format!("file:///{}", "a".repeat(MAX_OPEN_URL));
        assert!(matches!(
            OpenUrl::new(over).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    #[test]
    fn no_allowed_scheme_could_be_read_as_a_flag() {
        // The scheme allow-list already excludes every value a launcher would
        // read as an option, which is why the leading-dash rule is documented
        // as redundant-but-kept rather than as the thing doing the work.
        for scheme in ALLOWED_URL_SCHEMES {
            assert!(
                scheme.starts_with(|c: char| c.is_ascii_alphabetic()),
                "a scheme starting with anything but a letter would change that, got {scheme}"
            );
        }
    }

    // --- CopyText -----------------------------------------------------------

    #[test]
    fn copy_text_accepts_ordinary_text() {
        for text in ["", "42", "https://example.com", "café ☕ 家"] {
            assert!(
                CopyText::new(text).is_ok(),
                "ordinary text must be accepted, refused {text:?}"
            );
        }
    }

    #[test]
    fn copy_text_accepts_the_control_characters_it_explicitly_allows() {
        for control in ALLOWED_COPY_TEXT_CONTROLS {
            let text = format!("line one{control}line two");
            assert!(
                CopyText::new(&text).is_ok(),
                "an explicitly allowed control must be accepted, refused {text:?}"
            );
        }
    }

    #[test]
    fn copy_text_refuses_every_control_character_it_does_not_allow() {
        // The whole Cc category — C0, DEL and C1 — minus the allow-list.
        let controls = (0..=0x1f_u32).chain(0x7f..=0x9f);
        for codepoint in controls {
            let control = char::from_u32(codepoint).unwrap();
            if ALLOWED_COPY_TEXT_CONTROLS.contains(&control) {
                continue;
            }
            assert_eq!(
                CopyText::new(format!("ok{control}")).unwrap_err(),
                ContentError::ForbiddenChar {
                    field: CopyText::FIELD,
                    codepoint,
                },
                "U+{codepoint:04X} must be refused and named"
            );
        }
    }

    #[test]
    fn copy_text_refuses_carriage_return_even_though_it_allows_newline() {
        // The asymmetry the policy turns on, asserted against the allow-list
        // itself so that flipping either membership fails here rather than
        // leaving these three cases quietly testing the wrong pair.
        assert!(ALLOWED_COPY_TEXT_CONTROLS.contains(&'\n'));
        assert!(!ALLOWED_COPY_TEXT_CONTROLS.contains(&'\r'));

        assert!(CopyText::new("a\nb").is_ok());
        assert!(CopyText::new("a\rb").is_err());
        // CRLF is refused whole: the pair is not a unit here, and the carriage
        // return half is enough on its own.
        assert!(CopyText::new("a\r\nb").is_err());
    }

    #[test]
    fn copy_text_does_not_refuse_the_non_cc_separators_and_format_characters() {
        // Pins the documented gap rather than an intended feature: these are a
        // display-spoofing concern, not a terminal-control one, and this type
        // does not address display spoofing.
        for text in ["a\u{2028}b", "a\u{2029}b", "a\u{202e}b", "a\u{200e}b"] {
            assert!(CopyText::new(text).is_ok(), "unexpectedly refused {text:?}");
        }
    }

    #[test]
    fn copy_text_refuses_a_value_over_its_byte_bound() {
        assert!(CopyText::new("a".repeat(MAX_COPY_TEXT)).is_ok());
        assert!(matches!(
            CopyText::new("a".repeat(MAX_COPY_TEXT + 1)).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    #[test]
    fn copy_text_checks_its_length_before_its_content() {
        let over = format!("\r{}", "a".repeat(MAX_COPY_TEXT));
        assert!(matches!(
            CopyText::new(over).unwrap_err(),
            ContentError::TooLong(_)
        ));
    }

    // --- Opening an icon path -----------------------------------------------

    // The rule that needs the filesystem, exercised against real files. Every
    // case is a kind of file a hostile provider would point a client at, and
    // each is caught by the descriptor's own type rather than by anything read
    // off the path — see `IconPath::open_regular_file` for why that distinction
    // is the whole design.
    //
    // These say "not opened" rather than "refused" or "rejected", both of which
    // name something else in this codebase: nothing here sinks a frame or
    // declines an item, and the path itself was perfectly valid.
    //
    // These are flat rather than in a nested module so that every `tests::`
    // pointer in this file's docs stays a single path segment, which is what
    // `every_test_this_file_names_in_its_docs_exists` verifies.

    /// A validated [`IconPath`] for a path a test just created.
    ///
    /// Every path here is under a temporary directory rather than under one of
    /// the roots [`IconPath`] documents, which is the documented-not-enforced
    /// choice showing up in the tests: if the roots were a rule, none of these
    /// would construct.
    #[cfg(unix)]
    fn temp_icon_path(path: &Path) -> IconPath {
        IconPath::new(path.to_str().expect("a test path is UTF-8")).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn opening_an_icon_path_yields_the_regular_file_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firefox.png");
        std::fs::write(&path, b"not really a png").unwrap();

        let mut file = temp_icon_path(&path).open_regular_file().unwrap();
        let mut read = Vec::new();
        file.read_to_end(&mut read).unwrap();
        assert_eq!(
            read, b"not really a png",
            "the descriptor handed back must be the file the path named, readable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        // A directory opens read-only on Linux without complaint — only `read`
        // fails, and by then a caller has a descriptor it believes is an icon.
        // So this case is the fstat's, not the open's.
        let err = temp_icon_path(dir.path()).open_regular_file().unwrap_err();
        assert!(
            matches!(err, IconOpenError::NotARegularFile { .. }),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_character_device_is_not_opened() {
        // `/dev/zero` is the endless-read case from the issue: it opens, it
        // never ends, and a decoder reading it to the end never returns.
        let err = IconPath::new("/dev/zero")
            .unwrap()
            .open_regular_file()
            .unwrap_err();
        assert!(
            matches!(err, IconOpenError::NotARegularFile { .. }),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_opened_and_the_open_does_not_block() {
        // The hazard here happens *before* any check could run: opening a FIFO
        // for reading blocks until a writer appears, so a client on a
        // UI-adjacent thread would stop there forever and never reach the
        // fstat. `O_NONBLOCK` is what makes the open return.
        //
        // This test is therefore written so that losing that flag makes it
        // *fail* rather than hang: the open runs on a worker thread and the
        // result is awaited with a timeout. A hanging test tells nobody
        // anything, and a test whose only protection from hanging is the guard
        // under test is not a test of that guard.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fifo.png");
        // `libc::mkfifo` rather than `mkfifo(1)`: the crate this change already
        // takes on for `O_NONBLOCK` carries the call, so using it keeps the
        // suite from depending on coreutils being installed to test a guard
        // that has nothing to do with coreutils.
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c_path` is a live NUL-terminated string for the duration of
        // the call, which is all `mkfifo(3)` requires of it.
        //
        // The workspace denies `unsafe_code` (see `[workspace.lints.rust]` in
        // the root `Cargo.toml`), and this statement is the only exception in
        // the tree. It is scoped to the statement rather than the function or
        // the module so that a second `unsafe` added anywhere near it still
        // fails the build. `expect` rather than `allow`: if this call ever goes
        // away — a safe wrapper, a different way of making the FIFO — the
        // unfulfilled expectation becomes a warning, and CI's `-D warnings`
        // turns it into an error, so the exception deletes itself instead of
        // outliving its reason.
        #[expect(
            unsafe_code,
            reason = "mkfifo(3) has no safe wrapper in libc; test-only, and production code has none"
        )]
        let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(
            made,
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );

        let (tx, rx) = mpsc::channel();
        let opened = temp_icon_path(&path);
        std::thread::spawn(move || {
            let _ = tx.send(opened.open_regular_file().map(|_| ()));
        });

        let result = rx.recv_timeout(Duration::from_secs(10)).expect(
            "opening a FIFO must return rather than wait for a writer; \
             timing out here means the open blocked",
        );
        let err = result.expect_err("a FIFO is not a regular file");
        assert!(
            matches!(err, IconOpenError::NotARegularFile { .. }),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn opening_a_symlink_to_a_regular_file_succeeds() {
        // Icon themes are built out of symlinks — `/usr/share/icons/hicolor` is
        // full of them — so declining to follow one would leave ordinary icons
        // unopened. This is the case `O_NOFOLLOW` would have broken.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.png");
        let link = dir.path().join("link.png");
        std::fs::write(&target, b"icon bytes").unwrap();
        symlink(&target, &link).unwrap();

        let mut file = temp_icon_path(&link).open_regular_file().unwrap();
        let mut read = Vec::new();
        file.read_to_end(&mut read).unwrap();
        assert_eq!(read, b"icon bytes");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_character_device_is_not_opened() {
        // The other half of the pair above, and the reason allowing symlinks
        // costs nothing here: the fstat sees what was actually opened, not what
        // the path said, so a link pointing at a device is stopped by the same
        // check as the device itself.
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("innocent.png");
        symlink("/dev/zero", &link).unwrap();

        let err = temp_icon_path(&link).open_regular_file().unwrap_err();
        assert!(
            matches!(err, IconOpenError::NotARegularFile { .. }),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn opening_a_path_that_is_not_there_reports_the_open_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = temp_icon_path(&dir.path().join("absent.png"))
            .open_regular_file()
            .unwrap_err();
        assert!(matches!(err, IconOpenError::Open { .. }), "got: {err}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_procfs_file_is_opened_because_it_is_a_regular_file() {
        // Pins the documented gap rather than an intended feature.
        // `/proc/self/mem` is the path the issue behind this type names, and it
        // is a *regular file* by its mode — so this check accepts it, and what
        // excludes it is the documented roots, which are not enforced here.
        // Stated as a test so the docs and the behaviour cannot drift.
        let file = IconPath::new("/proc/self/mem")
            .unwrap()
            .open_regular_file()
            .expect("procfs files stat as regular, so this check accepts them");
        assert!(file.metadata().unwrap().is_file());
    }

    // --- The deserialization boundary ---------------------------------------

    fn executed_frame(outcome: serde_json::Value) -> String {
        json!({ "type": "executed", "query_id": 1, "outcome": outcome }).to_string()
    }

    #[test]
    fn an_ordinary_outcome_still_travels_as_a_bare_string() {
        // The newtypes must not change the wire form. Serialized and parsed
        // back through the tagged frame both outcomes travel as they did.
        let msg = DaemonMsg::Executed {
            query_id: 1,
            outcome: ExecOutcome::OpenUrl(OpenUrl::new("https://example.com").unwrap()),
        };
        let encoded = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"executed","query_id":1,"outcome":{"open_url":"https://example.com"}}"#
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&encoded).unwrap(), msg);

        let msg = DaemonMsg::Executed {
            query_id: 1,
            outcome: ExecOutcome::CopyText(CopyText::new("42").unwrap()),
        };
        let encoded = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            encoded,
            r#"{"type":"executed","query_id":1,"outcome":{"copy_text":"42"}}"#
        );
        assert_eq!(serde_json::from_str::<DaemonMsg>(&encoded).unwrap(), msg);
    }

    #[test]
    fn a_refused_url_cannot_be_produced_by_parsing() {
        for url in [
            "file:///home/user/.ssh/id_ed25519",
            "javascript:alert(1)",
            "-o",
            "example.com",
        ] {
            assert!(
                serde_json::from_str::<ExecOutcome>(&json!({ "open_url": url }).to_string())
                    .is_err(),
                "parsing must apply the same rules as the constructor, accepted {url:?}"
            );
            assert!(
                serde_json::from_str::<DaemonMsg>(&executed_frame(json!({ "open_url": url })))
                    .is_err(),
                "and must apply them inside a frame too, accepted {url:?}"
            );
        }
    }

    #[test]
    fn a_refused_character_in_a_url_cannot_be_produced_by_parsing() {
        // The other two URL rules are covered above; this is the one whose
        // refusal the parse had no test for, on either shape.
        for url in ["https://example.com/a b", "https://example.com/a\u{7f}b"] {
            for parsed in [
                serde_json::from_str::<ExecOutcome>(&json!({ "open_url": url }).to_string())
                    .err()
                    .map(|e| e.to_string()),
                serde_json::from_str::<DaemonMsg>(&executed_frame(json!({ "open_url": url })))
                    .err()
                    .map(|e| e.to_string()),
            ] {
                let err = parsed.unwrap_or_else(|| panic!("parsing must refuse {url:?}"));
                assert!(err.contains(OpenUrl::FIELD), "got: {err}");
            }
        }
    }

    #[test]
    fn a_refused_copy_text_cannot_be_produced_by_parsing() {
        // Both other copy-text parse tests go through a frame; this is the bare
        // value, which the routing measurements say takes the borrowed arm
        // whichever reader is used.
        let escaped =
            serde_json::from_str::<ExecOutcome>(&json!({ "copy_text": "a\rb" }).to_string())
                .expect_err("an escaped carriage return must be refused off a bare value");
        assert!(
            escaped.to_string().contains(CopyText::FIELD),
            "got: {escaped}"
        );

        let raw = serde_json::from_str::<ExecOutcome>("{\"copy_text\":\"a\u{7f}b\"}")
            .expect_err("a raw DEL must be refused off a bare value");
        assert!(raw.to_string().contains(CopyText::FIELD), "got: {raw}");
    }

    #[test]
    fn a_refused_value_is_refused_through_from_reader_too() {
        // Measured: a `from_reader` parse of a tagged frame hands every string
        // to the owned arm, raw or escaped — so the raw case, which `from_str`
        // routes to the borrowed arm, is only exercised on the owned arm here.
        let raw_del = format!(
            r#"{{"type":"executed","query_id":1,"outcome":{{"copy_text":"a{}b"}}}}"#,
            '\u{7f}'
        );
        for frame in [
            raw_del,
            executed_frame(json!({ "copy_text": "a\rb" })),
            executed_frame(json!({ "open_url": "file:///etc/passwd" })),
        ] {
            assert!(
                serde_json::from_reader::<_, DaemonMsg>(frame.as_bytes()).is_err(),
                "the rules must hold whichever reader parsed the frame: {frame:?}"
            );
        }
    }

    #[test]
    fn the_parse_checks_the_length_before_the_content_too() {
        // The ordering is claimed of the parse as well as of the constructor,
        // and the parse applies the two gates in two different places, so it is
        // asserted here rather than inferred from the constructor's test.
        let over = format!("file:///{}", "a".repeat(MAX_OPEN_URL));
        let err = serde_json::from_str::<DaemonMsg>(&executed_frame(json!({ "open_url": over })))
            .expect_err("a value breaking both gates must still be refused");
        assert!(
            err.to_string().contains("over its maximum of"),
            "the length must be what is reported, got: {err}"
        );
    }

    #[test]
    fn a_refusal_off_the_socket_names_the_wire_field() {
        let err = serde_json::from_str::<DaemonMsg>(&executed_frame(json!({ "open_url": "-o" })))
            .unwrap_err();
        assert!(err.to_string().contains(OpenUrl::FIELD), "got: {err}");
    }

    // --- A wrong-typed value names its field too (issue #82) ----------------
    //
    // Every `FIELD` constant in this module is documented as "named by every
    // refusal of one" — a universal. The length and content refusals above
    // keep that promise through `BoundError` and `ContentError`, both of which
    // carry `field`. A value of the *wrong JSON type* — a number or `null`
    // where a string is wanted — never reaches either: serde refuses it
    // through `invalid_type`, formatted from `Validated::expecting` in
    // `limits`, before `OpenUrl::new`/`CopyText::new`/`IconName::new`/
    // `IconPath::new` ever runs. That path used to build its message from the
    // byte maximum alone, naming no field, which is exactly the gap issue #82
    // found. These two tests are what would have caught it: one wrong-typed
    // case and one `null` case, for all four `FIELD`-carrying types this
    // module defines, not only the two the issue happened to name.

    #[test]
    fn a_wrong_typed_value_names_the_field_for_every_field_carrying_type() {
        let open_url = serde_json::from_str::<ExecOutcome>(&json!({ "open_url": 42 }).to_string())
            .expect_err("a number is not a string");
        assert!(
            open_url.to_string().contains(OpenUrl::FIELD),
            "got: {open_url}"
        );

        let copy_text =
            serde_json::from_str::<ExecOutcome>(&json!({ "copy_text": 42 }).to_string())
                .expect_err("a number is not a string");
        assert!(
            copy_text.to_string().contains(CopyText::FIELD),
            "got: {copy_text}"
        );

        let icon_name = serde_json::from_str::<IconSpec>(r#"{"name":42}"#)
            .expect_err("a number is not a string");
        assert!(
            icon_name.to_string().contains(IconName::FIELD),
            "got: {icon_name}"
        );

        let icon_path = serde_json::from_str::<IconSpec>(r#"{"path":42}"#)
            .expect_err("a number is not a string");
        assert!(
            icon_path.to_string().contains(IconPath::FIELD),
            "got: {icon_path}"
        );
    }

    #[test]
    fn a_null_value_names_the_field_for_every_field_carrying_type() {
        let open_url =
            serde_json::from_str::<ExecOutcome>(&json!({ "open_url": null }).to_string())
                .expect_err("null is not a string");
        assert!(
            open_url.to_string().contains(OpenUrl::FIELD),
            "got: {open_url}"
        );

        let copy_text =
            serde_json::from_str::<ExecOutcome>(&json!({ "copy_text": null }).to_string())
                .expect_err("null is not a string");
        assert!(
            copy_text.to_string().contains(CopyText::FIELD),
            "got: {copy_text}"
        );

        let icon_name =
            serde_json::from_str::<IconSpec>(r#"{"name":null}"#).expect_err("null is not a string");
        assert!(
            icon_name.to_string().contains(IconName::FIELD),
            "got: {icon_name}"
        );

        let icon_path =
            serde_json::from_str::<IconSpec>(r#"{"path":null}"#).expect_err("null is not a string");
        assert!(
            icon_path.to_string().contains(IconPath::FIELD),
            "got: {icon_path}"
        );
    }

    #[test]
    fn an_escaped_control_character_is_refused_inside_a_tagged_frame() {
        // A C0 control cannot appear raw in JSON, so this is the only way one
        // arrives. Under `from_str` an escaped string is what reaches the arm
        // taking an owned `String`, so this is that arm's case.
        let err =
            serde_json::from_str::<DaemonMsg>(&executed_frame(json!({ "copy_text": "a\rb" })))
                .unwrap_err();
        assert!(err.to_string().contains(CopyText::FIELD), "got: {err}");
    }

    #[test]
    fn a_raw_control_character_is_refused_inside_a_tagged_frame() {
        // DEL and C1 need no escaping in JSON, so under `from_str` they reach
        // the check raw, on the arm taking a borrowed `&str`. Written out
        // rather than built with `json!`, whose serializer would escape nothing
        // here but is not what is under test.
        for raw in ["\u{7f}", "\u{85}"] {
            let frame = format!(
                r#"{{"type":"executed","query_id":1,"outcome":{{"copy_text":"a{raw}b"}}}}"#
            );
            let err = serde_json::from_str::<DaemonMsg>(&frame)
                .expect_err("a raw control character must be refused");
            assert!(err.to_string().contains(CopyText::FIELD), "got: {err}");
        }
    }

    #[test]
    fn a_raw_c0_control_never_reaches_the_content_check() {
        // Pins the measured fact the docs rest on: serde_json refuses the
        // document itself, so no rule in this module ever sees an unescaped C0
        // control. For a character of that range, and not for DEL or C1, the
        // escaped path is the one that has to hold.
        let frame = "{\"type\":\"executed\",\"query_id\":1,\"outcome\":{\"copy_text\":\"a\rb\"}}";
        let err = serde_json::from_str::<DaemonMsg>(frame)
            .expect_err("a raw C0 control is not valid JSON");
        assert!(
            err.to_string().contains("control character"),
            "the refusal must come from the JSON parser, got: {err}"
        );
    }
}
