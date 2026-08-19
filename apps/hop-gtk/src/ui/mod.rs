//! The launcher window's widget tree: the pre-built [`window::HopWindow`],
//! the item [`model`] backing its results list, and the [`view`]-tree
//! renderer — issue #181's seam, today one node type ([`view::Node::Row`])
//! — whose `GtkListView` factory dispatches to [`row`] to build and
//! populate that node's widget.

pub mod marker_highlight;
pub mod mode_label;
pub mod model;
pub mod row;
pub mod view;
pub mod window;
