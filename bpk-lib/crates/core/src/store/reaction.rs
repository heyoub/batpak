//! Typed reactor output batch (Dispatch Chapter T2).
//!
//! [`ReactionBatch`] is the accumulator a [`TypedReactive`] handler writes
//! into. It is a thin, typed wrapper over [`Vec<BatchAppendItem>`]:
//!
//! * Items are pushed via [`ReactionBatch::push_typed`] — kind is inferred
//!   from the payload's `T::KIND`, so handler code never writes
//!   `EventKind::custom(...)`.
//! * The batch is flushed by the typed-reactor loop (via
//!   [`Store::append_reaction_batch`]), and only when the handler returned
//!   `Ok(())`. Each `ReactionBatch` flush is atomic with respect to its own
//!   batch append, but a whole cursor poll may flush several of these
//!   batches sequentially; the typed reactor is therefore at-least-once, not
//!   one giant atomic multi-event append.
//! * If the handler returns `Err`, the `ReactionBatch` is dropped and no
//!   items from that event land in the store — drop-on-error is structural,
//!   not runtime.
//! * Construction (`new`) and `flush` are `pub(crate)`. Users never build
//!   or flush a batch directly; the reactor loop owns both ends.
//!
//! [`TypedReactive`]: crate::event::sourcing::TypedReactive
//! [`Store::append_reaction_batch`]: crate::store::Store::append_reaction_batch
//! [`Vec<BatchAppendItem>`]: BatchAppendItem

use std::sync::Arc;

use crate::coordinate::Coordinate;
use crate::event::EventPayload;
use crate::store::append::{AppendOptions, BatchAppendItem, CausationRef};
use crate::store::{AppendReceipt, Open, Store, StoreError};

/// Typed output batch accumulated by a reactor handler and flushed by the
/// typed-reactor loop when the handler returns `Ok(())`.
///
/// See the module docs for the drop-on-error guarantee and the flush model.
pub struct ReactionBatch {
    items: Vec<BatchAppendItem>,
}

impl ReactionBatch {
    /// Construct an empty batch. Reactor loops own their own batches; users do
    /// not build this directly.
    pub(crate) fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Push a typed reaction — kind is inferred from `T::KIND`.
    ///
    /// # Errors
    /// Returns [`StoreError::Serialization`] if the payload cannot be
    /// serialized to MessagePack at stage time.
    pub fn push_typed<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        causation: CausationRef,
    ) -> Result<(), StoreError> {
        self.push_typed_with_options(coord, payload, AppendOptions::default(), causation)
    }

    /// Push a typed reaction with explicit [`AppendOptions`] — kind is inferred
    /// from `T::KIND`.
    ///
    /// # Errors
    /// Returns [`StoreError::Serialization`] if the payload cannot be
    /// serialized to MessagePack at stage time.
    pub fn push_typed_with_options<T: EventPayload>(
        &mut self,
        coord: Coordinate,
        payload: &T,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<(), StoreError> {
        let item = BatchAppendItem::typed(coord, payload, options, causation)?;
        self.items.push(item);
        Ok(())
    }

    /// Number of staged reactions.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when nothing has been staged.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Flush all staged reactions as one batch append with the supplied
    /// correlation and causation IDs inherited from the triggering source
    /// event.
    ///
    /// Per-item causation overrides passed via [`CausationRef::Absolute`] are
    /// preserved by [`Store::append_reaction_batch`] (it only fills the
    /// default causation when the item's causation is `None`).
    ///
    /// Called only by the typed-reactor loop after the handler returned
    /// `Ok(())`. A batch that is not flushed (because the handler errored)
    /// is dropped and no partial commits occur.
    ///
    /// # Errors
    /// Returns any [`StoreError`] surfaced by the underlying batch append.
    pub(crate) fn flush(
        self,
        store: &Arc<Store<Open>>,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        if self.items.is_empty() {
            return Ok(Vec::new());
        }
        store.append_reaction_batch(
            crate::id::CorrelationId::from(correlation_id),
            crate::id::CausationId::from(causation_id),
            self.items,
        )
    }
}

impl Default for ReactionBatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
// Unit tests exercise `pub(crate) flush`; setup uses `.expect(..)` so each
// failure carries a message, and they are gated by #[cfg(test)] so they never
// reach non-test builds.
mod tests {
    //! Internal unit tests for `ReactionBatch::flush`. `flush` is `pub(crate)`
    //! because users never call it directly — the typed-reactor loop (T4b)
    //! owns the call site. Until T4b ships, these unit tests are the only
    //! witness that `flush` works. After T4b lands, its integration tests
    //! are the primary witness; these stay as unit-level guards.
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::store::{Store, StoreConfig};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct InternalA {
        n: u64,
    }
    impl crate::event::EventPayload for InternalA {
        const KIND: crate::event::EventKind = crate::event::EventKind::custom(6, 1);
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct InternalB {
        s: String,
    }
    impl crate::event::EventPayload for InternalB {
        const KIND: crate::event::EventKind = crate::event::EventKind::custom(6, 2);
    }

    fn open_store() -> (Arc<Store<Open>>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open");
        (Arc::new(store), dir)
    }

    #[test]
    fn flush_returns_empty_receipts_for_empty_batch() {
        let (store, _dir) = open_store();
        let batch = ReactionBatch::new();
        let receipts = batch.flush(&store, 0, 0).expect("flush empty");
        assert!(receipts.is_empty());
    }

    #[test]
    fn flush_commits_multi_item_batch_atomically() {
        let (store, _dir) = open_store();
        let source = store
            .append_typed(
                &Coordinate::new("entity:reaction-internal-src", "scope:test")
                    .expect("valid coordinate"),
                &InternalA { n: 1 },
            )
            .expect("source append");

        let before = store.stats().global_sequence;

        let target_coord = Coordinate::new("entity:reaction-internal-tgt", "scope:test")
            .expect("valid coordinate");
        let mut batch = ReactionBatch::new();
        batch
            .push_typed(
                target_coord.clone(),
                &InternalA { n: 2 },
                CausationRef::None,
            )
            .expect("push reaction item into batch");
        batch
            .push_typed(
                target_coord.clone(),
                &InternalB {
                    s: "chained".into(),
                },
                CausationRef::PriorItem(0),
            )
            .expect("push reaction item into batch");
        assert_eq!(batch.len(), 2);

        let receipts = batch
            .flush(
                &store,
                {
                    use crate::id::EntityIdType;
                    source.event_id.as_u128()
                },
                {
                    use crate::id::EntityIdType;
                    source.event_id.as_u128()
                },
            )
            .expect("flush");
        assert_eq!(
            receipts.len(),
            2,
            "PROPERTY: flush returns one receipt per pushed item"
        );

        // Atomic visibility: both events appear together.
        let after = store.stats().global_sequence;
        assert_eq!(
            after - before,
            2,
            "PROPERTY: atomic flush advances sequence by exactly item count"
        );

        // Kind stamping survived flush.
        assert_eq!(store.by_fact_typed::<InternalA>().len(), 2);
        assert_eq!(store.by_fact_typed::<InternalB>().len(), 1);
    }

    #[test]
    fn prior_item_causation_resolves_within_flush() {
        let (store, _dir) = open_store();
        let source = store
            .append_typed(
                &Coordinate::new("entity:reaction-chain-src", "scope:test")
                    .expect("valid coordinate"),
                &InternalA { n: 10 },
            )
            .expect("source");
        let target =
            Coordinate::new("entity:reaction-chain-tgt", "scope:test").expect("valid coordinate");
        let mut batch = ReactionBatch::new();
        batch
            .push_typed(target.clone(), &InternalA { n: 11 }, CausationRef::None)
            .expect("push reaction item into batch");
        batch
            .push_typed(
                target.clone(),
                &InternalB {
                    s: "after-0".into(),
                },
                CausationRef::PriorItem(0),
            )
            .expect("push reaction item into batch");
        let receipts = batch
            .flush(
                &store,
                {
                    use crate::id::EntityIdType;
                    source.event_id.as_u128()
                },
                {
                    use crate::id::EntityIdType;
                    source.event_id.as_u128()
                },
            )
            .expect("flush");
        assert_eq!(receipts.len(), 2);

        // The second item was caused by the first. Fetch and verify.
        let second = store.get(receipts[1].event_id).expect("get second");
        assert_eq!(
            second.event.header.causation_id,
            Some({
                use crate::id::EntityIdType;
                crate::id::CausationId::from(receipts[0].event_id.as_u128())
            }),
            "PROPERTY: PriorItem causation resolves to first item's event_id"
        );
    }
}
