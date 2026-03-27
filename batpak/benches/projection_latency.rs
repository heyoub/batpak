//! Benchmark: projection replay latency for EventSourced.
//! [SPEC:benches/projection_latency.rs]
//!
//! NOTE: Store::project() currently always replays from segments (the _cache
//! field is not yet wired into the projection path). Both benchmarks measure
//! segment replay; "replay_cold" is the first read (OS page cache cold),
//! "replay_warm" is after the data is in the OS page cache.
//!
//! When the ProjectionCache is wired into project(), add a "redb_cache_hit"
//! / "redb_cache_miss" benchmark group gated behind #[cfg(feature = "redb")].

use batpak::prelude::*;
use batpak::store::{Freshness, Store, StoreConfig};
use criterion::{criterion_group, criterion_main, Criterion};
use tempfile::TempDir;

/// A minimal EventSourced implementation for benchmarking.
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

    // Setup: populate a store with events for one entity
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
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

    // replay_cold: first projection after store open.
    // OS page cache may or may not have the segment data.
    group.bench_function("replay_cold", |b| {
        b.iter(|| {
            let _: Option<Counter> = store
                .project("bench:entity", &Freshness::Consistent)
                .expect("project");
        });
    });

    // replay_warm: project once to populate OS page cache, then measure.
    // Both runs do full segment replay (NoCache), but warm benefits from
    // OS-level page caching of the segment files.
    let _: Option<Counter> = store
        .project("bench:entity", &Freshness::Consistent)
        .expect("warm OS page cache");

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

criterion_group!(benches, bench_projection_replay);
criterion_main!(benches);
