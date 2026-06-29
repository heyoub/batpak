// The fault injector (and the symbols it needs) live behind `dangerous-test-hooks`.
// This sentinel runs in that lane; without the feature the whole file is empty.
#![cfg(feature = "dangerous-test-hooks")]
//! Gauntlet Phase 0B — SENTINEL S3: crash-after-fsync-on-batch-commit recovery oracle.
//!
//! Harness pattern: Offensive sentinel (recovery_oracle seed; always-on lane that
//! has `dangerous-test-hooks`; ships a RED fixture).
//!
//! Requires `--features dangerous-test-hooks` (the fault injector lives behind it).
//!
//! THE SACRED RULE: a batch the store fsync-confirmed durable must come back
//! `Committed` (or a canonical refusal) on reopen — NEVER a half-ghost: no
//! partial batch, no undead receipt, no lost-after-fsync event. This sentinel
//! drives a short deterministic op log, injects a crash IMMEDIATELY AFTER the
//! batch-commit fsync, drops/reopens the store, and classifies the recovered
//! state against the op-log model as EXACTLY one of {Committed | RolledBack |
//! CanonicalRefusal}. Anything else is a hard fail.
//!
//! NOTE on the injection point: `InjectionPoint::BatchFsync` fires BEFORE the
//! real `sync_with_mode` call (it models "COMMIT written, power lost during/
//! before fsync" — the un-durable case covered by
//! `atomic_batch::batch_fsync_ambiguity_discards_uncommitted`). The genuine
//! POST-fsync crash — the one the sacred rule is about — is
//! `InjectionPoint::BatchPrePublish`, documented as "After successful fsync,
//! before index publish" (`src/store/fault.rs`). The batch fsync HAS completed
//! (durable on disk) but the in-memory index has not been published, exactly
//! modelling a crash in the window after the durability boundary. So S3 injects
//! at `BatchPrePublish`.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::{
    AppendOptions, BatchAppendItem, CausationRef, CountdownAction, CountdownInjector,
    InjectionPoint, Store, StoreConfig, StoreError, SyncMode,
};
use std::sync::Arc;
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xC, 1);

/// The recovered classification of a fsync-confirmed batch.
#[derive(Debug, PartialEq, Eq)]
enum RecoveredState {
    /// Every event in the fsync-confirmed batch is present and visible.
    Committed,
    /// None of the batch is present (legal ONLY if the batch was not yet
    /// fsync-confirmed; illegal for a post-fsync crash).
    RolledBack,
    /// The store refused to open with a typed error (canonical refusal).
    CanonicalRefusal,
}

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        // SyncAll so the fsync is a real durability boundary, not a no-op.
        .with_sync_mode(SyncMode::SyncAll)
        .with_sync_every_n_events(1)
}

fn user_visible_count(store: &Store) -> usize {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .count()
}

fn batch_items(coord: &Coordinate, n: u32) -> Vec<BatchAppendItem> {
    (0..n)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                KIND,
                &serde_json::json!({ "seq": i }),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct batch item")
        })
        .collect()
}

/// Drive a known op log (1 pre-event + a batch of `batch_n`), crash AFTER the
/// batch-commit fsync (`BatchPrePublish`), reopen, and classify the recovered
/// user-visible state against the op-log model.
///
/// Returns `(recovered_state, pre_event_present)`. `pre_event_present` lets the
/// caller confirm the pre-fault committed history was never lost.
fn run_post_fsync_crash_oracle(batch_n: u32) -> (RecoveredState, bool) {
    let dir = TempDir::new().expect("temp dir");
    let coord = Coordinate::new("entity:s3", "scope:recovery").expect("valid coord");

    // Op 1: a normal, plainly-committed pre-event to establish durable history.
    {
        let store = Store::open(config(&dir)).expect("open baseline store");
        let _ = store
            .append(&coord, KIND, &serde_json::json!({ "pre": 1 }))
            .expect("append committed pre-event");
        store.close().expect("close baseline store");
    }

    // Op 2: a batch that crashes AFTER its commit fsync (durable) but BEFORE the
    // in-memory index publish — the genuine post-durability-boundary crash.
    let crash_err = {
        let store = Store::open(
            config(&dir).with_fault_injector(Some(Arc::new(
                CountdownInjector::new(
                    1,
                    CountdownAction::Fail("simulated crash AFTER batch-commit fsync"),
                )
                .with_filter(|p| matches!(p, InjectionPoint::BatchPrePublish { .. })),
            ))),
        )
        .expect("open fault-injected store");
        let err = store
            .append_batch(batch_items(&coord, batch_n))
            .expect_err("post-fsync crash must surface as an error from append_batch");
        // The crash aborts the live process's view of the batch; drop = crash.
        drop(store);
        err
    };
    assert!(
        err_is_fault(&crash_err),
        "post-fsync crash must surface as BatchFailed/FaultInjected, got {crash_err:?}"
    );

    // Op 3: reopen (recovery). Either it opens (and we inspect visibility) or it
    // refuses canonically — both are legal; a panic or an Io/other untyped
    // failure is NOT.
    let reopen = Store::open(config(&dir));
    let store = match reopen {
        Ok(store) => store,
        Err(StoreError::MmapFutureVersion { .. })
        | Err(StoreError::IdempotencyFutureVersion { .. })
        | Err(StoreError::CorruptSegment { .. })
        | Err(StoreError::CorruptFrame { .. })
        | Err(StoreError::CrcMismatch { .. })
        | Err(StoreError::DataDirMalformed { .. }) => {
            return (RecoveredState::CanonicalRefusal, false);
        }
        Err(other) => unreachable!(
            "ILLEGAL RECOVERED STATE: reopen after a post-fsync crash failed with a \
             non-canonical error (not a typed refusal): {other:?}"
        ),
    };

    let pre_present = store.query(&Region::all()).into_iter().any(|entry| {
        entry.event_kind() == KIND && {
            store
                .get(entry.event_id())
                .map(|loaded| loaded.event.payload.get("pre") == Some(&serde_json::json!(1)))
                .unwrap_or(false)
        }
    });

    let visible = user_visible_count(&store);
    // The op-log model: pre-event (always durable) + the batch (fsync-confirmed).
    let expected_committed = 1 + batch_n as usize;

    let state = if visible == expected_committed {
        RecoveredState::Committed
    } else if visible == 1 {
        // Only the pre-event survived; the entire batch rolled back.
        RecoveredState::RolledBack
    } else {
        // Anything else is a HALF-GHOST: a partial batch, an undead receipt, or
        // a lost-after-fsync event. This is the bug the oracle exists to catch.
        unreachable!(
            "HALF-GHOST RECOVERED STATE: after a post-fsync crash the store came back with \
             {visible} visible events; the only legal counts are {expected_committed} \
             (Committed) or 1 (RolledBack). A partial/torn batch is forbidden."
        );
    };

    (state, pre_present)
}

fn err_is_fault(err: &StoreError) -> bool {
    matches!(err, StoreError::BatchFailed { .. }) || matches!(err, StoreError::FaultInjected(_))
}

/// GREEN (every-PR, dangerous-test-hooks lane): a batch crashed AFTER its commit
/// fsync must come back `Committed` (the sacred rule), and the pre-fault history
/// must be intact. The recovered state must be EXACTLY one of the three legal
/// outcomes — the oracle panics on any half-ghost.
#[test]
fn post_fsync_committed_batch_recovers_committed_or_canonical_refusal() {
    let (state, pre_present) = run_post_fsync_crash_oracle(3);

    assert!(
        pre_present,
        "the plainly-committed pre-fault event must never be lost on recovery"
    );

    // RED fixture: under `--cfg gauntlet_red_fixture`, assert the ILLEGAL
    // outcome (the fsync-confirmed batch rolled back). That assertion is FALSE
    // against the real recovery path (which recovers Committed), so the red
    // fixture FAILS — proving the oracle actually detects a lost-after-fsync
    // commit rather than passing vacuously.
    #[cfg(gauntlet_red_fixture)]
    assert_eq!(
        state,
        RecoveredState::RolledBack,
        "RED FIXTURE: asserts the (illegal) lost-after-fsync outcome; MUST fail because a \
         fsync-confirmed batch is required to recover Committed"
    );

    // GREEN: the sacred rule. A fsync-confirmed batch comes back Committed (or,
    // legally, a canonical refusal). Never RolledBack, never a half-ghost.
    #[cfg(not(gauntlet_red_fixture))]
    assert!(
        matches!(
            state,
            RecoveredState::Committed | RecoveredState::CanonicalRefusal
        ),
        "SACRED RULE: a batch confirmed durable by fsync must recover as Committed or a \
         canonical refusal, got {state:?} (RolledBack here would be a lost-after-fsync commit)"
    );
}
