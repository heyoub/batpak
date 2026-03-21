//! Benchmark: cache hit vs miss for EventSourced projection.
//! [SPEC:benches/projection_latency.rs]

use criterion::{criterion_group, criterion_main, Criterion};
use free_batteries::prelude::*;
use free_batteries::store::{Freshness, Store, StoreConfig};
use tempfile::TempDir;

/// A minimal EventSourced implementation for benchmarking.
#[derive(Default, Debug)]
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

fn bench_projection_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("projection_latency");

    // Setup: populate a store with events for one entity
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});
    for _ in 0..1_000 {
        store.append(&coord, kind, &payload).expect("append");
    }
    store.sync().expect("sync");

    group.bench_function("cache_miss", |b| {
        b.iter(|| {
            let _: Option<Counter> = store
                .project("bench:entity", Freshness::Consistent)
                .expect("project");
        });
    });

    // Cache hit: project once to warm cache, then measure subsequent calls.
    let _: Option<Counter> = store
        .project("bench:entity", Freshness::Consistent)
        .expect("warm cache");

    group.bench_function("cache_hit", |b| {
        b.iter(|| {
            let _: Option<Counter> = store
                .project("bench:entity", Freshness::Consistent)
                .expect("project");
        });
    });

    group.finish();
    store.close().expect("close");
}

criterion_group!(benches, bench_projection_latency);
criterion_main!(benches);
