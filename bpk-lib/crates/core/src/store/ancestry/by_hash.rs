use super::AncestryBoundary;
use crate::event::StoredEvent;
use crate::id::EventId;
use crate::store::Store;

/// Walk hash-chain ancestors of `event_id`, up to `limit` entries, reporting
/// where the walk stopped.
///
/// The per-hop parent lookup is a linear scan over the entity's stream
/// (`entity_stream.iter().find(…)`). That is O(N) per hop where N is
/// the entity's total event count, giving O(limit · N) worst case.
/// Entity streams in batpak typically contain tens to thousands of
/// events — the linear-scan cost is negligible compared to the segment
/// disk read that `read_entry_and_event` performs per hop. Benchmarks
/// showed the per-hop cost (~6 µs) is dominated by disk I/O, not the
/// scan, and a dedicated `by_event_hash` DashMap was rejected because
/// its ~15-30% scan saving did not justify a permanent per-event entry
/// across the whole index. This linear-scan shape is deliberate and
/// bounded; cycle detection lives in the caller (`ancestry::mod`) via
/// [`super::collect_ancestors`].
///
/// When the per-hop scan fails to resolve a non-genesis `prev_hash` (the
/// parent event is absent from the index — e.g. a Retention compaction
/// dropped a mid-chain event), the walk records the surviving child and
/// stops with [`AncestryBoundary::MissingParent`] instead of silently
/// returning a short prefix indistinguishable from one that reached genesis.
pub(crate) fn walk_ancestors_outcome_by_hash<State: crate::store::StoreState>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
    // Out-collector for the ids of crypto-shredded ancestors the key-aware walk
    // includes; exists only under `payload-encryption` (the plaintext build never
    // decrypts and so never shreds a payload).
    #[cfg(feature = "payload-encryption")] shredded: &mut Vec<EventId>,
) -> (Vec<StoredEvent<serde_json::Value>>, AncestryBoundary) {
    // All events in a hash chain belong to the same entity, so we load the
    // entity stream once and reuse it across every hop instead of re-querying
    // the DashMap (and re-cloning all IndexEntries) on each step.
    let start = match store.index.get_by_id(event_id) {
        Some(e) => e,
        None => return (Vec::new(), AncestryBoundary::NoAnchor),
    };
    let entity_stream = store.index.stream(start.coord.entity());

    super::collect_ancestors(store, Some(event_id), limit, |store, current_id| {
        // Encryption-enabled store: route the ancestor's payload decode through
        // the key-aware path so an encrypted payload is decrypted (or marked
        // shredded) rather than fail-closed as ciphertext. When no keyset is
        // configured this branch is skipped and the read is byte-identical to the
        // pre-E3 plaintext path below. The parent-link resolution is shared: it is
        // over the hash chain and unaffected by encryption.
        #[cfg(feature = "payload-encryption")]
        if store.key_store.is_some() {
            return super::step_ancestor_key_aware(store, current_id, &entity_stream, shredded);
        }
        let Some((entry, stored)) = super::read_entry_and_event(store, current_id) else {
            return Err(EventId::from(current_id));
        };
        Ok((stored, super::resolve_next_link(&entry, &entity_stream)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::store::{Store, StoreConfig, SyncConfig};
    use tempfile::TempDir;

    fn test_store() -> (Store, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 4096,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        (store, dir)
    }

    fn seeded_chain(store: &Store, entity: &str) -> Vec<crate::id::EventId> {
        let coord = Coordinate::new(entity, "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        (0..4)
            .map(|step| {
                store
                    .append(&coord, kind, &serde_json::json!({ "step": step }))
                    .expect("append")
                    .event_id
            })
            .collect()
    }

    /// Feature-agnostic wrapper: the key-aware `shredded` out-collector exists
    /// only under `payload-encryption`. These tests use plaintext stores (no
    /// keyset), so the collector is always empty and is discarded here, keeping
    /// the assertions identical across both builds.
    fn walk(
        store: &Store,
        event_id: u128,
        limit: usize,
    ) -> (Vec<StoredEvent<serde_json::Value>>, AncestryBoundary) {
        #[cfg(feature = "payload-encryption")]
        {
            let mut shredded = Vec::new();
            walk_ancestors_outcome_by_hash(store, event_id, limit, &mut shredded)
        }
        #[cfg(not(feature = "payload-encryption"))]
        {
            walk_ancestors_outcome_by_hash(store, event_id, limit)
        }
    }

    #[test]
    fn hash_helper_returns_exact_chain_in_reverse_order() {
        use crate::id::EntityIdType;
        let (store, _dir) = test_store();
        let ids = seeded_chain(&store, "entity:hash-helper");

        let (events, boundary) = walk(&store, ids.last().expect("last").as_u128(), 8);
        let actual: Vec<_> = events
            .into_iter()
            .map(|stored| stored.event.event_id())
            .collect();
        let expected: Vec<_> = ids.iter().rev().copied().collect();

        assert_eq!(
            actual,
            expected,
            "PROPERTY: hash-based ancestor traversal must return the exact chain in reverse append order."
        );
        assert_eq!(
            boundary,
            AncestryBoundary::ReachedGenesis,
            "PROPERTY: a fully intact chain must report ReachedGenesis, not a silent stop."
        );
    }

    #[test]
    fn hash_helper_honors_zero_limit_and_unknown_anchor() {
        use crate::id::EntityIdType;
        let (store, _dir) = test_store();
        let ids = seeded_chain(&store, "entity:hash-zero");

        let (zero_limit, zero_boundary) = walk(&store, ids.last().expect("last").as_u128(), 0);
        assert!(
            zero_limit.is_empty(),
            "PROPERTY: hash-based ancestor traversal with limit=0 must return an empty vector."
        );
        assert_eq!(
            zero_boundary,
            AncestryBoundary::LimitReached,
            "PROPERTY: a limit=0 walk is bounded by the limit, not a completed chain."
        );

        let (unknown, unknown_boundary) = walk(&store, 0xDEAD_BEEF, 4);
        assert!(
            unknown.is_empty(),
            "PROPERTY: hash-based ancestor traversal must return empty for an unknown anchor event."
        );
        assert_eq!(
            unknown_boundary,
            AncestryBoundary::NoAnchor,
            "PROPERTY: an unknown anchor must report NoAnchor, not a completed or truncated walk."
        );
    }
}
