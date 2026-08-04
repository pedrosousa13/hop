//! The seam between a connection and whatever answers its queries.
//!
//! A [`ResultSource`] answers one query with a stream of item batches behind
//! an `mpsc::Receiver`. The channel is the whole contract: batches arrive on
//! it, the source finishing closes it, and the *caller dropping it is
//! cancellation* — a source notices its next `send` fail and stops working.
//! That makes cancellation a property of the seam rather than a protocol
//! bolted onto it, and it is what issue #55's "a new query cancels the old
//! one server-side" hangs off.
//!
//! Until issue #56 lands a provider host, the one production source is
//! [`SkeletonSource`], which answers every query with the same hardcoded
//! item the walking skeleton always has.

use hop_protocol::{Action, ActionId, ActionKind, Item, ItemId, Kind, QueryText};
use tokio::sync::mpsc;

/// Answers queries with streams of item batches.
///
/// `Clone` because every connection gets its own handle; implementations are
/// expected to be cheap handles over shared state, not the state itself.
pub trait ResultSource: Clone + Send + Sync + 'static {
    /// Starts answering one query. Batches arrive on the returned receiver;
    /// the channel closing means the source is done; dropping the receiver
    /// cancels the work.
    fn start(&self, text: QueryText) -> mpsc::Receiver<Vec<Item>>;
}

/// The walking skeleton's source: one batch, one hardcoded item, done.
#[derive(Clone)]
pub struct SkeletonSource;

impl ResultSource for SkeletonSource {
    fn start(&self, _text: QueryText) -> mpsc::Receiver<Vec<Item>> {
        // Capacity 1 makes `try_send` infallible here, and dropping `tx` on
        // return is what closes the channel after the one batch — no task
        // needed for a source with nothing to wait on.
        let (tx, rx) = mpsc::channel(1);
        let _ = tx.try_send(vec![hardcoded_item()]);
        rx
    }
}

/// The walking skeleton's one and only result: every `query` frame gets
/// exactly this item back, regardless of what was typed.
pub(crate) fn hardcoded_item() -> Item {
    Item {
        id: ItemId::new("hop:walking-skeleton").expect("within bounds by construction"),
        kind: Kind::Action,
        title: "Hello from hopd".to_string(),
        subtitle: Some("M2.2 walking skeleton".to_string()),
        icon: None,
        actions: vec![Action {
            id: ActionId::new("open").expect("within bounds by construction"),
            kind: ActionKind::Open,
            label: "Open".to_string(),
        }],
        default_action: ActionId::new("open").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: "skeleton".to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn the_skeleton_source_yields_one_batch_then_finishes() {
        let mut rx = SkeletonSource.start(QueryText::new("anything").unwrap());
        let batch = rx.recv().await.expect("one batch must arrive");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].title, "Hello from hopd");
        assert!(
            rx.recv().await.is_none(),
            "the source must finish after its one batch"
        );
    }
}
