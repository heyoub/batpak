// justifies: benches/projection_latency.rs uses panic only in benchmark-only reopen helpers to fail fast on violated lock-release assumptions proved in tests/store_locking.rs.
#![allow(clippy::panic)]

//! Projection replay and cache benchmarks with explicit cold/hot lanes.

use batpak::prelude::*;
use batpak::store::{Freshness, IndexTopology, Store, StoreConfig, StoreError};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, Criterion};
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct Counter {
    count: u64,
}

impl EventSourced for Counter {
    type Input = batpak::prelude::JsonValueInput;

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

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct CounterRaw {
    count: u64,
}

impl EventSourced for CounterRaw {
    type Input = batpak::prelude::RawMsgpackInput;

    fn apply_event(&mut self, _event: &Event<Vec<u8>>) {
        self.count += 1;
    }

    fn from_events(events: &[Event<Vec<u8>>]) -> Option<Self> {
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

fn populate_projection_fixture(store: &Store, entity: &str, events: u64) {
    let coord = Coordinate::new(entity, "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});
    for _ in 0..events {
        store.append(&coord, kind, &payload).expect("append");
    }
}

fn reopen_with_retry<T, F>(label: &str, mut open: F) -> T
where
    F: FnMut() -> Result<T, StoreError>,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match open() {
            Ok(value) => return value,
            Err(err @ StoreError::StoreLocked { .. }) => {
                if Instant::now() >= deadline {
                    panic!("{label}: lock did not clear before deadline: {err:?}");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => panic!("{label}: unexpected reopen failure: {err:?}"),
        }
    }
}

fn bench_project_only<T, O, P>(b: &mut criterion::Bencher<'_>, mut open: O, mut project: P)
where
    O: FnMut() -> T,
    P: FnMut(&T),
    T: ProjectionBenchStore,
{
    b.iter_custom(|iters| {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            let store = open();
            let start = Instant::now();
            project(&store);
            total += start.elapsed();
            store.close_for_bench();
        }
        total
    });
}

fn bench_project_with_close<T, O, P>(b: &mut criterion::Bencher<'_>, mut open: O, mut project: P)
where
    O: FnMut() -> T,
    P: FnMut(&T),
    T: ProjectionBenchStore,
{
    b.iter_custom(|iters| {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            let store = open();
            let start = Instant::now();
            project(&store);
            store.close_for_bench();
            total += start.elapsed();
        }
        total
    });
}

trait ProjectionBenchStore {
    fn close_for_bench(self);
}

impl ProjectionBenchStore for Store {
    fn close_for_bench(self) {
        self.close().expect("close projection benchmark store");
    }
}

fn bench_projection_lanes(c: &mut Criterion) {
    let mut group = c.benchmark_group("projection_lanes");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    let fixture_dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: fixture_dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    populate_projection_fixture(&store, "bench:first-pass", 1_000);
    store.sync().expect("sync");
    store.close().expect("close");

    // project-only: measures ONLY the projection call, not close().
    // We explicitly close after timing each iteration so the lifetime-held
    // store lock clears deterministically before the next reopen.
    group.bench_function("projection_first_pass", |b| {
        bench_project_only(
            b,
            || {
                reopen_with_retry("projection_first_pass reopen", || {
                    let config = StoreConfig {
                        data_dir: fixture_dir.path().to_path_buf(),
                        ..StoreConfig::new("")
                    };
                    Store::open(config)
                })
            },
            |store| {
                let _: Option<Counter> = store
                    .project("bench:first-pass", &Freshness::Consistent)
                    .expect("project");
            },
        );
    });

    group.bench_function("projection_first_pass_raw", |b| {
        bench_project_only(
            b,
            || {
                reopen_with_retry("projection_first_pass_raw reopen", || {
                    let config = StoreConfig {
                        data_dir: fixture_dir.path().to_path_buf(),
                        ..StoreConfig::new("")
                    };
                    Store::open(config)
                })
            },
            |store| {
                let _: Option<CounterRaw> = store
                    .project("bench:first-pass", &Freshness::Consistent)
                    .expect("project raw");
            },
        );
    });

    // project + close: measures the full lifecycle including artifact writes.
    // Kept as a separate lane so "projection cost" and "lifecycle cost" are
    // never conflated.
    group.bench_function("projection_first_pass_with_close", |b| {
        bench_project_with_close(
            b,
            || {
                reopen_with_retry("projection_first_pass_with_close reopen", || {
                    let config = StoreConfig {
                        data_dir: fixture_dir.path().to_path_buf(),
                        ..StoreConfig::new("")
                    };
                    Store::open(config)
                })
            },
            |store| {
                let _: Option<Counter> = store
                    .project("bench:first-pass", &Freshness::Consistent)
                    .expect("project");
            },
        );
    });

    let replay_dir = TempDir::new().expect("create replay temp dir");
    let replay_config = StoreConfig::new(replay_dir.path())
        .with_index_topology(IndexTopology::aos())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false);
    let replay_store = Store::open(replay_config).expect("open replay store");
    let replay_entities: Vec<String> = (0..256).map(|i| format!("bench:replay-only:{i}")).collect();
    for entity in &replay_entities {
        populate_projection_fixture(&replay_store, entity, 64);
    }
    replay_store.sync().expect("sync replay-only store");

    let mut replay_index = 0usize;
    group.bench_function("projection_replay_only", |b| {
        b.iter(|| {
            let entity = &replay_entities[replay_index % replay_entities.len()];
            replay_index += 1;
            let _: Option<Counter> = replay_store
                .project(entity, &Freshness::Consistent)
                .expect("project replay only");
        });
    });

    let mut replay_raw_index = 0usize;
    group.bench_function("projection_replay_only_raw", |b| {
        b.iter(|| {
            let entity = &replay_entities[replay_raw_index % replay_entities.len()];
            replay_raw_index += 1;
            let _: Option<CounterRaw> = replay_store
                .project(entity, &Freshness::Consistent)
                .expect("project raw replay only");
        });
    });

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

    group.bench_function("projection_cache_hit", |b| {
        b.iter(|| {
            let _: Option<Counter> = store
                .project("bench:entity", &Freshness::Consistent)
                .expect("project");
        });
    });

    group.bench_function("projection_cache_hit_raw", |b| {
        b.iter(|| {
            let _: Option<CounterRaw> = store
                .project("bench:entity", &Freshness::Consistent)
                .expect("project raw");
        });
    });

    let mut miss_index = 0usize;
    group.bench_function("projection_cache_miss", |b| {
        b.iter(|| {
            let entity = &miss_entities[miss_index % miss_entities.len()];
            miss_index += 1;
            let _: Option<Counter> = store
                .project(entity, &Freshness::Consistent)
                .expect("project miss");
        });
    });

    let mut miss_raw_index = 0usize;
    group.bench_function("projection_cache_miss_raw", |b| {
        b.iter(|| {
            let entity = &miss_entities[miss_raw_index % miss_entities.len()];
            miss_raw_index += 1;
            let _: Option<CounterRaw> = store
                .project(entity, &Freshness::Consistent)
                .expect("project raw miss");
        });
    });

    group.finish();
    replay_store.close().expect("close replay-only store");
    store.close().expect("close");
}

fn bench_projection_strategy_lanes(c: &mut Criterion) {
    let mut group = c.benchmark_group("projection_strategy");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    // Cold path with NoCache (DirectReplay strategy)
    let nocache_dir = TempDir::new().expect("temp dir");
    let nocache_config = StoreConfig::new(nocache_dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false);
    let nocache_store = Store::open(nocache_config).expect("open nocache");
    populate_projection_fixture(&nocache_store, "bench:cold", 1_000);
    nocache_store.close().expect("close nocache");

    group.bench_function("cold_nocache", |b| {
        bench_project_only(
            b,
            || {
                reopen_with_retry("reopen nocache", || {
                    Store::open(
                        StoreConfig::new(nocache_dir.path())
                            .with_enable_checkpoint(false)
                            .with_enable_mmap_index(false),
                    )
                })
            },
            |store| {
                let _: Option<Counter> = store
                    .project("bench:cold", &Freshness::Consistent)
                    .expect("project");
            },
        );
    });

    // Cold path with NativeCache (ExternalCacheThenReplay strategy, guaranteed miss)
    let native_dir = TempDir::new().expect("temp dir");
    let native_config = StoreConfig::new(native_dir.path().join("data"))
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false);
    let native_store = Store::open(native_config).expect("open native");
    populate_projection_fixture(&native_store, "bench:cold-native", 1_000);
    native_store.close().expect("close native");

    group.bench_function("cold_native_cache", |b| {
        bench_project_only(
            b,
            || {
                let cache_path = native_dir
                    .path()
                    .join(format!("cache_{}", fastrand::u64(..)));
                reopen_with_retry("reopen native", || {
                    Store::open_with_native_cache(
                        StoreConfig::new(native_dir.path().join("data"))
                            .with_enable_checkpoint(false)
                            .with_enable_mmap_index(false),
                        cache_path.clone(),
                    )
                })
            },
            |store| {
                let _: Option<Counter> = store
                    .project("bench:cold-native", &Freshness::Consistent)
                    .expect("project");
            },
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_projection_lanes,
    bench_projection_strategy_lanes
);
criterion_main!(benches);
