use super::*;
use crate::id::EntityIdType;
use crate::id::EventId;
use crate::store::index::IndexEntry;
use std::collections::BTreeMap;

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
    /// current index state, returning only a boolean.
    ///
    /// Prefer [`Self::verify_append_receipt_detailed`] in new code when the
    /// caller needs proof language or a stable rejection reason.
    #[must_use]
    pub fn verify_append_receipt(&self, receipt: &AppendReceipt) -> bool {
        self.verify_append_receipt_detailed(receipt).is_valid()
    }

    /// Verify ack-shaped append receipt fields against the store's signing-key
    /// registry and current index state.
    ///
    /// Wire transports omit [`AppendReceipt::disk_pos`]; this helper hydrates
    /// it from the committed index entry before delegating to
    /// [`Self::verify_append_receipt_detailed`].
    #[must_use]
    pub fn verify_append_receipt_wire_detailed(
        &self,
        event_id: EventId,
        sequence: u64,
        content_hash: [u8; 32],
        key_id: [u8; 32],
        signature: Option<[u8; 64]>,
        extensions: BTreeMap<ExtensionKey, EncodedBytes>,
    ) -> ReceiptVerification {
        let Some(entry) = self.index.get_by_id(event_id.as_u128()) else {
            return ReceiptVerification::Invalid(ReceiptVerificationError::MissingCommittedEvent);
        };
        let receipt = AppendReceipt {
            event_id,
            sequence,
            disk_pos: entry.disk_pos,
            content_hash,
            key_id,
            signature,
            extensions,
        };
        self.verify_append_receipt_detailed(&receipt)
    }

    /// Verify a full persisted append receipt and return the exact acceptance
    /// or rejection reason.
    ///
    /// This API expects the native [`AppendReceipt`], including its committed
    /// disk position. Wire transports that only carry ack-shaped fields should
    /// use [`Self::verify_append_receipt_wire_detailed`] so the store can
    /// hydrate the disk position from the committed index entry.
    #[must_use]
    pub fn verify_append_receipt_detailed(&self, receipt: &AppendReceipt) -> ReceiptVerification {
        let Some(entry) = self.index.get_by_id(receipt.event_id.as_u128()) else {
            return ReceiptVerification::Invalid(ReceiptVerificationError::MissingCommittedEvent);
        };
        if let Some(error) = append_receipt_index_mismatch(receipt, &entry) {
            return ReceiptVerification::Invalid(error);
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
        self.verify_denial_receipt_detailed(receipt).is_valid()
    }

    /// Verify a persisted denial receipt and return the exact acceptance or
    /// rejection reason.
    #[must_use]
    pub fn verify_denial_receipt_detailed(&self, receipt: &DenialReceipt) -> ReceiptVerification {
        let Some(entry) = self.index.get_by_id(receipt.event_id.as_u128()) else {
            return ReceiptVerification::Invalid(ReceiptVerificationError::MissingCommittedEvent);
        };
        if let Some(error) = denial_receipt_index_mismatch(receipt, &entry) {
            return ReceiptVerification::Invalid(error);
        }
        self.runtime.signing_registry.verify_denial_receipt(
            receipt,
            &entry.coord,
            entry.kind,
            entry.hash_chain.prev_hash,
        )
    }

    /// READ: return every currently visible index entry matching a Region.
    ///
    /// This is a convenience snapshot read for small, already-bounded regions.
    /// For replay, audit, host parity, or user-facing pagination, prefer
    /// [`Self::query_entries_after`], which pages strictly by
    /// `global_sequence`.
    #[must_use]
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: return every currently visible index entry matching a Region on
    /// one exact DAG lane.
    ///
    /// The explicit `lane` argument is authoritative. Passing a `Region` that
    /// already carries a lane is only valid when it matches this argument.
    #[must_use]
    pub fn query_lane(&self, region: &Region, lane: u32) -> Vec<IndexEntry> {
        debug_assert!(
            region.lane.is_none() || region.lane == Some(lane),
            "query_lane lane argument must match any pre-set Region lane"
        );
        self.index.query(&region.clone().with_lane(lane))
    }

    /// READ: query a bounded page of visible events by Region in ascending
    /// `global_sequence` order.
    ///
    /// Pass `None` for the first page. Pass the last returned entry's
    /// [`IndexEntry::global_sequence`] as `Some(after_global_sequence)` to
    /// resume strictly after that entry. `limit == 0` returns an empty page.
    ///
    /// This is commit-order pagination, not a live cursor or server-held
    /// session. Durable delivery cursors live under the delivery APIs.
    #[must_use]
    pub fn query_entries_after(
        &self,
        region: &Region,
        after_global_sequence: Option<u64>,
        limit: usize,
    ) -> Vec<IndexEntry> {
        let after_seq = after_global_sequence.unwrap_or(0);
        let started = after_global_sequence.is_some();
        self.index
            .query_hits_after(region, after_seq, started, limit)
            .into_iter()
            .filter_map(|hit| self.index.upgrade_hit(hit))
            .collect()
    }

    /// READ: walk bounded hash-chain ancestors from an event id.
    ///
    /// This is substrate ancestry, not domain graph traversal.
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

    /// PROJECT: reconstruct two typed states from one consistent direct replay.
    ///
    /// Both projections must use the same replay input lane, and each is folded
    /// over only its declared [`EventSourced::relevant_event_kinds`]. This
    /// fused path intentionally bypasses projection caches so cache watermarks
    /// remain projection-specific.
    ///
    /// # Errors
    /// Returns any disk-read or replay decode error surfaced while loading the
    /// shared event stream.
    pub fn project_fused2<Left, Right>(
        &self,
        entity: &str,
    ) -> Result<(Option<Left>, Option<Right>), StoreError>
    where
        Left: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        Right: EventSourced<Input = Left::Input>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + 'static,
        Left::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_fused2(self, entity)
    }

    /// PROJECT: reconstruct three typed states from one consistent direct replay.
    ///
    /// The projections must use the same replay input lane. A projection whose
    /// [`EventSourced::relevant_event_kinds`] slice is empty receives the full
    /// shared stream; other projections receive only their declared kinds.
    ///
    /// # Errors
    /// Returns any disk-read or replay decode error surfaced while loading the
    /// shared event stream.
    pub fn project_fused3<First, Second, Third>(
        &self,
        entity: &str,
    ) -> Result<super::ProjectionFusion3<First, Second, Third>, StoreError>
    where
        First: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        Second: EventSourced<Input = First::Input>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + 'static,
        Third: EventSourced<Input = First::Input>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + 'static,
        First::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_fused3(self, entity)
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

    /// READ: query all events for an exact entity id on one DAG lane.
    #[must_use]
    pub fn by_entity_lane(&self, entity: &str, lane: u32) -> Vec<IndexEntry> {
        self.index.stream_lane(entity, lane)
    }

    /// READ: query all events for an exact entity id on one DAG lane.
    #[must_use]
    pub fn stream_lane(&self, entity: &str, lane: u32) -> Vec<IndexEntry> {
        self.by_entity_lane(entity, lane)
    }

    /// READ: return the latest visible event for an entity on one DAG lane.
    #[must_use]
    pub fn latest_lane(&self, entity: &str, lane: u32) -> Option<IndexEntry> {
        self.index.get_latest(entity, lane)
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
    /// process-local durable-delivery vocabulary, not query pagination. It
    /// does not persist its position, so restart-time at-least-once semantics
    /// require the checkpoint-bound cursor worker surface rather than this
    /// constructor.
    pub fn cursor_guaranteed(&self, region: &Region) -> Cursor {
        Cursor::new(region.clone(), Arc::clone(&self.index))
    }
}

fn append_receipt_index_mismatch(
    receipt: &AppendReceipt,
    entry: &IndexEntry,
) -> Option<ReceiptVerificationError> {
    if receipt.event_id.as_u128() != entry.event_id {
        return Some(ReceiptVerificationError::EventIdMismatch);
    }
    if receipt.sequence != entry.global_sequence {
        return Some(ReceiptVerificationError::SequenceMismatch);
    }
    if receipt.disk_pos != entry.disk_pos {
        return Some(ReceiptVerificationError::DiskPositionMismatch);
    }
    if receipt.content_hash != entry.hash_chain.event_hash {
        return Some(ReceiptVerificationError::ContentHashMismatch);
    }
    if receipt.extensions != entry.receipt_extensions {
        return Some(ReceiptVerificationError::ExtensionsMismatch);
    }
    None
}

fn denial_receipt_index_mismatch(
    receipt: &DenialReceipt,
    entry: &IndexEntry,
) -> Option<ReceiptVerificationError> {
    if entry.kind != EventKind::SYSTEM_DENIAL {
        return Some(ReceiptVerificationError::DenialKindMismatch);
    }
    if receipt.event_id.as_u128() != entry.event_id {
        return Some(ReceiptVerificationError::EventIdMismatch);
    }
    if receipt.sequence != entry.global_sequence {
        return Some(ReceiptVerificationError::SequenceMismatch);
    }
    if receipt.disk_pos != entry.disk_pos {
        return Some(ReceiptVerificationError::DiskPositionMismatch);
    }
    if receipt.content_hash != entry.hash_chain.event_hash {
        return Some(ReceiptVerificationError::ContentHashMismatch);
    }
    if receipt.extensions != entry.receipt_extensions {
        return Some(ReceiptVerificationError::ExtensionsMismatch);
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::store::index::DiskPos;
    use crate::store::{ReceiptVerification, ReceiptVerificationError, Store, StoreConfig};
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

        assert_eq!(
            store.verify_append_receipt_detailed(&receipt),
            ReceiptVerification::UnsignedAccepted
        );
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
        assert_eq!(
            store.verify_append_receipt_detailed(&receipt),
            ReceiptVerification::Invalid(ReceiptVerificationError::DiskPositionMismatch)
        );
    }

    #[test]
    fn wire_append_receipt_verification_hydrates_disk_pos_from_index() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false),
        )
        .expect("open store");
        let coord = Coordinate::new("entity:wire-verify", "scope:test").expect("coord");
        let receipt = store
            .append(
                &coord,
                EventKind::custom(0xA, 22),
                &serde_json::json!({"n": 1}),
            )
            .expect("append");

        let verification = store.verify_append_receipt_wire_detailed(
            receipt.event_id,
            receipt.sequence,
            receipt.content_hash,
            receipt.key_id,
            receipt.signature,
            receipt.extensions.clone(),
        );
        assert_eq!(verification, ReceiptVerification::UnsignedAccepted);
    }
}
