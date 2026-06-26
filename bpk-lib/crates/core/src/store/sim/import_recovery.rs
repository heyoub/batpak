//! StoreFs-level import crash recovery: crash mid-import, reopen, re-import must
//! deduplicate via durable import keys and preserve payload bytes + hash chains.

use super::fs::SimFs;
use super::recovery::{fold, FNV_OFFSET};
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::id::EntityIdType;
use crate::store::{ImportOptions, ImportSelector, ReadOnly, Store, StoreConfig};
use std::sync::Arc;

/// Outcome of one seeded import-under-fault scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportFaultOutcome {
    pub(crate) digest: u64,
    pub(crate) source_user_events: usize,
    pub(crate) dest_user_events: usize,
    pub(crate) reimport_deduplicated: u64,
}

fn outcome_digest(
    seed: u64,
    source_user_events: usize,
    dest_user_events: usize,
    reimport_deduplicated: u64,
) -> u64 {
    let mut d = fold(FNV_OFFSET, seed);
    d = fold(d, source_user_events as u64);
    d = fold(d, dest_user_events as u64);
    fold(d, reimport_deduplicated)
}

/// Drive import on a real `Store` over `SimFs`, crash without shutdown, reopen,
/// re-import, and verify deduplication plus byte-isomorphic payloads.
pub(crate) fn run_seeded_import_fault(seed: u64) -> Result<ImportFaultOutcome, String> {
    let root = tempfile::tempdir().map_err(|e| format!("seed=0x{seed:X}: tmpdir: {e}"))?;
    let source_path = root.path().join("source");
    let dest_path = root.path().join("dest");

    let event_count = 4 + (seed % 5) as usize;
    let entity = "entity:import-fault";
    let kind = EventKind::custom(0xF, 0x90);

    {
        let source = Store::open(
            StoreConfig::new(&source_path)
                .with_sync_every_n_events(1)
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false),
        )
        .map_err(|e| format!("seed=0x{seed:X}: open source: {e}"))?;
        let coord = Coordinate::new(entity, "scope:import")
            .map_err(|e| format!("seed=0x{seed:X}: coord: {e}"))?;
        for i in 0..event_count {
            drop(
                source
                    .append(&coord, kind, &serde_json::json!({ "n": i }))
                    .map_err(|e| format!("seed=0x{seed:X}: source append: {e}"))?,
            );
        }
        source
            .close()
            .map_err(|e| format!("seed=0x{seed:X}: close source: {e}"))?;
    }

    let source = Store::<ReadOnly>::open_read_only(StoreConfig::new(&source_path))
        .map_err(|e| format!("seed=0x{seed:X}: reopen source: {e}"))?;

    let options = ImportOptions::new("source-fault")
        .map_err(|e| format!("seed=0x{seed:X}: options: {e}"))?
        .with_chunk_size(1);

    let sim_fs = Arc::new(SimFs::new(seed ^ 0x1B00_0001, 0));
    {
        let config = StoreConfig::new(&dest_path)
            .with_fs(Arc::clone(&sim_fs) as Arc<dyn crate::store::platform::fs::StoreFs>)
            .with_sync_every_n_events(1_000_000)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false);
        let dest = Store::open(config).map_err(|e| format!("seed=0x{seed:X}: open dest: {e}"))?;
        dest.import_events(&source, &ImportSelector::all(), &options)
            .map_err(|e| format!("seed=0x{seed:X}: first import: {e}"))?;
        dest.abandon_without_shutdown();
        sim_fs.crash();
    }

    let dest = Store::open(
        StoreConfig::new(&dest_path)
            .with_sync_every_n_events(1)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .map_err(|e| format!("seed=0x{seed:X}: reopen dest: {e}"))?;

    let replay = dest
        .import_events(&source, &ImportSelector::all(), &options)
        .map_err(|e| format!("seed=0x{seed:X}: reimport: {e}"))?;

    let source_entries = source.by_entity(entity);
    let dest_entries = dest.by_entity(entity);
    if dest_entries.len() != source_entries.len() {
        return Err(format!(
            "seed=0x{seed:X}: dest user event count {} != source {}",
            dest_entries.len(),
            source_entries.len()
        ));
    }

    for window in dest_entries.windows(2) {
        if window[1].hash_chain().prev_hash != window[0].hash_chain().event_hash {
            return Err(format!(
                "seed=0x{seed:X}: broken hash chain at global_sequence {}",
                window[1].global_sequence()
            ));
        }
    }

    for (dest_entry, source_entry) in dest_entries.iter().zip(source_entries.iter()) {
        let dest_raw = dest
            .read_raw(dest_entry.event_id())
            .map_err(|e| format!("seed=0x{seed:X}: read dest raw: {e}"))?;
        let source_raw = source
            .read_raw(source_entry.event_id())
            .map_err(|e| format!("seed=0x{seed:X}: read source raw: {e}"))?;
        if dest_raw.event.payload != source_raw.event.payload {
            return Err(format!(
                "seed=0x{seed:X}: payload bytes diverged for source event {:032x}",
                source_entry.event_id().as_u128()
            ));
        }
        if dest_raw.event.header.content_hash != source_raw.event.header.content_hash {
            return Err(format!(
                "seed=0x{seed:X}: content hash diverged for source event {:032x}",
                source_entry.event_id().as_u128()
            ));
        }
    }

    Ok(ImportFaultOutcome {
        digest: outcome_digest(
            seed,
            source_entries.len(),
            dest_entries.len(),
            replay.deduplicated,
        ),
        source_user_events: source_entries.len(),
        dest_user_events: dest_entries.len(),
        reimport_deduplicated: replay.deduplicated,
    })
}

/// Doc-hidden public mirror for integration tests (hidden via the
/// `#[doc(hidden)] pub mod __sim` re-export, mirroring `ForkFaultOutcomePublic`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFaultOutcomePublic {
    /// Determinism digest for this seed + outcome.
    pub digest: u64,
    /// User events in the source store.
    pub source_user_events: usize,
    /// User events visible in the destination after recovery + re-import.
    pub dest_user_events: usize,
    /// Events counted as deduplicated on the post-crash re-import pass.
    pub reimport_deduplicated: u64,
}

/// Run one seeded import-under-fault scenario (StoreFs-level).
///
/// # Errors
/// Returns a seed-tagged description string when the scenario cannot run or the
/// post-crash re-import fails to preserve payload bytes, hash chains, or dedup.
pub fn run_seeded_import_fault_public(seed: u64) -> Result<ImportFaultOutcomePublic, String> {
    run_seeded_import_fault(seed).map(|o| ImportFaultOutcomePublic {
        digest: o.digest,
        source_user_events: o.source_user_events,
        dest_user_events: o.dest_user_events,
        reimport_deduplicated: o.reimport_deduplicated,
    })
}

/// Replay seed helper honoring `BATPAK_SEED`.
pub fn import_fault_replay_seed(default: u64) -> u64 {
    super::seed_from_env(default)
}
