//! Renders a widget's current on-screen state to a PNG and returns —
//! `hop-gtk --screenshot <path>`'s implementation (acceptance criterion 7),
//! and the mechanism the CI headless smoke test uses to capture the empty
//! and results states (criterion 8).
//!
//! # Why a `GskRenderer` capture rather than a GDK screenshot API
//!
//! GDK's own screenshot facilities (`gdk_pixbuf_get_from_window` and
//! friends) are GTK3-era and window-system-specific — exactly the wrong
//! shape for a headless backend (`broadway`, `offscreen`) where there is no
//! conventional window server to ask, only whichever of those two the build
//! and the environment actually have (see `app::run_screenshot`'s doc
//! comment for which one this issue verified). What every GDK backend
//! *does* have is a [`gsk::Renderer`] that can render a
//! [`gsk::RenderNode`] to a [`gdk::Texture`] in memory. Getting from "a
//! widget" to "a render node" goes through [`gtk::WidgetPaintable`] (a
//! [`gdk::Paintable`] view of any widget) and [`gtk::Snapshot`] (records
//! drawing operations into a node) — both ordinary public GTK4 API, no
//! window-system-specific code anywhere in this file.
//!
//! [`gsk::CairoRenderer`] is used deliberately rather than letting GTK pick
//! its default GL/Vulkan renderer: this function's whole purpose is running
//! where there may be no display server and no GPU context to attach to,
//! and Cairo (software) rendering needs neither.

use std::fmt;
use std::path::Path;

use gtk::prelude::*;
use gtk::{gdk, gsk};

/// Everything that can go wrong capturing a screenshot.
#[derive(Debug)]
pub enum ScreenshotError {
    /// `widget` recorded no drawing at all — an unrealized or zero-size
    /// widget, most likely.
    NoRenderNode,
    /// The Cairo renderer could not be realized.
    Realize(glib::Error),
    /// [`gdk::Texture::save_to_png`] failed — an unwritable `path`, most
    /// likely.
    Save(glib::BoolError),
}

impl fmt::Display for ScreenshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScreenshotError::NoRenderNode => {
                write!(f, "widget produced no drawing to capture")
            }
            ScreenshotError::Realize(err) => write!(f, "failed to realize a renderer: {err}"),
            ScreenshotError::Save(err) => write!(f, "failed to write PNG: {err}"),
        }
    }
}

impl std::error::Error for ScreenshotError {}

/// Renders `widget`'s current appearance to a PNG at `path`.
///
/// `widget` must already be realized and size-allocated — a widget that has
/// never been through a main-loop iteration since `present()` has no
/// geometry yet regardless of backend; see `app::run_screenshot`'s doc
/// comment for how it gives the offscreen backend that iteration before
/// calling this.
pub fn capture(widget: &gtk::Widget, path: &Path) -> Result<(), ScreenshotError> {
    let width = f64::from(widget.width().max(1));
    let height = f64::from(widget.height().max(1));

    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width, height);
    let node = snapshot.to_node().ok_or(ScreenshotError::NoRenderNode)?;

    let renderer = gsk::CairoRenderer::new();
    renderer
        .realize(None::<&gdk::Surface>)
        .map_err(ScreenshotError::Realize)?;
    let texture = renderer.render_texture(&node, None);
    renderer.unrealize();

    texture.save_to_png(path).map_err(ScreenshotError::Save)
}
