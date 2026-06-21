//! Cold-start recovery artifacts.
//! Harness pattern: Fault-Injection Harness (artifact recovery lane).
//!
//! [INV-COLD-START-ARTIFACTS] A clean close writes the expected on-disk
//! artifacts (segment + SIDX footer + the preferred fast-start artifact),
//! and reopening the store from those artifacts yields the same visible
//! events with no data loss. When both checkpoint and mmap snapshots are
//! enabled, close() writes only `index.fbati`; checkpoint is skipped as
//! redundant work. A surgical mid-frame truncation of a segment does not
//! crash the reopen path — the pre-truncation frames remain queryable and
//! the corruption is confined to frames after the cut.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::{ReadOnly, Store, StoreConfig};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xE, 1);

fn config_with_artifacts(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(true)
        .with_enable_mmap_index(true)
        .with_sync_every_n_events(1)
}

fn append_seed(store: &Store, entity: &str, scope: &str, count: u32) -> u32 {
    let coord = Coordinate::new(entity, scope).expect("valid coord");
    for i in 0..count {
        store
            .append(&coord, KIND, &serde_json::json!({"i": i}))
            .expect("append");
    }
    count
}

fn find_single_segment(dir: &TempDir) -> std::path::PathBuf {
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read data dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|s| s == "fbat")
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "PROPERTY: test seeds a single segment; found {} .fbat files in {:?}",
        entries.len(),
        entries
    );
    entries.into_iter().next().expect("exactly one segment")
}

fn user_visible_entries<State>(store: &Store<State>) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect()
}

#[test]
fn clean_close_writes_expected_artifacts_and_roundtrips() {
    let dir = TempDir::new().expect("temp dir");
    let count = {
        let store = Store::open(config_with_artifacts(&dir)).expect("open store");
        let count = append_seed(&store, "entity:cold", "scope:test", 32);
        store.close().expect("clean close");
        count
    };

    // Segment file must exist.
    let segment_path = find_single_segment(&dir);
    let seg_meta = std::fs::metadata(&segment_path).expect("segment metadata");
    assert!(
        seg_meta.len() > 0,
        "PROPERTY: a clean close must leave a non-empty segment on disk; got len=0 at {:?}",
        segment_path
    );

    // With both checkpoint and mmap enabled, close() prefers the mmap index
    // artifact and intentionally skips the redundant checkpoint.
    let ckpt_path = dir.path().join("index.ckpt");
    assert!(
        !ckpt_path.exists(),
        "PROPERTY: clean close with mmap enabled should skip redundant index.ckpt"
    );

    // mmap index artifact — written when enable_mmap_index is true.
    let mmap_path = dir.path().join("index.fbati");
    assert!(
        mmap_path.exists(),
        "PROPERTY: clean close with mmap index enabled must write index.fbati"
    );

    // SIDX footer must be present at the tail of the segment file.
    let bytes = std::fs::read(&segment_path).expect("read segment");
    assert!(
        bytes.len() >= 16,
        "PROPERTY: segment file must be at least 16 bytes (SIDX trailer) after clean close"
    );
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "PROPERTY: the last 4 bytes of the segment must be the SIDX magic b\"SDX3\""
    );

    // Reopening must surface every event we wrote.
    let store =
        Store::<ReadOnly>::open_read_only(config_with_artifacts(&dir)).expect("reopen store");
    let entries = user_visible_entries(&store);
    let entry_count =
        u32::try_from(entries.len()).expect("test fixture keeps event count within u32");
    assert_eq!(
        entry_count,
        count,
        "PROPERTY: reopen after clean close must yield all {count} events; got {}",
        entries.len()
    );
    drop(store);
}

#[test]
fn truncated_segment_mid_frame_does_not_crash_reopen() {
    let dir = TempDir::new().expect("temp dir");
    {
        let store = Store::open(config_with_artifacts(&dir)).expect("open store");
        append_seed(&store, "entity:trunc", "scope:test", 16);
        store.close().expect("clean close");
    }

    // Delete the checkpoint + mmap artifacts so the reopen path must frame-
    // scan the segment. Without this, the fast paths would bypass the
    // corruption entirely.
    let _ = std::fs::remove_file(dir.path().join("index.ckpt"));
    let _ = std::fs::remove_file(dir.path().join("index.fbati"));

    // Truncate the segment to roughly the midpoint so at least one frame is
    // cleanly preserved and the cut lands inside a later frame. The SIDX
    // footer goes along with the truncation — the reopen path must cope
    // with a segment that has neither a SIDX footer nor a clean EOF.
    let segment_path = find_single_segment(&dir);
    let bytes = std::fs::read(&segment_path).expect("read segment");
    let half = bytes.len() / 2;
    std::fs::write(&segment_path, &bytes[..half]).expect("write truncated segment");

    // Reopen must NOT panic or return an error; the scan is resilient and
    // stops at the first unreadable frame, preserving every frame before the
    // cut.
    let store = Store::open(config_with_artifacts(&dir)).expect("reopen after truncation");
    let entries = store.query(&Region::all());

    // Post-truncation we cannot predict the exact count, but it must be at
    // least 1 (some prefix survived) and never exceed the original 16.
    assert!(
        !entries.is_empty(),
        "PROPERTY: truncated segment must still expose the pre-truncation frames; got 0 entries"
    );
    assert!(
        entries.len() <= 16,
        "PROPERTY: truncated segment must not fabricate entries; got {} (max 16)",
        entries.len()
    );

    // Store must remain usable after the corruption — a subsequent append
    // lands cleanly on a fresh segment.
    let coord = Coordinate::new("entity:trunc", "scope:test").expect("valid coord");
    let post = store
        .append(&coord, KIND, &serde_json::json!({"post_truncation": true}))
        .expect("append after corrupt reopen");
    assert_ne!(
        post.event_id,
        batpak::id::EventId::from(0u128),
        "PROPERTY: post-truncation append must succeed with a non-zero event id"
    );
    store.close().expect("close after recovery");
}

/// C4: segment create/rotation fsyncs the parent directory entry. An
/// in-process test cannot cut power, but the observable consequence of the
/// directory fsync is that every rotated segment's directory entry is present
/// after a forced rotation and an unclean (drop, not close) shutdown — so a
/// re-open recovers the full event count from on-disk segments alone.
#[test]
fn forced_rotation_then_unclean_reopen_sees_all_segment_entries() {
    let dir = TempDir::new().expect("temp dir");

    // Tiny segment_max_bytes forces many rotations; disable checkpoint/mmap so
    // the reopen path must rebuild purely from the on-disk *.fbat segments,
    // making segment directory-entry visibility load-bearing for recovery.
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(256)
        .with_sync_every_n_events(1)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false);

    let count = 64u32;
    {
        let store = Store::open(config.clone()).expect("open store");
        let coord = Coordinate::new("entity:rot", "scope:test").expect("valid coord");
        for i in 0..count {
            store
                .append(&coord, KIND, &serde_json::json!({"i": i}))
                .expect("append");
        }
        // Drop without close(): no clean-shutdown checkpoint/index artifacts.
    }

    // At least one rotation must have happened, and every rotated segment's
    // directory entry must be visible via read_dir (the dir-fsync consequence).
    let segments: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
        .expect("read data dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .map(|s| s == "fbat")
                .unwrap_or(false)
        })
        .collect();
    assert!(
        segments.len() >= 2,
        "PROPERTY: tiny segment_max_bytes must force >=1 rotation; found {} segments: {:?}",
        segments.len(),
        segments
    );

    // Cold-start recovery from the visible segments must yield the full count.
    let store = Store::<ReadOnly>::open_read_only(config).expect("reopen after unclean shutdown");
    let recovered = user_visible_entries(&store).len();
    assert_eq!(
        recovered,
        usize::try_from(count).expect("seeded event count fits in usize"),
        "PROPERTY: every rotated segment's directory entry must survive an unclean shutdown so \
         cold-start recovers all {count} events; got {recovered}"
    );
}
