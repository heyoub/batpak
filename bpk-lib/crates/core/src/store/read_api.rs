use super::*;
use crate::id::EntityIdType;
use crate::id::EventId;
use crate::store::index::IndexEntry;

impl<State> Store<State> {
    /// READ: get a single event by ID.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading from disk fails.
    pub fn get(&self, event_id: EventId) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let raw = event_id.as_u128();
        let entry = self.index.get_by_id(raw).ok_or(StoreError::NotFound(raw))?;
        self.reader.read_entry(&entry.disk_pos)
    }

    /// READ: fetch a single event by ID with the payload left as raw
    /// MessagePack bytes.
    /// Mirrors [`get`](Self::get) but skips the JSON-decode step, suitable
    /// for the `RawMsgpackInput` lane of a multi-event reactor.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading
    /// from disk fails.
    pub fn read_raw(&self, event_id: EventId) -> Result<StoredEvent<Vec<u8>>, StoreError> {
        let raw = event_id.as_u128();
        let entry = self.index.get_by_id(raw).ok_or(StoreError::NotFound(raw))?;
        self.reader.read_entry_raw(&entry.disk_pos)
    }

    /// Verify an append receipt against the store's signing-key registry and
    /// current index state.
    #[must_use]
    pub fn verify_append_receipt(&self, receipt: &AppendReceipt) -> bool {
        let Some(entry) = self.index.get_by_id(receipt.event_id.as_u128()) else {
            return false;
        };
        if !append_receipt_matches_index(receipt, &entry) {
            return false;
        }
        self.runtime.signing_registry.verify_append_receipt(
            receipt,
            &entry.coord,
            entry.kind,
            entry.hash_chain.prev_hash,
        )
    }

    /// Verify a persisted denial receipt against the store's signing-key
    /// registry and current index state.
    #[must_use]
    pub fn verify_denial_receipt(&self, receipt: &DenialReceipt) -> bool {
        let Some(entry) = self.index.get_by_id(receipt.event_id.as_u128()) else {
            return false;
        };
        if !denial_receipt_matches_index(receipt, &entry) {
            return false;
        }
        self.runtime.signing_registry.verify_denial_receipt(
            receipt,
            &entry.coord,
            entry.kind,
            entry.hash_chain.prev_hash,
        )
    }

    /// READ: query by Region.
    #[must_use]
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: walk hash chain ancestors.
    pub fn walk_ancestors(
        &self,
        event_id: EventId,
        limit: usize,
    ) -> Vec<StoredEvent<serde_json::Value>> {
        ancestry::walk_ancestors(self, event_id.as_u128(), limit)
    }

    /// PROJECT: reconstruct typed state from events, with cache support.
    ///
    /// # Errors
    /// Returns any replay, deserialization, cache, or disk-read error surfaced
    /// while reconstructing the projection state.
    pub fn project<T>(&self, entity: &str, freshness: &Freshness) -> Result<Option<T>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project(self, entity, freshness)
    }

    /// Return the current per-entity generation if the entity exists.
    ///
    /// Generations advance monotonically on every insert for that entity.
    /// When entity-group overlays are disabled, this falls back to the entity
    /// stream length so callers still get a stable monotonic skip token.
    pub fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.index.entity_generation(entity)
    }

    /// Project only when the entity changed since `last_seen_generation`.
    ///
    /// Returns `Ok(None)` when no change is observed. Otherwise returns the
    /// generation at which the returned state was materialized together with
    /// the freshly projected state. The returned generation is honest: a
    /// cache-hit path returns the generation at which the cache was
    /// stamped, a replay path returns the generation sampled before replay
    /// started. Callers who persist this generation as a watermark (e.g.
    /// [`ProjectionWatcher`]) will not silently consume a relevant append
    /// against stale state (F5). To preserve that property, this API treats
    /// [`Freshness::MaybeStale`] the same as [`Freshness::Consistent`].
    ///
    /// # Errors
    /// Returns any error surfaced by [`Store::project`] when the entity has
    /// changed and the projection must be rebuilt.
    pub fn project_if_changed<T>(
        &self,
        entity: &str,
        last_seen_generation: u64,
        freshness: &Freshness,
    ) -> Result<Option<(u64, Option<T>)>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_if_changed(self, entity, last_seen_generation, freshness)
    }

    /// READ: query all events for an exact entity id.
    #[must_use]
    pub fn by_entity(&self, entity: &str) -> Vec<IndexEntry> {
        self.index.stream(entity)
    }

    /// READ: query all events in the given scope.
    #[must_use]
    pub fn by_scope(&self, scope: &str) -> Vec<IndexEntry> {
        self.query(&Region::scope(scope))
    }

    /// READ: query all events of the given event kind across all entities and scopes.
    #[must_use]
    pub fn by_fact(&self, kind: EventKind) -> Vec<IndexEntry> {
        self.query(&Region::all().with_fact(KindFilter::Exact(kind)))
    }

    /// READ (typed): query all events whose kind matches `T::KIND`.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`.
    #[must_use]
    pub fn by_fact_typed<T: EventPayload>(&self) -> Vec<IndexEntry> {
        self.by_fact(T::KIND)
    }

    /// CURSOR: pull-based, ordered delivery from the in-memory index.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`. This cursor is
    /// process-local only: it does not persist its position, so restart-time
    /// at-least-once semantics require the checkpoint-bound cursor worker
    /// surface rather than this constructor.
    pub fn cursor_guaranteed(&self, region: &Region) -> Cursor {
        Cursor::new(region.clone(), Arc::clone(&self.index))
    }
}

fn append_receipt_matches_index(receipt: &AppendReceipt, entry: &IndexEntry) -> bool {
    receipt.event_id.as_u128() == entry.event_id
        && receipt.sequence == entry.global_sequence
        && receipt.disk_pos == entry.disk_pos
        && receipt.content_hash == entry.hash_chain.event_hash
        && receipt.extensions == entry.receipt_extensions
}

fn denial_receipt_matches_index(receipt: &DenialReceipt, entry: &IndexEntry) -> bool {
    entry.kind == EventKind::SYSTEM_DENIAL
        && receipt.event_id.as_u128() == entry.event_id
        && receipt.sequence == entry.global_sequence
        && receipt.disk_pos == entry.disk_pos
        && receipt.content_hash == entry.hash_chain.event_hash
        && receipt.extensions == entry.receipt_extensions
}

#[cfg(test)]
mod tests {
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::store::index::DiskPos;
    use crate::store::{Store, StoreConfig};
    use tempfile::TempDir;

    #[test]
    fn append_receipt_verification_rejects_disk_position_tampering() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false),
        )
        .expect("open store");
        let coord = Coordinate::new("entity:receipt-disk-pos", "scope:test").expect("coord");
        let mut receipt = store
            .append(
                &coord,
                EventKind::custom(0xA, 20),
                &serde_json::json!({"n": 1}),
            )
            .expect("append");

        assert!(store.verify_append_receipt(&receipt));
        receipt.disk_pos = DiskPos::new(
            receipt.disk_pos.segment_id(),
            receipt.disk_pos.offset() + 1,
            receipt.disk_pos.length(),
        );

        assert!(
            !store.verify_append_receipt(&receipt),
            "disk position must match the committed index entry"
        );
    }
}
