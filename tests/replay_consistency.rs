//! Replay and checkpoint consistency proofs.
#![allow(clippy::panic)]

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, SyncConfig};
use proptest::prelude::*;
use tempfile::TempDir;

mod common;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Counter {
    count: u32,
}

impl EventSourced for Counter {
    type Input = batpak::prelude::JsonValueInput;

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

#[derive(Clone, Debug)]
struct AppendSpec {
    entity_idx: u8,
    scope_idx: u8,
    category: u8,
    type_id: u16,
    payload: i16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VisibleSummary {
    entity: String,
    scope: String,
    category: u8,
    type_id: u16,
    global_sequence: u64,
    payload: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoreSnapshot {
    global_sequence: u64,
    event_count: u64,
    visible: Vec<VisibleSummary>,
}

fn arb_append_specs() -> impl Strategy<Value = Vec<AppendSpec>> {
    prop::collection::vec(
        (
            0u8..4,
            0u8..3,
            prop_oneof![Just(0x1u8), Just(0x2u8), Just(0xFu8)],
            1u16..8,
            any::<i16>(),
        )
            .prop_map(
                |(entity_idx, scope_idx, category, type_id, payload)| AppendSpec {
                    entity_idx,
                    scope_idx,
                    category,
                    type_id,
                    payload,
                },
            ),
        1..24,
    )
}

fn entity_name(idx: u8) -> String {
    format!("entity:replay:{idx:02}")
}

fn scope_name(idx: u8) -> String {
    format!("scope:replay:{idx:02}")
}

fn event_kind(spec: &AppendSpec) -> EventKind {
    EventKind::custom(spec.category, spec.type_id)
}

fn capture_snapshot<State>(store: &Store<State>) -> StoreSnapshot {
    let stats = store.stats();
    let visible = store
        .query(&Region::all())
        .into_iter()
        .map(|entry| {
            let payload = store
                .get(entry.event_id)
                .expect("visible query result must be readable from disk")
                .event
                .payload;
            VisibleSummary {
                entity: entry.coord.entity().to_owned(),
                scope: entry.coord.scope().to_owned(),
                category: entry.kind.category(),
                type_id: entry.kind.type_id(),
                global_sequence: entry.global_sequence,
                payload,
            }
        })
        .collect();
    StoreSnapshot {
        global_sequence: stats.global_sequence,
        event_count: stats.event_count as u64,
        visible,
    }
}

fn populate_specs(store: &Store, specs: &[AppendSpec]) {
    for spec in specs {
        let coord = Coordinate::new(entity_name(spec.entity_idx), scope_name(spec.scope_idx))
            .expect("generated coordinates must be valid");
        store
            .append(
                &coord,
                event_kind(spec),
                &serde_json::json!({
                    "entity_idx": spec.entity_idx,
                    "scope_idx": spec.scope_idx,
                    "payload": spec.payload,
                }),
            )
            .expect("append");
    }
}

fn add_cancelled_fence_event(store: &Store, tag: &str) {
    let fence = store
        .begin_visibility_fence()
        .expect("begin visibility fence");
    let coord = Coordinate::new(format!("entity:hidden:{tag}"), "scope:hidden").expect("coord");
    let ticket = fence
        .submit(
            &coord,
            EventKind::custom(0xF, 0x77),
            &serde_json::json!({"hidden": true}),
        )
        .expect("submit hidden event");
    drop(fence);
    let err = match ticket.wait() {
        Ok(_) => panic!("PROPERTY: cancelled fence work must not resolve as visible success"),
        Err(err) => err,
    };
    assert!(
        matches!(err, batpak::store::StoreError::VisibilityFenceCancelled),
        "cancelled fence work must surface VisibilityFenceCancelled, got {err:?}"
    );
}

fn seeded_config(dir: &TempDir, enable_checkpoint: bool, enable_mmap_index: bool) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(enable_checkpoint)
        .with_enable_mmap_index(enable_mmap_index)
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1)
}

fn seeded_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..seeded_config(&dir, true, true)
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

proptest! {
    #![proptest_config(common::proptest::cfg(12))]

    #[test]
    fn reopen_paths_match_across_mmap_checkpoint_and_rebuild(specs in arb_append_specs()) {
        let cases = [
            ("mmap", false, true),
            ("checkpoint", true, false),
            ("rebuild", false, false),
        ];

        let mut reopened_snapshots = Vec::new();
        for (label, enable_checkpoint, enable_mmap_index) in cases {
            let dir = TempDir::new().expect("temp dir");
            let store = Store::open(seeded_config(&dir, enable_checkpoint, enable_mmap_index))
                .expect("open seeded store");
            populate_specs(&store, &specs);
            add_cancelled_fence_event(&store, label);
            store.sync().expect("sync");
            let live_snapshot = capture_snapshot(&store);
            store.close().expect("close seeded store");

            let reopened = Store::open(seeded_config(&dir, enable_checkpoint, enable_mmap_index))
                .expect("reopen store");
            let reopened_snapshot = capture_snapshot(&reopened);
            assert_eq!(
                reopened_snapshot, live_snapshot,
                "PROPERTY: reopening through {label} must preserve the same visible truth as the live store, including cancelled-fence invisibility."
            );
            reopened_snapshots.push((label, reopened_snapshot));
            reopened.close().expect("close reopened store");
        }

        let (baseline_label, baseline) = reopened_snapshots
            .first()
            .expect("at least one reopen path must be tested");
        for (label, snapshot) in reopened_snapshots.iter().skip(1) {
            assert_eq!(
                snapshot, baseline,
                "PROPERTY: mmap, checkpoint, and full segment rebuild cold-start paths must agree exactly.\n\
                 baseline={baseline_label}, candidate={label}."
            );
        }
    }
}
