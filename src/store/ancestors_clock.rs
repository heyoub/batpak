use crate::event::StoredEvent;
use crate::store::Store;

pub(crate) fn walk_ancestors_by_clock(
    store: &Store,
    event_id: u128,
    limit: usize,
) -> Vec<StoredEvent<serde_json::Value>> {
    let mut results = Vec::new();
    let Some(start_entry) = store.index.get_by_id(event_id) else {
        return results;
    };
    let stream = store.index.stream(start_entry.coord.entity());
    for entry in stream.iter().rev() {
        if results.len() >= limit {
            break;
        }
        if entry.clock > start_entry.clock {
            continue;
        }
        if let Ok(stored) = store.reader.read_entry(&entry.disk_pos) {
            results.push(stored);
        }
    }
    results
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
