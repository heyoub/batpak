// justifies: INV-TEST-PANIC-AS-ASSERTION; this frontier bootstrap harness uses panic! through assert macros for crisp invariant failures.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]

//! PROVES:
//!   - Step-1 frontier scaffolding compiles and exposes a coherent dangerous snapshot.
//!   - Immediately after mutable `Store::open`, the lifecycle open event seeds
//!     accepted, written, durable, visible, and emitted to the same HLC point.
//!   - Restart bootstrap is monotonic across mutable and read-only reopen.
//!
//! CATCHES: missing handle plumbing, missing public accessor coverage, or a
//! bootstrap snapshot that does not reflect `SYSTEM_OPEN_COMPLETED`.
//!
//! SEEDED: deterministic tempdir-based open.

use batpak::prelude::{
    Coordinate, Event, EventKind, EventSourced, Freshness, JsonValueInput, Region,
};
use batpak::store::{
    CountdownAction, CountdownInjector, FrontierView, HlcPoint, InjectionPoint, ReadOnly, Store,
    StoreConfig, StoreError, WatermarkSnapshot,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use tempfile::TempDir;

const FRONTIER_FAULT_ENTITY: &str = "entity:frontier-fault";

fn kind() -> EventKind {
    EventKind::custom(0xF, 0x90)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FrontierProjection {
    count: usize,
}

impl EventSourced for FrontierProjection {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 0x90)];
        &KINDS
    }
}

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, "scope:test").expect("coord")
}

fn point(entry: &batpak::store::IndexEntry) -> HlcPoint {
    HlcPoint {
        wall_ms: entry.wall_ms,
        global_sequence: entry.global_sequence,
    }
}

fn fixed_clock_config(dir: &TempDir, now_us: i64) -> StoreConfig {
    StoreConfig::new(dir.path()).with_clock(Some(Arc::new(move || now_us)))
}

fn lifecycle_open_count<State>(store: &Store<State>) -> usize {
    store
        .query(&Region::entity("batpak:store"))
        .into_iter()
        .filter(|entry| entry.kind == EventKind::SYSTEM_OPEN_COMPLETED)
        .count()
}

fn lifecycle_close_entries<State>(store: &Store<State>) -> Vec<batpak::store::IndexEntry> {
    store
        .query(&Region::entity("batpak:store"))
        .into_iter()
        .filter(|entry| entry.kind == EventKind::SYSTEM_CLOSE_COMPLETED)
        .collect()
}

fn config_with_fault(
    dir: &TempDir,
    filter: impl Fn(&InjectionPoint) -> bool + Send + Sync + 'static,
) -> StoreConfig {
    let mut config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    config.fault_injector = Some(Arc::new(
        CountdownInjector::new(1, CountdownAction::Fail("single append fault")).with_filter(filter),
    ));
    config
}

fn assert_fault_injected(result: Result<batpak::store::AppendReceipt, StoreError>) {
    match result {
        Ok(_) => panic!("PROPERTY: append must surface the injected error, not a receipt"),
        Err(err) => assert!(
            matches!(err, StoreError::FaultInjected(ref message) if message.contains("single append fault")),
            "PROPERTY: expected injected fault, got {err:?}"
        ),
    }
}

#[test]
fn bootstrap_watermark_snapshot_matches_lifecycle_open_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");

    let snapshot: WatermarkSnapshot = store.dangerous_watermark_snapshot();
    let frontier: FrontierView = store.diagnostics().frontier;
    let open_hlc = snapshot.durable_hlc;

    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(open_hlc.global_sequence, 0);
    assert_eq!(snapshot.accepted_hlc, open_hlc);
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, open_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);

    assert_eq!(frontier.durable_hlc, open_hlc);
    assert_eq!(frontier.current_visible_hlc, open_hlc);
    assert_eq!(frontier.visible_minus_durable_seq, 0);
    assert_eq!(frontier.oldest_pending_write_age_ms, None);
}

#[test]
fn open_after_close_advances_open_hlc_past_max_pre_close() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-reopen");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-reopen"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    let snapshot = reopened.dangerous_watermark_snapshot();
    let open_hlc = snapshot.accepted_hlc;

    assert!(
        open_hlc > max_hlc_before_close,
        "PROPERTY: mutable reopen lifecycle HLC must advance past pre-close max; open={open_hlc:?}, max={max_hlc_before_close:?}"
    );
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, open_hlc);
}

#[test]
fn read_only_reopen_does_not_emit_lifecycle_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-readonly-lifecycle");

    let (max_hlc_before_read_only, lifecycle_count_before_read_only) = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-readonly-lifecycle"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        let lifecycle_count = lifecycle_open_count(&store);
        assert_eq!(lifecycle_count, 1);
        store.close().expect("close");
        (max_hlc, lifecycle_count)
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let snapshot = read_only.dangerous_watermark_snapshot();

    assert_eq!(
        lifecycle_open_count(&read_only),
        lifecycle_count_before_read_only,
        "PROPERTY: read-only open must not append SYSTEM_OPEN_COMPLETED"
    );
    assert!(snapshot.accepted_hlc >= max_hlc_before_read_only);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.visible_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.applied_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);
}

#[test]
fn explicit_close_emits_system_close_completed_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-close-event");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-close-event"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let close_entries = lifecycle_close_entries(&read_only);

    assert_eq!(
        close_entries.len(),
        1,
        "PROPERTY: explicit close must emit exactly one SYSTEM_CLOSE_COMPLETED event"
    );
    assert!(
        point(&close_entries[0]) >= max_hlc_before_close,
        "PROPERTY: close lifecycle HLC must cover all visible events at close; close={:?}, max={max_hlc_before_close:?}",
        point(&close_entries[0])
    );
}

#[test]
fn drop_without_explicit_close_emits_no_close_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-drop-no-close");

    {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
    }

    {
        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        assert!(
            lifecycle_close_entries(&read_only).is_empty(),
            "PROPERTY: Drop must not emit SYSTEM_CLOSE_COMPLETED"
        );
    }

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    assert!(
        reopened.frontier().accepted_hlc > HlcPoint::ORIGIN,
        "PROPERTY: reopen without a close event must still bootstrap from recovered events and wall-time floor"
    );
}

#[test]
fn bootstrap_open_hlc_consumes_recorded_close_hlc() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-close-bootstrap");

    let close_hlc_1 = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        store.close().expect("close");

        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        let close_entries = lifecycle_close_entries(&read_only);
        assert_eq!(close_entries.len(), 1);
        point(&close_entries[0])
    };

    let close_hlc_2 = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
        assert!(
            store.frontier().accepted_hlc >= close_hlc_1,
            "PROPERTY: reopen must consume the recorded close frontier"
        );
        store
            .append(&coord, kind(), &serde_json::json!({"n": 2}))
            .expect("append");
        store.close().expect("close");

        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        let close_entries = lifecycle_close_entries(&read_only);
        assert_eq!(close_entries.len(), 2);
        let first = point(&close_entries[0]);
        let second = point(&close_entries[1]);
        assert!(
            second >= first,
            "PROPERTY: repeated graceful closes must advance monotonically; first={first:?}, second={second:?}"
        );
        second
    };

    let third = Store::open(StoreConfig::new(dir.path())).expect("third open");
    let open_hlc = third.frontier().accepted_hlc;
    assert!(open_hlc >= close_hlc_1);
    assert!(open_hlc >= close_hlc_2);
}

#[test]
#[ignore = "BLOCKS: requires segment-forging helper from tests/chaos/ (Phase 1B); corruption shape is close_hlc_2 < close_hlc_1, not multiple close events"]
fn close_hlc_monotonicity_violation_surfaces_invariant_violation() {
    panic!(
        "PROPERTY: a later SYSTEM_CLOSE_COMPLETED with close_hlc below the previous close_hlc must surface StoreError::InvariantViolation"
    );
}

#[test]
fn bootstrap_with_clock_skew_preserves_monotonicity() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-clock-skew");

    let max_hlc_before_close = {
        let store = Store::open(fixed_clock_config(&dir, 9_000_000_000)).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-clock-skew"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let reopened = Store::open(fixed_clock_config(&dir, 1_000_000)).expect("reopen store");
    let open_hlc = reopened.dangerous_watermark_snapshot().accepted_hlc;

    assert!(
        open_hlc > max_hlc_before_close,
        "PROPERTY: reopen must remain monotonic even when the configured clock moves backward; open={open_hlc:?}, max={max_hlc_before_close:?}"
    );
}

#[test]
fn empty_store_open_starts_with_lifecycle_frontier_then_append_advances() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let open_hlc = store.dangerous_watermark_snapshot().accepted_hlc;
    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(open_hlc.global_sequence, 0);

    let coord = coord("entity:frontier-empty-advance");
    store
        .append(&coord, kind(), &serde_json::json!({"n": 1}))
        .expect("append");
    let snapshot = store.dangerous_watermark_snapshot();
    assert!(snapshot.accepted_hlc > open_hlc);
}

#[test]
fn single_append_cadence_gt_1_visible_exceeds_durable_frontier() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord("entity:frontier");

    let receipt = store
        .append(&coord, kind(), &serde_json::json!({"n": 1}))
        .expect("append");

    let visible = store.query(&Region::entity("entity:frontier"));
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].event_id, receipt.event_id);

    let snapshot = store.dangerous_watermark_snapshot();
    let frontier = store.diagnostics().frontier;

    assert!(snapshot.visible_hlc > snapshot.durable_hlc);
    assert!(snapshot.accepted_hlc >= snapshot.written_hlc);
    assert!(snapshot.written_hlc >= snapshot.visible_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.applied_hlc, bootstrap.applied_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.visible_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());

    assert_eq!(frontier.current_visible_hlc, snapshot.visible_hlc);
    assert_eq!(frontier.durable_hlc, snapshot.durable_hlc);
    assert!(frontier.visible_minus_durable_seq > 0);
    assert!(frontier.oldest_pending_write_age_ms.is_some());
}

#[test]
fn explicit_sync_advances_durable_and_clears_pending_write_age() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let coord = coord("entity:frontier-sync");

    store
        .append(&coord, kind(), &serde_json::json!({"n": 1}))
        .expect("append");

    let before_sync = store.dangerous_watermark_snapshot();
    assert!(before_sync.visible_hlc > before_sync.durable_hlc);
    assert!(before_sync.oldest_pending_write_age_ms.is_some());

    store.sync().expect("sync");

    let after_sync = store.dangerous_watermark_snapshot();
    assert_eq!(after_sync.durable_hlc, after_sync.accepted_hlc);
    assert_eq!(after_sync.durable_hlc, after_sync.visible_hlc);
    assert_eq!(after_sync.oldest_pending_write_age_ms, None);
    assert_eq!(
        store.diagnostics().frontier.oldest_pending_write_age_ms,
        None
    );
}

#[test]
fn frontier_api_is_public_and_returns_consistent_view() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1);
    let store = Store::open(config).expect("open store");
    let coord = coord("entity:frontier-api");

    for n in 0..5 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }
    store.sync().expect("sync");

    let frontier = store.frontier();
    assert!(frontier.accepted_hlc > HlcPoint::ORIGIN);
    assert_eq!(frontier.accepted_hlc, frontier.written_hlc);
    assert_eq!(frontier.accepted_hlc, frontier.durable_hlc);
    assert_eq!(frontier.accepted_hlc, frontier.current_visible_hlc);
    assert_eq!(frontier.emitted_hlc, frontier.current_visible_hlc);
    assert!(frontier.current_visible_hlc >= frontier.applied_hlc);
    assert_eq!(frontier.visible_minus_durable_seq, 0);
    assert_eq!(frontier.oldest_pending_write_age_ms, None);
    assert_eq!(store.diagnostics().frontier, frontier);
}

#[test]
fn frontier_visible_minus_durable_seq_is_positive_under_cadence_gt_1() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let coord = coord("entity:frontier-api-gap");

    for n in 0..10 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let before_sync = store.frontier();
    assert!(before_sync.current_visible_hlc > before_sync.durable_hlc);
    assert!(before_sync.visible_minus_durable_seq > 0);
    assert!(before_sync.oldest_pending_write_age_ms.is_some());

    store.sync().expect("sync");

    let after_sync = store.frontier();
    assert_eq!(after_sync.current_visible_hlc, after_sync.durable_hlc);
    assert_eq!(after_sync.visible_minus_durable_seq, 0);
    assert_eq!(after_sync.oldest_pending_write_age_ms, None);
}

#[test]
fn concurrent_snapshot_never_observes_torn_emitted_below_visible() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Arc::new(Store::open(config).expect("open store"));
    let coord = coord("entity:frontier-concurrent");
    let start = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));

    let observer_store = Arc::clone(&store);
    let observer_start = Arc::clone(&start);
    let observer_done = Arc::clone(&done);
    let observer = thread::Builder::new()
        .name("frontier-snapshot-observer".to_string())
        .spawn(move || {
            observer_start.wait();
            let mut snapshots = Vec::new();
            while !observer_done.load(Ordering::Acquire) {
                let frontier = observer_store.frontier();
                if frontier.current_visible_hlc > HlcPoint::ORIGIN {
                    snapshots.push(frontier);
                }
                thread::yield_now();
            }
            for _ in 0..256 {
                let frontier = observer_store.frontier();
                if frontier.current_visible_hlc > HlcPoint::ORIGIN {
                    snapshots.push(frontier);
                }
            }
            snapshots
        })
        .expect("spawn frontier observer");

    start.wait();
    for n in 0..300 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
        if n % 8 == 0 {
            thread::yield_now();
        }
    }
    done.store(true, Ordering::Release);

    let snapshots = observer.join().expect("observer thread");
    assert!(
        !snapshots.is_empty(),
        "PROPERTY: concurrent observer must collect frontier snapshots"
    );
    for frontier in snapshots {
        assert!(
            frontier.emitted_hlc >= frontier.current_visible_hlc,
            "PROPERTY: emitted must never be observed below visible: {frontier:?}"
        );
        assert!(
            frontier.current_visible_hlc >= frontier.applied_hlc,
            "PROPERTY: applied must never be observed above visible: {frontier:?}"
        );
    }
}

#[test]
fn read_only_open_bootstraps_frontier_from_rebuilt_index() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-readonly");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-readonly"));
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let point = HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        };
        assert!(point > HlcPoint::ORIGIN);
        store.close().expect("close");
        point
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let snapshot = read_only.dangerous_watermark_snapshot();

    assert!(snapshot.accepted_hlc >= max_hlc_before_close);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.visible_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.applied_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);
}

#[test]
fn applied_starts_at_open_hlc_when_no_projections_registered() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let open_hlc = store.dangerous_watermark_snapshot().applied_hlc;
    let coord = coord("entity:frontier-applied-none");

    for n in 0..3 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let snapshot = store.dangerous_watermark_snapshot();
    assert_eq!(
        snapshot.applied_hlc, open_hlc,
        "PROPERTY: without registered projections, applied remains at the bootstrap frontier"
    );
    assert_ne!(snapshot.applied_hlc, snapshot.emitted_hlc);
}

#[test]
fn applied_advances_with_single_projection() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = coord("entity:frontier-applied-one");
    store.dangerous_register_projection_for::<FrontierProjection>("entity:frontier-applied-one");

    for n in 0..3 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let projected = store
        .project::<FrontierProjection>("entity:frontier-applied-one", &Freshness::Consistent)
        .expect("project")
        .expect("projection state");
    assert_eq!(projected.count, 3);

    let snapshot = store.dangerous_watermark_snapshot();
    let frontier = store.diagnostics().frontier;
    assert_eq!(snapshot.applied_hlc, snapshot.emitted_hlc);
    assert_eq!(frontier.applied_hlc, snapshot.applied_hlc);
}

#[test]
fn applied_is_min_across_two_projections() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = coord("entity:frontier-applied-two");

    for n in 0..5 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let entries = store.query(&Region::entity("entity:frontier-applied-two"));
    assert_eq!(entries.len(), 5);
    let second_event = point(&entries[1]);
    let fifth_event = point(&entries[4]);

    store.dangerous_register_projection("frontier:p1");
    store.dangerous_register_projection("frontier:p2");
    store.dangerous_notify_projection_applied("frontier:p1", fifth_event);
    store.dangerous_notify_projection_applied("frontier:p2", second_event);

    let snapshot = store.dangerous_watermark_snapshot();
    assert_eq!(snapshot.applied_hlc, second_event);
    assert_ne!(snapshot.applied_hlc, fifth_event);

    store.dangerous_notify_projection_applied("frontier:p2", fifth_event);
    assert_eq!(
        store.dangerous_watermark_snapshot().applied_hlc,
        fifth_event
    );
}

#[test]
fn applied_unregister_recomputes_from_remaining_projection_progress() {
    fn run_case(unregister_fast_first: bool) {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = coord(if unregister_fast_first {
            "entity:frontier-unregister-fast"
        } else {
            "entity:frontier-unregister-slow"
        });

        for n in 0..5 {
            store
                .append(&coord, kind(), &serde_json::json!({"n": n}))
                .expect("append");
        }

        let entries = store.query(&Region::entity(coord.entity()));
        assert_eq!(entries.len(), 5);
        let slow = point(&entries[1]);
        let fast = point(&entries[4]);

        store.dangerous_register_projection("frontier:fast");
        store.dangerous_register_projection("frontier:slow");
        store.dangerous_notify_projection_applied("frontier:fast", fast);
        store.dangerous_notify_projection_applied("frontier:slow", slow);
        assert_eq!(store.dangerous_watermark_snapshot().applied_hlc, slow);

        if unregister_fast_first {
            store.dangerous_unregister_projection("frontier:fast");
            assert_eq!(store.dangerous_watermark_snapshot().applied_hlc, slow);
        } else {
            store.dangerous_unregister_projection("frontier:slow");
            assert_eq!(store.dangerous_watermark_snapshot().applied_hlc, fast);
        }
    }

    run_case(true);
    run_case(false);
}

#[test]
fn single_append_start_fault_fires_before_watermarks_advance() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_with_fault(&dir, |point| {
        matches!(
            point,
            InjectionPoint::SingleAppendStart { entity }
                if entity == FRONTIER_FAULT_ENTITY
        )
    });
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord(FRONTIER_FAULT_ENTITY);

    assert_fault_injected(store.append(&coord, kind(), &serde_json::json!({"n": 1})));

    let snapshot = store.dangerous_watermark_snapshot();
    assert_eq!(snapshot, bootstrap);
    assert!(store
        .query(&Region::entity(FRONTIER_FAULT_ENTITY))
        .is_empty());
}

#[test]
fn single_append_written_fault_fires_after_written_before_visible() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_with_fault(&dir, |point| {
        matches!(
            point,
            InjectionPoint::SingleAppendWritten { entity }
                if entity == FRONTIER_FAULT_ENTITY
        )
    });
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord(FRONTIER_FAULT_ENTITY);

    assert_fault_injected(store.append(&coord, kind(), &serde_json::json!({"n": 1})));

    let snapshot = store.dangerous_watermark_snapshot();
    assert!(snapshot.accepted_hlc > bootstrap.accepted_hlc);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.visible_hlc, bootstrap.visible_hlc);
    assert_eq!(snapshot.emitted_hlc, bootstrap.emitted_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());
    assert!(store
        .query(&Region::entity(FRONTIER_FAULT_ENTITY))
        .is_empty());
}

#[test]
fn single_append_published_fault_fires_after_visibility_before_receipt() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_with_fault(&dir, |point| {
        matches!(
            point,
            InjectionPoint::SingleAppendPublished { entity }
                if entity == FRONTIER_FAULT_ENTITY
        )
    });
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord(FRONTIER_FAULT_ENTITY);

    assert_fault_injected(store.append(&coord, kind(), &serde_json::json!({"n": 1})));

    let visible = store.query(&Region::entity(FRONTIER_FAULT_ENTITY));
    assert_eq!(
        visible.len(),
        1,
        "PROPERTY: published injection fires after query visibility"
    );

    let snapshot = store.dangerous_watermark_snapshot();
    assert!(snapshot.visible_hlc > snapshot.durable_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.visible_hlc);
    assert_eq!(snapshot.written_hlc, snapshot.visible_hlc);
    assert_eq!(snapshot.accepted_hlc, snapshot.visible_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());
}
