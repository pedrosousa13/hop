//! The offline indicator — issue #200, and the first widget in this crate
//! to carry `.hop-honesty`. Combines two of `docs/theme-token-contract.md`'s
//! four reserved members in one widget, because one widget happens to need
//! both: the offline indicator itself (its text) and a cached-data "as of"
//! label (its stamp), shown together as "the daemon connection was lost,
//! and here is when we last knew otherwise."
//!
//! # Why this is `OfflineIndicator`, not `OfflineRow`
//!
//! It shipped under the name `OfflineRow` — this module's own file used to
//! be `ui/offline_row.rs` — and a code review of this issue caught that as
//! a vocabulary collision worth fixing before it spread further. `CONTEXT.md`
//! reserves "row" for the results list's per-item vocabulary, and
//! `ui::row` already establishes exactly one, specific meaning for
//! "Row" in this crate: a *recycled*, factory-bound `GtkListView` slot —
//! see that module's own `build`/`bind`/`unbind` shape and its top doc
//! comment, "`build` and `bind` do not blindly animate", for what that
//! recycling constraint actually requires of a widget that carries it. This
//! widget is the structural opposite of that, and the section right below
//! already said so before this rename — built once, owned by
//! `ui::window::HopWindow`, never recycled, never handed back to any module
//! as a bare `gtk::Widget` a factory found by name. Calling it a "row" that
//! is never recycled would read to anyone who has internalised `ui::row`'s
//! meaning as a contradiction. `docs/theme-token-contract.md:35` already
//! names this reserved member "the offline indicator" — non-colliding and
//! spec-aligned — so this rename adopts the contract's own term rather than
//! inventing a third name for the same thing. The CSS class this widget's
//! own, ordinary, non-locked layout rule carries moved with it,
//! `.hop-offline-row` → `.hop-offline-indicator`, so the Rust name and the
//! class name stay in lockstep the same way every other widget in this
//! crate keeps its type name and its own CSS class recognisably paired.
//!
//! # Why this widget owns no lookup-by-name machinery
//!
//! `ui::row`'s `build`/`bind`/[`find_named_child`] shape exists because a
//! `GtkListView` factory recycles one row widget across many different
//! items, and `bind` is only ever handed the generic `gtk::Widget` GTK's
//! recycling API gives back — see that module's own top doc comment,
//! "`build` and `bind` do not blindly animate", and `find_named_child`'s
//! own doc comment for the full account of why a widget name, not a stored
//! typed handle, is what makes a recycled child findable again.
//!
//! [`find_named_child`]: crate::ui::row
//!
//! None of that applies here. `ui::window::HopWindow` owns exactly one
//! [`OfflineIndicator`], built once and kept for the life of the window —
//! it is never recycled, never handed back to this module as a bare
//! `gtk::Widget`, and never shared with a factory that only knows how to
//! address it by name. [`OfflineIndicator::build`] can therefore just
//! return typed [`gtk::Label`] handles directly, the same shape
//! `ui::mode_label::build` already uses for the one label it owns — a
//! widget name here would be machinery with no caller that ever needs it.
//!
//! # `apply` does not animate, for the same reason `ui::row`'s `build`/`bind`
//! do not by default
//!
//! [`OfflineIndicator::apply`] toggles this widget's visibility and its
//! stamp text in response to [`crate::ipc::IpcEvent::Connected`]/
//! `Disconnected` — `ui::window::HopWindow::apply_event` is the one call
//! site. Nothing here starts a transition of any kind: an offline banner
//! popping in and out silently, with no visual flourish, is the correct
//! behavior for a signal whose entire job is truthfulness, not delight —
//! see `docs/theme-token-contract.md`'s "Why this boundary exists" section
//! for why an honesty-critical element is exactly the wrong place to spend
//! any of the credibility a fade or a slide would cost if it ever looked
//! decorative rather than factual.

use gtk::prelude::*;

use crate::tokens;

/// The offline indicator: a horizontal [`gtk::Box`] carrying `.hop-honesty`
/// (the reserved class `docs/theme-token-contract.md` names) and
/// `.hop-offline-indicator` (this widget's own, ordinary, non-locked
/// layout class — see `assets/stylesheet.css`'s own comment on that rule
/// for why it is deliberately kept separate from `.hop-honesty` itself),
/// holding two labels: `text` (`.hop-honesty-text`, the offline
/// indicator's own words) and `stamp` (`.hop-honesty-stamp`, the per-item
/// "as of HH:MM" label).
///
/// `Clone`, matching `ui::window::HopWindow`'s own field convention —
/// every GTK/glib handle it stores is a cheap, reference-counted clone, and
/// this struct is exactly that: one extra `gtk::Label` clone (`stamp`, the
/// only child [`OfflineIndicator::apply`] ever has to touch again after
/// construction) bundled alongside the `gtk::Box` a caller already has to
/// hold to place it in a window's content tree. `text` has no equivalent
/// field — see [`OfflineIndicator::build`]'s own comment on it for why.
#[derive(Clone)]
pub struct OfflineIndicator {
    /// This widget's own container — what a caller appends into a window's
    /// content tree.
    pub widget: gtk::Box,
    stamp: gtk::Label,
}

/// The offline indicator's own words. A literal, not a token or a format
/// argument: the contract's own design rule — "a member's meaning must
/// live in its words and its shape, never in a colour" — is precisely
/// about *this* string being what carries the meaning, not a colour a
/// theme could desaturate out from under it.
const OFFLINE_TEXT: &str = "Offline";

impl OfflineIndicator {
    /// Builds the offline indicator. Starts hidden
    /// (`gtk::Widget::set_visible(false)`) — presence is a widget property,
    /// never a CSS one (`assets/stylesheet.css`'s own "PRESENCE IS NEVER
    /// EXPRESSED HERE" section), and a freshly built window has not yet
    /// heard from `ipc` at all, connected or otherwise, so there is nothing
    /// honest to show yet. [`OfflineIndicator::apply`] is what ever changes
    /// that.
    pub fn build() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, *tokens::OFFLINE_ROW_GAP_PX);
        widget.add_css_class("hop-honesty");
        widget.add_css_class("hop-offline-indicator");
        widget.set_visible(false);

        // Built, appended, and then never touched again — `text`'s own
        // words are the one constant, [`OFFLINE_TEXT`], for the life of the
        // process, so unlike `stamp` below there is no reason to keep a
        // handle to it on [`OfflineIndicator`] itself. `widget.append`
        // gives it an owner (the box), which is all that keeps it alive
        // once this local binding goes out of scope.
        let text = gtk::Label::new(Some(OFFLINE_TEXT));
        text.add_css_class("hop-honesty-text");
        text.set_xalign(0.0);
        widget.append(&text);

        let stamp = gtk::Label::new(None);
        stamp.add_css_class("hop-honesty-stamp");
        stamp.set_xalign(0.0);
        widget.append(&stamp);

        OfflineIndicator { widget, stamp }
    }

    /// Shows or hides the offline indicator. `Some(as_of_hh_mm)` — a
    /// caller-resolved, already-formatted "14:32" string, per this issue's
    /// own key-interfaces note that a widget module receives display
    /// strings already resolved rather than resolving a clock itself
    /// (`ui::row`'s `activate_key_display: Option<&str>` is the same
    /// shape, for the same reason: `ui::window::HopWindow::apply_event` is
    /// where a real wall-clock read belongs, not a widget-construction
    /// module that otherwise has no reason to import a time API at all) —
    /// shows the indicator with that stamp; `None` hides it.
    ///
    /// `text`'s own words never change between calls — [`OFFLINE_TEXT`] is
    /// the offline indicator's one, constant truth; only `stamp` and this
    /// widget's own visibility ever move.
    pub fn apply(&self, as_of_hh_mm: Option<&str>) {
        match as_of_hh_mm {
            Some(as_of_hh_mm) => {
                self.stamp.set_text(&stamp_text(as_of_hh_mm));
                self.widget.set_visible(true);
            }
            None => {
                self.widget.set_visible(false);
            }
        }
    }
}

/// Composes the stamp label's own text from an already-formatted "HH:MM"
/// string — isolated as a pure function, the same reason
/// `ui::row::hint_entered_shown` is isolated from the GTK calls around it:
/// this is a plain string transform with no widget dependency, so it can be
/// unit-tested with no `gtk::init()` at all.
fn stamp_text(as_of_hh_mm: &str) -> String {
    format!("as of {as_of_hh_mm}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the stamp's exact wording — a future edit changing it becomes a
    /// visible, deliberate diff here rather than a silent rewording nobody
    /// meant to make, the same reason `ui::mode_label`'s
    /// `label_for_names_every_mode` test pins its own strings.
    #[test]
    fn stamp_text_reads_as_of_the_given_time() {
        assert_eq!(stamp_text("14:32"), "as of 14:32");
        assert_eq!(stamp_text("09:05"), "as of 09:05");
    }
}
