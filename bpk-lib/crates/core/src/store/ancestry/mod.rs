use crate::event::StoredEvent;
use crate::id::{EntityIdType, EventId};
use crate::store::index::IndexEntry;
use crate::store::Store;
use std::collections::HashSet;

#[cfg(feature = "payload-encryption")]
use crate::event::Event;

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
/// Under opt-in `payload-encryption`, an encrypted ancestor is transparently
/// decrypted under the store's keyset so the walk returns its plaintext payload.
/// An ancestor whose payload key has been crypto-shredded still EXISTS in the
/// chain — its hash links are intact — so the walk does NOT truncate at it as a
/// false [`AncestryBoundary::MissingParent`]; it is INCLUDED (keeping the lineage
/// structure complete) with a placeholder `Value::Null` payload and flagged in
/// `shredded` (see `is_shredded` / `shredded_ancestors`). Only the erased payload
/// is marked; the walk continues through it to genesis.
///
/// [`Store::walk_ancestors`]: crate::store::Store::walk_ancestors
#[derive(Clone, Debug)]
pub struct AncestorWalk {
    /// Ancestors in reverse append order (newest first), bounded by `limit`.
    ///
    /// Under `payload-encryption`, a crypto-shredded ancestor appears here with a
    /// placeholder `serde_json::Value::Null` payload; the authoritative signal
    /// that a payload was erased is `shredded` / `is_shredded`, NEVER a `Null`
    /// payload on its own (a live event may legitimately carry `Null`).
    pub ancestors: Vec<StoredEvent<serde_json::Value>>,
    /// The boundary at which the walk stopped.
    pub boundary: AncestryBoundary,
    /// Event ids of the ancestors in `ancestors` whose payload key was
    /// crypto-shredded (opt-in `payload-encryption`).
    ///
    /// Each id here names an ancestor that is PRESENT in the chain (its hash
    /// links resolved and the walk continued through it) but whose plaintext is
    /// permanently unrecoverable — its `ancestors` entry carries the placeholder
    /// `Value::Null` payload. Empty when no keyset is configured or no ancestor
    /// was shredded, so an intact encrypted chain reports the SAME empty set as a
    /// plaintext one.
    #[cfg(feature = "payload-encryption")]
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "payload-encryption"))
    )]
    pub shredded: Vec<EventId>,
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

    /// Whether the ancestor with `event_id` is present in the walk but had its
    /// payload key crypto-shredded — its `ancestors` entry carries the
    /// placeholder `Value::Null` payload and its lineage links are intact
    /// (opt-in `payload-encryption`).
    #[cfg(feature = "payload-encryption")]
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "payload-encryption"))
    )]
    #[must_use]
    pub fn is_shredded(&self, event_id: EventId) -> bool {
        self.shredded.contains(&event_id)
    }

    /// The event ids of the walk's crypto-shredded ancestors (opt-in
    /// `payload-encryption`); empty when none were shredded.
    #[cfg(feature = "payload-encryption")]
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "payload-encryption"))
    )]
    #[must_use]
    pub fn shredded_ancestors(&self) -> &[EventId] {
        &self.shredded
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

/// Resolve how the walk continues from an event, given its index entry and the
/// entity stream. The parent edge is driven by the hash chain (`prev_hash` →
/// `event_hash`), so it is IDENTICAL for plaintext and encrypted events —
/// encryption changes only the payload decode, never this linkage. Genesis is a
/// zero `prev_hash`; an unresolved `prev_hash` (parent absent from the index —
/// e.g. a retention-dropped mid-chain event) truncates as `MissingParent`.
pub(super) fn resolve_next_link(
    entry: &IndexEntry,
    entity_stream: &[IndexEntry],
) -> NextLink<u128> {
    let prev = entry.hash_chain.prev_hash;
    if prev == [0_u8; 32] {
        NextLink::Genesis
    } else {
        match parent_event_id_by_hash(entity_stream, prev) {
            Some(parent_id) => NextLink::Continue(parent_id),
            None => NextLink::MissingParent,
        }
    }
}

/// Key-aware ancestry read for ONE event under opt-in `payload-encryption`
/// (reached only when a keyset is configured).
///
/// Reads the RAW frame (never routing ciphertext through the Value-decode seam),
/// resolves the parent link from the index entry (the hash-chain linkage is
/// UNAFFECTED by encryption), and then materializes the payload:
///
/// * a plaintext / system-carve-out event decodes exactly as the plaintext read
///   would (byte-identical), and
/// * an encrypted event decrypts through the shared Stage C primitive
///   ([`Store::open_encrypted_payload_bytes`]).
///
/// A crypto-shredded ancestor still EXISTS in the chain, so it is INCLUDED — its
/// `event_id` is pushed to `shredded` and it is surfaced with a placeholder
/// `Value::Null` payload — and the walk CONTINUES to its parent via `next`. It is
/// NEVER reported as a false `MissingParent`. A present-but-unauthenticated
/// ciphertext (tamper) or a corrupt read is a genuine read failure that truncates
/// the walk (`Err(event_id)` → `ReadFailure`), exactly like a corrupt plaintext
/// read — never a shred.
///
/// [`Store::open_encrypted_payload_bytes`]: crate::store::Store::open_encrypted_payload_bytes
#[cfg(feature = "payload-encryption")]
pub(super) fn step_ancestor_key_aware<State: crate::store::StoreState>(
    store: &Store<State>,
    current_id: u128,
    entity_stream: &[IndexEntry],
    shredded: &mut Vec<EventId>,
) -> StepOutcome<u128> {
    use crate::store::read_api::PayloadPlaintext;

    let Some(entry) = store.index.get_by_id(current_id) else {
        // Only index-proven ids reach the walk; a vanished entry truncates.
        return Err(EventId::from(current_id));
    };
    let raw = match store.reader.read_entry_raw(&entry.disk_pos) {
        Ok(raw) => raw,
        Err(error) => {
            tracing::error!(
                event_id = %format!("{current_id:#034x}"),
                %error,
                "ancestry walk failed to read an index-proven event — store corruption; returning truncated prefix"
            );
            return Err(EventId::from(current_id));
        }
    };
    let next = resolve_next_link(&entry, entity_stream);
    let StoredEvent { coordinate, event } = raw;
    let Event {
        header,
        payload: payload_bytes,
        hash_chain,
    } = event;

    // Plaintext / system-carve-out event (no encryption metadata): decode the
    // stored bytes to a Value exactly as the plaintext read would, so a mixed
    // chain folds identically.
    let Some(meta) = header.payload_encryption.clone() else {
        return finish_value(coordinate, header, hash_chain, &payload_bytes, next);
    };

    let event_id = header.event_id;
    match store.open_encrypted_payload_bytes(
        &coordinate,
        header.event_kind,
        event_id,
        &meta,
        &payload_bytes,
    ) {
        Ok(PayloadPlaintext::Plaintext(plaintext)) => {
            finish_value(coordinate, header, hash_chain, &plaintext, next)
        }
        Ok(PayloadPlaintext::Shredded) => {
            shredded.push(event_id);
            tracing::debug!(
                target: "batpak::ancestry",
                event_id = %format!("{:#034x}", event_id.as_u128()),
                "ancestry walk reached a crypto-shredded ancestor; including it (Null placeholder) \
                 and continuing to its parent — the chain links are intact, only the payload is gone"
            );
            let stored = StoredEvent {
                coordinate,
                event: Event {
                    header,
                    payload: serde_json::Value::Null,
                    hash_chain,
                },
            };
            Ok((stored, next))
        }
        Err(error) => {
            tracing::error!(
                event_id = %format!("{:#034x}", event_id.as_u128()),
                %error,
                "ancestry walk failed to decrypt an index-proven encrypted event; returning truncated prefix"
            );
            Err(event_id)
        }
    }
}

/// Decode recovered plaintext MessagePack `bytes` into an ancestor's
/// `StoredEvent<Value>`, or truncate the walk as a read failure when the
/// (already-plaintext) bytes do not decode. Shared by the plaintext-carve-out and
/// decrypted branches of [`step_ancestor_key_aware`].
#[cfg(feature = "payload-encryption")]
fn finish_value(
    coordinate: crate::coordinate::Coordinate,
    header: crate::event::EventHeader,
    hash_chain: Option<crate::event::HashChain>,
    bytes: &[u8],
    next: NextLink<u128>,
) -> StepOutcome<u128> {
    let event_id = header.event_id;
    match crate::encoding::from_bytes::<serde_json::Value>(bytes) {
        Ok(value) => Ok((
            StoredEvent {
                coordinate,
                event: Event {
                    header,
                    payload: value,
                    hash_chain,
                },
            },
            next,
        )),
        Err(error) => {
            tracing::error!(
                event_id = %format!("{:#034x}", event_id.as_u128()),
                %error,
                "ancestry walk failed to decode a plaintext payload; returning truncated prefix"
            );
            Err(event_id)
        }
    }
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

#[cfg(feature = "payload-encryption")]
pub(crate) fn walk_ancestors_outcome<State: crate::store::StoreState>(
    store: &Store<State>,
    event_id: u128,
    limit: usize,
) -> AncestorWalk {
    // The key-aware walk collects the ids of any crypto-shredded ancestors it
    // included (with a Null placeholder) so callers can distinguish an erased
    // payload from a live one. An intact/plaintext chain leaves this empty.
    let mut shredded: Vec<EventId> = Vec::new();
    let (ancestors, boundary) =
        by_hash::walk_ancestors_outcome_by_hash(store, event_id, limit, &mut shredded);
    AncestorWalk {
        ancestors,
        boundary,
        shredded,
    }
}

#[cfg(not(feature = "payload-encryption"))]
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
