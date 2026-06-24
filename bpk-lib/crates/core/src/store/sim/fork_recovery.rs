//! StoreFs-level fork crash recovery: classify fork destinations using the
//! same {CommittedPrefix | RolledBack | CanonicalRefusal} oracle as B3.

use super::fs::SimFs;
use super::recovery::{fold, is_canonical_refusal, FNV_OFFSET};
use super::recovery_matrix::Classification;
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::fork_report::ForkOptions;
use crate::store::{Open, Store, StoreConfig, StoreError};
use std::path::Path;
use std::sync::Arc;

/// How a fork destination classifies after a StoreFs-level crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForkFaultOutcome {
    pub classification: Classification,
    pub digest: u64,
    pub dest_event_count: usize,
}

fn classification_token(classification: Classification) -> u64 {
    match classification {
        Classification::CommittedPrefix => 0xC0_01,
        Classification::RolledBack => 0xC0_02,
        Classification::CanonicalRefusal => 0xC0_03,
    }
}

fn outcome_digest(seed: u64, classification: Classification, dest_event_count: usize) -> u64 {
    let mut d = fold(FNV_OFFSET, seed);
    d = fold(d, classification_token(classification));
    fold(d, dest_event_count as u64)
}

/// Classify `dest` after a fault: legal outcomes only.
pub(crate) fn classify_fork_destination(dest: &Path) -> Result<(Classification, usize), StoreError> {
    if !dest.exists() {
        return Ok((Classification::RolledBack, 0));
    }
    match Store::open_read_only(StoreConfig::new(dest)) {
        Ok(store) => {
            let count = store.stats().event_count;
            if count == 0 {
                Ok((Classification::RolledBack, 0))
            } else {
                Ok((Classification::CommittedPrefix, count))
            }
        }
        Err(error) if is_canonical_refusal(&error) => Ok((Classification::CanonicalRefusal, 0)),
        Err(error) => Err(error),
    }
}

/// One seeded fork-under-fault scenario over SimFs (StoreFs-level faults only).
pub(crate) fn run_seeded_fork_fault(seed: u64) -> Result<ForkFaultOutcome, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("seed=0x{seed:X}: tmpdir: {e}"))?;
    let source_dir = dir.path().join("source");
    let dest_dir = dir.path().join("dest");

    let fsync_drop = if seed.is_multiple_of(5) { 4 } else { 0 };
    let sim_fs = Arc::new(SimFs::new(seed ^ 0xF0_0F_00, fsync_drop));
    let config = StoreConfig::new(&source_dir)
        .with_sync_every_n_events(1)
        .with_segment_max_bytes(512)
        .with_fs(Arc::clone(&sim_fs) as Arc<dyn crate::store::platform::fs::StoreFs>);

    let store = Store::<Open>::open(config).map_err(|e| format!("seed=0x{seed:X}: open: {e}"))?;
    let steps = 3 + (seed % 5) as usize;
    let kind = EventKind::custom(0xF0, 0x0A);
    for i in 0..steps {
        let coord = Coordinate::new(format!("entity-{i}"), "scope:fork")
            .map_err(|e| format!("seed=0x{seed:X}: coord: {e}"))?;
        store
            .append(&coord, kind, &serde_json::json!({ "n": i }))
            .map_err(|e| format!("seed=0x{seed:X}: append: {e}"))?;
    }
    crate::store::lifecycle::sync(&store)
        .map_err(|e| format!("seed=0x{seed:X}: sync: {e}"))?;

    store
        .fork_with_evidence(&dest_dir, ForkOptions::default())
        .map_err(|e| format!("seed=0x{seed:X}: fork: {e}"))?;

    sim_fs.crash();

    let (classification, dest_event_count) =
        classify_fork_destination(&dest_dir).map_err(|e| format!("seed=0x{seed:X}: classify: {e}"))?;

    if matches!(classification, Classification::CommittedPrefix) && dest_event_count != steps {
        return Err(format!(
            "seed=0x{seed:X}: fork dest event count {dest_event_count} != source {steps}"
        ));
    }

    Ok(ForkFaultOutcome {
        classification,
        digest: outcome_digest(seed, classification, dest_event_count),
        dest_event_count,
    })
}

/// Doc-hidden public mirror for integration tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkFaultOutcomePublic {
    /// Recovered classification using the B3 oracle.
    pub classification: super::recovery_matrix::RecoveredClass,
    /// Determinism digest for this seed + outcome.
    pub digest: u64,
    /// Visible events in the fork destination when classification is CommittedPrefix.
    pub dest_event_count: usize,
}

/// Run one seeded fork-under-fault scenario (StoreFs-level).
pub fn run_seeded_fork_fault_public(seed: u64) -> Result<ForkFaultOutcomePublic, String> {
    run_seeded_fork_fault(seed).map(|o| ForkFaultOutcomePublic {
        classification: o.classification.into(),
        digest: o.digest,
        dest_event_count: o.dest_event_count,
    })
}

/// Replay seed helper honoring `BATPAK_SEED`.
pub fn fork_fault_replay_seed(default: u64) -> u64 {
    super::seed_from_env(default)
}
