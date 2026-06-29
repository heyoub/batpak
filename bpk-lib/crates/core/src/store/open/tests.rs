use super::*;
use tempfile::TempDir;

#[test]
fn next_active_segment_id_is_one_past_latest_existing_segment() -> Result<(), StoreError> {
    let dir = TempDir::new()?;
    crate::store::platform::fs::write_derivative_file_atomically(
        dir.path(),
        &dir.path().join(segment::segment_filename(1)),
        "test segment",
        b"",
    )?;
    crate::store::platform::fs::write_derivative_file_atomically(
        dir.path(),
        &dir.path().join(segment::segment_filename(7)),
        "test segment",
        b"",
    )?;

    assert_eq!(
        next_active_segment_id(dir.path())?,
        8,
        "PROPERTY: reader active segment must be one past the highest existing segment so the last sealed segment remains mmap-eligible"
    );
    Ok(())
}

/// GAUNTLET (#63 scheduler seam): the COOPERATIVE writer drive runs with ZERO
/// OS writer threads — it is pumped inline at the reply funnel — while the
/// THREADED drive spawns the writer as before.
///
/// CATCHES: a cooperative path that silently falls back to spawning a thread
/// (the deadlock the recovery.rs scheduler-note used to defer). This is the red
/// fixture for the seam: the threaded branch first proves the spawn counter
/// actually ticks (`>= 1`), so a cooperative regression to spawning would fail
/// the `== 0` assertion rather than pass vacuously.
#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn cooperative_open_spawns_zero_writer_threads_but_threaded_spawns_one() {
    use crate::store::platform::spawn::{JobHandle, Spawn, ThreadSpawn};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Counts every Spawn::spawn call, then delegates to a real ThreadSpawn so
    // the threaded join contract still holds end-to-end.
    struct CountingSpawn {
        count: Arc<AtomicUsize>,
        inner: ThreadSpawn,
    }
    impl Spawn for CountingSpawn {
        fn spawn(
            &self,
            name: String,
            stack_size: Option<usize>,
            body: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<Box<dyn JobHandle>, crate::store::platform::spawn::SpawnError> {
            self.count.fetch_add(1, Ordering::Release);
            self.inner.spawn(name, stack_size, body)
        }
    }

    // Threaded baseline: opening spawns the writer thread (counter must tick —
    // this is what proves the fixture would catch a cooperative spawn-fallback).
    let threaded_count = Arc::new(AtomicUsize::new(0));
    let threaded_dir = TempDir::new().expect("temp dir");
    let threaded_spawner: Arc<dyn Spawn> = Arc::new(CountingSpawn {
        count: Arc::clone(&threaded_count),
        inner: ThreadSpawn,
    });
    let threaded =
        Store::open(StoreConfig::new(threaded_dir.path()).with_spawner(threaded_spawner))
            .expect("open threaded");
    threaded.close().expect("close threaded");
    assert!(
        threaded_count.load(Ordering::Acquire) >= 1,
        "PROPERTY: the THREADED writer must spawn at least one OS thread (proves the counter bites)"
    );

    // Cooperative: opening spawns NOTHING; the writer is driven inline by the
    // reply-funnel pump. A real append proves the inline drive actually runs.
    let coop_count = Arc::new(AtomicUsize::new(0));
    let coop_dir = TempDir::new().expect("temp dir");
    let coop_spawner: Arc<dyn Spawn> = Arc::new(CountingSpawn {
        count: Arc::clone(&coop_count),
        inner: ThreadSpawn,
    });
    let coop =
        Store::open_cooperative(StoreConfig::new(coop_dir.path()).with_spawner(coop_spawner))
            .expect("open cooperative");
    let coord = Coordinate::new("entity:coop-seam", "scope:test").expect("coord");
    let _ = coop
        .append(
            &coord,
            EventKind::custom(0xC, 0x0B),
            &serde_json::json!({ "x": 1 }),
        )
        .expect("cooperative append drives inline");
    coop.close().expect("close cooperative");
    assert_eq!(
        coop_count.load(Ordering::Acquire),
        0,
        "PROPERTY: the COOPERATIVE writer must spawn ZERO OS threads — it runs inline via the reply-funnel pump"
    );
}

#[test]
fn highest_index_hlc_reports_non_origin_point_for_appended_entry() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = Coordinate::new("entity:highest-hlc", "scope:test").expect("coord");
    let receipt = store
        .append(
            &coord,
            EventKind::custom(0xF, 0x77),
            &serde_json::json!({"x": 1}),
        )
        .expect("append");

    let point = highest_index_hlc(&store.index);

    assert_eq!(
        point.global_sequence, receipt.global_sequence,
        "PROPERTY: highest_index_hlc must observe the committed entry's global sequence"
    );
    assert!(
        point > HlcPoint::ORIGIN,
        "PROPERTY: highest_index_hlc must not collapse a non-empty index to origin/default"
    );

    store.close().expect("close");
}
