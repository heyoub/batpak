use crate::event::StoredEvent;
use crate::store::Store;

/// Walk hash-chain ancestors of `event_id`, up to `limit` entries.
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
pub(crate) fn walk_ancestors_by_hash<State>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Vec<StoredEvent<serde_json::Value>> {
    // All events in a hash chain belong to the same entity, so we load the
    // entity stream once and reuse it across every hop instead of re-querying
    // the DashMap (and re-cloning all IndexEntries) on each step.
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
