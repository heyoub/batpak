use crate::event::StoredEvent;
use crate::store::Store;

pub(crate) fn walk_ancestors_by_hash<State>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Vec<StoredEvent<serde_json::Value>> {
    // All events in a hash chain belong to the same entity, so we load the
    // entity stream once and reuse it across every hop instead of re-querying
    // the DashMap (and re-cloning all IndexEntries) on each step.
    //
    // A by_event_hash DashMap was considered to turn the per-hop linear scan
    // into O(1). Benchmarks showed the per-hop cost (~6 µs) is dominated by
    // the segment disk read, not the stream scan. The ~15-30% scan saving does
    // not justify a permanent DashMap entry per event across the whole index.
    let start = match store.index.get_by_id(event_id) {
        Some(e) => e,
        None => return Vec::new(),
    };
    let entity_stream = store.index.stream(start.coord.entity());

    super::collect_ancestors(store, Some(event_id), limit, |store, current_id| {
        let (entry, stored) = super::read_entry_and_event(store, current_id)?;
        let prev = entry.hash_chain.prev_hash;
        let next = if prev == [0_u8; 32] {
            None
        } else {
            entity_stream
                .iter()
                .find(|candidate| candidate.hash_chain.event_hash == prev)
                .map(|candidate| candidate.event_id)
        };
        Some((stored, next))
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

    fn seeded_chain(store: &Store, entity: &str) -> Vec<u128> {
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

    #[test]
    fn hash_helper_returns_exact_chain_in_reverse_order() {
        let (store, _dir) = test_store();
        let ids = seeded_chain(&store, "entity:hash-helper");

        let actual: Vec<_> = walk_ancestors_by_hash(&store, *ids.last().expect("last"), 8)
            .into_iter()
            .map(|stored| stored.event.event_id())
            .collect();
        let expected: Vec<_> = ids.iter().rev().copied().collect();

        assert_eq!(
            actual,
            expected,
            "PROPERTY: hash-based ancestor traversal must return the exact chain in reverse append order."
        );
    }

    #[test]
    fn hash_helper_honors_zero_limit_and_unknown_anchor() {
        let (store, _dir) = test_store();
        let ids = seeded_chain(&store, "entity:hash-zero");

        assert!(
            walk_ancestors_by_hash(&store, *ids.last().expect("last"), 0).is_empty(),
            "PROPERTY: hash-based ancestor traversal with limit=0 must return an empty vector."
        );
        assert!(
            walk_ancestors_by_hash(&store, 0xDEAD_BEEF, 4).is_empty(),
            "PROPERTY: hash-based ancestor traversal must return empty for an unknown anchor event."
        );
    }
}
