//! Benchmark query cost across the explicit topology presets.

use batpak::prelude::*;
use batpak::store::{IndexTopology, Store, StoreConfig};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use tempfile::TempDir;

fn bench_topology_matrix(c: &mut Criterion) {
    let mut group = c.benchmark_group("topology_matrix");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    let kind = EventKind::custom(0xF, 1);
    let coord = Coordinate::new("bench:topology", "bench:scope").expect("coord");
    let cases = [
        ("aos", IndexTopology::aos()),
        ("scan", IndexTopology::scan()),
        ("entity_local", IndexTopology::entity_local()),
        ("tiled", IndexTopology::tiled()),
        ("all", IndexTopology::all()),
    ];

    for (name, topology) in cases {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_index_topology(topology)
                .with_sync_every_n_events(10_000),
        )
        .expect("open");
        for i in 0u32..1_000 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        group.bench_function(name, |b| {
            b.iter(|| {
                black_box(store.by_fact(kind));
            });
        });
        store.close().expect("close");
    }

    group.finish();
}

criterion_group!(benches, bench_topology_matrix);
criterion_main!(benches);
