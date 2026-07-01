//! W3 proof: the crash-sensitive atomic-rename / persist cluster is now routed
//! through [`StoreFs`], so a [`SimFs`] fault SURFACES on the compaction swap, the
//! visibility-range persist, and the cursor-checkpoint persist — where the same
//! ops, as direct `platform::fs::*` free functions, were unfaultable.
//!
//! Each test pairs a [`RealFs`] CONTROL (the routed call succeeds normally) with
//! a [`SimFs`] fault on the SAME op (the call now fails). The contrast is the
//! evidence: before routing, the free fn took no fs handle, so no backend could
//! intercept it; after routing, the configured backend dispatches it and a
//! seeded fault tears it.

use super::fs::{CrashOp, SimFs};
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::delivery::cursor::{Cursor, CursorCheckpoint};
use crate::store::delivery::observation::CheckpointId;
use crate::store::hidden_ranges::{write_cancelled_ranges, CancelledVisibilityRanges};
use crate::store::platform::fs::{RealFs, StoreFs};
use crate::store::segment::CompactionOutcome;
use crate::store::{CompactionConfig, CompactionStrategy, Open, Store, StoreConfig, StoreError};
use std::sync::Arc;

/// Non-empty cancelled ranges so `write_cancelled_ranges` takes the
/// temp-create + atomic-publish branch (the empty branch only removes the file).
fn sample_ranges() -> CancelledVisibilityRanges {
    CancelledVisibilityRanges {
        global: vec![(1, 5)],
        ..Default::default()
    }
}

#[test]
fn visibility_persist_is_routed_and_faultable() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let ranges = sample_ranges();

    // CONTROL: through the production backend the persist succeeds — proving the
    // routed function is behavior-preserving.
    write_cancelled_ranges(dir.path(), &ranges, &RealFs)
        .expect("RealFs persist of visibility ranges must succeed");

    // FAULT: a SimFs armed on the atomic publish tears the persist. Before W3
    // this op was the free fn `persist_temp_with_parent_sync` (no fs handle), so
    // no backend could intercept it.
    let fresh = tempfile::tempdir().expect("tmpdir");
    let fs = SimFs::new(0xA70_C0DE, 0).with_fault_on(CrashOp::PersistTemp, 1);
    let result = write_cancelled_ranges(fresh.path(), &ranges, &fs);
    assert!(
        matches!(result, Err(StoreError::Io(_))),
        "PROPERTY: a SimFs fault on the routed visibility-range persist must surface as StoreError::Io, got {result:?}"
    );
}

#[test]
fn checkpoint_persist_is_routed_and_faultable() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let id = CheckpointId::new("atomic-fault-checkpoint").expect("valid checkpoint id");
    let ckpt = CursorCheckpoint {
        position: 7,
        started: true,
        process_boot_ns: None,
        region_identity: Some("region:atomic-fault".to_owned()),
    };

    // CONTROL: production backend publishes the checkpoint.
    Cursor::save_checkpoint_with_fs(dir.path(), &id, &ckpt, &RealFs)
        .expect("RealFs checkpoint persist must succeed");

    // FAULT: SimFs tears the atomic publish. Before W3 the durable-cursor path
    // reached the free fn `persist_temp_with_parent_sync`, unfaultable.
    let fresh = tempfile::tempdir().expect("tmpdir");
    let fs = SimFs::new(0xC4EC_4001, 0).with_fault_on(CrashOp::PersistTemp, 1);
    let result = Cursor::save_checkpoint_with_fs(fresh.path(), &id, &ckpt, &fs);
    assert!(
        result.is_err(),
        "PROPERTY: a SimFs fault on the routed cursor-checkpoint persist must surface as an error, got {result:?}"
    );
}

/// Build a real `Store` over `sim_fs` with enough small segments that ≥2 are
/// sealed (so a `min_segments: 2` compaction runs). Mirrors the fork-hostile
/// `build_source` setup.
fn build_store_with_sealed_segments(
    dir: &std::path::Path,
    sim_fs: &Arc<SimFs>,
    events: usize,
) -> Store<Open> {
    let config = StoreConfig::new(dir)
        .with_sync_every_n_events(1)
        .with_segment_max_bytes(512)
        .with_fs(Arc::clone(sim_fs) as Arc<dyn StoreFs>);
    let store = Store::<Open>::open(config).expect("open store over SimFs");
    let kind = EventKind::custom(0xC, 0x3);
    for i in 0..events {
        let coord =
            Coordinate::new(format!("entity-{i}"), "scope:atomic-fault").expect("coordinate");
        let _receipt = store
            .append(
                &coord,
                kind,
                &serde_json::json!({ "n": i, "pad": "xxxxxxxxxxxxxxxx" }),
            )
            .expect("append");
    }
    crate::store::lifecycle::sync(&store).expect("sync source");
    store
}

fn merge_compaction() -> CompactionConfig {
    CompactionConfig {
        strategy: CompactionStrategy::Merge,
        min_segments: 2,
    }
}

#[test]
fn compaction_swap_rename_is_routed_and_faultable() {
    // CONTROL: an honest SimFs compacts to completion (Performed) — proving the
    // setup actually has ≥2 sealed segments and the swap rename runs.
    let control_dir = tempfile::tempdir().expect("tmpdir");
    let control_fs = Arc::new(SimFs::new(0x5043_0001, 0));
    let control = build_store_with_sealed_segments(control_dir.path(), &control_fs, 16);
    let (control_result, _report) = control
        .compact(&merge_compaction())
        .expect("control compaction must not error");
    assert_eq!(
        control_result.outcome,
        CompactionOutcome::Performed,
        "PROPERTY: the control compaction must Perform (≥2 sealed segments, swap rename runs)"
    );

    // FAULT: arm the swap rename AFTER the store is built so only compaction's
    // own renames count. The relocate-merged-source rename is the first
    // StoreFs::rename on the compaction path, so it tears, the off-side
    // materialize aborts, and the swap protocol rolls back to a Failed outcome.
    // Before W3 this rename was the free fn `platform::fs::rename`, which a
    // SimFs could not intercept — compaction would always Perform.
    let fault_dir = tempfile::tempdir().expect("tmpdir");
    let fault_fs = Arc::new(SimFs::new(0x5043_0002, 0));
    let faulted = build_store_with_sealed_segments(fault_dir.path(), &fault_fs, 16);
    fault_fs.arm_fault_on(CrashOp::Rename, 1);
    let (fault_result, _report) = faulted
        .compact(&merge_compaction())
        .expect("rollback after a torn swap rename is a clean Failed outcome, not an Err");
    let outcome = fault_result.outcome;
    assert!(
        matches!(&outcome, CompactionOutcome::Failed { .. }),
        "PROPERTY: a SimFs fault on the routed compaction swap rename must surface as a Failed compaction, got {outcome:?}"
    );
    if let CompactionOutcome::Failed { reason } = &outcome {
        assert!(
            reason.contains("Rename"),
            "PROPERTY: the Failed reason must carry the injected SimFs rename fault, got {reason:?}"
        );
    }
}

#[test]
fn cold_start_checkpoint_persist_is_routed_and_faultable() {
    use crate::store::cold_start::checkpoint::write_checkpoint_with_reserved_kind_fallbacks;
    use crate::store::cold_start::ReservedKindFallbackStats;
    use crate::store::index::StoreIndex;

    // An empty index still writes a full checkpoint artifact (header + footer),
    // which is enough to drive the temp-create + atomic-publish sequence.
    let index = StoreIndex::new();
    let fallbacks = ReservedKindFallbackStats::default();

    // CONTROL: the production backend publishes the checkpoint artifact — proving
    // the routed `write_file_atomically_with_fs` path is behavior-preserving.
    let dir = tempfile::tempdir().expect("tmpdir");
    write_checkpoint_with_reserved_kind_fallbacks(&index, dir.path(), 0, 0, &fallbacks, &RealFs)
        .expect("RealFs checkpoint artifact persist must succeed");

    // FAULT: a SimFs armed on the atomic publish tears the checkpoint persist.
    // Before this routing the checkpoint reached the free fn
    // `write_file_atomically` (no fs handle), so no backend could intercept it —
    // the cold-start artifact write was unfaultable.
    let fresh = tempfile::tempdir().expect("tmpdir");
    let fs = SimFs::new(0xC4EC_9001, 0).with_fault_on(CrashOp::PersistTemp, 1);
    let result =
        write_checkpoint_with_reserved_kind_fallbacks(&index, fresh.path(), 0, 0, &fallbacks, &fs);
    assert!(
        matches!(result, Err(StoreError::Io(_))),
        "PROPERTY: a SimFs fault on the routed checkpoint artifact persist must surface as StoreError::Io, got {result:?}"
    );
}
