//! Benchmark write-side topology cost so overlay choices are not evaluated on
//! read latency alone.

use batpak::prelude::*;
use batpak::store::{IndexTopology, Store, StoreConfig, SyncMode};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::hint::black_box;
use tempfile::TempDir;

const EVENT_COUNT: u32 = 1_000;
const ENTITY_FANOUT: u32 = 64;
const SCOPE_FANOUT: u32 = 8;

fn open_topology_store(topology: IndexTopology) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_index_topology(topology)
            .with_sync_every_n_events(10_000)
            .with_sync_mode(SyncMode::SyncData)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    (store, dir)
}

fn coord_for(event_ix: u32) -> Coordinate {
    Coordinate::new(
        format!("bench:entity:{}", event_ix % ENTITY_FANOUT),
        format!("bench:scope:{}", event_ix % SCOPE_FANOUT),
    )
    .expect("coordinate")
}

fn bench_topology_write_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("topology_write_cost");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, EVENT_COUNT as u64);

    let kind = EventKind::custom(0xF, 1);
    let cases = [
        ("aos", IndexTopology::aos(), false),
        ("scan", IndexTopology::scan(), false),
        ("entity_local", IndexTopology::entity_local(), false),
        ("tiled", IndexTopology::tiled(), true),
        ("all", IndexTopology::all(), true),
    ];

    for (name, topology, expects_tiles) in cases {
        group.bench_with_input(BenchmarkId::from_parameter(name), &topology, |b, topology| {
            b.iter_batched(
                || open_topology_store(topology.clone()),
                |(store, _dir)| {
                    for event_ix in 0..EVENT_COUNT {
                        let coord = coord_for(event_ix);
                        store
                            .append(
                                &coord,
                                kind,
                                &serde_json::json!({
                                    "i": event_ix,
                                    "bucket": event_ix % SCOPE_FANOUT,
                                    "payload": "x".repeat(32),
                                }),
                            )
                            .expect("append");
                    }

                    let diagnostics = store.diagnostics();
                    assert_eq!(
                        diagnostics.index_topology,
                        match name {
                            "entity_local" => "entity-local",
                            other => other,
                        },
                        "BENCH SETUP: diagnostics should expose the configured topology label"
                    );
                    if expects_tiles {
                        assert!(
                            diagnostics.tile_count > 0,
                            "BENCH SETUP: tiled topology should report live tiles after write population"
                        );
                    } else {
                        assert_eq!(
                            diagnostics.tile_count, 0,
                            "BENCH SETUP: non-tiled topology should not accumulate tile footprint"
                        );
                    }
                    black_box(diagnostics.tile_count);
                    store.close().expect("close store");
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_topology_write_cost);
criterion_main!(benches);
