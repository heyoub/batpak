use crate::event::StoredEvent;
use crate::id::{EntityIdType, EventId};
use crate::store::index::IndexEntry;
use crate::store::Store;
use std::collections::HashSet;

mod by_hash;

/// Why a bounded ancestry walk stopped.
///
/// A bare `Vec` of ancestors cannot tell a caller whether the chain it returns
/// is the *complete* lineage back to genesis or merely a *truncated* prefix
/// that gave up at a dangling link (for example, a Retention compaction that
/// dropped a mid-chain event, leaving a surviving descendant whose `prev_hash`
/// references a now-absent event). This enum makes that boundary observable so
/// `ReachedGenesis` (coherent, complete) is never confused with
/// `MissingParent` (truncated, lossy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AncestryBoundary {
    /// The walk reached a genesis event (`prev_hash` all-zero). The returned
    /// ancestors are the COMPLETE chain back to genesis, within `limit`.
    ReachedGenesis,
    /// The walk stopped because it reached the requested `limit` while a parent
    /// edge was still pending. The chain may extend further than what was
    /// returned; this is a bounded read, not a coherence failure.
    LimitReached,
    /// The walk stopped at a surviving child whose recorded `prev_hash`
    /// resolves to no event in the index — a dangling link. The chain is
    /// TRUNCATED, not complete. This is the retention-drop / mid-chain-loss
    /// boundary: the returned prefix ends at `child`, whose parent is absent.
    MissingParent {
        /// The surviving child event whose recorded parent edge dangles.
        child: EventId,
    },
    /// The walk stopped because a read of an index-proven event failed
    /// (CRC/IO corruption on a known-present event). The chain is TRUNCATED.
    ReadFailure {
        /// The event whose on-disk read failed.
        event_id: EventId,
    },
    /// The walk stopped because it revisited an event id (store corruption).
    /// The chain is TRUNCATED at the cycle point.
    Cycle {
        /// The revisited event id at which the cycle was detected.
        event_id: EventId,
    },
    /// The anchor event id was not present in the index, or `limit == 0`, so no
    /// ancestor walk was performed and no boundary was traversed.
    NoAnchor,
}

/// Outcome of a bounded ancestry walk: the collected ancestors plus the
/// [`AncestryBoundary`] at which the walk stopped.
///
/// Callers that only need the events can use [`Store::walk_ancestors`], which
/// returns just the `Vec`. Callers that must distinguish a complete chain from
/// a truncated one read [`AncestorWalk::boundary`] (or the
/// [`AncestorWalk::reached_genesis`] / [`AncestorWalk::truncated_at`]
/// conveniences).
///
/// [`Store::walk_ancestors`]: crate::store::Store::walk_ancestors
#[derive(Clone, Debug)]
pub struct AncestorWalk {
    /// Ancestors in reverse append order (newest first), bounded by `limit`.
    pub ancestors: Vec<StoredEvent<serde_json::Value>>,
    /// The boundary at which the walk stopped.
    pub boundary: AncestryBoundary,
}

impl AncestorWalk {
    /// Whether the walk reached a genesis event — i.e. `ancestors` is the
    /// complete lineage back to the chain root (within `limit`), not a prefix
    /// truncated at a missing/corrupt/cyclic link.
    #[must_use]
    pub fn reached_genesis(&self) -> bool {
        matches!(self.boundary, AncestryBoundary::ReachedGenesis)
    }

    /// The surviving child event at which the walk truncated because its
    /// recorded parent link could not be resolved (the parent event is absent,
    /// e.g. retention-dropped mid-chain). `None` when the walk reached genesis,
    /// hit the `limit`, found no anchor, or stopped on a read failure / cycle —
    /// those boundaries are reported via [`AncestorWalk::boundary`].
    #[must_use]
    pub fn truncated_at(&self) -> Option<EventId> {
        match self.boundary {
            AncestryBoundary::MissingParent { child } => Some(child),
            AncestryBoundary::ReachedGenesis
            | AncestryBoundary::LimitReached
            | AncestryBoundary::ReadFailure { .. }
            | AncestryBoundary::Cycle { .. }
            | AncestryBoundary::NoAnchor => None,
        }
    }
}

/// How a single recorded step continues the walk.
pub(super) enum NextLink<Cursor> {
    /// Follow this cursor to the parent event.
    Continue(Cursor),
    /// This event is genesis (`prev_hash` all-zero); the chain is complete.
    Genesis,
    /// This event's recorded parent edge dangles (the parent is absent from
    /// the index); the chain truncates here.
    MissingParent,
}

/// Outcome of asking the per-hop closure to advance the walk by one event:
/// either the event was read (`Ok((stored, next))`, where `next` says how the
/// walk continues) or its on-disk read failed (`Err(event_id)`, truncating
/// before the event is recorded). Modelled as a `Result` so the small
/// read-failure case never inflates a large enum with the recorded event.
pub(super) type StepOutcome<Cursor> =
    Result<(StoredEvent<serde_json::Value>, NextLink<Cursor>), EventId>;

/// Bounded ancestor collection with cycle detection that also reports WHERE the
/// walk stopped.
///
/// The walk records each event the per-hop `step` closure reads, up to `limit`
/// events, and returns the collected prefix paired with an [`AncestryBoundary`]
/// describing the stop reason. Genesis (`ReachedGenesis`) is distinguished from
/// a dangling/dropped parent (`MissingParent`), a corrupt read (`ReadFailure`),
/// a cycle (`Cycle`), and the `limit` (`LimitReached`), so a truncated prefix
/// is never silently mistaken for a complete chain. Cycles are additionally
/// logged at `error` level because they indicate store corruption.
pub(super) fn collect_ancestors<State: crate::store::StoreState, Cursor, Step>(
    store: &Store<State>,
    mut cursor: Option<Cursor>,
    limit: usize,
    mut step: Step,
) -> (Vec<StoredEvent<serde_json::Value>>, AncestryBoundary)
where
    Step: FnMut(&Store<State>, Cursor) -> StepOutcome<Cursor>,
{
    let mut results = Vec::new();
    let mut visited: HashSet<u128> = HashSet::new();
    let boundary = loop {
        if results.len() >= limit {
            break AncestryBoundary::LimitReached;
        }
        let Some(current) = cursor.take() else {
            break AncestryBoundary::NoAnchor;
        };
        let (stored, next) = match step(store, current) {
            Ok(recorded) => recorded,
            Err(event_id) => break AncestryBoundary::ReadFailure { event_id },
        };
        let id = stored.event.event_id();
        if !visited.insert(id.as_u128()) {
            tracing::error!(
                cycle_at = %format!("{:#034x}", id.as_u128()),
                "ancestry walk hit a cycle — store corruption; returning prefix"
            );
            break AncestryBoundary::Cycle { event_id: id };
        }
        results.push(stored);
        match next {
            NextLink::Continue(parent) => cursor = Some(parent),
            NextLink::Genesis => break AncestryBoundary::ReachedGenesis,
            NextLink::MissingParent => break AncestryBoundary::MissingParent { child: id },
        }
    };
    (results, boundary)
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

pub(crate) fn walk_ancestors_outcome<State: crate::store::StoreState>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> AncestorWalk {
    let (ancestors, boundary) = by_hash::walk_ancestors_outcome_by_hash(store, event_id, limit);
    AncestorWalk {
        ancestors,
        boundary,
    }
}
