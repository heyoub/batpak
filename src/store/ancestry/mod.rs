use crate::event::StoredEvent;
use crate::store::IndexEntry;
use crate::store::{Store, StoreError};
use std::collections::HashSet;

#[cfg(not(feature = "blake3"))]
mod by_clock;
#[cfg(feature = "blake3")]
mod by_hash;

/// Cycle-aware bounded ancestor collection.
///
/// Advances the cursor-by-step state up to `limit` hops, recording each
/// visited event's `event_id` in a `HashSet` to detect pathological
/// cycles in the ancestry chain. A cycle is structural corruption:
/// events form a DAG by construction (each event's `causation_id` points
/// to an earlier event), so the only way this fires is if the store has
/// been tampered with or a bug mis-wrote a frame. On detection we return
/// [`StoreError::AncestryCorrupt`] so callers can surface it rather than
/// loop forever or produce duplicate entries.
///
/// Callers that need a fire-and-forget variant (`Vec<_>` return) can use
/// [`collect_ancestors`].
// justifies: Result-shaped sibling of collect_ancestors; crate-private seam used by try_walk_ancestors so cycle-surfacing has one source of truth.
#[allow(dead_code)]
pub(super) fn try_collect_ancestors<State, Cursor, Step>(
    store: &Store<State>,
    mut cursor: Option<Cursor>,
    limit: usize,
    mut step: Step,
) -> Result<Vec<StoredEvent<serde_json::Value>>, StoreError>
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
            return Err(StoreError::AncestryCorrupt { cycle_at: next_id });
        }
        results.push(stored);
        cursor = next;
    }
    Ok(results)
}

/// Bounded ancestor collection with cycle detection; on cycle it
/// truncates the walk at the cycle point and logs at `error` level,
/// then returns the prefix collected up to that point. This matches
/// the public `Store::walk_ancestors` signature, which returns `Vec<_>`
/// unconditionally. Downstream code that wants the structured error
/// should call [`try_collect_ancestors`] directly.
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

/// Cycle-surfacing sibling of [`walk_ancestors`]: returns
/// [`StoreError::AncestryCorrupt`] on the first repeat event_id, instead
/// of logging and truncating. Intended for callers that treat an
/// ancestry cycle as a store-integrity failure rather than a traversal
/// hiccup (e.g. repair tooling, integrity checks).
// justifies: crate-private cycle-aware walk delivers D7's Err(StoreError::AncestryCorrupt) contract so the Result-shaped path stays compiled alongside collect_ancestors.
#[allow(dead_code)]
pub(crate) fn try_walk_ancestors<State>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Result<Vec<StoredEvent<serde_json::Value>>, StoreError> {
    #[cfg(feature = "blake3")]
    {
        try_walk_ancestors_by_hash(store, event_id, limit)
    }

    #[cfg(not(feature = "blake3"))]
    {
        try_walk_ancestors_by_clock(store, event_id, limit)
    }
}

#[cfg(feature = "blake3")]
fn try_walk_ancestors_by_hash<State>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Result<Vec<StoredEvent<serde_json::Value>>, StoreError> {
    let start = match store.index.get_by_id(event_id) {
        Some(e) => e,
        None => return Ok(Vec::new()),
    };
    let entity_stream = store.index.stream(start.coord.entity());

    try_collect_ancestors(store, Some(event_id), limit, |store, current_id| {
        let (entry, stored) = read_entry_and_event(store, current_id)?;
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

#[cfg(not(feature = "blake3"))]
fn try_walk_ancestors_by_clock<State>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> Result<Vec<StoredEvent<serde_json::Value>>, StoreError> {
    try_collect_ancestors(
        store,
        clock_cursor(store, event_id),
        limit,
        |store, mut ids| {
            let event_id = ids.next()?;
            let (_, stored) = read_entry_and_event(store, event_id)?;
            Some((stored, Some(ids)))
        },
    )
}
