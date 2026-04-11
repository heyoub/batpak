//! Replay and checkpoint consistency proofs.
//! [SPEC:tests/replay_consistency.rs]

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, SyncConfig};
use tempfile::TempDir;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Counter {
    count: u32,
}

impl EventSourced<serde_json::Value> for Counter {
    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        Some(Self {
            count: u32::try_from(events.len()).expect("small test corpus"),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[]
    }
}

fn seeded_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("entity:replay", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    for n in 0..6 {
        store
            .append(&coord, kind, &serde_json::json!({"n": n}))
            .expect("append");
    }
    store.sync().expect("sync");
    (store, dir)
}

#[test]
fn cold_start_replay_matches_live_projection() {
    let (store, dir) = seeded_store();
    let live: Option<Counter> = store
        .project("entity:replay", &batpak::store::Freshness::Consistent)
        .expect("live project");
    assert_eq!(live, Some(Counter { count: 6 }));
    store.close().expect("close");

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen");
    let replayed: Option<Counter> = reopened
        .project("entity:replay", &batpak::store::Freshness::Consistent)
        .expect("replay project");
    assert_eq!(
        replayed, live,
        "Cold-start replay must match the live store projection exactly."
    );
    reopened.close().expect("close reopened");
}

#[test]
fn snapshot_checkpoint_matches_source_projection() {
    let (store, _dir) = seeded_store();
    let live_stats = store.stats();
    let live: Option<Counter> = store
        .project("entity:replay", &batpak::store::Freshness::Consistent)
        .expect("project");

    let snapshot_dir = TempDir::new().expect("snapshot");
    store.snapshot(snapshot_dir.path()).expect("snapshot");

    let reopened = Store::open(StoreConfig::new(snapshot_dir.path())).expect("open snapshot");
    let snap_stats = reopened.stats();
    let snap_projection: Option<Counter> = reopened
        .project("entity:replay", &batpak::store::Freshness::Consistent)
        .expect("snapshot project");

    assert_eq!(snap_stats.event_count, live_stats.event_count);
    assert_eq!(
        snap_stats.global_sequence, live_stats.global_sequence,
        "PROPERTY: a snapshot reopen must produce the same global_sequence \
         as the source store. Drift here means the rebuild path is using a \
         different sequence-allocation scheme than the live writer. \
         Investigate: ReplayCursor::commit / synthesize_next empty-cursor \
         handling, src/store/index.rs."
    );
    assert_eq!(
        snap_projection, live,
        "Snapshot reopen must preserve the same projection output as the source store."
    );

    reopened.close().expect("close");
    store.close().expect("close source");
}
