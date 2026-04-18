// justifies: mmap cold-start tests use panic! as the assertion style when invariants around checkpoint/mmap dispatch fail.
#![allow(clippy::panic)]

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::{OpenIndexPath, OpenIndexReport, ReadOnly, Store, StoreConfig};
use tempfile::TempDir;

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
    let read_only = Store::<ReadOnly>::open_read_only(mmap_config(&dir)).expect("open read-only");

    let open_stream = open_store.stream("entity:mmap");
    let ro_stream = read_only.stream("entity:mmap");
    assert_eq!(
        open_stream.len(),
        24,
        "mmap-backed reopen must preserve the full entity stream"
    );
    assert_eq!(
        ro_stream.len(),
        open_stream.len(),
        "ReadOnly and Open cold-start paths must agree on stream cardinality"
    );

    let open_query = open_store.query(&Region::scope("scope:test"));
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
    let stream = store.stream("entity:mmap");
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
    let stream = store.stream("entity:mmap");
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
    assert_eq!(
        store2.stream("entity:default").len(),
        100,
        "all events must be present after mmap reopen"
    );
    store2.close().expect("close");
}
