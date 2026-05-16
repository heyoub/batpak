use crate::event::StoredEvent;
use crate::store::Store;

/// Walk clock-ordered ancestors of `event_id`, up to `limit` entries.
///
/// Used when the `blake3` hash-chain feature is not compiled in: we
/// iterate over the entity's stream in reverse clock order starting at
/// the anchor's clock value. The per-hop step is O(1) amortised (the
/// IDs were precomputed in `clock_cursor`) so the whole walk is
/// O(limit) plus the per-hop disk read. Cycle detection still runs at
/// the caller (`ancestry::mod`) via [`super::collect_ancestors`] —
/// clock walks should never cycle (strictly decreasing clocks), but
/// the caller's `HashSet` is the defensive oracle.
pub(crate) fn walk_ancestors_by_clock<State>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Vec<StoredEvent<serde_json::Value>> {
    super::collect_ancestors(
        store,
        super::clock_cursor(store, event_id),
        limit,
        |store, mut ids| {
            let event_id = ids.next()?;
            let (_, stored) = super::read_entry_and_event(store, event_id)?;
            Some((stored, Some(ids)))
        },
    )
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
    fn clock_helper_returns_anchor_and_older_events_only() {
        let (store, _dir) = test_store();
        let ids = seeded_chain(&store, "entity:clock-helper");

        let actual: Vec<_> = walk_ancestors_by_clock(&store, ids[2], 8)
            .into_iter()
            .map(|stored| stored.event.event_id())
            .collect();
        let expected: Vec<_> = ids[..=2].iter().rev().copied().collect();

        assert_eq!(
            actual,
            expected,
            "PROPERTY: clock-based fallback traversal must return the anchor and only older events from the same stream."
        );
    }

    #[test]
    fn clock_helper_honors_limits_and_unknown_anchor() {
        let (store, _dir) = test_store();
        let ids = seeded_chain(&store, "entity:clock-limit");

        assert_eq!(
            walk_ancestors_by_clock(&store, *ids.last().expect("last"), 2).len(),
            2,
            "PROPERTY: clock-based fallback traversal must stop once the requested limit is reached."
        );
        assert!(
            walk_ancestors_by_clock(&store, 0xDEAD_BEEF, 4).is_empty(),
            "PROPERTY: clock-based fallback traversal must return empty for an unknown anchor event."
        );
    }
}
