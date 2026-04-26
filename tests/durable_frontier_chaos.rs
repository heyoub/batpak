// justifies: INV-TEST-PANIC-AS-ASSERTION; panic-path chaos tests intentionally panic in a test-local FaultInjector and use panic! for invariant failures.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]

//! PROVES:
//!   - Writer-thread panics at frontier fault-injection seams surface as
//!     `WriterCrashed` to the caller without poisoning frontier observation.
//!   - Reopen after a panic preserves the lifecycle-open frontier monotonicity
//!     established by Phase 0 bootstrap policy.
//!   - The current batch COMMIT-written/pre-fsync panic window is pinned to
//!     the implementation's recovery behavior.
//!
//! CATCHES: accidental production panic actions, poisoned frontier mutexes,
//! and drift in writer-panic recovery semantics.
//!
//! SEEDED: deterministic tempdir-based opens; no randomness.

use batpak::prelude::{Coordinate, EventKind, Region};
use batpak::store::{
    AppendOptions, BatchAppendItem, CausationRef, FaultInjector, HlcPoint, InjectionPoint, Store,
    StoreConfig, StoreError,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

const CHAOS_SCOPE: &str = "scope:frontier-chaos";

struct PanicAtInjector {
    target: InjectionPoint,
    fired: Arc<AtomicBool>,
}

impl PanicAtInjector {
    fn new(target: InjectionPoint) -> (Self, Arc<AtomicBool>) {
        let fired = Arc::new(AtomicBool::new(false));
        (
            Self {
                target,
                fired: Arc::clone(&fired),
            },
            fired,
        )
    }
}

impl FaultInjector for PanicAtInjector {
    fn check(&self, point: InjectionPoint) -> Option<StoreError> {
        if point == self.target && !self.fired.swap(true, Ordering::AcqRel) {
            panic!("PROPERTY: simulated writer panic at {point:?}");
        }
        None
    }
}

fn kind() -> EventKind {
    EventKind::custom(0xF, 0x91)
}

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, CHAOS_SCOPE).expect("valid chaos coordinate")
}

fn point(entry: &batpak::store::IndexEntry) -> HlcPoint {
    HlcPoint {
        wall_ms: entry.wall_ms,
        global_sequence: entry.global_sequence,
    }
}

fn config_with_panic(dir: &TempDir, target: InjectionPoint) -> (StoreConfig, Arc<AtomicBool>) {
    let (injector, fired) = PanicAtInjector::new(target);
    let mut config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    config.fault_injector = Some(Arc::new(injector));
    (config, fired)
}

fn append_baseline(store: &Store, prefix: &str) -> HlcPoint {
    for n in 0..2 {
        let coord = coord(&format!("entity:{prefix}:baseline-{n}"));
        store
            .append(&coord, kind(), &serde_json::json!({"baseline": n}))
            .expect("append baseline event");
    }

    let entries = store.query(&Region::scope(CHAOS_SCOPE));
    assert_eq!(entries.len(), 2);
    point(&entries[1])
}

fn assert_writer_crashed<T>(result: Result<T, StoreError>) {
    match result {
        Err(StoreError::WriterCrashed) => {}
        Err(err) => panic!("PROPERTY: expected writer crash, got {err:?}"),
        Ok(_) => panic!("PROPERTY: panic injector must not let the caller observe success"),
    }
}

fn batch_items(prefix: &str, count: usize) -> Vec<BatchAppendItem> {
    (0..count)
        .map(|n| {
            BatchAppendItem::new(
                coord(&format!("entity:{prefix}:batch-{n}")),
                kind(),
                &serde_json::json!({"batch": n}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct batch item")
        })
        .collect()
}

#[test]
fn writer_panic_at_single_append_published_is_durable_on_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let target_entity = "entity:chaos-published-target";
    let (config, fired) = config_with_panic(
        &dir,
        InjectionPoint::SingleAppendPublished {
            entity: target_entity.to_string(),
        },
    );
    let store = Store::open(config).expect("open store");
    let _ = append_baseline(&store, "published");

    assert_writer_crashed(store.append(
        &coord(target_entity),
        kind(),
        &serde_json::json!({"target": 3}),
    ));
    assert!(fired.load(Ordering::Acquire));
    assert_eq!(
        store.query(&Region::scope(CHAOS_SCOPE)).len(),
        3,
        "PROPERTY: published panic fires after live query visibility"
    );

    let _ = store.close();

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    let entries = reopened.query(&Region::scope(CHAOS_SCOPE));
    assert_eq!(entries.len(), 3);
    let third = point(&entries[2]);
    let frontier = reopened.frontier();
    assert!(frontier.accepted_hlc >= third);
    assert_eq!(frontier.current_visible_hlc, frontier.accepted_hlc);
}

#[test]
#[ignore = "BLOCKS: in-process writer panic leaves the complete unsynced frame recoverable from host page cache; needs VM/block-device torn-tail harness to prove SingleAppendWritten non-durability"]
fn writer_panic_at_single_append_written_is_not_durable_on_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let target_entity = "entity:chaos-written-target";
    let (config, fired) = config_with_panic(
        &dir,
        InjectionPoint::SingleAppendWritten {
            entity: target_entity.to_string(),
        },
    );
    let store = Store::open(config).expect("open store");
    let baseline_hlc = append_baseline(&store, "written");

    assert_writer_crashed(store.append(
        &coord(target_entity),
        kind(),
        &serde_json::json!({"target": 3}),
    ));
    assert!(fired.load(Ordering::Acquire));

    drop(store);

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    let entries = reopened.query(&Region::scope(CHAOS_SCOPE));
    assert_eq!(
        entries.len(),
        2,
        "PROPERTY: SingleAppendWritten panic must not recover the unsynced tail frame"
    );
    assert!(reopened.frontier().accepted_hlc <= baseline_hlc);
}

#[test]
fn writer_panic_at_batch_commit_written_before_fsync() {
    let dir = TempDir::new().expect("temp dir");
    let (config, fired) =
        config_with_panic(&dir, InjectionPoint::BatchCommitWritten { batch_id: 3 });
    let store = Store::open(config).expect("open store");
    let _ = append_baseline(&store, "batch");

    assert_writer_crashed(store.append_batch(batch_items("batch", 2)));
    assert!(fired.load(Ordering::Acquire));

    drop(store);

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    let entries = reopened.query(&Region::scope(CHAOS_SCOPE));
    assert_eq!(
        entries.len(),
        4,
        "PROPERTY: current in-process panic recovery replays a complete batch whose COMMIT marker reached the host page cache before fsync"
    );
}

#[test]
fn frontier_open_hlc_strictly_advances_across_panic_restart() {
    let dir = TempDir::new().expect("temp dir");
    let target_entity = "entity:chaos-open-target";
    let (config, fired) = config_with_panic(
        &dir,
        InjectionPoint::SingleAppendPublished {
            entity: target_entity.to_string(),
        },
    );
    let store = Store::open(config).expect("open store");
    let max_hlc_before_panic = append_baseline(&store, "open");

    assert_writer_crashed(store.append(
        &coord(target_entity),
        kind(),
        &serde_json::json!({"target": 3}),
    ));
    assert!(fired.load(Ordering::Acquire));

    let _ = store.close();

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    let frontier = reopened.frontier();
    assert!(frontier.accepted_hlc > max_hlc_before_panic);

    let lifecycle_open_hlc = reopened
        .query(&Region::entity("batpak:store"))
        .into_iter()
        .filter(|entry| entry.kind == EventKind::SYSTEM_OPEN_COMPLETED)
        .map(|entry| point(&entry))
        .max()
        .expect("mutable reopen emits SYSTEM_OPEN_COMPLETED");
    assert!(lifecycle_open_hlc > max_hlc_before_panic);
    assert!(frontier.accepted_hlc >= lifecycle_open_hlc);
}
