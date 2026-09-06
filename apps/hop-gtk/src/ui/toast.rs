//! The copy-feedback toast, built once with the launcher window.
//!
//! A toast keeps its widget and lifecycle sources for the lifetime of the
//! window. Showing a new variant cancels every source from the previous
//! lifecycle and advances a generation guard, so a callback that was already
//! queued can never hide a newer message.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

const TOAST_NAME: &str = "hop-toast";
const SHOWN_CLASS: &str = "hop-toast-shown";
const EXITING_CLASS: &str = "hop-toast-exiting";
const ERROR_CLASS: &str = "hop-toast-error";
const HOLD_DURATION: Duration = Duration::from_millis(2_000);
// Long enough for the full-motion close token and deliberately conservative
// for reduced motion's shorter opacity transition.
const EXIT_TRANSITION_DURATION: Duration = Duration::from_millis(140);

#[derive(Default)]
struct Sources {
    idle: Option<glib::SourceId>,
    hold: Option<glib::SourceId>,
    hide: Option<glib::SourceId>,
}

/// One reusable copy-feedback toast.
///
/// `widget` is the overlay child owned by [`crate::ui::window::HopWindow`].
/// The label and source state stay private so every presentation uses the
/// same typed handles and the same cancellable lifecycle.
#[derive(Clone)]
pub struct Toast {
    pub widget: gtk::Box,
    label: gtk::Label,
    generation: Rc<Cell<u64>>,
    sources: Rc<RefCell<Sources>>,
}

impl Toast {
    /// Builds the hidden toast and all of its child widgets exactly once.
    pub fn build() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        widget.set_widget_name(TOAST_NAME);
        widget.add_css_class(TOAST_NAME);
        widget.set_visible(false);
        widget.set_can_target(false);
        widget.set_can_focus(false);
        widget.set_accessible_role(gtk::AccessibleRole::Status);

        let label = gtk::Label::new(None);
        label.set_xalign(0.5);
        label.set_wrap(true);
        label.set_can_target(false);
        widget.append(&label);

        Self {
            widget,
            label,
            generation: Rc::new(Cell::new(0)),
            sources: Rc::new(RefCell::new(Sources::default())),
        }
    }

    /// Shows the exact success message required for a completed copy.
    pub fn show_success(&self) {
        self.show("Result copied", false);
    }

    /// Shows a copy failure while preserving the original error message.
    pub fn show_error(&self, message: &str) {
        self.show(&format!("Copy failed: {message}"), true);
    }

    /// Returns the current message for the widget-level GTK regression test.
    #[cfg(test)]
    pub(crate) fn text(&self) -> glib::GString {
        self.label.text()
    }

    fn show(&self, text: &str, error: bool) {
        self.cancel_lifecycle();
        self.label.set_text(text);
        self.widget.remove_css_class(ERROR_CLASS);
        if error {
            self.widget.add_css_class(ERROR_CLASS);
        }
        self.widget.set_visible(true);

        let generation = self.generation.get();
        let toast = self.clone();
        let idle = glib::idle_add_local_once(move || {
            // Clear the slot before checking the generation. A stale queued
            // callback is still a completed source and must not be removed a
            // second time by the next retrigger.
            toast.sources.borrow_mut().idle.take();
            if toast.generation.get() != generation {
                return;
            }

            toast.widget.add_css_class(SHOWN_CLASS);
            let held_toast = toast.clone();
            let hold = glib::timeout_add_local_once(HOLD_DURATION, move || {
                held_toast.sources.borrow_mut().hold.take();
                if held_toast.generation.get() != generation {
                    return;
                }

                held_toast.widget.remove_css_class(SHOWN_CLASS);
                held_toast.widget.add_css_class(EXITING_CLASS);
                let exiting_toast = held_toast.clone();
                let hide_toast = exiting_toast.clone();
                let hide = glib::timeout_add_local_once(EXIT_TRANSITION_DURATION, move || {
                    hide_toast.sources.borrow_mut().hide.take();
                    if hide_toast.generation.get() != generation {
                        return;
                    }
                    hide_toast.widget.set_visible(false);
                    hide_toast.widget.remove_css_class(EXITING_CLASS);
                    hide_toast.widget.remove_css_class(ERROR_CLASS);
                });
                exiting_toast.sources.borrow_mut().hide = Some(hide);
            });
            toast.sources.borrow_mut().hold = Some(hold);
        });
        self.sources.borrow_mut().idle = Some(idle);
    }

    fn cancel_lifecycle(&self) {
        let mut sources = self.sources.borrow_mut();
        if let Some(source) = sources.idle.take() {
            source.remove();
        }
        if let Some(source) = sources.hold.take() {
            source.remove();
        }
        if let Some(source) = sources.hide.take() {
            source.remove();
        }
        drop(sources);

        self.generation.set(self.generation.get().wrapping_add(1));
        self.widget.remove_css_class(SHOWN_CLASS);
        self.widget.remove_css_class(EXITING_CLASS);
    }
}
