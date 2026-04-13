//! Benchmark: reopening a populated store through explicit cold-start lanes.
//!
//! [SPEC:benches/cold_start.rs]
//!
//! This benchmark measures `Store::open()` against a copied on-disk fixture,
//! which is more honest than repeatedly reopening the same temp directory and
//! calling it "cold start".

mod common;

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use common::{apply_profile, profile_for_event_count, throughput_elements};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use tempfile::TempDir;

#[derive(Clone, Copy)]
enum SnapshotLane {
    Mmap,
    Checkpoint,
}

fn lane_config(dir: &std::path::Path, lane: SnapshotLane) -> StoreConfig {
    match lane {
        SnapshotLane::Mmap => StoreConfig::new(dir)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(true),
        SnapshotLane::Checkpoint => StoreConfig::new(dir)
            .with_enable_checkpoint(true)
            .with_enable_mmap_index(false),
    }
}

fn rebuild_only_config(dir: &std::path::Path) -> StoreConfig {
    StoreConfig::new(dir)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
}

fn populate_store(config: StoreConfig, count: u64) {
    let store = Store::open(config).expect("open store for populate");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});
    for _ in 0..count {
        store.append(&coord, kind, &payload).expect("append");
    }
    store.sync().expect("sync");
    store.close().expect("close");
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).expect("create destination directory");
    for entry in std::fs::read_dir(src).expect("read source directory") {
        let entry = entry.expect("directory entry");
        let source_path = entry.path();
        let destination_path = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir_recursive(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).expect("copy fixture file");
        }
    }
}

fn prepare_fixture(count: u64, config: StoreConfig) -> TempDir {
    let fixture_dir = TempDir::new().expect("create fixture temp dir");
    let config = StoreConfig {
        data_dir: fixture_dir.path().to_path_buf(),
        ..config
    };
    populate_store(config, count);
    fixture_dir
}

fn append_tail_without_refreshing_snapshot(
    fixture_dir: &TempDir,
    tail_count: u64,
    lane: SnapshotLane,
) {
    let stale_config = match lane {
        SnapshotLane::Mmap => rebuild_only_config(fixture_dir.path()),
        SnapshotLane::Checkpoint => rebuild_only_config(fixture_dir.path()),
    };
    let store = Store::open(stale_config).expect("open stale snapshot fixture");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    for i in 0..tail_count {
        store
            .append(&coord, kind, &serde_json::json!({"tail": i}))
            .expect("append tail");
    }
    store.sync().expect("sync tail");
    store.close().expect("close stale snapshot fixture");
}

fn bench_cold_start_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_start_paths");

    for count in [1_000u64, 10_000, 100_000, 1_000_000] {
        apply_profile(&mut group, profile_for_event_count(count));
        throughput_elements(&mut group, count);
        let tail_count = count.clamp(32, 1_024);

        let default_fixture = prepare_fixture(
            count,
            StoreConfig {
                data_dir: std::path::PathBuf::new(),
                ..StoreConfig::new("")
            },
        );
        let mmap_fixture = prepare_fixture(
            count,
            StoreConfig {
                data_dir: std::path::PathBuf::new(),
                ..lane_config(std::path::Path::new(""), SnapshotLane::Mmap)
            },
        );
        let checkpoint_fixture = prepare_fixture(
            count,
            StoreConfig {
                data_dir: std::path::PathBuf::new(),
                ..lane_config(std::path::Path::new(""), SnapshotLane::Checkpoint)
            },
        );
        let rebuild_fixture = prepare_fixture(
            count,
            StoreConfig {
                data_dir: std::path::PathBuf::new(),
                ..rebuild_only_config(std::path::Path::new(""))
            },
        );
        let mmap_tail_fixture = prepare_fixture(
            count,
            StoreConfig {
                data_dir: std::path::PathBuf::new(),
                ..lane_config(std::path::Path::new(""), SnapshotLane::Mmap)
            },
        );
        append_tail_without_refreshing_snapshot(&mmap_tail_fixture, tail_count, SnapshotLane::Mmap);
        let checkpoint_tail_fixture = prepare_fixture(
            count,
            StoreConfig {
                data_dir: std::path::PathBuf::new(),
                ..lane_config(std::path::Path::new(""), SnapshotLane::Checkpoint)
            },
        );
        append_tail_without_refreshing_snapshot(
            &checkpoint_tail_fixture,
            tail_count,
            SnapshotLane::Checkpoint,
        );

        group.bench_with_input(
            BenchmarkId::new("reopen_holistic_default", count),
            &count,
            |b, &_count| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("create iteration dir");
                        copy_dir_recursive(default_fixture.path(), iter_dir.path());
                        iter_dir
                    },
                    |iter_dir| {
                        let config = StoreConfig {
                            data_dir: iter_dir.path().to_path_buf(),
                            ..StoreConfig::new("")
                        };
                        let store = Store::open(config).expect("reopen populated store");
                        store.close().expect("close");
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("open_mmap_snapshot", count),
            &count,
            |b, &_count| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("create iteration dir");
                        copy_dir_recursive(mmap_fixture.path(), iter_dir.path());
                        iter_dir
                    },
                    |iter_dir| {
                        let store = Store::open(lane_config(iter_dir.path(), SnapshotLane::Mmap))
                            .expect("open mmap snapshot");
                        store.close().expect("close");
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("open_checkpoint_snapshot", count),
            &count,
            |b, &_count| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("create iteration dir");
                        copy_dir_recursive(checkpoint_fixture.path(), iter_dir.path());
                        iter_dir
                    },
                    |iter_dir| {
                        let store =
                            Store::open(lane_config(iter_dir.path(), SnapshotLane::Checkpoint))
                                .expect("open checkpoint snapshot");
                        store.close().expect("close");
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rebuild_segments_parallel", count),
            &count,
            |b, &_count| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("create iteration dir");
                        copy_dir_recursive(rebuild_fixture.path(), iter_dir.path());
                        iter_dir
                    },
                    |iter_dir| {
                        let store = Store::open(rebuild_only_config(iter_dir.path()))
                            .expect("rebuild only");
                        store.close().expect("close");
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("replay_tail_after_snapshot_mmap", count),
            &count,
            |b, &_count| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("create iteration dir");
                        copy_dir_recursive(mmap_tail_fixture.path(), iter_dir.path());
                        iter_dir
                    },
                    |iter_dir| {
                        let store = Store::open(lane_config(iter_dir.path(), SnapshotLane::Mmap))
                            .expect("open mmap tail replay");
                        store.close().expect("close");
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("replay_tail_after_snapshot_checkpoint", count),
            &count,
            |b, &_count| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("create iteration dir");
                        copy_dir_recursive(checkpoint_tail_fixture.path(), iter_dir.path());
                        iter_dir
                    },
                    |iter_dir| {
                        let store =
                            Store::open(lane_config(iter_dir.path(), SnapshotLane::Checkpoint))
                                .expect("open checkpoint tail replay");
                        store.close().expect("close");
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_cold_start_paths);
criterion_main!(benches);
