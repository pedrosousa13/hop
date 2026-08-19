//! The one implementation of string sanitization this workspace has, per spec
//! §9 ("string sanitization: one implementation in hop-core").
//!
//! It sanitizes provider-supplied error text — the free-form `String` in
//! [`ProviderError::Failed`](crate::provider::ProviderError::Failed), which is
//! untrusted text a provider chooses and which is bound for a GTK label by way
//! of [`ProtoError`](hop_protocol::ProtoError) — and display text built by
//! in-process providers before it reaches the validating item newtypes. Issue
//! #34 is the original finding: nothing capped error text, nothing escaped it,
//! and a provider failing every query with a 50 MB string prefixed by terminal
//! escapes would have had all of it rendered.
//!
//! # Why this is not in `hop-protocol`'s content rules
//!
//! [`content`](hop_protocol::content) *refuses* a value that breaks a rule —
//! that is right for a value arriving off the wire, where a refusal names a
//! peer's mistake. In-process providers instead rewrite their own display
//! text before constructing an item, while diagnostics are already a failure
//! explanation and refusing one would replace the original reason with the
//! reason its explanation was unacceptable. So this module rewrites rather
//! than refuses, and the rewrite is lossy on purpose.

use std::path::Path;

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

/// Rewrites single-line text into something safe to render: every
/// [`char::is_control`] character and every [`BIDI_CONTROLS`] character
/// removed, then truncated to `max` bytes at a `char` boundary.
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
pub fn sanitize_single_line(raw: &str, max: usize) -> String {
    let stripped: String = raw
        .chars()
        .filter(|c| !c.is_control() && !BIDI_CONTROLS.contains(c))
        .collect();

    if stripped.len() <= max {
        return stripped;
    }

    let mut end = max;
    while end > 0 && !stripped.is_char_boundary(end) {
        end -= 1;
    }
    stripped[..end].to_string()
}

/// Rewrites provider-supplied text using the provider-message budget.
pub fn sanitize_provider_message(raw: &str) -> String {
    sanitize_single_line(raw, MAX_PROVIDER_MESSAGE)
}

/// Rewrites a filesystem- or environment-derived path into a form that is
/// safe to interpolate into a single-line, human-readable diagnostic. Issue
/// #159's fix for every call site that used to build a diagnostic line with
/// `path.display()`: `apps.rs`'s `malformed_log_line`, `config.rs`'s
/// `ConfigError` variants, `source.rs`'s learning-save lines
/// (`record_launch`'s `eprintln!`), and `server.rs`'s
/// `ListenerError::AlreadyListening`. `learning.rs` itself has no production
/// diagnostic built this way — its two `path.display()` uses are test-only
/// panic messages, untouched by this change.
/// Unix filenames may contain newlines, carriage returns, ESC, other C0/C1
/// controls, DEL, and Unicode bidirectional overrides — a `.desktop` file
/// whose name carries one of these could otherwise open a second
/// forged-looking log line, or drive the terminal reading the journal.
///
/// # Escape, not strip — the opposite choice from [`sanitize_single_line`]
///
/// [`sanitize_single_line`] is lossy on purpose (see this module's own doc
/// comment). That is right for a provider's free-form error prose, where
/// dropping a control character costs the reader nothing they needed. A
/// path is not prose: it is an identifier the reader has to match, exactly,
/// against their own filesystem to find the file a diagnostic is naming.
/// Silently removing a character from it does not make the diagnostic
/// safer — it makes the diagnostic useless in a way a visible `\x0a` does
/// not, because the reader can no longer tell which of several
/// similarly-named files failed, or search for the real name. So this
/// function encodes every byte that could otherwise forge a log record or a
/// terminal control sequence, rather than discarding it: the escaped path
/// always identifies the same file the raw path did. Do not "unify" this
/// with [`sanitize_single_line`] — they solve different problems on
/// purpose, and collapsing them would silently reintroduce whichever
/// problem the collapsed function does not itself have.
///
/// # Non-UTF-8 bytes
///
/// A Unix path is not required to be valid UTF-8. [`Path::display`] already
/// knows this and papers over it by lossily replacing every invalid byte
/// with U+FFFD — discarding it, the same way [`sanitize_single_line`]
/// discards a control character, and for the same wrong reason: a replaced
/// byte cannot be matched back to the real file name. This function instead
/// reads the path's raw bytes via
/// [`OsStrExt::as_bytes`](std::os::unix::ffi::OsStrExt::as_bytes) and, for
/// any byte that is not part of a valid, decodable UTF-8 sequence, emits
/// that exact byte's own `\xHH` escape — so the escaped output determines
/// the original bytes exactly, rather than losing the ones `Display` would
/// have thrown away. Reaching for a Unix-specific API here is not new scope
/// for this workspace: every crate binds a Unix domain socket
/// unconditionally and `hopd`'s `Cargo.toml` says outright that it "has no
/// non-Unix target", and `learning.rs` already gates its own Unix-only
/// steps (durability fsyncs, `0600` permissions) behind `#[cfg(unix)]` by
/// *omitting* the step off Unix rather than shipping a parallel
/// implementation. This function follows that same precedent, one level
/// up: the whole function is `#[cfg(unix)]`, so off Unix it is simply
/// absent rather than present-but-lossy. A `to_string_lossy` fallback was
/// considered and rejected — it would reintroduce the exact `U+FFFD` loss
/// this function exists to avoid, silently, on the one target nothing in
/// this workspace ever builds for: `hopd`, this crate's only consumer,
/// has no non-Unix target either.
///
/// # The escaping vocabulary, and why it cannot be misread
///
/// Applied to the path's *bytes*, in this order of precedence:
///
/// - `\\` for a literal backslash byte.
/// - `\xHH` (lowercase hex) for every byte [`char::is_control`] reaches —
///   C0, DEL, and C1 all fit in one byte, since that category tops out at
///   U+009F — and, separately, for every raw byte that is not part of a
///   valid UTF-8 sequence.
/// - `\u{HEX}` for every [`BIDI_CONTROLS`] character — the same enumerated
///   Trojan Source set [`sanitize_provider_message`] strips, escaped here
///   instead of removed, for the identical underlying reason: a direction
///   override reorders how surrounding text *displays* without changing
///   what it says, which `char::is_control` does not reach.
///
/// Every other byte — including `/`, which must survive unescaped or the
/// result is not a readable path, and ordinary non-ASCII Unicode (accents,
/// CJK, emoji) — passes through unchanged.
///
/// This is unambiguous because a literal `\` byte is *always* rewritten to
/// `\\`, so the only way a lone `\` can appear in the output is as the
/// first byte of one of these three escapes. A reader can therefore always
/// tell which one produced a given `\`: `\` immediately followed by another
/// `\` is one literal backslash byte, `\x` followed by two hex digits is
/// one escaped byte, `\u{` … `}` is one escaped bidi codepoint — no other
/// `\` sequence is ever emitted, so there is nothing else it could mean. A
/// file literally named backslash-n (the two ordinary bytes `\` and `n`)
/// therefore renders as `\\n`, visibly different from a file containing one
/// real newline byte, which renders as `\x0a`. Named escapes such as `\n`
/// for newline were considered and rejected for exactly this reason:
/// introducing a *second* meaning for `\` followed by an ordinary letter
/// would reopen the ambiguity this function exists to close.
#[cfg(unix)]
pub fn escape_path(path: &Path) -> String {
    let bytes = path_to_bytes(path);
    let mut out = String::with_capacity(bytes.len());
    let mut remaining: &[u8] = &bytes;

    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                push_escaped_str(valid, &mut out);
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                #[expect(
                    clippy::unwrap_used,
                    reason = "`Utf8Error::valid_up_to` guarantees the prefix it names is valid \
                              UTF-8 by construction; from_utf8 cannot fail on it"
                )]
                let valid = std::str::from_utf8(&remaining[..valid_up_to]).unwrap();
                push_escaped_str(valid, &mut out);

                // `error_len` is `None` only when the invalid tail is an
                // incomplete sequence truncated by the end of input (e.g. a
                // lead byte with no continuation byte after it) — in which
                // case every remaining byte is that incomplete sequence.
                let invalid_len = err.error_len().unwrap_or(remaining.len() - valid_up_to);
                for &b in &remaining[valid_up_to..valid_up_to + invalid_len] {
                    push_hex_byte(b, &mut out);
                }
                remaining = &remaining[valid_up_to + invalid_len..];
            }
        }
    }

    out
}

/// The raw bytes behind a path. See [`escape_path`]'s "Non-UTF-8 bytes"
/// section for why this reads the path's actual bytes via
/// [`OsStrExt::as_bytes`](std::os::unix::ffi::OsStrExt::as_bytes) rather
/// than going through [`Path::display`]'s lossy conversion. `#[cfg(unix)]`
/// like [`escape_path`] itself — there is deliberately no non-Unix arm; see
/// [`escape_path`]'s doc comment for why one was rejected.
#[cfg(unix)]
fn path_to_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    std::borrow::Cow::Borrowed(path.as_os_str().as_bytes())
}

/// Escapes one already-valid-UTF-8 chunk's characters into `out`, per
/// [`escape_path`]'s documented vocabulary.
#[cfg(unix)]
fn push_escaped_str(s: &str, out: &mut String) {
    for c in s.chars() {
        if c == '\\' {
            out.push_str("\\\\");
        } else if c.is_control() {
            // `char::is_control` never exceeds U+009F, so this always fits
            // one byte — see `escape_path`'s doc comment.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "is_control()'s range (C0, DEL, C1) tops out at U+009F, which always \
                          fits a u8"
            )]
            push_hex_byte(c as u32 as u8, out);
        } else if BIDI_CONTROLS.contains(&c) {
            use std::fmt::Write as _;
            #[expect(
                clippy::unwrap_used,
                reason = "writing to a String via fmt::Write is infallible"
            )]
            write!(out, "\\u{{{:x}}}", c as u32).unwrap();
        } else {
            out.push(c);
        }
    }
}

/// Appends one raw byte's `\xHH` escape to `out`.
#[cfg(unix)]
fn push_hex_byte(b: u8, out: &mut String) {
    use std::fmt::Write as _;
    #[expect(
        clippy::unwrap_used,
        reason = "writing to a String via fmt::Write is infallible"
    )]
    write!(out, "\\x{b:02x}").unwrap();
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

    #[test]
    fn bounded_single_line_sanitization_strips_controls_and_truncates() {
        assert_eq!(
            sanitize_single_line("keep\u{202e}visible\ntext", 11),
            "keepvisible"
        );
        assert_eq!(sanitize_single_line("語語語", 7), "語語");
    }

    // --- `escape_path` (issue #159) ---
    //
    // Unlike the tests above, these are written against the *escaping*
    // contract: a control byte must never disappear, it must turn into a
    // visible, unambiguous escape sequence instead. See `escape_path`'s doc
    // comment for the vocabulary these pin.
    //
    // `escape_path` itself is `#[cfg(unix)]` (see its doc comment), so the
    // tests pinning it live in their own `#[cfg(unix)]` submodule rather than
    // each carrying the attribute separately.
    #[cfg(unix)]
    mod escape_path_tests {
        use super::*;
        use std::path::Path;

        #[test]
        fn an_ordinary_ascii_path_passes_through_unchanged() {
            let path = Path::new("/home/pedro/.config/hop/config.toml");
            assert_eq!(escape_path(path), "/home/pedro/.config/hop/config.toml");
        }

        #[test]
        fn ordinary_non_ascii_unicode_survives_untouched() {
            // Accents, CJK, and emoji are not the target of this function —
            // only control and direction characters are. Mangling any of these
            // would make the escaped path useless for the exact reason
            // stripping would: it would no longer name the real file.
            let path = Path::new("/home/pedro/Documents/résumé_日本語_😀.pdf");
            assert_eq!(
                escape_path(path),
                "/home/pedro/Documents/résumé_日本語_😀.pdf"
            );
        }

        #[test]
        fn path_separators_survive_unescaped() {
            // A path with every separator escaped would not be a path anymore —
            // the reader has to be able to walk it.
            let path = Path::new("/a/b/c/d.txt");
            let out = escape_path(path);
            assert_eq!(out.matches('/').count(), 4);
            assert_eq!(out, "/a/b/c/d.txt");
        }

        #[test]
        fn an_ordinary_path_escapes_identically_to_display() {
            // `config.rs`'s existing tests assert
            // `err.to_string().contains(&path.display().to_string())` for
            // ordinary tempdir paths. That must keep passing untouched, and it
            // can only do that if this function is a no-op on paths with
            // nothing to escape — this pins that invariant directly, at the
            // source, rather than relying on the config tests to notice a
            // regression.
            let dir = std::env::temp_dir().join("hop-issue-159-fixture");
            assert_eq!(escape_path(&dir), dir.display().to_string());
        }

        #[test]
        fn a_newline_in_a_file_name_cannot_start_a_second_log_line() {
            let path = Path::new("/home/pedro/apps/evil\nname.desktop");
            let out = escape_path(path);
            assert!(
                !out.contains('\n'),
                "a raw newline must never reach the escaped output: {out:?}"
            );
            assert_eq!(out, "/home/pedro/apps/evil\\x0aname.desktop");
        }

        #[test]
        fn a_carriage_return_is_escaped() {
            let path = Path::new("evil\rname");
            assert_eq!(escape_path(path), "evil\\x0dname");
        }

        #[test]
        fn esc_cannot_open_a_terminal_control_sequence() {
            let path = Path::new("evil\u{1b}[31mname");
            let out = escape_path(path);
            assert!(
                !out.contains('\u{1b}'),
                "a raw ESC must never reach the escaped output: {out:?}"
            );
            assert_eq!(out, "evil\\x1b[31mname");
        }

        #[test]
        fn del_is_escaped() {
            let path = Path::new("evil\u{7f}name");
            assert_eq!(escape_path(path), "evil\\x7fname");
        }

        #[test]
        fn a_c1_control_character_is_escaped() {
            // U+0085 NEL, a C1 control — `char::is_control` reaches it even
            // though it is outside the ASCII C0 range.
            let path = Path::new("evil\u{85}name");
            assert_eq!(escape_path(path), "evil\\x85name");
        }

        #[test]
        fn every_bidi_control_character_is_escaped() {
            // Reuses `BIDI_CONTROLS`, the same enumerated Trojan Source set
            // `sanitize_provider_message` strips, for the identical reason: a
            // direction override can make a path *display* as something other
            // than what it says.
            for c in BIDI_CONTROLS {
                let name = format!("evil{c}name");
                let path = Path::new(&name);
                let out = escape_path(path);
                assert!(
                    !out.contains(*c),
                    "{c:?} must not reach the escaped output unescaped"
                );
                assert!(
                    out.contains(&format!("\\u{{{:x}}}", *c as u32)),
                    "{c:?} must escape to its \\u{{HEX}} form: {out:?}"
                );
            }
        }

        #[test]
        fn a_right_to_left_override_does_not_reorder_the_escaped_output() {
            let path = Path::new("apps\u{202e}desktop.exe");
            assert_eq!(escape_path(path), "apps\\u{202e}desktop.exe");
        }

        #[test]
        fn a_literal_backslash_is_escaped_so_it_cannot_be_confused_with_an_escape() {
            // The ambiguity this function exists to close: a file literally
            // named backslash-n (two ordinary bytes) must render differently
            // from a file containing one real newline byte, or a reader could
            // not tell them apart.
            let literal_backslash_n = Path::new("evil\\nname");
            let real_newline = Path::new("evil\nname");

            let literal_out = escape_path(literal_backslash_n);
            let newline_out = escape_path(real_newline);

            assert_ne!(literal_out, newline_out);
            assert_eq!(literal_out, "evil\\\\nname");
            assert_eq!(newline_out, "evil\\x0aname");
        }

        #[test]
        fn non_utf8_path_bytes_are_escaped_byte_for_byte_not_replaced_with_u_fffd() {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            // 0xFF is not a valid UTF-8 lead byte anywhere — guaranteed invalid
            // on its own, unlike a lone continuation byte which could in
            // principle be mistaken for one half of something else.
            let mut raw = b"evil".to_vec();
            raw.push(0xFF);
            raw.extend_from_slice(b"name");

            let os_string = OsString::from_vec(raw);
            let path = std::path::PathBuf::from(os_string);

            let out = escape_path(&path);

            assert!(
                !out.contains('\u{fffd}'),
                "the byte must be escaped, not lossily replaced: {out:?}"
            );
            assert_eq!(out, "evil\\xffname");
        }

        #[test]
        fn a_run_of_invalid_bytes_each_escapes_individually() {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            // Two bytes that together are not a valid UTF-8 sequence, so both
            // must appear in the output as their own two-digit escape.
            let raw = vec![b'x', 0xC0, 0xC0, b'y'];
            let os_string = OsString::from_vec(raw);
            let path = std::path::PathBuf::from(os_string);

            assert_eq!(escape_path(&path), "x\\xc0\\xc0y");
        }
    }
}
