//! The mode label — issue #184's first signal: names the mode that answered
//! an **exclusive** route, and is absent entirely otherwise (D3's "mirrors
//! `exclusive`, and nothing else" rule).
//!
//! # D5 — the human label belongs here, not in `hop-protocol`
//!
//! [`Mode`] is a wire enum: its spellings (`Mode::WebSearch` serializes as
//! `"web_search"`) are snake_case contract, and it carries no `Display`. A
//! label like "Web Search" is presentation, and a localization surface
//! later — D5 of the plan this issue implements rules that mapping belongs
//! in `hop-gtk`, the one crate that renders anything, not in `hop-protocol`,
//! which every peer (including a future non-GTK client, or a Tier 2 sandbox
//! plugin) shares. [`label_for`] is that mapping, and it is written as an
//! exhaustive `match` with no catch-all arm on purpose: `Mode` is not
//! `#[non_exhaustive]`, so a future variant fails this file to compile
//! rather than silently reusing some other mode's label or falling through
//! to a placeholder nobody chose.

use gtk::prelude::*;

use hop_protocol::Mode;

use crate::tokens;

/// Maps a routed [`Mode`] to the human-readable name the mode label shows.
/// See this module's doc comment, "D5", for why this mapping lives here
/// rather than on [`Mode`] itself, and why the match below has no catch-all.
pub fn label_for(mode: Mode) -> &'static str {
    match mode {
        Mode::All => "All",
        Mode::Windows => "Windows",
        Mode::Apps => "Apps",
        Mode::Files => "Files",
        Mode::Emoji => "Emoji",
        Mode::Timezone => "Timezone",
        Mode::Currency => "Currency",
        Mode::Calculator => "Calculator",
        Mode::Weather => "Weather",
        Mode::Actions => "Actions",
        Mode::WebSearch => "Web Search",
    }
}

/// Builds the mode label widget: typography read from `--hop-text-section`
/// (weight, size, family) and `--hop-tracking-section` (letter-spacing, D7's
/// legibility signal), coloured with the muted `--hop-neutral-400` ramp step
/// (see [`tokens::MODE_LABEL_RGB`]'s own doc comment for why that step).
/// Starts absent — see [`apply`] — so a freshly built window, before any
/// `QueryRouted` frame has ever arrived, shows nothing: criterion 1's
/// "absent entirely otherwise" covers the state that precedes the first
/// query too, not only a later non-exclusive one.
///
/// # No layout shift — an overlay child, not a box sibling
///
/// The caller (`ui::window::HopWindow::build`) places this label as an
/// overlay child over the query entry — the same technique the selection
/// indicator already uses over the results list, and for the identical
/// reason (see that module's own doc comment, "Selection is one indicator
/// that moves"). `gtk::Overlay` does not count an overlay child toward its
/// own measured size by default (`measure` defaults to `false`, per
/// `gtk_overlay_add_overlay`'s own documentation) — so this label's text
/// changing length, or the label appearing or disappearing outright, can
/// never change the entry's own allocated size or position, and therefore
/// never moves the results list beneath it (criterion 5 / D6).
///
/// This is `ui::row`'s "reserve space before content exists" discipline,
/// carried over from *size* to *presence*: that module reserves a row's
/// height before any title is known, so an arriving title cannot shift
/// layout; this label is placed somewhere layout negotiation ignores
/// entirely, so its content — or the lack of it — has nothing to shift in
/// the first place.
pub fn build() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("hop-mode-label");
    label.set_halign(gtk::Align::End);
    label.set_valign(gtk::Align::Center);
    label.set_margin_end(*tokens::MODE_LABEL_MARGIN_END_PX);
    // Decorative with respect to input: a click meant for the entry beneath
    // it must never be intercepted by this label sitting visually on top.
    label.set_can_target(false);
    label.set_attributes(Some(&typography_attributes()));
    apply(&label, None);
    label
}

/// Shows or hides the mode label. `Some(mode)` names the mode that answered
/// (criterion 1's "shows the mode label naming that mode"); `None` clears
/// both its text and its visibility (criterion 1's "shows no label at all").
///
/// Callers pass `exclusive.then_some(mode)`, computed from the frame's own
/// two fields at the one call site (`ui::window::HopWindow::apply_event`) —
/// this function does not itself inspect `exclusive`, on purpose: D3's
/// "mirrors `exclusive`, and nothing else" rule is a property of what the
/// *caller* decides to pass in, not something this function should have an
/// opinion about by depending on a frame type it otherwise has no reason to
/// know.
///
/// `set_visible(false)` — not only clearing the text — is what removes the
/// label from the accessibility tree for the "no label at all" case
/// (criterion 6 read in reverse: a screen reader must announce *nothing*
/// here, not an empty-named element). It costs nothing in layout either way,
/// for the reason [`build`]'s own doc comment gives.
pub fn apply(label: &gtk::Label, shown: Option<Mode>) {
    match shown {
        Some(mode) => {
            label.set_text(label_for(mode));
            label.set_visible(true);
        }
        None => {
            label.set_text("");
            label.set_visible(false);
        }
    }
}

/// Maps a CSS-scale numeric weight, as `--hop-text-section` spells it
/// (`600`), to the [`gtk::pango::Weight`] enum `AttrInt::new_weight` wants.
/// Every weight `assets/tokens.css`'s `TYPE SCALE` section actually uses
/// (400, 500, 600) has a named variant here; anything else falls back to the
/// numeric `Weight::__Unknown` — Pango's own escape hatch for a raw weight
/// number outside its named set — rather than this function refusing to
/// build at all over a token value this crate does not happen to consume
/// today. Unlike [`label_for`]'s closed, wire-contract `Mode` enum, a CSS
/// font-weight is an open numeric range by nature, so a catch-all here is
/// the correct shape, not the gap D5 warns against for `Mode`.
fn pango_weight(css_weight: u16) -> gtk::pango::Weight {
    match css_weight {
        400 => gtk::pango::Weight::Normal,
        500 => gtk::pango::Weight::Medium,
        600 => gtk::pango::Weight::Semibold,
        700 => gtk::pango::Weight::Bold,
        other => gtk::pango::Weight::__Unknown(i32::from(other)),
    }
}

/// Converts a pixel value to the 1024-per-unit scale Pango's own attribute
/// constructors (`AttrSize::new_size_absolute`, `AttrInt::new_letter_spacing`)
/// measure in — `gtk::pango::SCALE` is that constant (`PANGO_SCALE`, 1024).
/// "Absolute" size is what makes the result a device pixel count rather than
/// a value further scaled by the display's own point-to-pixel conversion,
/// matching CSS `font-size: <N>px`'s own meaning — the meaning
/// `assets/tokens.css`'s `px` units are authored with throughout.
fn px_to_pango_units(px: f64) -> i32 {
    (px * f64::from(gtk::pango::SCALE)).round() as i32
}

/// Builds the mode label's typography as one `pango::AttrList`, read
/// entirely from `--hop-text-section` and `--hop-tracking-section`.
///
/// Direct Pango attributes rather than a GTK CSS rule: this crate has no
/// `gtk::CssProvider` installed anywhere yet — `tokens.rs`'s own doc comment
/// records why (`tokens.css` is authored web CSS, not GTK CSS, and a real
/// stylesheet pass that hardcodes literal values out of it is explicitly
/// named as future work, not this issue's to start). Applying the parsed
/// values directly as Pango attributes gets every value from its token
/// (criterion 4) without taking on that larger, separately-scoped decision
/// here.
fn typography_attributes() -> gtk::pango::AttrList {
    let font = &*tokens::MODE_LABEL_FONT;
    let list = gtk::pango::AttrList::new();

    list.insert(gtk::pango::AttrString::new_family(font.family));
    list.insert(gtk::pango::AttrInt::new_weight(pango_weight(font.weight)));
    list.insert(gtk::pango::AttrSize::new_size_absolute(px_to_pango_units(
        font.size_px,
    )));

    // Letter-spacing is `em`, relative to this same token's own font size —
    // see `tokens::MODE_LABEL_TRACKING_EM`'s doc comment for the pairing.
    let tracking_px = *tokens::MODE_LABEL_TRACKING_EM * font.size_px;
    list.insert(gtk::pango::AttrInt::new_letter_spacing(px_to_pango_units(
        tracking_px,
    )));

    let (r, g, b) = *tokens::MODE_LABEL_RGB;
    list.insert(gtk::pango::AttrColor::new_foreground(
        tokens::widen_channel(r),
        tokens::widen_channel(g),
        tokens::widen_channel(b),
    ));

    list
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`Mode`] variant has a real, non-empty label — the exhaustive
    /// match in [`label_for`] is what the compiler enforces; this pins the
    /// actual strings so a future edit changing one is a visible, deliberate
    /// diff here rather than a silent rename nobody meant to make.
    #[test]
    fn label_for_names_every_mode() {
        assert_eq!(label_for(Mode::All), "All");
        assert_eq!(label_for(Mode::Windows), "Windows");
        assert_eq!(label_for(Mode::Apps), "Apps");
        assert_eq!(label_for(Mode::Files), "Files");
        assert_eq!(label_for(Mode::Emoji), "Emoji");
        assert_eq!(label_for(Mode::Timezone), "Timezone");
        assert_eq!(label_for(Mode::Currency), "Currency");
        assert_eq!(label_for(Mode::Calculator), "Calculator");
        assert_eq!(label_for(Mode::Weather), "Weather");
        assert_eq!(label_for(Mode::Actions), "Actions");
        assert_eq!(label_for(Mode::WebSearch), "Web Search");
    }

    #[test]
    fn pango_weight_maps_the_type_scales_own_weights() {
        assert_eq!(pango_weight(400), gtk::pango::Weight::Normal);
        assert_eq!(pango_weight(500), gtk::pango::Weight::Medium);
        assert_eq!(pango_weight(600), gtk::pango::Weight::Semibold);
    }

    #[test]
    fn pango_weight_falls_back_to_the_raw_number_off_the_known_set() {
        assert_eq!(
            pango_weight(350),
            gtk::pango::Weight::__Unknown(350),
            "an unrecognized CSS weight must still produce a usable Pango weight"
        );
    }

    #[test]
    fn px_to_pango_units_scales_by_pango_scale() {
        assert_eq!(px_to_pango_units(11.0), 11 * gtk::pango::SCALE);
        assert_eq!(px_to_pango_units(0.0), 0);
    }
}
