//! What the two command-shaped outcomes are allowed to contain.
//!
//! [`ExecOutcome::CopyText`](crate::wire::ExecOutcome::CopyText) and
//! [`ExecOutcome::OpenUrl`](crate::wire::ExecOutcome::OpenUrl) are the only
//! values in this contract that tell a client to *act* — put this on the
//! clipboard, hand this to a URL launcher — rather than to display something.
//! Every other field is text a client draws. Both originate in a provider, and
//! a provider is the plugin seam every later extension tier adapts to, so "the
//! daemon is trusted" does not reach these two: it degrades to "every installed
//! provider is trusted".
//!
//! [`limits`] says how *long* these values may be; this module
//! says what they may *contain*. The two compose rather than replace one
//! another, and they compose in a fixed order: at the parse and in each
//! constructor the length is checked first, so a value that breaks both is
//! reported as over-long.
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
//! `an_escaped_control_character_is_refused_inside_a_tagged_frame`,
//! `a_raw_control_character_is_refused_inside_a_tagged_frame`,
//! `a_refused_value_is_refused_through_from_reader_too` and
//! `a_raw_c0_control_never_reaches_the_content_check` hold: a refusal fires
//! whichever of these paths a value arrives by.
//!
//! The table itself is not held that way, and deliberately is not. Nothing here
//! observes which arm a parse reached — that is serde's internal routing, which
//! this crate neither controls nor should pin — so if that routing changed, the
//! table would go stale while every test above stayed green. It is a
//! measurement, dated by the commit that took it, and the conclusion drawn from
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
//! sequence dressed as text. A value that is exactly what its variant says it
//! is can still be hostile — accepted copy text may hold a newline and so a
//! second line, which [`CopyText`]'s own section on that says plainly, and an
//! accepted URL is unparsed past its scheme, which [`OpenUrl`]'s does. The
//! client's own launching and clipboard handling is the other half of this, and
//! it is not in this crate.

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::limits::{self, BoundError, MAX_COPY_TEXT, MAX_OPEN_URL, check_len};

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
/// Each variant names the wire field it came from and nothing else. In
/// particular no variant carries the offending text: these values are
/// peer-controlled, an error made from one travels into logs and diagnostics,
/// and the field plus the rule is what a reader needs. The one exception is a
/// code point, which is a number.
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
///    pinned by the test `no_allowed_scheme_could_be_read_as_a_flag`. It is
///    kept as a rule of its own regardless: it is what has to survive a later
///    edit to the allow-list, and it names the actual hazard in the refusal
///    instead of reporting a strange scheme.
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
///    Pinned by the test
///    `open_url_accepts_characters_rfc_3986_would_percent_encode` on one side
///    and `open_url_refuses_a_space_or_a_control_character` on the other.
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
/// `open_url_does_not_check_what_follows_the_scheme`, so a later widening of
/// the rules updates this list rather than silently contradicting it.
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
/// Tab and newline are kept. A snippet has indentation and lines, and
/// [`MAX_COPY_TEXT`] is the most generous bound in the size budget precisely
/// because copy text is allowed to be a chunk of prose rather than a label.
/// Refusing a newline would mean an honest multi-line answer could not be
/// copied at all, which is a bigger, likelier failure than the one it would
/// avert.
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
/// Windows origin, in HTTP bodies, and in `\r\n`-terminated files — so a provider that
/// copies such text through unchanged has the value refused, and because the
/// refusal happens at the parse, the whole frame refused with it. That is the
/// same "silently break honest providers" failure that argues for allowing a
/// newline two paragraphs up, and here it is accepted rather than avoided: a
/// carriage return returns a terminal to column zero, which makes it the one
/// character that can overwrite what was already shown of a line, and a paste
/// able to hide what it contains is worth more than CRLF convenience.
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
/// `copy_text_does_not_refuse_the_non_cc_separators_and_format_characters`, so
/// a later widening of the rule updates this paragraph rather than silently
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;
    use crate::wire::{DaemonMsg, ExecOutcome};

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
        // The pair that decides the policy: a newline is ordinary text, a
        // carriage return is not, and allowing one must not allow the other.
        assert!(CopyText::new("a\nb").is_ok());
        assert!(CopyText::new("a\rb").is_err());
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
        // control and the escaped path is the only one that has to hold.
        let frame = "{\"type\":\"executed\",\"query_id\":1,\"outcome\":{\"copy_text\":\"a\rb\"}}";
        let err = serde_json::from_str::<DaemonMsg>(frame)
            .expect_err("a raw C0 control is not valid JSON");
        assert!(
            err.to_string().contains("control character"),
            "the refusal must come from the JSON parser, got: {err}"
        );
    }
}
