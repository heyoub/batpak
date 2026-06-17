// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; perf-gate tests in tests/perf_gates_cold_start.rs stream benchmark timings to stderr, panic! on regressions, and narrow timing counters into smaller integer types.
#![allow(clippy::panic, clippy::print_stderr, clippy::cast_possible_truncation)]
//! PROVES: LAW-004 (Composition Over Construction — quadratic dogfooding) for the
//! cold-start latency gate and the batch-append throughput gate, plus the
//! tripwire test that the ColdStartGate itself rejects an over-budget cold start.
//! CATCHES: gross cold-start regressions, batch-append throughput category
//! regressions (O(n²) loops / lost batching on the SyncData path), and a
//! vacuous ColdStartGate that always passes.
//! SEEDED: deterministic 1K single-coordinate corpus (cold start) and a
//! 100x100 SyncData batch corpus; thresholds are generous CI floors.
//!
//! Split out of the original 1322-line `tests/perf_gates.rs`. Holds the
//! ColdStartGate/ColdStartContext family and the BatchThroughputGate family.
//! Harness pattern: Property Harness (catastrophic threshold lane).

#[path = "support/mod.rs"]
mod support;
use batpak::store::{Store, StoreConfig, SyncMode};
use std::time::Instant;
use support::prelude::*;
use tempfile::TempDir;

/// A Gate that checks cold-start performance.
/// This is a reusable assertion harness, not a precision benchmark.
struct ColdStartGate {
    max_ms: u128,
}

impl Gate<ColdStartContext> for ColdStartGate {
    fn name(&self) -> &'static str {
        "cold_start_performance"
    }

    fn evaluate(&self, ctx: &ColdStartContext) -> Result<(), Denial> {
        if ctx.cold_start_ms <= self.max_ms {
            Ok(())
        } else {
            Err(Denial::new(
                "cold_start_performance",
                format!(
                    "Cold start took {}ms for {} events (max: {}ms). \
                     Investigate: src/store/mod.rs Store::open cold start scan, \
                     src/store/segment/scan.rs scan_segment.",
                    ctx.cold_start_ms, ctx.event_count, self.max_ms
                ),
            )
            .with_context("event_count", ctx.event_count.to_string())
            .with_context("cold_start_ms", ctx.cold_start_ms.to_string())
            .with_context("max_ms", self.max_ms.to_string()))
        }
    }
}

struct ColdStartContext {
    cold_start_ms: u128,
    event_count: u64,
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates` or `cargo nextest run --test perf_gates -- --ignored`. Uses Instant::now() and asserts on wall-clock; flakes on shared CI runners."]
fn cold_start_1k_events_under_threshold() {
    let dir = TempDir::new().expect("create temp dir");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});

    // Populate
    {
        let config = StoreConfig::new(dir.path());
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
        for _ in 0..1_000 {
            store.append(&coord, kind, &payload).expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Measure cold start
    let start = Instant::now();
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("cold start");
    let cold_start_ms = start.elapsed().as_millis();

    // Use GateSet as a reusable assertion harness for catastrophic regressions.
    let mut gates = GateSet::new();
    // Reference target: cold start < 200ms for 1K events on production hardware.
    // CI threshold: 2000ms (10x) because CI runners are slow, virtualized, and
    // share resources. The criterion bench (benches/cold_start.rs) tracks the
    // actual distribution — this gate catches gross regressions.
    gates.push(ColdStartGate { max_ms: 2000 });

    let ctx = ColdStartContext {
        cold_start_ms,
        event_count: 1_000,
    };

    let proposal = Proposal::new(cold_start_ms);
    let result = gates.evaluate(&ctx, proposal);

    match result {
        Ok(receipt) => {
            let (ms, gate_names) = receipt.into_parts();
            assert_eq!(
                gate_names,
                vec!["cold_start_performance"],
                "PROPERTY: GateSet receipt must record the gate name 'cold_start_performance'.\n\
                 Investigate: src/guard/mod.rs GateSet::evaluate() receipt gate_names collection.\n\
                 Common causes: Gate::name() not being called or stored in the receipt, \
                 or receipt.into_parts() returning an empty name list.\n\
                 Run: cargo test --test perf_gates_cold_start cold_start_1k_events_under_threshold"
            );
            eprintln!(
                "SELF-BENCHMARK: cold start for 1K events: {}ms (passed {})",
                ms,
                gate_names.join(", ")
            );
        }
        Err(denial) => {
            panic!(
                "SELF-BENCHMARK FAILED: {}\n\
                    The catastrophic regression guard detected a performance regression.\n\
                    Context: {:?}",
                denial, denial.context
            );
        }
    }

    store.sync().expect("sync");
}

/// Verify the gate harness correctly rejects slow cold starts.
/// This ensures the catastrophic-regression tripwire itself is not vacuous.
#[test]
fn cold_start_gate_rejects_slow() {
    let mut gates = GateSet::new();
    gates.push(ColdStartGate { max_ms: 1 }); // impossibly tight

    let ctx = ColdStartContext {
        cold_start_ms: 100, // simulated slow cold start
        event_count: 1_000,
    };

    let proposal = Proposal::new(100u128);
    let result = gates.evaluate(&ctx, proposal);
    assert!(
        result.is_err(),
        "PROPERTY: ColdStartGate must reject a cold start that exceeds the configured max_ms.\n\
         Investigate: src/guard/mod.rs GateSet::evaluate() ColdStartGate::evaluate().\n\
         Common causes: Gate::evaluate() ignoring the threshold and always returning Ok, \
         or GateSet::evaluate() not propagating Denial from a gate.\n\
         Run: cargo test --test perf_gates_cold_start cold_start_gate_rejects_slow"
    );
}

/// Batch throughput gate: batch events/sec must meet minimum.
struct BatchThroughputGate {
    min_events_per_sec: f64,
}

impl Gate<BatchPerfContext> for BatchThroughputGate {
    fn name(&self) -> &'static str {
        "batch_throughput"
    }

    fn evaluate(&self, ctx: &BatchPerfContext) -> Result<(), Denial> {
        if ctx.batch_events_per_sec >= self.min_events_per_sec {
            Ok(())
        } else {
            Err(Denial::new(
                "batch_throughput",
                format!(
                    "Batch throughput {:.0} events/sec < minimum {:.0}. \
                     Batch size: {}, batches: {}. \
                     Investigate: src/store/write/writer.rs handle_append_batch, \
                     two-phase commit overhead.",
                    ctx.batch_events_per_sec,
                    self.min_events_per_sec,
                    ctx.batch_size,
                    ctx.batch_count
                ),
            )
            .with_context("events_per_sec", format!("{:.0}", ctx.batch_events_per_sec))
            .with_context("min_required", format!("{:.0}", self.min_events_per_sec))
            .with_context("batch_size", ctx.batch_size.to_string())
            .with_context("batch_count", ctx.batch_count.to_string()))
        }
    }
}

struct BatchPerfContext {
    batch_size: usize,
    batch_count: usize,
    batch_events_per_sec: f64,
}

/// Self-benchmark for batch append throughput.
#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Already softened to 2K events/sec floor after FLAKY 3/3 / TRY 3 FAIL on Windows CI."]
fn batch_throughput_performance_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_sync_every_n_events(1)
        // Each batch is a sync on the benchmark-relevant data-fsync path.
        .with_sync_mode(SyncMode::SyncData);
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("perf:batch", "batch_scope").expect("valid");
    let kind = EventKind::custom(0xF, 1);

    let batch_size = 100usize;
    let batch_count = 100usize;
    let total_events = (batch_size * batch_count) as u64;

    // Build batch items once
    let batch_template: Vec<_> = (0..batch_size)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"i": i, "payload": "x".repeat(50)}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("valid item")
        })
        .collect();

    // Measure batch append throughput
    let write_start = Instant::now();
    for _ in 0..batch_count {
        store
            .append_batch(batch_template.clone())
            .expect("batch append");
    }
    let write_elapsed = write_start.elapsed();
    let batch_events_per_sec = total_events as f64 / write_elapsed.as_secs_f64();

    let ctx = BatchPerfContext {
        batch_size,
        batch_count,
        batch_events_per_sec,
    };

    // Threshold: 2K events/sec on the SyncData path — set well below the
    // typical observed throughput (35K-42K events/sec on a developer machine,
    // ~12K on slow shared CI runners) so the gate catches CATEGORY
    // regressions only (e.g., a refactor that introduces an O(n²) loop or
    // removes batching on the benchmark-relevant durability mode
    // entirely), not run-to-run jitter on noisy CI hardware. Tightening
    // this threshold has historically caused flake-by-retry failures
    // (`FLAKY 3/3` on Linux, `TRY 3 FAIL` on Windows) without catching
    // any real regression.
    let mut gates = GateSet::new();
    gates.push(BatchThroughputGate {
        min_events_per_sec: 2_000.0,
    });

    let proposal = Proposal::new(batch_events_per_sec);
    match gates.evaluate(&ctx, proposal) {
        Ok(receipt) => {
            eprintln!(
                "  BATCH SELF-BENCHMARK: {} batches of {} = {} events in {:?} ({:.0} events/sec) - passed {}",
                batch_count,
                batch_size,
                total_events,
                write_elapsed,
                batch_events_per_sec,
                receipt.gates_passed().join(", ")
            );
        }
        Err(denial) => {
            panic!(
                "BATCH SELF-BENCHMARK FAILED: {}\n\
                 Batch append throughput regression detected.\n\
                 Context: {:?}",
                denial, denial.context
            );
        }
    }

    store.close().expect("close");
}
