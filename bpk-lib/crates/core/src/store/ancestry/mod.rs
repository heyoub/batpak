use crate::event::StoredEvent;
use crate::store::IndexEntry;
use crate::store::Store;
use std::collections::HashSet;

#[cfg(not(feature = "blake3"))]
mod by_clock;
#[cfg(feature = "blake3")]
mod by_hash;

/// Bounded ancestor collection with cycle detection; on cycle it
/// truncates the walk at the cycle point and logs at `error` level,
/// then returns the prefix collected up to that point. This matches
/// the public `Store::walk_ancestors` signature, which returns `Vec<_>`
/// unconditionally.
pub(super) fn collect_ancestors<State, Cursor, Step>(
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
        let next_id = stored.event.header.event_id;
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

pub(super) fn read_entry_and_event<State>(
    store: &Store<State>,
    event_id: u128,
) -> Option<(IndexEntry, StoredEvent<serde_json::Value>)> {
    let entry = store.index.get_by_id(event_id)?;
    let stored = store.reader.read_entry(&entry.disk_pos).ok()?;
    Some((entry, stored))
}

#[cfg(feature = "blake3")]
pub(crate) fn parent_event_id_by_hash(
    entity_stream: &[IndexEntry],
    parent_hash: [u8; 32],
) -> Option<u128> {
    entity_stream
        .iter()
        .find(|candidate| candidate.hash_chain.event_hash == parent_hash)
        .map(|candidate| candidate.event_id)
}

#[cfg(not(feature = "blake3"))]
pub(super) fn clock_cursor<State>(
    store: &Store<State>,
    event_id: u128,
) -> Option<std::vec::IntoIter<u128>> {
    let start_entry = store.index.get_by_id(event_id)?;
    let ids = store
        .index
        .stream(start_entry.coord.entity())
        .iter()
        .rev()
        .filter(|entry| entry.clock <= start_entry.clock)
        .map(|entry| entry.event_id)
        .collect::<Vec<_>>();
    Some(ids.into_iter())
}

pub(crate) fn walk_ancestors<State>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Vec<StoredEvent<serde_json::Value>> {
    #[cfg(feature = "blake3")]
    {
        by_hash::walk_ancestors_by_hash(store, event_id, limit)
    }

    #[cfg(not(feature = "blake3"))]
    {
        by_clock::walk_ancestors_by_clock(store, event_id, limit)
    }
}
