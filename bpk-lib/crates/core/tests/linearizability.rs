//! GAUNTLET linearizability / single-writer linearization gate (GAUNT-LIN-1).
//!
//! Invariant: INV-LINEARIZABILITY-SINGLE-WRITER.
//!
//! batpak commits through a single writer behind a gated visibility watermark
//! (see `crates/core/src/store/index/visibility.rs`): `allocated` advances when
//! sequences are reserved (writer-only) and `visible` is the exclusive upper
//! bound readers filter against, with `visible <= allocated` and `visible`
//! advancing monotonically. The contract this gate proves, against a REAL
//! `Store` fed a seeded operation stream under a fixed injected clock:
//!
//!   - LINEARIZATION ORDER == `global_sequence` ORDER: the ordered visible
//!     history has strictly increasing `global_sequence` with no gaps among the
//!     visible events (the writer assigns a dense, monotonic sequence).
//!   - PREFIX / NO PREMATURE VISIBILITY: an event is visible only if every
//!     earlier `global_sequence` is also visible — visibility advances a prefix,
//!     never a hole (the `visible` watermark is an exclusive upper bound).
//!   - MONOTONIC READS: re-querying never drops a previously-visible event nor
//!     reorders the observed history (an Elle-style monotonic-reads check).
//!   - NO REAL-TIME/SEQ INVERSION: because `Store::append` blocks until commit,
//!     if append A returns before append B is issued then
//!     `A.global_sequence < B.global_sequence`.
//!   - CONVERGENCE: two independent readers of the same store observe an
//!     identical history.
//!
//! The pure checker `check_linearizable` makes the property testable in
//! isolation; the live-store proptest is the green property; the
//! GateNegativePath fixture `checker_rejects_*` feeds the checker deliberately
//! inverted / gapped / non-prefix histories and asserts they are REJECTED,
//! proving the checker is not vacuous.
//!
//! Determinism: every store runs on a FIXED injected clock so HLC coordinates
//! are reproducible; the only nondeterminism removed is real wall time, which is
//! not under test. After `sync()` the test settles on the visible watermark via
//! `wait_for_visible` before snapshotting, so captures are taken at a SETTLED
//! state rather than racing the watermark.

use batpak::id::EntityIdType;
use batpak::store::index::IndexEntry;
use batpak::store::{Store, StoreConfig};
use batpak_testkit::prelude::*;
use proptest::prelude::*;
use tempfile::TempDir;

#[path = "common/proptest.rs"]
mod proptest_support;

/// Fixed wall-clock (ms) so HLC coordinates are reproducible across runs.
const FIXED_WALL_MS: i64 = 1_700_000_000_000;

// ---------------------------------------------------------------------------
// Pure linearization checker (testable in isolation).
// ---------------------------------------------------------------------------

/// A single event as OBSERVED by a reader: its committed global sequence plus
/// enough identity to detect reordering between two observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedEvent {
    global_sequence: u64,
    event_id: u128,
}

/// A linearization violation. Each variant is a distinct anti-vacuous failure
/// the checker must be able to flag.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Violation {
    /// Two visible events share a `global_sequence` (the writer must assign
    /// unique, dense sequences).
    DuplicateSequence { sequence: u64 },
    /// The ordered history is not strictly increasing in `global_sequence`
    /// (a real-time / sequence inversion among visible events).
    SeqInversion { prev: u64, next: u64 },
    /// A gap in the visible prefix: an event is visible but an earlier
    /// `global_sequence` (within the contiguous run rooted at the first visible
    /// sequence) is missing — premature visibility / non-prefix.
    NonPrefix { expected: u64, found: u64 },
}

/// THE CHECKER. Given a history ORDERED as a reader returned it, verify the
/// single-writer linearization contract:
///   1. strictly increasing `global_sequence` (no inversion, no duplicate);
///   2. dense prefix — sequences are contiguous from the first observed one (no
///      hole below the visible high-water).
///
/// An empty history is trivially linearizable. The first visible sequence is
/// the root of the prefix; everything after must be exactly `+1`.
fn check_linearizable(history: &[ObservedEvent]) -> Result<(), Violation> {
    let mut iter = history.iter();
    let Some(first) = iter.next() else {
        return Ok(());
    };
    let mut prev = first.global_sequence;
    for event in iter {
        let seq = event.global_sequence;
        if seq == prev {
            return Err(Violation::DuplicateSequence { sequence: seq });
        }
        if seq < prev {
            return Err(Violation::SeqInversion { prev, next: seq });
        }
        if seq != prev + 1 {
            return Err(Violation::NonPrefix {
                expected: prev + 1,
                found: seq,
            });
        }
        prev = seq;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Live-store harness.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct AppendSpec {
    entity_idx: u8,
    scope_idx: u8,
    type_id: u16,
    payload: i16,
}

fn arb_append_specs() -> impl Strategy<Value = Vec<AppendSpec>> {
    prop::collection::vec(
        (0u8..4, 0u8..3, 1u16..8, any::<i16>()).prop_map(
            |(entity_idx, scope_idx, type_id, payload)| AppendSpec {
                entity_idx,
                scope_idx,
                type_id,
                payload,
            },
        ),
        1..24,
    )
}

fn entity_name(idx: u8) -> String {
    format!("entity:{idx:02}")
}

fn scope_name(idx: u8) -> String {
    format!("scope:{idx:02}")
}

fn lin_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_sync_every_n_events(8)
        .with_clock_fn(|| FIXED_WALL_MS)
}

/// True for a user-issued append (not a substrate SYSTEM_OPEN/CLOSE lifecycle
/// event). Lifecycle events are real, visible, sequenced events that satisfy
/// the prefix/density contract; we only drop them when comparing the visible
/// set against the user-issued append receipts.
fn is_user_event(entry: &IndexEntry) -> bool {
    entry.event_kind() != EventKind::SYSTEM_CLOSE_COMPLETED
        && entry.event_kind() != EventKind::SYSTEM_OPEN_COMPLETED
}

/// Snapshot the ordered visible history exactly as a reader returns it (no
/// re-sort: the inversion check below depends on the reader's native order).
fn observe(store: &Store) -> Vec<ObservedEvent> {
    store
        .query(&Region::all())
        .into_iter()
        .map(|entry| ObservedEvent {
            global_sequence: entry.global_sequence(),
            event_id: entry.event_id().as_u128(),
        })
        .collect()
}

/// Append every spec, then settle: `sync()` is DURABILITY; visibility advances
/// on a separate watermark, so wait until the visible frontier reaches the
/// written frontier before snapshotting. Returns the append receipts' sequences
/// in issue order (each `append` blocks to commit, so this is real-time order).
fn populate(store: &Store, specs: &[AppendSpec]) -> Result<Vec<u64>, StoreError> {
    let mut issued_sequences = Vec::with_capacity(specs.len());
    for spec in specs {
        let coord = Coordinate::new(entity_name(spec.entity_idx), scope_name(spec.scope_idx))
            .expect("generated coordinates must be valid");
        let receipt = store.append(
            &coord,
            EventKind::custom(0x1, spec.type_id),
            &serde_json::json!({ "payload": spec.payload }),
        )?;
        // Real-time order: this append RETURNED (committed) before the next is
        // issued, so its sequence must be below every later one.
        issued_sequences.push(receipt.global_sequence);
    }
    store.sync()?;
    let written = store.frontier().written_hlc;
    store.wait_for_visible(written, std::time::Duration::from_secs(10))?;
    Ok(issued_sequences)
}

// ---------------------------------------------------------------------------
// GREEN property: live Store against the checker + Elle-style observations.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_support::cfg(24))]

    /// Single-writer => the ordered visible history is a dense, strictly
    /// increasing `global_sequence` prefix (the checker accepts it); reads are
    /// monotonic across re-query; two independent readers converge; and no real
    /// append-receipt sequence inverts against issue order.
    #[test]
    fn live_store_history_is_linearizable(specs in arb_append_specs()) {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(lin_config(&dir)).expect("open store");
        let issued_sequences = populate(&store, &specs).expect("populate store");

        // (1) The full ordered visible history satisfies the linearization
        // contract: strictly increasing, dense prefix, no premature hole.
        let history = observe(&store);
        prop_assert!(
            check_linearizable(&history).is_ok(),
            "PROPERTY (linearizability): single-writer visible history must be a \
             dense strictly-increasing global_sequence prefix, but the checker \
             rejected it: {:?}",
            check_linearizable(&history)
        );

        // (2) MONOTONIC READS: a re-query drops nothing previously visible and
        // does not reorder the observed history.
        let reread = observe(&store);
        prop_assert_eq!(
            &reread, &history,
            "PROPERTY (monotonic reads): a re-query must not drop or reorder any \
             previously-visible event"
        );

        // (3) CONVERGENCE: a second independent reader of the SAME store sees an
        // identical history (single source of visible truth).
        let reader_b = observe(&store);
        prop_assert_eq!(
            &reader_b, &history,
            "PROPERTY (convergence): two independent readers of the same store must \
             observe identical history"
        );

        // (4) NO REAL-TIME/SEQ INVERSION: append receipts returned in issue order
        // carry strictly increasing sequences (A committed before B was issued =>
        // A.sequence < B.sequence).
        for window in issued_sequences.windows(2) {
            prop_assert!(
                window[0] < window[1],
                "PROPERTY (no real-time inversion): append A returned before B was \
                 issued, so A.sequence ({}) must be < B.sequence ({})",
                window[0],
                window[1]
            );
        }

        // (5) Every user-issued committed sequence is present in the visible
        // history (no committed-but-invisible append after settling).
        let visible_sequences: std::collections::BTreeSet<u64> = store
            .query(&Region::all())
            .into_iter()
            .filter(is_user_event)
            .map(|entry| entry.global_sequence())
            .collect();
        for seq in &issued_sequences {
            prop_assert!(
                visible_sequences.contains(seq),
                "PROPERTY (no premature/lost visibility): committed sequence {seq} \
                 must be visible after settling on the watermark"
            );
        }

        store.close().expect("close store");
    }
}

// ---------------------------------------------------------------------------
// RED fixture (GateNegativePath): the checker must REJECT real violations.
// ---------------------------------------------------------------------------

/// Anti-vacuity: feed the pure checker deliberately INVERTED, GAP-containing,
/// and DUPLICATE histories and assert each is REJECTED (`.is_err()`). A checker
/// that accepted these would be a tautology and could not qualify the blocking
/// gate. The matching valid prefix is accepted, proving the checker is not
/// always-`Err`.
#[test]
fn checker_rejects_inverted_gapped_and_duplicate_histories() {
    let ev = |global_sequence: u64, event_id: u128| ObservedEvent {
        global_sequence,
        event_id,
    };

    // A valid dense prefix is ACCEPTED (the checker is not always-`Err`).
    let valid = vec![ev(0, 100), ev(1, 101), ev(2, 102)];
    assert!(
        check_linearizable(&valid).is_ok(),
        "a dense strictly-increasing prefix must be accepted"
    );

    // INVERSION: a later event has a SMALLER sequence than its predecessor
    // (the reader returned a reordered history — a real-time/seq inversion).
    let inverted = vec![ev(0, 100), ev(1, 101), ev(0, 102)];
    assert_eq!(
        check_linearizable(&inverted),
        Err(Violation::SeqInversion { prev: 1, next: 0 }),
        "a real-time/seq inversion must be REJECTED"
    );

    // GAP / NON-PREFIX / PREMATURE VISIBILITY: sequence 1 is missing, so 2 is
    // visible while its predecessor is not — a hole in the prefix.
    let gapped = vec![ev(0, 100), ev(2, 102)];
    assert_eq!(
        check_linearizable(&gapped),
        Err(Violation::NonPrefix {
            expected: 1,
            found: 2,
        }),
        "a gap below the visible high-water (non-prefix) must be REJECTED"
    );

    // DUPLICATE: two visible events share a global_sequence.
    let duplicate = vec![ev(0, 100), ev(1, 101), ev(1, 102)];
    assert_eq!(
        check_linearizable(&duplicate),
        Err(Violation::DuplicateSequence { sequence: 1 }),
        "a duplicate global_sequence must be REJECTED"
    );
}
