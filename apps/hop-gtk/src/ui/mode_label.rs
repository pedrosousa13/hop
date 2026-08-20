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
//!
//! # CSS supersedes the Pango stand-in — issue #193
//!
//! Until this change, [`build`] set this label's font, colour and tracking
//! by constructing a `pango::AttrList` in Rust and calling
//! `gtk::Label::set_attributes` with it — the module's own prior comment
//! named that a "documented stand-in", because this crate had no
//! `gtk::CssProvider` installed anywhere yet, so a CSS rule for
//! `.hop-mode-label` (the class [`build`] already applied) would have had
//! nothing to load it and would never have rendered at all.
//!
//! `assets/stylesheet.css` now carries that rule — see its own
//! `.hop-mode-label` section. This module's Pango code was **removed**, not
//! kept alongside it, and that is a deliberate call, not an oversight: GTK
//! applies a label's own `set_attributes` `PangoAttrList` *on top of*
//! whatever the label's CSS style resolves to, for every property that list
//! sets (`gtk_label_set_attributes`'s own documentation: attributes are
//! "applied and merged with any other attributes previously effected"). Kept
//! side by side, the CSS rule would have been permanently dead — masked by
//! the Pango attributes for every property they both set — while still
//! being, textually, the exact same design value (the same `--hop-text-
//! section`/`--hop-tracking-section`/`--hop-neutral-400` tokens) expressed a
//! second time. That is precisely "a design value living in two places" in
//! the sense this issue's own global constraint forbids, whether or not it
//! currently renders — a CSS rule nobody will ever see take effect is not a
//! *safer* duplicate, it is a *quieter* one.
//!
//! The honest cost of removing it now, disclosed rather than silently
//! accepted: no `gtk::CssProvider` is installed by this same change (that is
//! issue #193's own Task 3, the very next step in this plan), so between
//! this commit landing and that one, `.hop-mode-label` renders with GTK's
//! default label styling — no custom weight, size, family, tracking, or
//! colour — rather than the typography it had a moment ago. That gap is
//! temporary, scoped to this one label, and closes the moment Task 3 loads
//! `assets/stylesheet.css` into a real provider; it was judged the smaller
//! cost against shipping a stylesheet rule this issue's own review would
//! have to explain away as inert on arrival.

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

/// Builds the mode label widget. Its typography (weight, size, family,
/// tracking, colour) is `assets/stylesheet.css`'s `.hop-mode-label` rule's
/// job now, not this function's — see this module's own top doc comment,
/// "CSS supersedes the Pango stand-in", for why the Rust-side Pango
/// construction this function used to do was removed rather than kept
/// alongside it. `add_css_class` below is what gives that rule something to
/// match. Starts absent — see [`apply`] — so a freshly built window, before any
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
}
