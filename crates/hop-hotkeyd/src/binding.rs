//! The spelling a `[hotkey]` binding is written in, and its translation into
//! the two things an X11 grab actually needs: a modifier bitmask and a
//! keysym.
//!
//! # The notation, and where it comes from
//!
//! Design spec §9 leaves the hotkey notation unspecified; the one binding
//! notation this workspace already ships is `hop-gtk::keymap`'s (issue #182):
//! an optional run of modifier names joined by `+` (`ctrl`, `shift`, `alt`,
//! `super`; case-insensitive), then the key itself, named the way the
//! platform's own key table names it. This module follows that vocabulary
//! exactly — `ctrl+alt+space`, `super+space`, `ctrl+shift+Page_Up` — so a
//! user who has already written a `[keymap]` table (or read that module's
//! documentation) already knows how to write a `[hotkey]` one, and the two
//! sections of one `config.toml` never teach two grammars for the same
//! shape of value.
//!
//! The key half differs in one documented way: `keymap` resolves names
//! through GDK's `gdk_keyval_from_name`, which this crate cannot reach (it
//! has no GTK dependency, by design — it is a daemon, not a frontend).
//! [`keysym_from_name`] below is this crate's own, deliberately small
//! table: every key a global launcher hotkey could plausibly want (the
//! modifiers' own keys, the navigation block, the function row), plus the
//! fallback that any single character names itself — X11 keysyms for
//! Latin-1 *are* the Unicode code points, so `"a"` is `0x61` and `"?"` is
//! `0x3f` without a table entry. A multi-character name the table does not
//! list is refused rather than guessed at: a hotkey that silently grabbed
//! some other key than the one the user spelled would be worse than one
//! that refuses to start.
//!
//! # What a parsed [`Binding`] holds
//!
//! The X modifier bitmask (`ShiftMask`, `ControlMask`, `Mod1Mask` for alt,
//! `Mod4Mask` for super — the mapping X's own `xmodmap` convention uses on
//! a default keymap) and the keysym. The keycode is *not* held: keysym →
//! keycode is a property of the connected server's keyboard mapping
//! (`GetKeyboardMapping`), not of the spelling, so `run.rs` resolves it
//! against the live connection it is about to grab on.

use std::fmt;

/// One parsed `[hotkey]` binding: the modifiers that must be held, and the
/// keysym of the key itself. Constructed only by [`Binding::parse`], which
/// refuses anything it cannot fully resolve — a half-understood spelling
/// must never reach the grab loop as something it looks like but is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// The X modifier bitmask this binding requires: `ShiftMask` bits set
    /// for every `shift` in the spelling, and so on.
    pub modifiers: u16,
    /// The keysym of the key itself, resolved through [`keysym_from_name`].
    pub keysym: u32,
}

/// Every way [`Binding::parse`] can refuse a spelling. The `Display` impl
/// names the offending spelling so the log line a degraded startup prints
/// says what the user actually wrote, not just that something was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    /// The spelling was empty, or every `+`-separated token was.
    Empty,
    /// A token before the final one named no modifier this module knows.
    UnknownModifier(String),
    /// The final token named no key this module's table or the
    /// single-character fallback can resolve.
    UnknownKey(String),
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindingError::Empty => write!(f, "empty binding"),
            BindingError::UnknownModifier(name) => {
                write!(
                    f,
                    "unknown modifier `{name}` (known: ctrl, shift, alt, super)"
                )
            }
            BindingError::UnknownKey(name) => write!(f, "unknown key `{name}`"),
        }
    }
}

/// Modifier names this module accepts, mapped to their X modifier bitmask.
/// `control` is accepted alongside `ctrl` for the same reason `keymap`'s
/// parser accepts both: a user typing a config value should not have to
/// remember which spelling this module picked. Matching is
/// case-insensitive — `CTRL+ALT+Space` is the same binding as
/// `ctrl+alt+space`, exactly as in `[keymap]`.
fn modifier_mask(name: &str) -> Option<u16> {
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(0x0004), // ControlMask
        "shift" => Some(0x0001),            // ShiftMask
        "alt" => Some(0x0008),              // Mod1Mask — alt's modifier on a default keymap
        "super" => Some(0x0040),            // Mod4Mask — super's modifier on a default keymap
        _ => None,
    }
}

/// The keysyms this module names beyond the single-character fallback: the
/// navigation block, the editing keys, the function row, and the modifier
/// keys themselves (binding a bare `super` press is unusual but legal, and
/// the e2e harness fakes `Control_L`/`Alt_L` presses to build its synthetic
/// chord). Values are the X keysym constants — `0xff00`-page for the
/// function/navigator keys, per X11's keysymdef.h.
fn named_keysym(name: &str) -> Option<u32> {
    let keysym = match name.to_ascii_lowercase().as_str() {
        "backspace" => 0xff08,
        "tab" => 0xff09,
        "return" | "enter" => 0xff0d,
        "escape" | "esc" => 0xff1b,
        "delete" | "del" => 0xffff,
        "home" => 0xff50,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "page_up" | "pageup" => 0xff55,
        "page_down" | "pagedown" => 0xff56,
        "end" => 0xff57,
        "insert" => 0xff63,
        "menu" => 0xff67,
        "pause" => 0xff13,
        "scroll_lock" => 0xff14,
        "num_lock" => 0xff7f,
        "caps_lock" => 0xffe5,
        "shift_l" => 0xffe1,
        "shift_r" => 0xffe2,
        "control_l" => 0xffe3,
        "control_r" => 0xffe4,
        "alt_l" => 0xffe9,
        "alt_r" => 0xffea,
        "super_l" => 0xffeb,
        "super_r" => 0xffec,
        "space" => 0x0020,
        name if name.len() == 2 && name.starts_with('f') => {
            // F1–F12: keysyms 0xffbe..0xffc9, one run in keysymdef.h.
            let n: u32 = name[1..].parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            0xffbe + n - 1
        }
        _ => return None,
    };
    Some(keysym)
}

/// Resolves a key name — the final `+`-token of a binding spelling — to its
/// X keysym. See this module's doc comment, "The notation", for the two
/// resolution rules and why an unresolvable multi-character name is an
/// error rather than a guess.
pub fn keysym_from_name(name: &str) -> Option<u32> {
    if let Some(keysym) = named_keysym(name) {
        return Some(keysym);
    }
    // Single-character fallback: X11 assigns Latin-1 keysyms the identical
    // numeric value as the Unicode code point, so every printable ASCII
    // character names itself. Anything longer that the table above does not
    // list stays unresolved — refused by the caller, never guessed.
    let mut chars = name.chars();
    let (only, rest) = (chars.next()?, chars.next());
    if rest.is_none() {
        let code = only as u32;
        if code <= 0xff {
            return Some(code);
        }
    }
    None
}

impl Binding {
    /// Parses one binding spelling — `ctrl+alt+space` — into a [`Binding`],
    /// or refuses it with a [`BindingError`] naming what failed.
    ///
    /// The final `+`-token is the key; every token before it is a modifier.
    /// Tokens are trimmed of surrounding whitespace (a config written as
    /// `ctrl + alt + space` still parses) but may not be empty — `ctrl++`
    /// is a refusal, not a silent `+` key, because the reader's eye cannot
    /// tell a doubled separator from a bound plus-sign and neither should
    /// the parser.
    pub fn parse(spelling: &str) -> Result<Binding, BindingError> {
        let mut modifiers = 0u16;
        let mut tokens = spelling.split('+').peekable();
        let mut any = false;
        while let Some(token) = tokens.next() {
            let token = token.trim();
            if token.is_empty() {
                if !any && tokens.peek().is_some() {
                    // A leading `+` (`+x`): the empty first token names no
                    // modifier, and falling through to treat `x` as the key
                    // would silently reinterpret what was written.
                    return Err(BindingError::UnknownModifier(String::new()));
                }
                return Err(BindingError::Empty);
            }
            any = true;
            if tokens.peek().is_some() {
                let mask = modifier_mask(token)
                    .ok_or_else(|| BindingError::UnknownModifier(token.to_string()))?;
                modifiers |= mask;
            } else {
                let keysym = keysym_from_name(token)
                    .ok_or_else(|| BindingError::UnknownKey(token.to_string()))?;
                return Ok(Binding { modifiers, keysym });
            }
        }
        Err(BindingError::Empty)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_plain_key_binds_with_no_modifiers() {
        let binding = Binding::parse("space").unwrap();
        assert_eq!(binding.modifiers, 0);
        assert_eq!(binding.keysym, 0x0020);
    }

    #[test]
    fn modifiers_accumulate_in_any_order_and_case() {
        let lower = Binding::parse("ctrl+alt+space").unwrap();
        let upper = Binding::parse("SUPER+CTRL+Space").unwrap();
        // ctrl (0x4) + alt (0x8) in both, plus super (0x40) in the second.
        assert_eq!(lower.modifiers, 0x0c);
        assert_eq!(upper.modifiers, 0x44);
        assert_eq!(lower.keysym, upper.keysym);
    }

    #[test]
    fn control_is_accepted_as_the_long_spelling_of_ctrl() {
        assert_eq!(
            Binding::parse("control+space").unwrap(),
            Binding::parse("ctrl+space").unwrap()
        );
    }

    #[test]
    fn whitespace_around_tokens_is_trimmed() {
        assert_eq!(
            Binding::parse("ctrl + alt + space").unwrap(),
            Binding::parse("ctrl+alt+space").unwrap()
        );
    }

    #[test]
    fn single_characters_resolve_through_the_latin1_fallback() {
        assert_eq!(Binding::parse("ctrl+a").unwrap().keysym, 0x61);
        assert_eq!(Binding::parse("ctrl+?").unwrap().keysym, 0x3f);
        assert_eq!(Binding::parse("ctrl+1").unwrap().keysym, 0x31);
    }

    #[test]
    fn the_navigation_and_function_rows_resolve() {
        assert_eq!(Binding::parse("Page_Up").unwrap().keysym, 0xff55);
        assert_eq!(Binding::parse("escape").unwrap().keysym, 0xff1b);
        assert_eq!(Binding::parse("F5").unwrap().keysym, 0xffc2);
        assert_eq!(Binding::parse("enter").unwrap().keysym, 0xff0d);
    }

    #[test]
    fn an_unknown_key_is_refused_naming_itself() {
        assert_eq!(
            Binding::parse("ctrl+notakey"),
            Err(BindingError::UnknownKey("notakey".to_string()))
        );
    }

    #[test]
    fn an_unknown_modifier_is_refused_naming_itself() {
        assert_eq!(
            Binding::parse("meta+space"),
            Err(BindingError::UnknownModifier("meta".to_string()))
        );
    }

    #[test]
    fn empty_and_degenerate_spellings_are_refused() {
        assert_eq!(Binding::parse(""), Err(BindingError::Empty));
        assert_eq!(Binding::parse("   "), Err(BindingError::Empty));
        assert_eq!(
            Binding::parse("ctrl++"),
            Err(BindingError::Empty),
            "an empty token between separators is a refusal, never a bound `+`"
        );
        assert_eq!(
            Binding::parse("+x"),
            Err(BindingError::UnknownModifier(String::new())),
            "a leading `+` names no modifier and must not be read as a bare `x`"
        );
    }
}
