// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MMAP-SEALED-READS; mmap cold-start tests in tests/mmap_cold_start.rs use panic! as the assertion style when invariants around checkpoint/mmap dispatch fail.
#![allow(clippy::panic)]
//! Mmap cold-start path proofs.
//! Harness pattern: Equivalence Harness (artifact-path parity lane).

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::{
    OpenIndexPath, OpenIndexReport, OpenReportObserver, ReadOnly, Store, StoreConfig, StoreError,
    StoreLockMode,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tempfile::TempDir;

fn assert_open_index_report_phase_micros_sane(report: &OpenIndexReport) {
    assert!(
        report.elapsed_us > 0,
        "PROPERTY: open_index elapsed_us should be non-zero for exercised paths, got {}",
        report.elapsed_us
    );
    let sum = report
        .phase_plan_build_us
        .saturating_add(report.phase_interner_us)
        .saturating_add(report.phase_restore_index_us)
        .saturating_add(report.phase_hidden_ranges_us);
    assert!(
        sum <= report.elapsed_us,
        "PROPERTY: cold-start phase micros must not exceed total elapsed; sum={sum} elapsed_us={}",
        report.elapsed_us
    );
}

/// Seeded mmap/checkpoint reopen paths must record non-zero phase work (guards defaulted zeros).
fn assert_open_index_report_phase_buckets_nonzero(report: &OpenIndexReport) {
    assert_open_index_report_phase_micros_sane(report);
    let sum = report
        .phase_plan_build_us
        .saturating_add(report.phase_interner_us)
        .saturating_add(report.phase_restore_index_us)
        .saturating_add(report.phase_hidden_ranges_us);
    assert!(
        sum > 0,
        "PROPERTY: seeded reopen must attribute cold-start work to at least one phase bucket (sum={sum}, elapsed_us={})",
        report.elapsed_us
    );
}

fn mmap_entries_offset(bytes: &[u8]) -> usize {
    const PREFIX_LEN: usize = 6 + 2 + 4;
    const HEADER_TAIL_LEN_V3: usize = (8 * 7) + 4;
    let version = u16::from_le_bytes(bytes[6..8].try_into().expect("version slice"));
    assert_eq!(
        version, 5,
        "test helper expects the live mmap snapshot format"
    );
    let header_tail = &bytes[PREFIX_LEN..PREFIX_LEN + HEADER_TAIL_LEN_V3];
    let interner_bytes_len =
        u64::from_le_bytes(header_tail[36..44].try_into().expect("interner size slice"));
    let summary_bytes_len =
        u64::from_le_bytes(header_tail[44..52].try_into().expect("summary size slice"));
    let extension_blob_len = u64::from_le_bytes(
        header_tail[52..60]
            .try_into()
            .expect("extension blob size slice"),
    );
    PREFIX_LEN
        + HEADER_TAIL_LEN_V3
        + usize::try_from(interner_bytes_len).expect("interner bytes fit usize")
        + usize::try_from(summary_bytes_len).expect("summary bytes fit usize")
        + usize::try_from(extension_blob_len).expect("extension blob bytes fit usize")
}

fn rewrite_first_mmap_kind(artifact: &std::path::Path, raw_kind: u16) {
    let mut bytes = std::fs::read(artifact).expect("read mmap artifact");
    let entries_offset = mmap_entries_offset(&bytes);
    let kind_offset = entries_offset + 24;
    bytes[kind_offset..kind_offset + 2].copy_from_slice(&raw_kind.to_le_bytes());
    let crc = crc32fast::hash(&bytes[12..]);
    bytes[8..12].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(artifact, bytes).expect("rewrite mmap artifact");
}

fn mmap_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(true)
        .with_sync_every_n_events(1)
}

fn seed_store(dir: &TempDir, count: u32) {
    let store = Store::open(mmap_config(dir)).expect("open store");
    let coord = Coordinate::new("entity:mmap", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..count {
        store
            .append(&coord, kind, &serde_json::json!({ "i": i }))
            .expect("append");
    }

    store.close().expect("close store");
}

#[test]
fn mmap_index_written_and_open_read_only_matches_open() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 24);

    let artifact = dir.path().join("index.fbati");
    assert!(
        artifact.exists(),
        "PROPERTY: close() with mmap index enabled must write index.fbati."
    );

    let open_store = Store::open(mmap_config(&dir)).expect("reopen open store");
    let lock_err = match Store::<ReadOnly>::open_read_only(mmap_config(&dir)) {
        Ok(_) => panic!("read-only open must fail while mutable store holds the lifetime lock"),
        Err(err) => err,
    };
    assert!(
        matches!(
            lock_err,
            StoreError::StoreLocked {
                mode: StoreLockMode::ReadOnly,
                ..
            }
        ),
        "read-only open while mutable owner is live must surface StoreLocked(ReadOnly), got {lock_err:?}"
    );

    let open_stream = open_store.by_entity("entity:mmap");
    assert_eq!(
        open_stream.len(),
        24,
        "mmap-backed reopen must preserve the full entity stream"
    );

    let open_query = open_store.query(&Region::scope("scope:test"));
    open_store.close().expect("close mutable reopen");

    let read_only = Store::<ReadOnly>::open_read_only(mmap_config(&dir)).expect("open read-only");
    let ro_stream = read_only.by_entity("entity:mmap");
    assert_eq!(
        ro_stream.len(),
        open_stream.len(),
        "ReadOnly reopen after mutable close must preserve stream cardinality"
    );

    let ro_query = read_only.query(&Region::scope("scope:test"));
    assert_eq!(
        ro_query.len(),
        open_query.len(),
        "ReadOnly and Open cold-start paths must agree on scoped query results"
    );
}

#[test]
fn corrupt_mmap_index_falls_back_cleanly() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 12);

    let artifact = dir.path().join("index.fbati");
    let mut bytes = std::fs::read(&artifact).expect("read mmap artifact");
    let len = bytes.len();
    bytes[len - 1] ^= 0x5A;
    std::fs::write(&artifact, bytes).expect("rewrite corrupt mmap artifact");

    let store = Store::open(mmap_config(&dir)).expect("reopen with corrupt mmap artifact");
    let stream = store.by_entity("entity:mmap");
    assert_eq!(
        stream.len(),
        12,
        "corrupt mmap artifact must fall back to durable segment rebuild without data loss"
    );
}

#[test]
fn truncated_mmap_index_falls_back_cleanly() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 12);

    let artifact = dir.path().join("index.fbati");
    assert!(
        artifact.exists(),
        "PROPERTY: close() with mmap index enabled must write index.fbati."
    );

    // Truncate the mmap index to half its original length.
    let bytes = std::fs::read(&artifact).expect("read mmap artifact");
    let half = bytes.len() / 2;
    std::fs::write(&artifact, &bytes[..half]).expect("write truncated mmap artifact");

    // Reopen must not panic — the store should detect the truncation and
    // fall back to a full segment scan to rebuild the index.
    let store = Store::open(mmap_config(&dir)).expect("reopen with truncated mmap artifact");
    let stream = store.by_entity("entity:mmap");
    assert_eq!(
        stream.len(),
        12,
        "PROPERTY: truncated mmap index must fall back to segment scan and recover all 12 events \
         without data loss."
    );
}

#[test]
fn default_config_reopen_uses_mmap_path() {
    let dir = TempDir::new().expect("temp dir");

    // Populate with default config (mmap=true, checkpoint=true)
    let default_config = StoreConfig::new(dir.path()).with_sync_every_n_events(1);
    let store = Store::open(default_config).expect("open store");
    let coord = Coordinate::new("entity:default", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    for i in 0..100u32 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.close().expect("close");

    // When mmap is enabled (default), only the mmap artifact is written.
    // Checkpoint is skipped to avoid redundant serialization on close.
    assert!(
        dir.path().join("index.fbati").exists(),
        "close() with default config must write index.fbati"
    );
    assert!(
        !dir.path().join("index.ckpt").exists(),
        "close() with mmap enabled should skip checkpoint (redundant)"
    );

    // Reopen with default config and check which path was used
    let default_config2 = StoreConfig::new(dir.path());
    let store2 = Store::open(default_config2).expect("reopen store");
    let diag = store2.diagnostics();
    let report: OpenIndexReport = diag
        .open_report
        .expect("open_report must be populated after open");
    assert_eq!(
        report.path,
        OpenIndexPath::Mmap,
        "PROPERTY: default config reopen must use the mmap path (fastest). \
         Got {:?} with {} restored + {} tail entries in {}us.",
        report.path,
        report.restored_entries,
        report.tail_entries,
        report.elapsed_us,
    );
    assert_open_index_report_phase_buckets_nonzero(&report);
    assert_eq!(
        store2.by_entity("entity:default").len(),
        100,
        "all events must be present after mmap reopen"
    );
    store2.close().expect("close");
}

#[test]
fn mmap_reopen_open_report_phase_micros_sane() {
    let dir = TempDir::new().expect("temp dir");
    // Larger seed so cold-start phase micros are unlikely to all truncate to 0µs on fast hosts.
    seed_store(&dir, 256);

    let store = Store::open(mmap_config(&dir)).expect("reopen mmap store");
    let report = store
        .diagnostics()
        .open_report
        .clone()
        .expect("open_report after mmap reopen");
    assert_eq!(report.path, OpenIndexPath::Mmap);
    assert_open_index_report_phase_buckets_nonzero(&report);
    assert_eq!(
        store.by_entity("entity:mmap").len(),
        256,
        "mmap reopen after larger seed must preserve full stream cardinality"
    );
    store.close().expect("close");
}

#[test]
fn mmap_open_report_counts_reserved_kind_fallbacks() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 12);

    let artifact = dir.path().join("index.fbati");
    rewrite_first_mmap_kind(&artifact, 0x000A);

    let store = Store::open(mmap_config(&dir)).expect("reopen with reserved-kind fallback");
    let report = store
        .diagnostics()
        .open_report
        .expect("open report after mmap reopen");
    assert_eq!(report.path, OpenIndexPath::Mmap);
    assert_eq!(
        report.unknown_reserved_system_kind_fallbacks, 1,
        "PROPERTY: mmap reopen must surface reserved system-kind fallback counts through open_report."
    );
    assert_eq!(
        report.unknown_reserved_system_kind_histogram.get(&0x000A),
        Some(&1)
    );
    assert_eq!(report.unknown_reserved_effect_kind_fallbacks, 0);
    assert!(
        report.unknown_reserved_effect_kind_histogram.is_empty(),
        "effect histogram must stay empty when only a system fallback occurs"
    );
    assert_eq!(report.cumulative_unknown_reserved_system_kind_fallbacks, 1);
    assert_eq!(
        report
            .cumulative_unknown_reserved_system_kind_histogram
            .get(&0x000A),
        Some(&1)
    );
    assert_eq!(
        store.by_entity("entity:mmap").len(),
        12,
        "reserved-kind fallback accounting must not drop live entries on reopen"
    );
    store.close().expect("close");

    let store = Store::open(mmap_config(&dir)).expect("second reopen after artifact refresh");
    let report = store
        .diagnostics()
        .open_report
        .expect("open report after second reopen");
    assert_eq!(report.unknown_reserved_system_kind_fallbacks, 0);
    assert!(
        report.unknown_reserved_system_kind_histogram.is_empty(),
        "refreshed artifact should not re-emit current reopen fallbacks"
    );
    assert_eq!(report.cumulative_unknown_reserved_system_kind_fallbacks, 1);
    assert_eq!(
        report
            .cumulative_unknown_reserved_system_kind_histogram
            .get(&0x000A),
        Some(&1)
    );
    store.close().expect("close second reopen");
}

#[test]
fn open_report_observer_runs_and_panics_do_not_abort_open() {
    let dir = TempDir::new().expect("temp dir");
    let observed = Arc::new(AtomicUsize::new(0));
    let reports = Arc::new(Mutex::new(Vec::<OpenIndexReport>::new()));
    let observer: OpenReportObserver = {
        let observed = Arc::clone(&observed);
        let reports = Arc::clone(&reports);
        Arc::new(move |report: &OpenIndexReport| {
            observed.fetch_add(1, Ordering::SeqCst);
            reports.lock().expect("lock reports").push(report.clone());
        })
    };

    let config = mmap_config(&dir).with_open_report_observer(Some(observer));
    let store = Store::open(config.clone()).expect("mutable open with observer");
    store.close().expect("close mutable");

    let read_only =
        Store::<ReadOnly>::open_read_only(config).expect("read-only open with observer");
    drop(read_only);

    assert_eq!(
        observed.load(Ordering::SeqCst),
        2,
        "observer must run once for mutable open and once for read-only open"
    );
    assert_eq!(
        reports.lock().expect("lock reports").len(),
        2,
        "observer must receive the structured open report on each successful open"
    );

    let panic_config = mmap_config(&dir).with_open_report_observer(Some(Arc::new(|_| {
        panic!("observer panic should not abort open");
    })));
    let store = Store::open(panic_config).expect("observer panic must not abort open");
    store.close().expect("close after panic observer");
}

#[test]
fn mutable_open_appends_system_open_completed_and_read_only_does_not() {
    let dir = TempDir::new().expect("temp dir");
    let config = mmap_config(&dir);

    let store = Store::open(config.clone()).expect("first mutable open");
    let lifecycle_events = store.by_fact(EventKind::SYSTEM_OPEN_COMPLETED);
    assert_eq!(
        lifecycle_events.len(),
        1,
        "mutable open must append exactly one SYSTEM_OPEN_COMPLETED event"
    );
    let lifecycle_entry = lifecycle_events[0].clone();
    assert_eq!(lifecycle_entry.coord.entity(), "batpak:store");
    assert_eq!(lifecycle_entry.coord.scope(), "batpak:lifecycle");
    let stored = store
        .get(lifecycle_entry.event_id)
        .expect("read lifecycle event payload");
    assert_eq!(
        stored.event.header.event_kind,
        EventKind::SYSTEM_OPEN_COMPLETED,
        "lifecycle event kind must round-trip as SYSTEM_OPEN_COMPLETED, not a fallback"
    );
    store.close().expect("close first mutable open");

    let reopened = Store::open(config.clone()).expect("second mutable open");
    assert_eq!(
        reopened.by_fact(EventKind::SYSTEM_OPEN_COMPLETED).len(),
        2,
        "second mutable open must append one additional lifecycle event"
    );
    reopened.close().expect("close second mutable open");

    let read_only =
        Store::<ReadOnly>::open_read_only(config).expect("read-only reopen after mutable close");
    assert_eq!(
        read_only.by_fact(EventKind::SYSTEM_OPEN_COMPLETED).len(),
        2,
        "read-only opens must not append additional lifecycle events"
    );
}
