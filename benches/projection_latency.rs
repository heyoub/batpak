//! Projection replay and cache benchmarks.
//!
//! Groups:
//!   projection_replay        - NoCache: first pass after open vs warm replay
//!   projection_cache_native  - NativeCache: cache hit vs cache miss

mod common;

use batpak::prelude::*;
use batpak::store::{Freshness, Store, StoreConfig};
use common::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use tempfile::TempDir;

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct Counter {
    count: u64,
}

impl EventSourced<serde_json::Value> for Counter {
    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
}

fn bench_projection_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("projection_replay");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    let fixture_dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: fixture_dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});
    for _ in 0..1_000 {
        store.append(&coord, kind, &payload).expect("append");
    }
    store.sync().expect("sync");
    store.close().expect("close");

    group.bench_function("replay_first_pass", |b| {
        b.iter_batched(
            || {
                let config = StoreConfig {
                    data_dir: fixture_dir.path().to_path_buf(),
                    ..StoreConfig::new("")
                };
                Store::open(config).expect("reopen populated store")
            },
            |store| {
                let _: Option<Counter> = store
                    .project("bench:entity", &Freshness::Consistent)
                    .expect("project");
                store.close().expect("close");
            },
            BatchSize::SmallInput,
        );
    });

    let config = StoreConfig {
        data_dir: fixture_dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let _: Option<Counter> = store
        .project("bench:entity", &Freshness::Consistent)
        .expect("warm replay path");

    apply_profile(&mut group, BenchProfile::QuickWarm);
    group.bench_function("replay_warm", |b| {
        b.iter(|| {
            let _: Option<Counter> = store
                .project("bench:entity", &Freshness::Consistent)
                .expect("project");
        });
    });

    group.finish();
    store.close().expect("close");
}

fn populate_projection_fixture(store: &Store, entity: &str, events: u64) {
    let coord = Coordinate::new(entity, "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});
    for _ in 0..events {
        store.append(&coord, kind, &payload).expect("append");
    }
}

fn bench_projection_caches(c: &mut Criterion) {
    let mut group = c.benchmark_group("projection_cache_native");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().join("data"),
        ..StoreConfig::new("")
    };
    let store = Store::open_with_native_cache(config, dir.path().join("cache"))
        .expect("open store with native cache");
    populate_projection_fixture(&store, "bench:entity", 1_000);
    let miss_entities: Vec<String> = (0..64).map(|i| format!("bench:miss:{i}")).collect();
    for entity in &miss_entities {
        populate_projection_fixture(&store, entity, 1_000);
    }
    store.sync().expect("sync");
    let _: Option<Counter> = store
        .project("bench:entity", &Freshness::Consistent)
        .expect("warm native cache");

    group.bench_function("cache_hit", |b| {
        b.iter(|| {
            let _: Option<Counter> = store
                .project("bench:entity", &Freshness::Consistent)
                .expect("project");
        });
    });

    let mut miss_index = 0usize;
    group.bench_function("cache_miss", |b| {
        b.iter(|| {
            let entity = &miss_entities[miss_index % miss_entities.len()];
            miss_index += 1;
            let _: Option<Counter> = store
                .project(entity, &Freshness::Consistent)
                .expect("project miss");
        });
    });

    group.finish();
    store.close().expect("close");
}

criterion_group!(benches, bench_projection_replay, bench_projection_caches);
criterion_main!(benches);
