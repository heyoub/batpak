use crate::event::StoredEvent;
use crate::store::index::IndexEntry;
use crate::store::Store;
use std::collections::HashSet;

mod by_hash;

/// Bounded ancestor collection with cycle detection; on cycle it
/// truncates the walk at the cycle point and logs at `error` level,
/// then returns the prefix collected up to that point. The walk also
/// truncates-and-logs at `error` level when a read of an index-proven
/// event fails (CRC/IO corruption on a known-present event), rather than
/// only on cycle. This matches the public `Store::walk_ancestors`
/// signature, which returns `Vec<_>` unconditionally.
pub(super) fn collect_ancestors<State: crate::store::StoreState, Cursor, Step>(
    store: &Store<State>,
    mut cursor: Option<Cursor>,
    limit: usize,
    mut step: Step,
) -> Vec<StoredEvent<serde_json::Value>>
where
    Step: FnMut(&Store<State>, Cursor) -> Option<(StoredEvent<serde_json::Value>, Option<Cursor>)>,
{
    let mut results = Vec::new();
    let mut visited: HashSet<u128> = HashSet::new();
    while results.len() < limit {
        let Some(current) = cursor.take() else {
            break;
        };
        let Some((stored, next)) = step(store, current) else {
            break;
        };
        let next_id = {
            use crate::id::EntityIdType;
            stored.event.header.event_id.as_u128()
        };
        if !visited.insert(next_id) {
            tracing::error!(
                cycle_at = %format!("{next_id:#034x}"),
                "ancestry walk hit a cycle — store corruption; returning prefix"
            );
            break;
        }
        results.push(stored);
        cursor = next;
    }
    results
}

pub(super) fn read_entry_and_event<State: crate::store::StoreState>(
    store: &Store<State>,
    event_id: u128,
) -> Option<(IndexEntry, StoredEvent<serde_json::Value>)> {
    let entry = store.index.get_by_id(event_id)?;
    let stored = match store.reader.read_entry(&entry.disk_pos) {
        Ok(stored) => stored,
        Err(error) => {
            tracing::error!(
                event_id = %format!("{event_id:#034x}"),
                %error,
                "ancestry walk failed to read an index-proven event — store corruption; returning truncated prefix"
            );
            return None;
        }
    };
    Some((entry, stored))
}

pub(crate) fn parent_event_id_by_hash(
    entity_stream: &[IndexEntry],
    parent_hash: [u8; 32],
) -> Option<u128> {
    entity_stream
        .iter()
        .find(|candidate| candidate.hash_chain.event_hash == parent_hash)
        .map(|candidate| candidate.event_id)
}

pub(crate) fn walk_ancestors<State: crate::store::StoreState>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Vec<StoredEvent<serde_json::Value>> {
    by_hash::walk_ancestors_by_hash(store, event_id, limit)
}
