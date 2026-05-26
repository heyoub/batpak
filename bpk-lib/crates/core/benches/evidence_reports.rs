//! Benchmark: evidence report construction sanity surfaces.
//!
//! This bench is regression-sensitive, not a precision performance contract.

use batpak::prelude::*;
use batpak::schema::{compare_schema_snapshot, SchemaSnapshot};
use batpak::store::{
    ChainWalkRequest, ChainWalkStartRef, IndexTopology, LossPrecision, ReadWalkRequest,
    SubscriberDeliveryState, SubscriberFrontierRequest, SubscriberFrontierSource,
};
use batpak_bench_support::{apply_profile, BenchProfile};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::fmt::Display;
use std::hint::black_box;
use tempfile::TempDir;

struct TopologyCase {
    label: &'static str,
    topology: IndexTopology,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct BenchProjection {
    count: u64,
}

impl EventSourced for BenchProjection {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len() as u64,
        })
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        black_box(event.event_kind());
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 0x51)];
        &KINDS
    }
}

fn build_store(count: u32) -> (Store, TempDir, Coordinate, EventKind, batpak::id::EventId) {
    build_store_with_topology(count, IndexTopology::default())
}

fn build_store_with_topology(
    count: u32,
    topology: IndexTopology,
) -> (Store, TempDir, Coordinate, EventKind, batpak::id::EventId) {
    let dir = must(TempDir::new(), "create temp dir");
    let store = must(
        Store::open(
            StoreConfig::new(dir.path())
                .with_sync_every_n_events(100_000)
                .with_segment_max_bytes(1 << 20)
                .with_index_topology(topology),
        ),
        "open bench store",
    );
    let coord = must(
        Coordinate::new("bench:evidence:entity", "bench:evidence:scope"),
        "build bench coordinate",
    );
    let kind = EventKind::custom(0xF, 0x51);
    let mut last = batpak::id::EventId::from(0_u128);
    for i in 0..count {
        last = must(
            store.append(&coord, kind, &serde_json::json!({ "i": i })),
            "append bench event",
        )
        .event_id;
    }
    (store, dir, coord, kind, last)
}

fn topology_cases() -> [TopologyCase; 6] {
    [
        TopologyCase {
            label: "aos",
            topology: IndexTopology::aos(),
        },
        TopologyCase {
            label: "scan",
            topology: IndexTopology::scan(),
        },
        TopologyCase {
            label: "entity-local",
            topology: IndexTopology::entity_local(),
        },
        TopologyCase {
            label: "tiled",
            topology: IndexTopology::tiled(),
        },
        TopologyCase {
            label: "tiled-simd",
            topology: IndexTopology::tiled_simd(),
        },
        TopologyCase {
            label: "all",
            topology: IndexTopology::all(),
        },
    ]
}

fn must<T, E>(result: Result<T, E>, context: &str) -> T
where
    E: Display,
{
    match result {
        Ok(value) => value,
        Err(error) => fatal(context, error),
    }
}

fn fatal(error_context: &str, error: impl Display) -> ! {
    std::hint::black_box(format!("bench setup failed: {error_context}: {error}"));
    std::process::abort();
}

fn bench_schema_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("evidence/schema_snapshot_compare");
    apply_profile(&mut group, BenchProfile::Quick);
    let expected = SchemaSnapshot::from_hashes("bench.schema.v1", [1_u8; 32], [2_u8; 32]);
    let observed = SchemaSnapshot::from_hashes("bench.schema.v1", [1_u8; 32], [2_u8; 32]);
    group.bench_function("unchanged", |b| {
        b.iter(|| {
            black_box(must(
                compare_schema_snapshot(&expected, &observed),
                "compare schema snapshots",
            ))
        });
    });
    group.finish();
}

fn bench_chain_walk(c: &mut Criterion) {
    let mut group = c.benchmark_group("evidence/chain_walk");
    apply_profile(&mut group, BenchProfile::Heavy);
    for n in [100_u32, 1000_u32] {
        let (store, data_dir_guard, coord, kind, last) = build_store(n);
        assert!(data_dir_guard.path().exists());
        black_box(coord.entity());
        black_box(kind);
        let request = ChainWalkRequest::linear(ChainWalkStartRef::EventId(last.into()), n as usize);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("linear", n), &n, |b, _| {
            b.iter(|| {
                black_box(must(
                    store.chain_walk_evidence(&request),
                    "build chain walk evidence",
                ))
            });
        });
        must(store.close(), "close chain walk bench store");
    }
    group.finish();
}

fn bench_read_walk(c: &mut Criterion) {
    let mut group = c.benchmark_group("evidence/read_walk");
    apply_profile(&mut group, BenchProfile::Quick);
    for case in topology_cases() {
        let (store, data_dir_guard, coord, kind, last_event_id) =
            build_store_with_topology(1000, case.topology);
        assert!(data_dir_guard.path().exists());
        black_box(last_event_id);
        let mut req = ReadWalkRequest::full(
            Region::scope(coord.scope()).with_fact(batpak::coordinate::KindFilter::Exact(kind)),
        );
        req.include_proof_refs = true;
        group.bench_with_input(
            BenchmarkId::new("query_with_report", case.label),
            case.label,
            |b, _| {
                b.iter(|| {
                    black_box(must(
                        store.query_with_read_walk_evidence(&req),
                        "build read walk evidence",
                    ))
                });
            },
        );
        must(store.close(), "close read walk bench store");
    }
    group.finish();
}

fn bench_projection_run(c: &mut Criterion) {
    let mut group = c.benchmark_group("evidence/projection_run");
    apply_profile(&mut group, BenchProfile::Quick);
    for case in topology_cases() {
        let (store, data_dir_guard, coord, kind, last_event_id) =
            build_store_with_topology(250, case.topology);
        assert!(data_dir_guard.path().exists());
        black_box(kind);
        black_box(last_event_id);
        group.bench_with_input(
            BenchmarkId::new("project_run_evidence", case.label),
            case.label,
            |b, _| {
                b.iter(|| {
                    black_box(must(
                        store.project_run_evidence::<BenchProjection>(
                            coord.entity(),
                            &Freshness::Consistent,
                        ),
                        "build projection run evidence",
                    ))
                });
            },
        );
        must(store.close(), "close projection bench store");
    }
    group.finish();
}

fn bench_store_resource(c: &mut Criterion) {
    let mut group = c.benchmark_group("evidence/store_resource");
    apply_profile(&mut group, BenchProfile::Quick);
    let (store, data_dir_guard, coord, kind, _) = build_store(256);
    assert!(data_dir_guard.path().exists());
    black_box(coord.entity());
    black_box(kind);
    group.bench_function("diagnostics_snapshot", |b| {
        b.iter(|| {
            black_box(must(
                store.store_resource_evidence_report(),
                "build store resource evidence",
            ))
        });
    });
    must(store.close(), "close store resource bench store");
    group.finish();
}

fn bench_subscriber_frontier(c: &mut Criterion) {
    let mut group = c.benchmark_group("evidence/subscriber_frontier");
    apply_profile(&mut group, BenchProfile::Quick);
    let (store, data_dir_guard, coord, kind, last_event_id) = build_store(64);
    assert!(data_dir_guard.path().exists());
    black_box(coord.scope());
    black_box(kind);
    black_box(last_event_id);
    let request = SubscriberFrontierRequest {
        source: SubscriberFrontierSource::LossyPush,
        consumed_frontier_sequence: Some(32),
        delivery_state: SubscriberDeliveryState::Lagging,
        loss_precision: LossPrecision::Unknown,
        exact_dropped_ranges: Vec::new(),
    };
    group.bench_function("observation", |b| {
        b.iter(|| {
            black_box(must(
                store.subscriber_frontier_observation(&request),
                "build subscriber frontier evidence",
            ))
        });
    });
    must(store.close(), "close subscriber frontier bench store");
    group.finish();
}

criterion_group!(
    benches,
    bench_schema_snapshot,
    bench_chain_walk,
    bench_read_walk,
    bench_projection_run,
    bench_store_resource,
    bench_subscriber_frontier
);
criterion_main!(benches);
