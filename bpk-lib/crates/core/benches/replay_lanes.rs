//! Dedicated replay-lane benchmark for JsonValueInput vs RawMsgpackInput.

use batpak::prelude::*;
use batpak::store::{Freshness, Store, StoreConfig};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, Criterion};
use tempfile::TempDir;

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct JsonCounter {
    count: u64,
}

impl EventSourced for JsonCounter {
    type Input = JsonValueInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("bench-replay-lanes-json-counter");

    fn from_events(events: &[ProjectionEvent<Self>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        Some(Self {
            count: events.len() as u64,
        })
    }

    fn apply_event(&mut self, _event: &ProjectionEvent<Self>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct RawCounter {
    count: u64,
}

impl EventSourced for RawCounter {
    type Input = RawMsgpackInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("bench-replay-lanes-raw-counter");

    fn from_events(events: &[ProjectionEvent<Self>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        Some(Self {
            count: events.len() as u64,
        })
    }

    fn apply_event(&mut self, _event: &ProjectionEvent<Self>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

fn bench_replay_lanes(c: &mut Criterion) {
    let mut group = c.benchmark_group("replay_lanes");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("bench:replay", "bench:scope").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    for i in 0u64..1_000 {
        let _ = store
            .append(
                &coord,
                kind,
                &serde_json::json!({"i": i, "payload": [1, 2, 3, 4]}),
            )
            .expect("append");
    }

    group.bench_function("json_value_input", |b| {
        b.iter(|| {
            let _: Option<JsonCounter> = store
                .project("bench:replay", &Freshness::Consistent)
                .expect("project json");
        });
    });

    group.bench_function("raw_msgpack_input", |b| {
        b.iter(|| {
            let _: Option<RawCounter> = store
                .project("bench:replay", &Freshness::Consistent)
                .expect("project raw");
        });
    });

    group.finish();
    store.close().expect("close");
}

criterion_group!(benches, bench_replay_lanes);
criterion_main!(benches);
