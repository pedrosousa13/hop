//! The one implementation of string sanitization this workspace has, per spec
//! §9 ("string sanitization: one implementation in hop-core").
//!
//! What it sanitizes today is provider-supplied error text — the free-form
//! `String` in [`ProviderError::Failed`](crate::provider::ProviderError::Failed),
//! which is untrusted text a provider chooses and which is bound for a GTK
//! label by way of
//! [`ProtoError`](hop_protocol::ProtoError). Issue #34 is the finding: nothing
//! capped it, nothing escaped it, and a provider failing every query with a
//! 50 MB string prefixed by terminal escapes would have had all of it rendered.
//!
//! # Why this is not in `hop-protocol`'s content rules
//!
//! [`content`](hop_protocol::content) *refuses* a value that breaks a rule —
//! that is right for a value arriving off the wire, where a refusal names a
//! peer's mistake. Here the value is a diagnostic about a failure that already
//! happened, and refusing it would replace the reason a provider failed with
//! the reason its explanation was unacceptable. So this module rewrites rather
//! than refuses, and the rewrite is lossy on purpose.

/// The most bytes of provider-supplied text that may leave the daemon, after
/// stripping.
///
/// It has to fit *inside*
/// [`MAX_ERROR_MESSAGE`](hop_protocol::limits::MAX_ERROR_MESSAGE), the 1 024-byte
/// bound on the wire field this text ends up in, with room left for the host's
/// own attribution — which provider, and what kind of failure. 256 leaves 768
/// bytes for that framing, which is more than any of it needs and keeps the
/// arithmetic obvious rather than tight.
///
/// The unit is bytes, not characters, because every bound in
/// [`limits`](hop_protocol::limits) counts bytes and a second unit here would
/// make the two impossible to compare.
pub const MAX_PROVIDER_MESSAGE: usize = 256;

/// The bidirectional formatting characters this module removes.
///
/// These are Unicode's explicit bidi controls — the "Trojan Source" set. They
/// reorder how the characters around them *display* without changing the
/// characters themselves, so text carrying one can render as something other
/// than what it says: an error message that appears to name a different
/// provider, or to end before it does.
///
/// [`char::is_control`] does not reach them, and
/// [`CopyText`](hop_protocol::content::CopyText) says so in place — "nor the
/// bidirectional format characters such as U+202E, which can reorder how a
/// string renders ... this type does not address" — deferring the concern to
/// whoever needed it first. This is that place.
///
/// # Why this list rather than all of `Cf`
///
/// Unicode's format category also holds characters that carry meaning in
/// ordinary text — U+200D ZERO WIDTH JOINER is what holds a multi-codepoint
/// emoji together, and a provider whose failure message contains an emoji has
/// done nothing wrong. Stripping the whole category would mangle honest text to
/// reach a set that is enumerable and stable, so the set is enumerated.
pub const BIDI_CONTROLS: &[char] = &[
    '\u{061C}', // ARABIC LETTER MARK
    '\u{200E}', // LEFT-TO-RIGHT MARK
    '\u{200F}', // RIGHT-TO-LEFT MARK
    '\u{202A}', // LEFT-TO-RIGHT EMBEDDING
    '\u{202B}', // RIGHT-TO-LEFT EMBEDDING
    '\u{202C}', // POP DIRECTIONAL FORMATTING
    '\u{202D}', // LEFT-TO-RIGHT OVERRIDE
    '\u{202E}', // RIGHT-TO-LEFT OVERRIDE
    '\u{2066}', // LEFT-TO-RIGHT ISOLATE
    '\u{2067}', // RIGHT-TO-LEFT ISOLATE
    '\u{2068}', // FIRST STRONG ISOLATE
    '\u{2069}', // POP DIRECTIONAL ISOLATE
];

/// Rewrites provider-supplied text into something safe to render: every
/// [`char::is_control`] character and every [`BIDI_CONTROLS`] character
/// removed, then truncated to [`MAX_PROVIDER_MESSAGE`] bytes at a `char`
/// boundary.
///
/// # Strip before truncate
///
/// In that order, and it matters: truncating first would let characters that
/// are about to be removed spend the budget, so a message padded with 300
/// escape characters would arrive empty rather than arriving as its first 256
/// readable bytes.
///
/// Truncation stops at a `char` boundary, so the result is always valid UTF-8
/// and is never a partial code point — which is what would otherwise happen at
/// a byte cut through a multi-byte character.
pub fn sanitize_provider_message(raw: &str) -> String {
    let stripped: String = raw
        .chars()
        .filter(|c| !c.is_control() && !BIDI_CONTROLS.contains(c))
        .collect();

    if stripped.len() <= MAX_PROVIDER_MESSAGE {
        return stripped;
    }

    let mut end = MAX_PROVIDER_MESSAGE;
    while end > 0 && !stripped.is_char_boundary(end) {
        end -= 1;
    }
    stripped[..end].to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn ordinary_text_passes_through_unchanged() {
        assert_eq!(
            sanitize_provider_message("could not reach the index"),
            "could not reach the index"
        );
    }

    #[test]
    fn an_oversized_message_is_truncated_to_the_documented_maximum() {
        let raw = "a".repeat(MAX_PROVIDER_MESSAGE * 4);
        let out = sanitize_provider_message(&raw);
        assert_eq!(out.len(), MAX_PROVIDER_MESSAGE);
    }

    #[test]
    fn escape_sequences_and_newlines_are_removed() {
        let out = sanitize_provider_message("\u{1b}[31mred\u{1b}[0m\nand more\t here");
        assert!(
            !out.contains('\u{1b}'),
            "ESC opens a terminal control sequence and must not survive"
        );
        assert!(!out.contains('\n'));
        assert!(!out.contains('\t'));
        assert_eq!(out, "[31mred[0mand more here");
    }

    #[test]
    fn direction_override_characters_are_removed() {
        // A right-to-left override is what lets text render as something other
        // than what it says — the display-spoofing case `CopyText`'s docs
        // defer to this module.
        let out = sanitize_provider_message("apps\u{202e}failed\u{202c}");
        assert_eq!(out, "appsfailed");
        for c in BIDI_CONTROLS {
            assert!(
                !sanitize_provider_message(&format!("x{c}y")).contains(*c),
                "{c:?} must be stripped"
            );
        }
    }

    #[test]
    fn a_zero_width_joiner_survives_because_it_is_not_a_direction_control() {
        // The reason `BIDI_CONTROLS` is enumerated rather than being all of
        // Unicode's format category: this character holds an emoji together.
        let out = sanitize_provider_message("\u{1f468}\u{200d}\u{1f4bb} failed");
        assert!(out.contains('\u{200d}'));
    }

    #[test]
    fn stripping_happens_before_truncation() {
        // A message padded with controls up to the cap, then followed by
        // readable text: strip-then-truncate keeps the readable text, and
        // truncate-then-strip would have returned an empty string.
        let raw = format!("{}{}", "\u{1b}".repeat(MAX_PROVIDER_MESSAGE), "visible");
        assert_eq!(sanitize_provider_message(&raw), "visible");
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // '語' is three bytes, so 256 % 3 == 1: byte 256 is provably mid-character
        // unless the boundary is respected. This forces the while loop to actually
        // walk backward and correct the offset. A naive slice at MAX_PROVIDER_MESSAGE
        // would panic on 3-byte chars; the walk-back prevents that.
        let raw = "語".repeat(MAX_PROVIDER_MESSAGE);
        let out = sanitize_provider_message(&raw);
        assert!(
            out.len() <= MAX_PROVIDER_MESSAGE,
            "result must fit within the bound"
        );
        assert!(
            out.len() < MAX_PROVIDER_MESSAGE,
            "walk-back must have corrected a misaligned offset; result must be strictly less"
        );
        assert!(
            out.chars().all(|c| c == '語'),
            "a byte cut through a code point would not round-trip as chars"
        );
        assert_eq!(std::str::from_utf8(out.as_bytes()).unwrap(), out);
    }

    #[test]
    fn an_all_control_message_becomes_empty_rather_than_being_refused() {
        // Lossy on purpose: this module rewrites, it never refuses — see the
        // module docs on why a refusal would be the wrong answer here.
        assert_eq!(sanitize_provider_message("\u{1b}\u{7f}\u{202e}"), "");
    }
}
