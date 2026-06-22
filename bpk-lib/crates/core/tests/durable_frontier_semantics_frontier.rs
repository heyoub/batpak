#![cfg(feature = "dangerous-test-hooks")]
//! PROVES: INV-FRONTIER-MONOTONIC, INV-FRONTIER-ORDERING, INV-FRONTIER-TORN-FREE,
//! INV-FRONTIER-OPEN-MONOTONIC, INV-FRONTIER-APPLIED-MIN, and
//! INV-FRONTIER-FAULT-ORDINALS for the live (post-bootstrap) watermark. Under
//! cadence > 1, visible runs ahead of durable and `sync` collapses the gap,
//! clearing the oldest-pending-write age. The public
//! `frontier()`/`diagnostics().frontier` views stay consistent and torn-free even
//! under a concurrent observer. `applied` tracks the minimum projection progress,
//! advancing on apply and recomputing on unregister. Single-append fault ordinals
//! fire at exactly their watermark stage.
//!
//! CATCHES: visible/durable ordering drift, torn frontier reads, applied-min
//! regressions on (un)register, or fault-injection ordinals that fire at the
//! wrong watermark stage.
//!
//! SEEDED: deterministic tempdir-based open with fixed sync cadence and
//! countdown fault injection.

use batpak_testkit::durable_frontier_semantics as dfs_support;

use batpak::prelude::{Event, EventKind, EventSourced, Freshness, JsonValueInput, Region};
use batpak::store::{
    CountdownAction, CountdownInjector, HlcPoint, InjectionPoint, Store, StoreConfig, StoreError,
};
use dfs_support::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use tempfile::TempDir;

const FRONTIER_FAULT_ENTITY: &str = "entity:frontier-fault";

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

fn config_with_fault(
    dir: &TempDir,
    filter: impl Fn(&InjectionPoint) -> bool + Send + Sync + 'static,
) -> StoreConfig {
    let injector =
        CountdownInjector::new(1, CountdownAction::Fail("single append fault")).with_filter(filter);
    StoreConfig::new(dir.path())
        .with_sync_every_n_events(1000)
        .with_fault_injector(Some(Arc::new(injector)))
}

fn assert_fault_injected(result: Result<batpak::store::AppendReceipt, StoreError>) {
    let err = result
        .map(|_| ())
        .expect_err("PROPERTY: append must surface the injected error, not a receipt");
    assert!(
        matches!(err, StoreError::FaultInjected(ref message) if message.contains("single append fault")),
        "PROPERTY: expected injected fault, got {err:?}"
    );
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
    assert_eq!(visible[0].event_id(), receipt.event_id);

    let snapshot = store.dangerous_watermark_snapshot();
    let frontier = store.diagnostics().frontier;

    assert!(snapshot.visible_hlc > snapshot.durable_hlc);
    assert!(snapshot.accepted_hlc >= snapshot.written_hlc);
    assert!(snapshot.written_hlc >= snapshot.visible_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.applied_hlc, bootstrap.applied_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.visible_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());

    assert_eq!(frontier.visible_hlc, snapshot.visible_hlc);
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
    assert_eq!(frontier.accepted_hlc, frontier.visible_hlc);
    assert_eq!(frontier.emitted_hlc, frontier.visible_hlc);
    assert!(frontier.visible_hlc >= frontier.applied_hlc);
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
    assert!(before_sync.visible_hlc > before_sync.durable_hlc);
    assert!(before_sync.visible_minus_durable_seq > 0);
    assert!(before_sync.oldest_pending_write_age_ms.is_some());

    store.sync().expect("sync");

    let after_sync = store.frontier();
    assert_eq!(after_sync.visible_hlc, after_sync.durable_hlc);
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
    // Intentional: barrier waits coordinate exactly one observer and one writer.
    let observer = thread::Builder::new()
        .name("frontier-snapshot-observer".to_string())
        .spawn(move || {
            observer_start.wait();
            let mut snapshots = Vec::new();
            while !observer_done.load(Ordering::Acquire) {
                let frontier = observer_store.frontier();
                if frontier.visible_hlc > HlcPoint::ORIGIN {
                    snapshots.push(frontier);
                }
                thread::yield_now();
            }
            for _ in 0..256 {
                let frontier = observer_store.frontier();
                if frontier.visible_hlc > HlcPoint::ORIGIN {
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
            frontier.emitted_hlc >= frontier.visible_hlc,
            "PROPERTY: emitted must never be observed below visible: {frontier:?}"
        );
        assert!(
            frontier.visible_hlc >= frontier.applied_hlc,
            "PROPERTY: applied must never be observed above visible: {frontier:?}"
        );
    }
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
