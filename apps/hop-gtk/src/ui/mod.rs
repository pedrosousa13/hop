//! The launcher window's widget tree: the pre-built [`window::HopWindow`],
//! the item [`model`] backing its results list, and the [`view`]-tree
//! renderer — issue #181's seam, today one node type ([`view::Node::Row`])
//! — whose `GtkListView` factory dispatches to [`row`] to build and
//! populate that node's widget.
//!
//! [`action_panel`] is a later, structurally separate addition (issue
//! #254): the ctrl-K action panel is not a node this window's own
//! `GtkListView` recycles through [`view`]/[`row`] at all — it is its own,
//! self-contained, presented-on-demand widget, built once and populated
//! per selected [`hop_protocol::Item`] the same way [`offline_indicator`]
//! is built once and re-applied per event. See that module's own top doc
//! comment for the full account of why it owns no dependency on `ipc` or
//! [`window`].

pub mod action_panel;
pub mod marker_highlight;
pub mod mode_label;
pub mod model;
pub mod offline_indicator;
pub mod row;
pub mod view;
pub mod window;
