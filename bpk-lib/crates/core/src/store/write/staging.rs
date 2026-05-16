use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{Event, EventHeader, EventKind, HashChain, StoredEvent};
use crate::store::append::checked_payload_len;
use crate::store::append::{AppendOptions, BatchAppendItem, CausationRef};
use crate::store::index::interner::InternId;
use crate::store::index::StoreIndex;
use crate::store::StoreError;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PreparedBatchItem {
    coord: Coordinate,
    entity: Arc<str>,
    scope: Arc<str>,
    kind: EventKind,
    payload_bytes: Vec<u8>,
    options: AppendOptions,
    causation: CausationRef,
}

impl PreparedBatchItem {
    fn from_shared_parts(
        entity: Arc<str>,
        scope: Arc<str>,
        kind: EventKind,
        payload_bytes: Vec<u8>,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<Self, StoreError> {
        let coord = Coordinate::from_shared_parts(Arc::clone(&entity), Arc::clone(&scope))?;
        Ok(Self {
            coord,
            entity,
            scope,
            kind,
            payload_bytes,
            options,
            causation,
        })
    }

    pub(crate) fn coord(&self) -> &Coordinate {
        &self.coord
    }

    pub(crate) fn entity_arc(&self) -> &Arc<str> {
        &self.entity
    }

    pub(crate) fn scope_arc(&self) -> &Arc<str> {
        &self.scope
    }

    pub(crate) fn kind(&self) -> EventKind {
        self.kind
    }

    pub(crate) fn payload_bytes(&self) -> &[u8] {
        &self.payload_bytes
    }

    pub(crate) fn options(&self) -> AppendOptions {
        self.options.clone()
    }

    pub(crate) fn causation(&self) -> CausationRef {
        self.causation
    }
}

pub(crate) struct PreparedBatchBuilder {
    expected_len: usize,
    items: Vec<PreparedBatchItem>,
    total_bytes: usize,
    entity_pool: HashMap<String, Arc<str>>,
    scope_pool: HashMap<String, Arc<str>>,
    unique_entities: Vec<Arc<str>>,
    unique_scopes: Vec<Arc<str>>,
}

impl PreparedBatchBuilder {
    pub(crate) fn new(expected_len: usize) -> Self {
        Self {
            expected_len,
            items: Vec::with_capacity(expected_len),
            total_bytes: 0,
            entity_pool: HashMap::new(),
            scope_pool: HashMap::new(),
            unique_entities: Vec::new(),
            unique_scopes: Vec::new(),
        }
    }

    pub(crate) fn push_item(&mut self, item: BatchAppendItem) -> Result<(), StoreError> {
        let (coord, kind, payload_bytes, options, causation) = item.into_parts();
        self.push_shared_parts(
            coord.entity_arc(),
            coord.scope_arc(),
            kind,
            payload_bytes,
            options,
            causation,
        )
    }

    fn push_shared_parts(
        &mut self,
        entity: Arc<str>,
        scope: Arc<str>,
        kind: EventKind,
        payload_bytes: Vec<u8>,
        options: AppendOptions,
        causation: CausationRef,
    ) -> Result<(), StoreError> {
        let entity = self.intern_entity_arc(entity);
        let scope = self.intern_scope_arc(scope);
        self.total_bytes += payload_bytes.len();
        self.items.push(PreparedBatchItem::from_shared_parts(
            entity,
            scope,
            kind,
            payload_bytes,
            options,
            causation,
        )?);
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<PreparedBatch, StoreError> {
        if self.items.len() != self.expected_len {
            return Err(StoreError::InvariantViolation {
                reason: format!(
                    "prepared batch item count changed during staging: expected {}, got {}",
                    self.expected_len,
                    self.items.len()
                ),
            });
        }
        Ok(PreparedBatch {
            items: self.items,
            total_bytes: self.total_bytes,
            unique_entities: self.unique_entities,
            unique_scopes: self.unique_scopes,
        })
    }

    fn intern_entity_arc(&mut self, entity: Arc<str>) -> Arc<str> {
        if let Some(shared) = self.entity_pool.get(entity.as_ref()) {
            return Arc::clone(shared);
        }
        self.entity_pool
            .insert(entity.to_string(), Arc::clone(&entity));
        self.unique_entities.push(Arc::clone(&entity));
        entity
    }

    fn intern_scope_arc(&mut self, scope: Arc<str>) -> Arc<str> {
        if let Some(shared) = self.scope_pool.get(scope.as_ref()) {
            return Arc::clone(shared);
        }
        self.scope_pool
            .insert(scope.to_string(), Arc::clone(&scope));
        self.unique_scopes.push(Arc::clone(&scope));
        scope
    }
}

pub(crate) struct PreparedBatch {
    items: Vec<PreparedBatchItem>,
    total_bytes: usize,
    unique_entities: Vec<Arc<str>>,
    unique_scopes: Vec<Arc<str>>,
}

impl PreparedBatch {
    pub(crate) fn from_items(items: Vec<BatchAppendItem>) -> Result<Self, StoreError> {
        let mut builder = PreparedBatchBuilder::new(items.len());
        for item in items {
            builder.push_item(item)?;
        }
        builder.finish()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub(crate) fn items(&self) -> &[PreparedBatchItem] {
        &self.items
    }

    #[cfg(test)]
    pub(crate) fn unique_entity_count(&self) -> usize {
        self.unique_entities.len()
    }

    #[cfg(test)]
    pub(crate) fn unique_scope_count(&self) -> usize {
        self.unique_scopes.len()
    }

    pub(crate) fn interned_ids(&self, index: &StoreIndex) -> PreparedBatchInternedIds {
        let entity_ids = self
            .unique_entities
            .iter()
            .map(|entity| (Arc::clone(entity), index.interner.intern(entity)))
            .collect();
        let scope_ids = self
            .unique_scopes
            .iter()
            .map(|scope| (Arc::clone(scope), index.interner.intern(scope)))
            .collect();
        PreparedBatchInternedIds {
            entity_ids,
            scope_ids,
        }
    }
}

pub(crate) struct PreparedBatchInternedIds {
    entity_ids: HashMap<Arc<str>, InternId>,
    scope_ids: HashMap<Arc<str>, InternId>,
}

impl PreparedBatchInternedIds {
    // justifies: src/store/write/staging.rs builds PreparedBatchInternedIds from the exact batch items, so absence here indicates an internal bug.
    #[allow(clippy::expect_used)]
    pub(crate) fn entity_id(&self, item: &PreparedBatchItem) -> InternId {
        *self
            .entity_ids
            .get(item.entity_arc())
            .expect("prepared batch entity dedupe must include every item entity")
    }

    // justifies: src/store/write/staging.rs builds PreparedBatchInternedIds from the exact batch items, so absence here indicates an internal bug.
    #[allow(clippy::expect_used)]
    pub(crate) fn scope_id(&self, item: &PreparedBatchItem) -> InternId {
        *self
            .scope_ids
            .get(item.scope_arc())
            .expect("prepared batch scope dedupe must include every item scope")
    }
}

#[derive(Clone)]
pub(crate) struct StagedCommittedEvent {
    pub(crate) coord: Coordinate,
    pub(crate) meta: StagedCommitMeta,
    pub(crate) timing: StagedCommitTiming,
    pub(crate) hash_chain: HashChain,
}

#[derive(Clone, Copy)]
pub(crate) struct StagedCommitMeta {
    pub(crate) event_id: u128,
    pub(crate) correlation_id: u128,
    pub(crate) causation_id: Option<u128>,
    pub(crate) kind: EventKind,
    pub(crate) global_sequence: u64,
}

impl StagedCommitMeta {
    pub(crate) fn new(
        event_id: u128,
        correlation_id: u128,
        causation_id: Option<u128>,
        kind: EventKind,
        global_sequence: u64,
    ) -> Self {
        Self {
            event_id,
            correlation_id,
            causation_id,
            kind,
            global_sequence,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct StagedCommitTiming {
    pub(crate) timestamp_us: i64,
    pub(crate) wall_ms: u64,
    pub(crate) clock: u32,
    pub(crate) dag_lane: u32,
    pub(crate) dag_depth: u32,
}

impl StagedCommitTiming {
    pub(crate) fn new(
        timestamp_us: i64,
        wall_ms: u64,
        clock: u32,
        dag_lane: u32,
        dag_depth: u32,
    ) -> Self {
        Self {
            timestamp_us,
            wall_ms,
            clock,
            dag_lane,
            dag_depth,
        }
    }
}

impl StagedCommittedEvent {
    pub(crate) fn new(
        coord: Coordinate,
        meta: StagedCommitMeta,
        timing: StagedCommitTiming,
        hash_chain: HashChain,
    ) -> Self {
        Self {
            coord,
            meta,
            timing,
            hash_chain,
        }
    }

    pub(crate) fn event_id(&self) -> u128 {
        self.meta.event_id
    }

    pub(crate) fn global_sequence(&self) -> u64 {
        self.meta.global_sequence
    }

    pub(crate) fn position(&self) -> DagPosition {
        DagPosition::with_hlc(
            self.timing.wall_ms,
            0,
            self.timing.dag_depth,
            self.timing.dag_lane,
            self.timing.clock,
        )
    }

    fn event_header(&self, payload_size: u32) -> EventHeader {
        EventHeader::new(
            self.meta.event_id,
            self.meta.correlation_id,
            self.meta.causation_id,
            self.timing.timestamp_us,
            self.position(),
            payload_size,
            self.meta.kind,
        )
    }

    pub(crate) fn borrowed_frame_event<'a>(
        &self,
        payload_bytes: &'a [u8],
    ) -> Result<Event<&'a [u8]>, StoreError> {
        let payload_size = checked_payload_len(payload_bytes)?;
        let mut event = Event::new(self.event_header(payload_size), payload_bytes);
        event.hash_chain = Some(self.hash_chain.clone());
        event.header.content_hash = self.hash_chain.event_hash;
        Ok(event)
    }

    pub(crate) fn stored_event(
        &self,
        payload_bytes: &[u8],
        flags: u8,
    ) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let mut event = Event::new(
            self.event_header(checked_payload_len(payload_bytes)?)
                .with_flags(flags),
            rmp_serde::from_slice::<serde_json::Value>(payload_bytes)
                .map_err(|error| StoreError::Serialization(Box::new(error)))?,
        );
        event.hash_chain = Some(self.hash_chain.clone());
        event.header.content_hash = self.hash_chain.event_hash;

        Ok(StoredEvent {
            coordinate: self.coord.clone(),
            event,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PreparedBatch, PreparedBatchBuilder};
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::store::{AppendOptions, BatchAppendItem, CausationRef, StoreError};

    #[test]
    fn prepared_batch_dedupes_entity_and_scope_strings() {
        let coord_a = Coordinate::new("entity:a", "scope:shared").expect("coord a");
        let coord_b = Coordinate::new("entity:b", "scope:shared").expect("coord b");
        let kind = EventKind::custom(0xF, 1);
        let items = vec![
            BatchAppendItem::new(
                coord_a.clone(),
                kind,
                &serde_json::json!({"i": 1}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("item 1"),
            BatchAppendItem::new(
                coord_a,
                kind,
                &serde_json::json!({"i": 2}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("item 2"),
            BatchAppendItem::new(
                coord_b,
                kind,
                &serde_json::json!({"i": 3}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("item 3"),
        ];

        let prepared = PreparedBatch::from_items(items).expect("prepare batch");
        assert_eq!(prepared.len(), 3);
        assert_eq!(prepared.unique_entity_count(), 2);
        assert_eq!(prepared.unique_scope_count(), 1);
        assert_eq!(
            prepared.total_bytes(),
            prepared
                .items()
                .iter()
                .map(|item| item.payload_bytes().len())
                .sum::<usize>()
        );
        assert!(
            std::sync::Arc::ptr_eq(
                prepared.items()[0].entity_arc(),
                prepared.items()[1].entity_arc()
            ),
            "duplicate entity text should converge onto one shared Arc<str>"
        );
        assert!(
            std::sync::Arc::ptr_eq(
                prepared.items()[0].scope_arc(),
                prepared.items()[2].scope_arc()
            ),
            "duplicate scope text should converge onto one shared Arc<str>"
        );
    }

    #[test]
    fn prepared_batch_finish_fails_closed_on_item_count_drift() -> Result<(), String> {
        let coord = Coordinate::new("entity:a", "scope:shared").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        let item = BatchAppendItem::new(
            coord,
            kind,
            &serde_json::json!({"i": 1}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("item");
        let mut builder = PreparedBatchBuilder::new(2);
        builder.push_item(item).expect("stage one item");

        let err = match builder.finish() {
            Ok(_) => return Err("PROPERTY: item-count drift must fail closed".into()),
            Err(err) => err,
        };

        assert!(
            matches!(err, StoreError::InvariantViolation { ref reason } if reason.contains("expected 2, got 1")),
            "wrong error for prepared batch count drift: {err:?}"
        );
        Ok(())
    }
}
