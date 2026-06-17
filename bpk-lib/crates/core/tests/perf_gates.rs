// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; lifecycle perf-gate tests in tests/perf_gates.rs panic! on regressions and narrow wall-clock timing counters into smaller integer types.
#![allow(clippy::panic, clippy::cast_possible_truncation)]
//! PROVES: LAW-004 (Composition Over Construction — quadratic dogfooding) for the
//! store-lifecycle latency gates: open-only and close-only cold start across
//! single-entity and mixed-entity 100K corpora, plus mmap-restore and
//! rebuild-restore open-only paths (asserting the chosen OpenIndexPath).
//! CATCHES: gross cold-start / shutdown regressions and a restore path that
//! silently falls back to the wrong materialization strategy.
//! SEEDED: deterministic 100K-event single-entity and 1000x100 mixed-entity
//! corpora; thresholds are generous CI floors, not precision benchmarks.
//!
//! Split out of the original 1322-line `tests/perf_gates.rs`. This binary keeps
//! the `perf_gates` stem and owns the LifecycleLatencyGate family and its
//! corpus-population helpers. The throughput/latency, cold-start/batch, and
//! correctness families live in sibling `perf_gates_*` binaries.
//! Harness pattern: Property Harness (catastrophic threshold lane).

#[path = "support/mod.rs"]
mod support;
use batpak::store::cold_start::rebuild::OpenIndexPath;
use batpak::store::{Store, StoreConfig};
use std::time::Instant;
use support::prelude::*;
use tempfile::TempDir;

struct LifecycleContext {
    phase: &'static str,
    corpus: &'static str,
    elapsed_ms: u128,
    event_count: u64,
}

struct LifecycleLatencyGate {
    max_ms: u128,
}

impl Gate<LifecycleContext> for LifecycleLatencyGate {
    fn name(&self) -> &'static str {
        "lifecycle_latency"
    }

    fn evaluate(&self, ctx: &LifecycleContext) -> Result<(), Denial> {
        if ctx.elapsed_ms <= self.max_ms {
            Ok(())
        } else {
            Err(Denial::new(
                "lifecycle_latency",
                format!(
                    "{} {} took {}ms for {} events (max: {}ms). \
                     Investigate: src/store/cold_start/rebuild.rs planner lanes, \
                     src/store/cold_start/mmap.rs, src/store/cold_start/checkpoint.rs, src/store/index/mod.rs restore materialization.",
                    ctx.corpus, ctx.phase, ctx.elapsed_ms, ctx.event_count, self.max_ms
                ),
            )
            .with_context("phase", ctx.phase.to_owned())
            .with_context("corpus", ctx.corpus.to_owned())
            .with_context("elapsed_ms", ctx.elapsed_ms.to_string())
            .with_context("event_count", ctx.event_count.to_string())
            .with_context("max_ms", self.max_ms.to_string()))
        }
    }
}

fn perf_store_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path()).with_sync_every_n_events(10_000)
}

fn populate_single_entity_corpus(config: StoreConfig, count: u64) {
    let store = Store::open(config).expect("open corpus store");
    let coord = Coordinate::new("perf:single", "perf:scope").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    for i in 0..count {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.close().expect("close corpus store");
}

fn populate_mixed_entity_corpus(config: StoreConfig, entity_count: u64, per_entity: u64) {
    let store = Store::open(config).expect("open corpus store");
    let kind = EventKind::custom(0xF, 1);
    for entity_idx in 0..entity_count {
        let coord = Coordinate::new(
            format!("perf:mixed:{entity_idx:04}"),
            format!("perf:scope:{:02}", entity_idx % 16),
        )
        .expect("coord");
        for seq in 0..per_entity {
            store
                .append(
                    &coord,
                    kind,
                    &serde_json::json!({"entity": entity_idx, "seq": seq}),
                )
                .expect("append");
        }
    }
    store.close().expect("close corpus store");
}

fn assert_lifecycle_under_threshold(
    phase: &'static str,
    corpus: &'static str,
    elapsed_ms: u128,
    event_count: u64,
    max_ms: u128,
) {
    let mut gates = GateSet::new();
    gates.push(LifecycleLatencyGate { max_ms });
    let ctx = LifecycleContext {
        phase,
        corpus,
        elapsed_ms,
        event_count,
    };
    let proposal = Proposal::new(elapsed_ms);
    if let Err(denial) = gates.evaluate(&ctx, proposal) {
        panic!(
            "LIFECYCLE PERF GATE FAILED: {denial}\nContext: {:?}",
            denial.context
        );
    }
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Measures open-only cold start on a skewed single-entity corpus."]
fn open_only_single_entity_100k_under_threshold() {
    let dir = TempDir::new().expect("temp dir");
    populate_single_entity_corpus(perf_store_config(&dir), 100_000);

    let start = Instant::now();
    let store = Store::open(perf_store_config(&dir)).expect("open");
    let elapsed_ms = start.elapsed().as_millis();
    assert_lifecycle_under_threshold(
        "open_only",
        "single_entity_100k",
        elapsed_ms,
        100_000,
        5_000,
    );
    store.close().expect("close");
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Measures close-only lifecycle cost on a skewed single-entity corpus."]
fn close_only_single_entity_100k_under_threshold() {
    let dir = TempDir::new().expect("temp dir");
    populate_single_entity_corpus(perf_store_config(&dir), 100_000);
    let store = Store::open(perf_store_config(&dir)).expect("open");

    let start = Instant::now();
    store.close().expect("close");
    let elapsed_ms = start.elapsed().as_millis();
    assert_lifecycle_under_threshold(
        "close_only",
        "single_entity_100k",
        elapsed_ms,
        100_000,
        12_000,
    );
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Measures open-only cold start on a mixed-entity corpus."]
fn open_only_mixed_entity_100k_under_threshold() {
    let dir = TempDir::new().expect("temp dir");
    populate_mixed_entity_corpus(perf_store_config(&dir), 1_000, 100);

    let start = Instant::now();
    let store = Store::open(perf_store_config(&dir)).expect("open");
    let elapsed_ms = start.elapsed().as_millis();
    assert_lifecycle_under_threshold("open_only", "mixed_entity_100k", elapsed_ms, 100_000, 5_000);
    store.close().expect("close");
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Measures close-only lifecycle cost on a mixed-entity corpus."]
fn close_only_mixed_entity_100k_under_threshold() {
    let dir = TempDir::new().expect("temp dir");
    populate_mixed_entity_corpus(perf_store_config(&dir), 1_000, 100);
    let store = Store::open(perf_store_config(&dir)).expect("open");

    let start = Instant::now();
    store.close().expect("close");
    let elapsed_ms = start.elapsed().as_millis();
    assert_lifecycle_under_threshold(
        "close_only",
        "mixed_entity_100k",
        elapsed_ms,
        100_000,
        12_000,
    );
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Measures mmap restore open-only on a skewed single-entity corpus."]
fn mmap_restore_single_entity_100k_under_threshold() {
    let dir = TempDir::new().expect("temp dir");
    populate_single_entity_corpus(
        perf_store_config(&dir)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(true),
        100_000,
    );

    let start = Instant::now();
    let store = Store::open(
        perf_store_config(&dir)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(true),
    )
    .expect("open");
    let elapsed_ms = start.elapsed().as_millis();
    let report = store
        .diagnostics()
        .open_report
        .expect("open report after mmap restore");
    assert!(
        matches!(report.path, OpenIndexPath::Mmap),
        "expected mmap restore path, got {:?}",
        report.path
    );
    assert_lifecycle_under_threshold(
        "open_only_mmap",
        "single_entity_100k",
        elapsed_ms,
        100_000,
        5_000,
    );
    store.close().expect("close");
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Measures rebuild restore open-only on a skewed single-entity corpus."]
fn rebuild_restore_single_entity_100k_under_threshold() {
    let dir = TempDir::new().expect("temp dir");
    populate_single_entity_corpus(
        perf_store_config(&dir)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
        100_000,
    );

    let start = Instant::now();
    let store = Store::open(
        perf_store_config(&dir)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open");
    let elapsed_ms = start.elapsed().as_millis();
    let report = store
        .diagnostics()
        .open_report
        .expect("open report after rebuild restore");
    assert!(
        matches!(report.path, OpenIndexPath::Rebuild),
        "expected rebuild restore path, got {:?}",
        report.path
    );
    assert_lifecycle_under_threshold(
        "open_only_rebuild",
        "single_entity_100k",
        elapsed_ms,
        100_000,
        5_000,
    );
    store.close().expect("close");
}
