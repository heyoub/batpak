//! Offensive hostile-filesystem fork fixtures (#48 `fork_hostile_fs`).
//!
//! Where [`super::fork_recovery`] proves the fork is crash-atomic under seeded
//! fsync-drop faults, this module is ADVERSARIAL: it drives the real
//! [`Store::fork_with_evidence`] over a [`SimFs`] under four hostile
//! filesystem conditions and asserts each is met with an `Err`/canonical
//! refusal — never a silent partial publish:
//!
//!   1. **symlink destination leaf** — the fork target is a symlink. The fork
//!      must refuse (`reject_symlink_leaf`), not follow the link and write
//!      through it.
//!   2. **destination == source** — the fork target canonicalizes to the
//!      source data dir. The fork must refuse rather than copy a store onto
//!      itself.
//!   3. **stale destination** — the target already holds store artifacts from a
//!      prior aborted fork. The fork must CLEAR them first (a
//!      `DestinationCleared` finding) and then succeed, so the result is the
//!      source's state, never a merge of old + new.
//!   4. **ENOSPC mid-copy** — the disk fills partway through the segment copy
//!      walk. The fork must return `Err` and leave NO openable complete fork at
//!      the destination (no partial publish).
//!
//! The runners compose the genuine production fork path; the only injected
//! fault is [`SimFs::with_enospc_on_copy`], which fails a chosen
//! file-materialization op with `ENOSPC`. Each runner returns a typed outcome
//! the integration test (`crates/core/tests/fork_hostile_fs.rs`) asserts.

use super::fs::SimFs;
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::fork_report::{ForkFinding, ForkOptions};
use crate::store::{Open, Store, StoreConfig, StoreError};
use std::path::Path;
use std::sync::Arc;

/// Build a small synced source store over `sim_fs` with `events` user appends.
fn build_source(
    source_dir: &Path,
    sim_fs: &Arc<SimFs>,
    events: usize,
) -> Result<Store<Open>, String> {
    let config = StoreConfig::new(source_dir)
        .with_sync_every_n_events(1)
        .with_segment_max_bytes(512)
        .with_fs(Arc::clone(sim_fs) as Arc<dyn crate::store::platform::fs::StoreFs>);
    let store = Store::<Open>::open(config).map_err(|e| format!("open source: {e}"))?;
    let kind = EventKind::custom(0xF, 0x0B);
    for i in 0..events {
        let coord = Coordinate::new(format!("entity-{i}"), "scope:fork-hostile")
            .map_err(|e| format!("coord: {e}"))?;
        let _receipt = store
            .append(&coord, kind, &serde_json::json!({ "n": i }))
            .map_err(|e| format!("append: {e}"))?;
    }
    crate::store::lifecycle::sync(&store).map_err(|e| format!("sync: {e}"))?;
    Ok(store)
}

/// `true` when `dest` opens read-only as a non-empty, valid store — i.e. a fork
/// was actually published there. A hostile fork that refused must leave this
/// `false` (the destination is absent, empty, or fails to open cleanly).
fn dest_is_published_store(dest: &Path) -> bool {
    if !dest.exists() {
        return false;
    }
    match Store::open_read_only(StoreConfig::new(dest)) {
        Ok(store) => store.stats().event_count > 0,
        Err(_) => false,
    }
}

/// Outcome of the symlink-destination-leaf hostile fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymlinkDestOutcome {
    /// The fork returned `Err` (refused the symlink leaf).
    pub refused: bool,
    /// No openable store was published at the symlink's real target.
    pub no_publish: bool,
}

/// Drive a fork whose destination LEAF is a symlink. Must refuse.
///
/// `link_dest` is a caller-created symlink (the OS-specific symlink syscall is
/// kept out of this store-runtime module per the platform-isolation lint);
/// `real_target` is the directory the symlink resolves to. The runner forks at
/// the LINK path and asserts nothing was published through it.
///
/// # Errors
/// Returns a description string when the fixture cannot be set up.
pub fn run_fork_symlink_dest(
    link_dest: &Path,
    real_target: &Path,
) -> Result<SymlinkDestOutcome, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("tmpdir: {e}"))?;
    let source_dir = dir.path().join("source");
    let sim_fs = Arc::new(SimFs::new(0x5117_0001, 0));
    let store = build_source(&source_dir, &sim_fs, 3)?;

    let result = store.fork_with_evidence(link_dest, ForkOptions::default());
    let refused = matches!(result, Err(StoreError::Io(_)));
    // Even if the link target resolves to a real dir, nothing complete must
    // have been published THROUGH the symlink.
    let no_publish = !dest_is_published_store(real_target);

    store.close().map_err(|e| format!("close: {e}"))?;
    Ok(SymlinkDestOutcome {
        refused,
        no_publish,
    })
}

/// Outcome of the destination-equals-source hostile fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestEqualsSourceOutcome {
    /// The fork returned `Err` (refused to fork onto the source dir).
    pub refused: bool,
    /// The error was `InvalidInput` (the same-dir canonical refusal), not an
    /// unrelated IO failure.
    pub refused_invalid_input: bool,
}

/// Drive a fork whose destination canonicalizes to the SOURCE data dir. Must
/// refuse.
///
/// # Errors
/// Returns a description string when the fixture cannot be set up.
pub fn run_fork_dest_equals_source() -> Result<DestEqualsSourceOutcome, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("tmpdir: {e}"))?;
    let source_dir = dir.path().join("source");
    let sim_fs = Arc::new(SimFs::new(0x5117_0002, 0));
    let store = build_source(&source_dir, &sim_fs, 3)?;

    let result = store.fork_with_evidence(&source_dir, ForkOptions::default());
    let refused = result.is_err();
    let refused_invalid_input = matches!(
        &result,
        Err(StoreError::Io(io_err)) if io_err.kind() == std::io::ErrorKind::InvalidInput
    );

    store.close().map_err(|e| format!("close: {e}"))?;
    Ok(DestEqualsSourceOutcome {
        refused,
        refused_invalid_input,
    })
}

/// Outcome of the stale-destination hostile fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleDestOutcome {
    /// The fork succeeded after clearing the stale artifacts.
    pub forked_ok: bool,
    /// The fork emitted a `DestinationCleared` finding (the stale artifacts
    /// were positively cleared, not merged).
    pub cleared_stale: bool,
    /// The published fork holds exactly the source's committed event count
    /// (no merge of stale + fresh state).
    pub dest_matches_source: bool,
}

/// The two clearable stale-artifact file names the test plants at the fork
/// destination before calling [`run_fork_stale_dest`]: a leftover segment
/// (`.fbat`) and a leftover visibility-ranges file — both names the fork's file
/// classifier recognizes as clearable store artifacts.
pub const STALE_SEGMENT_FILE: &str = "000099.fbat";
/// See [`STALE_SEGMENT_FILE`].
pub const STALE_RANGES_FILE: &str = "visibility_ranges.fbv";

/// Drive a fork whose destination (`dest_dir`) already holds STALE store
/// artifacts from a prior aborted fork (planted by the caller). The fork must
/// clear them, then succeed with the source's state.
///
/// Raw stale-artifact planting is left to the caller's test scaffolding so this
/// store-runtime module makes no direct filesystem contact (platform-boundary
/// ratchet) beyond the production fork/store seams.
///
/// # Errors
/// Returns a description string when the fixture cannot be set up.
pub fn run_fork_stale_dest(dest_dir: &Path) -> Result<StaleDestOutcome, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("tmpdir: {e}"))?;
    let source_dir = dir.path().join("source");
    let sim_fs = Arc::new(SimFs::new(0x5117_0003, 0));
    let store = build_source(&source_dir, &sim_fs, 4)?;
    let source_committed = store.stats().event_count;

    let report = store
        .fork_with_evidence(dest_dir, ForkOptions::default())
        .map_err(|e| format!("fork over stale dest: {e}"))?;
    let forked_ok = true;
    let cleared_stale = report
        .body
        .findings
        .iter()
        .any(|f| matches!(f, ForkFinding::DestinationCleared { .. }));

    // Reopen the fork and confirm it carries exactly the source's committed
    // state — proof the stale bytes were cleared, not merged.
    let dest_count = match Store::open_read_only(StoreConfig::new(dest_dir)) {
        Ok(forked) => forked.stats().event_count,
        Err(e) => return Err(format!("reopen forked dest: {e}")),
    };
    let dest_matches_source = dest_count == source_committed;

    store.close().map_err(|e| format!("close: {e}"))?;
    Ok(StaleDestOutcome {
        forked_ok,
        cleared_stale,
        dest_matches_source,
    })
}

/// Outcome of the ENOSPC-mid-copy hostile fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnospcMidCopyOutcome {
    /// The fork returned `Err` when the disk filled mid-copy.
    pub refused: bool,
    /// The error was a storage-full IO error.
    pub refused_storage_full: bool,
    /// No openable complete fork was published at the destination (no partial
    /// publish).
    pub no_partial_publish: bool,
}

/// Drive a fork that runs out of disk space (ENOSPC) partway through the
/// segment-copy walk. Must return `Err` and publish no partial fork.
///
/// # Errors
/// Returns a description string when the fixture cannot be set up.
pub fn run_fork_enospc_mid_copy() -> Result<EnospcMidCopyOutcome, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("tmpdir: {e}"))?;
    let source_dir = dir.path().join("source");
    let dest_dir = dir.path().join("dest");

    // Enough events across small segments that the fork copy walk materializes
    // several files; arm ENOSPC on the first materialization so the fork fails
    // mid-walk with at least one copy still pending.
    let sim_fs = Arc::new(SimFs::new(0x5117_0004, 0).with_enospc_on_copy(1));
    let store = build_source(&source_dir, &sim_fs, 8)?;

    let result = store.fork_with_evidence(&dest_dir, ForkOptions::default());
    let refused = result.is_err();
    let refused_storage_full = matches!(
        &result,
        Err(StoreError::Io(io_err)) if io_err.kind() == std::io::ErrorKind::StorageFull
    );
    // The destination must NOT open as a complete, non-empty store — a failed
    // fork leaves no partial publish a reader could mistake for a valid fork.
    let no_partial_publish = !dest_is_published_store(&dest_dir);

    store.close().map_err(|e| format!("close: {e}"))?;
    Ok(EnospcMidCopyOutcome {
        refused,
        refused_storage_full,
        no_partial_publish,
    })
}
