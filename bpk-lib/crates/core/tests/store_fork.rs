//! Fork clone proofs.
//!
//! PROVES: INV-FORK-ISOLATION. A fork materializes a self-contained directory:
//! sealed segments may be shared, active segments and mutable authorities are
//! copied, caches are excluded by default, and parent/fork writes remain
//! isolated after the fork boundary.
//! CATCHES: accidental hardlinking of active/idempotency/visibility files,
//! stale destination leakage, symlink traversal, cache leakage, and fork output
//! that cannot reopen after compaction or fork-of-fork.
//! SEEDED: tempfile-backed stores, tiny segment rotation, hardlink-only fork
//! options, deterministic coordinates, cancelled visibility fences.

mod support;
use batpak::store::{
    fork_report_body_hash, ForkCopyStrategy, ForkEvidenceHash, ForkOptions, ForkReport,
    ForkReportBody, ForkStrategyCounts, ReadOnly, Store, StoreConfig,
    FORK_EVIDENCE_REPORT_SCHEMA_VERSION,
};
use std::path::{Path, PathBuf};
use support::prelude::*;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn store_with_small_segments(dir: &TempDir) -> TestResult<Store> {
    Ok(Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )?)
}

fn append_blob_events(store: &Store, entity: &str, count: usize) -> TestResult {
    let coord = Coordinate::new(entity, "scope:fork")?;
    let kind = EventKind::custom(0xF, 0x71);
    let blob = "x".repeat(300);
    for i in 0..count {
        store.append(&coord, kind, &serde_json::json!({"i": i, "blob": blob}))?;
    }
    Ok(())
}

fn segment_path(dir: &Path, segment_id: u64) -> PathBuf {
    dir.join(format!("{segment_id:06}.fbat"))
}

fn file_bytes(path: &Path) -> TestResult<Vec<u8>> {
    Ok(std::fs::read(path)?)
}

#[cfg(unix)]
fn file_identity(path: &Path) -> TestResult<(u64, u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino(), metadata.nlink()))
}

fn append_cancelled_visibility_range(store: &Store, entity: &str) -> TestResult {
    let coord = Coordinate::new(entity, "scope:fork")?;
    let kind = EventKind::custom(0xF, 0x72);
    let fence = store.begin_visibility_fence()?;
    let ticket = fence.submit(&coord, kind, &serde_json::json!({"hidden": true}))?;
    fence.cancel()?;
    let err = ticket
        .wait()
        .err()
        .ok_or_else(|| std::io::Error::other("cancelled fence ticket unexpectedly committed"))?;
    assert!(
        matches!(err, StoreError::VisibilityFenceCancelled),
        "cancelled fence ticket must surface VisibilityFenceCancelled, got {err:?}"
    );
    Ok(())
}

#[test]
fn fork_with_evidence_reopens_to_same_count_and_isolates_parent_writes() -> TestResult {
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:parent", 8)?;
    let before = store.stats();

    let fork_dir = TempDir::new()?;
    let report = store.fork_with_evidence(
        fork_dir.path(),
        ForkOptions {
            use_reflink: false,
            use_hardlink: false,
            exclude_caches: true,
        },
    )?;
    let envelope: ForkReport = report.clone();
    let body: ForkReportBody = envelope.body.clone();
    let report_hash: ForkEvidenceHash = envelope.body_hash;
    let _strategy_counts: ForkStrategyCounts = body.strategy_counts;
    assert_eq!(body.schema_version, FORK_EVIDENCE_REPORT_SCHEMA_VERSION);
    assert_eq!(report_hash, fork_report_body_hash(&body)?);
    assert_eq!(report_hash, body.body_hash()?);
    assert!(body
        .findings
        .iter()
        .any(|finding| { matches!(finding, batpak::store::ForkFinding::FenceTokenCancelled) }));

    let forked = Store::<ReadOnly>::open_read_only(StoreConfig::new(fork_dir.path()))?;
    assert_eq!(
        forked.stats().event_count,
        before.event_count,
        "fork must reopen to the source event count at the fork boundary"
    );
    assert_eq!(
        forked.stats().global_sequence,
        before.global_sequence,
        "fork must preserve the source global sequence at the fork boundary"
    );

    append_blob_events(&store, "entity:fork:parent", 1)?;
    assert_eq!(
        forked.stats().event_count,
        before.event_count,
        "parent writes after fork must not appear in the already-open fork"
    );

    store.close()?;
    Ok(())
}

#[test]
fn fork_convenience_wrapper_creates_openable_directory() -> TestResult {
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:wrapper", 2)?;
    let before = store.stats();

    let fork_dir = TempDir::new()?;
    store.fork(fork_dir.path())?;
    let forked = Store::<ReadOnly>::open_read_only(StoreConfig::new(fork_dir.path()))?;
    assert_eq!(forked.stats().event_count, before.event_count);

    store.close()?;
    Ok(())
}

#[test]
fn fork_hardlinks_sealed_segments_and_deep_copies_active_segment() -> TestResult {
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:hardlink", 10)?;

    let fork_dir = TempDir::new()?;
    let report = store.fork_with_evidence(
        fork_dir.path(),
        ForkOptions {
            use_reflink: false,
            use_hardlink: true,
            exclude_caches: true,
        },
    )?;

    assert!(
        !report.body.shared_segment_ids_sorted.is_empty(),
        "small segment fixture should create at least one sealed segment to share"
    );
    assert!(
        report.body.strategy_counts.hardlink > 0,
        "hardlink-only fork should report hardlinked sealed segments"
    );
    assert!(
        report
            .body
            .deep_copied_segment_ids_sorted
            .contains(&report.body.active_segment_id),
        "active segment must be copied, not shared"
    );
    assert!(report.body.findings.iter().any(|finding| {
        matches!(
            finding,
            batpak::store::ForkFinding::FileCopied {
                strategy: ForkCopyStrategy::Hardlink,
                ..
            }
        )
    }));

    store.close()?;
    Ok(())
}

#[test]
fn fork_reused_destination_clears_stale_store_artifacts() -> TestResult {
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:fresh", 3)?;
    let before = store.stats();

    let fork_dir = TempDir::new()?;
    {
        let stale = store_with_small_segments(&fork_dir)?;
        append_blob_events(&stale, "entity:fork:stale", 6)?;
        stale.close()?;
    }

    let report = store.fork_with_evidence(fork_dir.path(), ForkOptions::default())?;
    assert!(report.body.findings.iter().any(|finding| {
        matches!(
            finding,
            batpak::store::ForkFinding::DestinationCleared { .. }
        )
    }));
    let forked = Store::<ReadOnly>::open_read_only(StoreConfig::new(fork_dir.path()))?;
    assert_eq!(forked.stats().event_count, before.event_count);
    assert_eq!(forked.stats().global_sequence, before.global_sequence);

    store.close()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn fork_hardlink_only_shares_sealed_segments_but_copies_active_segment() -> TestResult {
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:nlink", 10)?;

    let fork_dir = TempDir::new()?;
    let report = store.fork_with_evidence(
        fork_dir.path(),
        ForkOptions {
            use_reflink: false,
            use_hardlink: true,
            exclude_caches: true,
        },
    )?;

    let sealed_id = *report
        .body
        .shared_segment_ids_sorted
        .first()
        .ok_or_else(|| std::io::Error::other("hardlink fixture did not create sealed segment"))?;
    let source_sealed = file_identity(&segment_path(source_dir.path(), sealed_id))?;
    let fork_sealed = file_identity(&segment_path(fork_dir.path(), sealed_id))?;
    assert_eq!(
        (source_sealed.0, source_sealed.1),
        (fork_sealed.0, fork_sealed.1),
        "sealed hardlink-only fork segment must share the same inode"
    );
    assert_eq!(source_sealed.2, 2, "sealed segment must have two links");

    let active_id = report.body.active_segment_id;
    let source_active = file_identity(&segment_path(source_dir.path(), active_id))?;
    let fork_active = file_identity(&segment_path(fork_dir.path(), active_id))?;
    assert_ne!(
        (source_active.0, source_active.1),
        (fork_active.0, fork_active.1),
        "active segment must be deep-copied, not hardlinked"
    );
    assert_eq!(
        source_active.2, 1,
        "source active segment must not gain a link"
    );
    assert_eq!(fork_active.2, 1, "fork active segment must be standalone");

    store.close()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn fork_rejects_symlink_destination_leaf() -> TestResult {
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:symlink", 1)?;

    let parent = TempDir::new()?;
    let real_dest = parent.path().join("real-dest");
    std::fs::create_dir(&real_dest)?;
    let link_dest = parent.path().join("link-dest");
    std::os::unix::fs::symlink(&real_dest, &link_dest)?;

    let err = store
        .fork_with_evidence(&link_dest, ForkOptions::default())
        .err()
        .ok_or_else(|| std::io::Error::other("fork through symlink destination succeeded"))?;
    assert!(
        matches!(err, StoreError::Io(_)),
        "symlink destination must be rejected as an IO boundary error, got {err:?}"
    );

    store.close()?;
    Ok(())
}

#[test]
fn fork_excludes_regenerable_caches_by_default() -> TestResult {
    let source_dir = TempDir::new()?;
    {
        let store = Store::open(
            StoreConfig::new(source_dir.path())
                .with_segment_max_bytes(512)
                .with_sync_every_n_events(1),
        )?;
        append_blob_events(&store, "entity:fork:caches", 3)?;
        store.close()?;
    }
    {
        let store = Store::open(
            StoreConfig::new(source_dir.path())
                .with_segment_max_bytes(512)
                .with_sync_every_n_events(1)
                .with_enable_checkpoint(true)
                .with_enable_mmap_index(false),
        )?;
        append_blob_events(&store, "entity:fork:caches", 1)?;
        store.close()?;
    }
    assert!(source_dir.path().join("index.ckpt").exists());
    assert!(source_dir.path().join("index.fbati").exists());

    let store = Store::open(
        StoreConfig::new(source_dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1),
    )?;
    let fork_dir = TempDir::new()?;
    let report = store.fork_with_evidence(fork_dir.path(), ForkOptions::default())?;

    assert!(!fork_dir.path().join("index.ckpt").exists());
    assert!(!fork_dir.path().join("index.fbati").exists());
    assert!(
        report.body.strategy_counts.cache_regenerable >= 2,
        "cache exclusion count must include checkpoint and mmap artifacts"
    );
    assert!(report.body.findings.iter().any(|finding| {
        matches!(
            finding,
            batpak::store::ForkFinding::CacheRegenerableExcluded { file_name }
                if file_name == "index.ckpt"
        )
    }));
    assert!(report.body.findings.iter().any(|finding| {
        matches!(
            finding,
            batpak::store::ForkFinding::CacheRegenerableExcluded { file_name }
                if file_name == "index.fbati"
        )
    }));

    store.close()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn fork_deep_copies_idempotency_and_visibility_authorities() -> TestResult {
    use batpak::id::IdempotencyKey;

    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    let coord = Coordinate::new("entity:fork:authorities", "scope:fork")?;
    let kind = EventKind::custom(0xF, 0x73);
    store.append_with_options(
        &coord,
        kind,
        &serde_json::json!({"keyed": true}),
        AppendOptions::default()
            .with_idempotency(IdempotencyKey::for_operation("fork-authority", &["seed"])),
    )?;
    append_cancelled_visibility_range(&store, "entity:fork:hidden")?;

    let fork_dir = TempDir::new()?;
    let report = store.fork_with_evidence(
        fork_dir.path(),
        ForkOptions {
            use_reflink: false,
            use_hardlink: true,
            exclude_caches: true,
        },
    )?;

    assert!(report.body.copied_idempotency_store_present);
    assert!(report.body.copied_visibility_ranges_present);
    for file_name in ["index.idemp", "visibility_ranges.fbv"] {
        let source_identity = file_identity(&source_dir.path().join(file_name))?;
        let fork_identity = file_identity(&fork_dir.path().join(file_name))?;
        assert_ne!(
            (source_identity.0, source_identity.1),
            (fork_identity.0, fork_identity.1),
            "{file_name} must be copied, not hardlinked"
        );
        assert_eq!(source_identity.2, 1, "{file_name} source link count");
        assert_eq!(fork_identity.2, 1, "{file_name} fork link count");
    }

    let fork_visibility_before = file_bytes(&fork_dir.path().join("visibility_ranges.fbv"))?;
    append_cancelled_visibility_range(&store, "entity:fork:hidden-after")?;
    let fork_visibility_after = file_bytes(&fork_dir.path().join("visibility_ranges.fbv"))?;
    assert_eq!(
        fork_visibility_after, fork_visibility_before,
        "parent post-fork visibility cancellation must not mutate fork visibility ranges"
    );

    store.close()?;
    Ok(())
}

#[test]
fn fork_after_compaction_and_fork_of_fork_are_openable() -> TestResult {
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:compact", 12)?;
    let _ = store.compact(&CompactionConfig {
        min_segments: 1,
        strategy: CompactionStrategy::Merge,
    })?;

    let fork_dir = TempDir::new()?;
    store.fork(fork_dir.path())?;
    let forked = Store::open(
        StoreConfig::new(fork_dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )?;
    append_blob_events(&forked, "entity:fork:forked", 1)?;
    let forked_stats = forked.stats();

    let second_fork_dir = TempDir::new()?;
    forked.fork(second_fork_dir.path())?;
    let second_fork = Store::<ReadOnly>::open_read_only(StoreConfig::new(second_fork_dir.path()))?;
    assert_eq!(second_fork.stats().event_count, forked_stats.event_count);
    assert_eq!(
        second_fork.stats().global_sequence,
        forked_stats.global_sequence
    );

    forked.close()?;
    store.close()?;
    Ok(())
}

#[test]
fn fork_refuses_destination_equal_to_source() -> TestResult {
    // A fork onto the store's own data directory must be rejected BEFORE the
    // destination-clearing pass — otherwise clear_fork_store_artifacts would
    // delete the live store's files (data loss). The source must stay intact.
    let source_dir = TempDir::new()?;
    let store = store_with_small_segments(&source_dir)?;
    append_blob_events(&store, "entity:fork:self", 5)?;
    let n = store.stats().event_count;

    assert!(
        store.fork(source_dir.path()).is_err(),
        "forking a store onto its own data directory must be rejected, not silently destroy it"
    );

    store.sync()?;
    assert_eq!(
        store.stats().event_count,
        n,
        "a rejected self-fork must leave the source store's events fully intact"
    );
    store.close()?;
    Ok(())
}
