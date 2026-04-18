// justifies: perf-gate tests stream benchmark timings to stderr, panic! on regressions, and narrow timing counters into smaller integer types.
#![allow(clippy::panic, clippy::print_stderr, clippy::cast_possible_truncation)]
//! Performance gate tests use the library's Gate/Pipeline primitives as a
//! reusable harness for catastrophic-regression checks.
//! These thresholds are intentionally generous and exist to catch obvious
//! regressions, not to act as precision benchmark authority: no current
//! environment is both canonical and timing-stable.
//!
//! PROVES: LAW-004 (Composition Over Construction — quadratic dogfooding)
//! DEFENDS: FM-013 (Coverage Mirage — gates test themselves), FM-007 (Island Syndrome)
//! INVARIANTS: INV-PERF (performance thresholds), INV-STATE (gate evaluation)
//!
//! This is deliberate dogfooding of shared control-flow primitives, not a
//! claim that these tests are the sole performance authority. Criterion
//! benches provide trend visibility; these gates catch gross regressions.

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, SyncConfig};
use std::time::Instant;
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
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            ..StoreConfig::new("")
        };
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
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
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
                 Run: cargo test --test perf_gates cold_start_1k_events_under_threshold"
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
         Run: cargo test --test perf_gates cold_start_gate_rejects_slow"
    );
}

// ---- Multi-dimension performance gates ----
// The benchmark tells you WHAT needs improving, not just pass/fail.

/// Write throughput gate: events/sec must meet minimum.
struct WriteThroughputGate {
    min_events_per_sec: f64,
}

impl Gate<PerfContext> for WriteThroughputGate {
    fn name(&self) -> &'static str {
        "write_throughput"
    }

    fn evaluate(&self, ctx: &PerfContext) -> Result<(), Denial> {
        if ctx.events_per_sec >= self.min_events_per_sec {
            Ok(())
        } else {
            Err(Denial::new(
                "write_throughput",
                format!(
                    "Write throughput {:.0} events/sec < minimum {:.0}. \
                     Investigate: src/store/write/writer.rs handle_append (10-step commit), \
                     src/store/segment/mod.rs write_frame, CRC overhead.",
                    ctx.events_per_sec, self.min_events_per_sec
                ),
            )
            .with_context("events_per_sec", format!("{:.0}", ctx.events_per_sec))
            .with_context("min_required", format!("{:.0}", self.min_events_per_sec)))
        }
    }
}

/// Query latency gate: microseconds per query must meet maximum.
struct QueryLatencyGate {
    max_us_per_query: f64,
}

impl Gate<PerfContext> for QueryLatencyGate {
    fn name(&self) -> &'static str {
        "query_latency"
    }

    fn evaluate(&self, ctx: &PerfContext) -> Result<(), Denial> {
        if ctx.query_us <= self.max_us_per_query {
            Ok(())
        } else {
            Err(Denial::new(
                "query_latency",
                format!(
                    "Query latency {:.1}µs > max {:.1}µs. \
                     Investigate: src/store/index/mod.rs query() DashMap scan, \
                     Region::matches_event hot path.",
                    ctx.query_us, self.max_us_per_query
                ),
            )
            .with_context("query_us", format!("{:.1}", ctx.query_us))
            .with_context("max_us", format!("{:.1}", self.max_us_per_query)))
        }
    }
}

/// Projection gate: replay time must be bounded.
struct ProjectionGate {
    max_ms: f64,
}

impl Gate<PerfContext> for ProjectionGate {
    fn name(&self) -> &'static str {
        "projection_replay"
    }

    fn evaluate(&self, ctx: &PerfContext) -> Result<(), Denial> {
        if ctx.projection_ms <= self.max_ms {
            Ok(())
        } else {
            Err(Denial::new(
                "projection_replay",
                format!(
                    "Projection replay {:.1}ms > max {:.1}ms for {} events. \
                     Investigate: src/store/projection/flow.rs project(), \
                     src/store/segment/scan.rs read_entry deserialization.",
                    ctx.projection_ms, self.max_ms, ctx.event_count
                ),
            )
            .with_context("projection_ms", format!("{:.1}", ctx.projection_ms))
            .with_context("max_ms", format!("{:.1}", self.max_ms))
            .with_context("event_count", ctx.event_count.to_string()))
        }
    }
}

struct PerfContext {
    event_count: u64,
    events_per_sec: f64,
    query_us: f64,
    projection_ms: f64,
}

/// The multi-gate self-benchmark. Uses evaluate_all() to collect ALL denials,
/// not fail-fast — so it reports EVERYTHING that needs improvement in one pass.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct BenchCounter {
    count: u64,
}

impl EventSourced for BenchCounter {
    type Input = batpak::prelude::JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut s = Self::default();
        for e in events {
            s.apply_event(e);
        }
        Some(s)
    }
    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }
    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Uses Instant::now() for write/query/projection timing; flakes on shared CI runners."]
fn multi_gate_performance_feedback() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("perf:entity", "perf:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 1_000u64;

    // Measure write throughput
    let write_start = Instant::now();
    for i in 0..n {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    let write_elapsed = write_start.elapsed();
    let events_per_sec = n as f64 / write_elapsed.as_secs_f64();

    // Measure query latency
    let query_iters = 100u64;
    let query_start = Instant::now();
    let region = Region::entity("perf:entity");
    for _ in 0..query_iters {
        let _ = store.query(&region);
    }
    let query_elapsed = query_start.elapsed();
    let query_us = query_elapsed.as_micros() as f64 / query_iters as f64;

    // Measure projection replay
    let proj_start = Instant::now();
    let _: Option<BenchCounter> = store
        .project("perf:entity", &batpak::store::Freshness::Consistent)
        .expect("project");
    let projection_ms = proj_start.elapsed().as_secs_f64() * 1000.0;

    let ctx = PerfContext {
        event_count: n,
        events_per_sec,
        query_us,
        projection_ms,
    };

    // Build gate set with thresholds (generous for CI, tighten for prod)
    let mut gates = GateSet::new();
    gates.push(WriteThroughputGate {
        min_events_per_sec: 1_000.0,
    }); // 1K/sec minimum
    gates.push(QueryLatencyGate {
        max_us_per_query: 50_000.0,
    }); // 50ms max
    gates.push(ProjectionGate { max_ms: 5_000.0 }); // 5s max for 1K events

    // evaluate_all: collect ALL denials, don't stop at first
    let denials = gates.evaluate_all(&ctx);

    // Report — this IS the benchmark feedback
    eprintln!("\n  SELF-BENCHMARK REPORT ({n} events):");
    eprintln!("    Write throughput:  {events_per_sec:.0} events/sec");
    eprintln!("    Query latency:     {query_us:.1} µs/query");
    eprintln!("    Projection replay: {projection_ms:.1} ms");

    if denials.is_empty() {
        eprintln!("    Result: ALL GATES PASSED");
    } else {
        eprintln!("    Result: {} GATES FAILED:", denials.len());
        for d in &denials {
            eprintln!("      [{gate}] {msg}", gate = d.gate, msg = d.message);
            for (k, v) in &d.context {
                eprintln!("        {k} = {v}");
            }
        }
        panic!(
            "SELF-BENCHMARK FAILED: {} performance gate(s) denied.\n\
             The denials above point to the likely investigation sites.\n\
             This is the library using the shared guard primitives to catch gross regressions.",
            denials.len()
        );
    }

    store.close().expect("close");
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
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        }, // Each batch is a sync
        ..StoreConfig::new("")
    };
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

    // Threshold: 2K events/sec — set well below the typical observed
    // throughput (35K-42K events/sec on a developer machine, ~12K on slow
    // shared CI runners) so the gate catches CATEGORY regressions only
    // (e.g., a refactor that introduces an O(n²) loop or removes batching
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

/// Verify multi-gate reports ALL failures, not just the first.
#[test]
fn multi_gate_collects_all_denials() {
    let ctx = PerfContext {
        event_count: 1000,
        events_per_sec: 1.0,      // way too slow
        query_us: 999_999.0,      // way too slow
        projection_ms: 999_999.0, // way too slow
    };

    let mut gates = GateSet::new();
    gates.push(WriteThroughputGate {
        min_events_per_sec: 1_000.0,
    });
    gates.push(QueryLatencyGate {
        max_us_per_query: 50_000.0,
    });
    gates.push(ProjectionGate { max_ms: 5_000.0 });

    let denials = gates.evaluate_all(&ctx);
    assert_eq!(
        denials.len(),
        3,
        "PROPERTY: evaluate_all must collect ALL 3 gate failures, not stop at the first denial.\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all().\n\
         Common causes: evaluate_all() short-circuiting on first Err like evaluate() does, \
         or not iterating all gates before returning.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );

    // Verify each denial points to the right gate and has actionable context
    assert_eq!(
        denials[0].gate, "write_throughput",
        "PROPERTY: First denial gate name must be 'write_throughput' (gates evaluated in order).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all() gate ordering.\n\
         Common causes: evaluate_all() not preserving insertion order, or \
         gate names being overwritten with a generic label.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
    assert_eq!(
        denials[1].gate, "query_latency",
        "PROPERTY: Second denial gate name must be 'query_latency' (gates evaluated in order).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all() gate ordering.\n\
         Common causes: evaluate_all() not preserving insertion order of gates.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
    assert_eq!(
        denials[2].gate,
        "projection_replay",
        "PROPERTY: Third denial gate name must be 'projection_replay' (gates evaluated in order).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all() gate ordering.\n\
         Common causes: evaluate_all() not preserving insertion order of gates.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );

    // Verify context has the "investigate" pointers
    assert!(
        denials[0].message.contains("writer.rs"),
        "PROPERTY: WriteThroughputGate denial must point to src/store/write/writer.rs for investigation.\n\
         Investigate: WriteThroughputGate::evaluate() denial message in tests/perf_gates.rs.\n\
         Common causes: Gate message missing 'writer.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
    assert!(
        denials[1].message.contains("index/mod.rs"),
        "PROPERTY: QueryLatencyGate denial must point to src/store/index/mod.rs for investigation.\n\
         Investigate: QueryLatencyGate::evaluate() denial message in tests/perf_gates.rs.\n\
         Common causes: Gate message missing 'index.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
    assert!(
        denials[2].message.contains("scan.rs"),
        "PROPERTY: ProjectionGate denial must point to src/store/segment/scan.rs for investigation.\n\
         Investigate: ProjectionGate::evaluate() denial message in tests/perf_gates.rs.\n\
         Common causes: Gate message missing 'scan.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
}

// ================================================================
// CORRECTNESS GATES: the library uses the same guard primitives to express
// resilience assertions. These are code-level tripwires, not an independent
// production certification system.
// ================================================================

/// Context for correctness gates — collected by exercising the store
/// under adversarial conditions.
struct CorrectnessContext {
    /// After fd eviction + re-read, does the data round-trip?
    fd_eviction_round_trips: bool,
    /// After segment rotation, can we still read old events?
    cross_segment_reads_ok: bool,
    /// Does CAS actually reject stale sequences?
    cas_rejects_stale: bool,
    /// Does idempotency return the same event_id?
    idempotency_deduplicates: bool,
    /// Can cursors see every event (including global_sequence 0)?
    cursor_sees_all_events: bool,
    /// Does snapshot produce a bootable store?
    snapshot_boots: bool,
}

struct FdEvictionGate;
impl Gate<CorrectnessContext> for FdEvictionGate {
    fn name(&self) -> &'static str {
        "fd_eviction_integrity"
    }
    fn evaluate(&self, ctx: &CorrectnessContext) -> Result<(), Denial> {
        if ctx.fd_eviction_round_trips {
            Ok(())
        } else {
            Err(Denial::new(
                "fd_eviction_integrity",
                "Data corrupted after FD cache eviction. \
                 Investigate: src/store/segment/scan.rs get_fd() LRU eviction, \
                 try_clone() correctness.",
            ))
        }
    }
}

struct CrossSegmentGate;
impl Gate<CorrectnessContext> for CrossSegmentGate {
    fn name(&self) -> &'static str {
        "cross_segment_reads"
    }
    fn evaluate(&self, ctx: &CorrectnessContext) -> Result<(), Denial> {
        if ctx.cross_segment_reads_ok {
            Ok(())
        } else {
            Err(Denial::new(
                "cross_segment_reads",
                "Cannot read events across segment boundaries. \
                 Investigate: src/store/write/writer.rs STEP 7 rotation, \
                 src/store/segment/scan.rs read_entry offset calculation.",
            ))
        }
    }
}

struct CasGate;
impl Gate<CorrectnessContext> for CasGate {
    fn name(&self) -> &'static str {
        "cas_correctness"
    }
    fn evaluate(&self, ctx: &CorrectnessContext) -> Result<(), Denial> {
        if ctx.cas_rejects_stale {
            Ok(())
        } else {
            Err(Denial::new(
                "cas_correctness",
                "CAS did NOT reject a stale expected_sequence. \
                 Investigate: src/store/mod.rs append_with_options CAS check.",
            ))
        }
    }
}

struct IdempotencyGate;
impl Gate<CorrectnessContext> for IdempotencyGate {
    fn name(&self) -> &'static str {
        "idempotency"
    }
    fn evaluate(&self, ctx: &CorrectnessContext) -> Result<(), Denial> {
        if ctx.idempotency_deduplicates {
            Ok(())
        } else {
            Err(Denial::new(
                "idempotency",
                "Idempotency key did NOT deduplicate. \
                 Investigate: src/store/mod.rs append_with_options idempotency check.",
            ))
        }
    }
}

struct CursorCompletenessGate;
impl Gate<CorrectnessContext> for CursorCompletenessGate {
    fn name(&self) -> &'static str {
        "cursor_completeness"
    }
    fn evaluate(&self, ctx: &CorrectnessContext) -> Result<(), Denial> {
        if ctx.cursor_sees_all_events {
            Ok(())
        } else {
            Err(Denial::new(
                "cursor_completeness",
                "Cursor missed events (possibly global_sequence=0). \
                 Investigate: src/store/delivery/cursor.rs poll() started flag.",
            ))
        }
    }
}

struct SnapshotBootGate;
impl Gate<CorrectnessContext> for SnapshotBootGate {
    fn name(&self) -> &'static str {
        "snapshot_bootable"
    }
    fn evaluate(&self, ctx: &CorrectnessContext) -> Result<(), Denial> {
        if ctx.snapshot_boots {
            Ok(())
        } else {
            Err(Denial::new(
                "snapshot_bootable",
                "Snapshot did not produce a bootable store. \
                 Investigate: src/store/mod.rs snapshot(), src/store/segment/scan.rs scan_segment.",
            ))
        }
    }
}

/// THE CORRECTNESS SELF-TEST.
/// The library exercises itself under adversarial conditions, collects
/// the results, then feeds them through the shared guard primitives.
/// Every denial points to the likely bug site; the comments do not claim
/// stronger proof than the exercised probes provide.
#[test]
fn correctness_gates_self_validate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512, // tiny → many segments
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        fd_budget: 2, // tiny → forces LRU eviction
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("correctness:entity", "correctness:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 50u64;

    // Populate with enough events to trigger segment rotation + fd eviction
    for i in 0..n {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");

    // --- Probe 1: FD eviction round-trip ---
    let entries = store.stream("correctness:entity");
    let first = store.get(entries[0].event_id);
    let last = store.get(entries[entries.len() - 1].event_id);
    let first_again = store.get(entries[0].event_id); // re-read after eviction
    let fd_eviction_round_trips = first.is_ok()
        && last.is_ok()
        && first_again.is_ok()
        && first.as_ref().expect("ok").event.event_id()
            == first_again.as_ref().expect("ok").event.event_id();

    // --- Probe 2: Cross-segment reads ---
    let segment_count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();
    let cross_segment_reads_ok = segment_count > 1 && entries.len() == n as usize;

    // --- Probe 3: CAS rejection ---
    store
        .append(&coord, kind, &serde_json::json!({"extra": true}))
        .expect("one more");
    let cas_result = store.append_with_options(
        &coord,
        kind,
        &serde_json::json!({"cas": "stale"}),
        batpak::store::AppendOptions {
            expected_sequence: Some(0), // stale
            ..Default::default()
        },
    );
    let cas_rejects_stale = cas_result.is_err();

    // --- Probe 4: Idempotency ---
    let idem_key: u128 = 0xCAFE_BABE_DEAD_BEEF_1234_5678_9ABC_DEF0;
    let idem_opts = batpak::store::AppendOptions {
        idempotency_key: Some(idem_key),
        ..Default::default()
    };
    let r1 = store
        .append_with_options(&coord, kind, &serde_json::json!({"x": 1}), idem_opts)
        .expect("first idem");
    let r2 = store
        .append_with_options(&coord, kind, &serde_json::json!({"x": 2}), idem_opts)
        .expect("second idem");
    let idempotency_deduplicates = r1.event_id == r2.event_id;

    // --- Probe 5: Cursor completeness ---
    let coord2 = Coordinate::new("cursor:test", "correctness:scope").expect("valid");
    for i in 0..5 {
        store
            .append(&coord2, kind, &serde_json::json!({"c": i}))
            .expect("append");
    }
    let region = Region::entity("cursor:test");
    let mut cursor = store.cursor_guaranteed(&region);
    let mut cursor_count = 0;
    while cursor.poll().is_some() {
        cursor_count += 1;
    }
    let cursor_sees_all_events = cursor_count == 5;

    // --- Probe 6: Snapshot bootability ---
    let snap_dir = TempDir::new().expect("snap dir");
    store.snapshot(snap_dir.path()).expect("snapshot");
    let snap_config = StoreConfig {
        data_dir: snap_dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let snap_boot = Store::open(snap_config);
    let snapshot_boots = snap_boot.is_ok();
    if let Ok(s) = snap_boot {
        let _ = s.close();
    }

    // --- Feed through the shared guard primitives ---
    let ctx = CorrectnessContext {
        fd_eviction_round_trips,
        cross_segment_reads_ok,
        cas_rejects_stale,
        idempotency_deduplicates,
        cursor_sees_all_events,
        snapshot_boots,
    };

    let mut gates = GateSet::new();
    gates.push(FdEvictionGate);
    gates.push(CrossSegmentGate);
    gates.push(CasGate);
    gates.push(IdempotencyGate);
    gates.push(CursorCompletenessGate);
    gates.push(SnapshotBootGate);

    let denials = gates.evaluate_all(&ctx);

    eprintln!("\n  CORRECTNESS GATE REPORT:");
    eprintln!("    fd_eviction_round_trips:   {fd_eviction_round_trips}");
    eprintln!("    cross_segment_reads:        {cross_segment_reads_ok}");
    eprintln!("    cas_rejects_stale:          {cas_rejects_stale}");
    eprintln!("    idempotency_deduplicates:   {idempotency_deduplicates}");
    eprintln!("    cursor_sees_all_events:     {cursor_sees_all_events}");
    eprintln!("    snapshot_boots:             {snapshot_boots}");

    if denials.is_empty() {
        eprintln!("    Result: ALL 6 CORRECTNESS GATES PASSED");
    } else {
        eprintln!("    Result: {} CORRECTNESS GATES FAILED:", denials.len());
        for d in &denials {
            eprintln!("      [{gate}] {msg}", gate = d.gate, msg = d.message);
        }
        panic!(
            "CORRECTNESS SELF-TEST FAILED: {} gate(s) denied.\n\
             Each denial above points to the likely file + function to investigate.\n\
             This is the library stress-testing itself with the shared guard primitives.",
            denials.len()
        );
    }

    store.close().expect("close");
}

/// Append throughput gate: dedicated catastrophic-regression harness.
#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Asserts events/sec on shared hardware."]
fn append_throughput_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("gate:append", "gate:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 5_000u64;

    let start = Instant::now();
    for i in 0..n {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    let elapsed = start.elapsed();
    let events_per_sec = n as f64 / elapsed.as_secs_f64();

    let mut gates = GateSet::new();
    // CI threshold: 5K events/sec minimum (generous for slow runners)
    gates.push(WriteThroughputGate {
        min_events_per_sec: 5_000.0,
    });

    let ctx = PerfContext {
        event_count: n,
        events_per_sec,
        query_us: 0.0,
        projection_ms: 0.0,
    };
    let denials = gates.evaluate_all(&ctx);

    eprintln!("\n  APPEND THROUGHPUT GATE ({n} events):");
    eprintln!("    Throughput: {events_per_sec:.0} events/sec");

    if !denials.is_empty() {
        for d in &denials {
            eprintln!("    DENIED: [{gate}] {msg}", gate = d.gate, msg = d.message);
        }
        panic!(
            "APPEND THROUGHPUT GATE FAILED: {:.0} events/sec < 5000 minimum.\n\
             Investigate: src/store/write/writer.rs handle_append.",
            events_per_sec
        );
    }

    store.close().expect("close");
}

/// Projection latency gate: dedicated catastrophic-regression harness.
#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Asserts projection latency in ms on shared hardware."]
fn projection_latency_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("gate:proj", "gate:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 1_000u64;

    for i in 0..n {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    let start = Instant::now();
    let _: Option<BenchCounter> = store
        .project("gate:proj", &batpak::store::Freshness::Consistent)
        .expect("project");
    let projection_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut gates = GateSet::new();
    // CI threshold: 5s max for 1K event projection (generous)
    gates.push(ProjectionGate { max_ms: 5_000.0 });

    let ctx = PerfContext {
        event_count: n,
        events_per_sec: 0.0,
        query_us: 0.0,
        projection_ms,
    };
    let denials = gates.evaluate_all(&ctx);

    eprintln!("\n  PROJECTION LATENCY GATE ({n} events):");
    eprintln!("    Replay: {projection_ms:.1} ms");

    if !denials.is_empty() {
        for d in &denials {
            eprintln!("    DENIED: [{gate}] {msg}", gate = d.gate, msg = d.message);
        }
        panic!(
            "PROJECTION LATENCY GATE FAILED: {:.1}ms > 5000ms max.\n\
             Investigate: src/store/projection/flow.rs project(), src/store/segment/scan.rs.",
            projection_ms
        );
    }

    store.close().expect("close");
}

/// Projection cold-path gate: measures first-pass projection on a freshly
/// reopened store. This isolates the cold projection cost from warm caches.
#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Asserts cold-path projection latency."]
fn projection_cold_path_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("gate:cold-proj", "gate:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 1_000u64;

    for i in 0..n {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.close().expect("close");

    // Reopen for a true cold path
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("reopen");

    let start = Instant::now();
    let _: Option<BenchCounter> = store
        .project("gate:cold-proj", &batpak::store::Freshness::Consistent)
        .expect("project cold path");
    let projection_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut gates = GateSet::new();
    // Cold-path threshold: 50ms for 1K events (generous for CI,
    // observed ~9ms on dev hardware after projection-specific reader).
    gates.push(ProjectionGate { max_ms: 50.0 });

    let ctx = PerfContext {
        event_count: n,
        events_per_sec: 0.0,
        query_us: 0.0,
        projection_ms,
    };
    let denials = gates.evaluate_all(&ctx);

    eprintln!("\n  PROJECTION COLD-PATH GATE ({n} events):");
    eprintln!("    First-pass replay: {projection_ms:.1} ms");

    if !denials.is_empty() {
        for d in &denials {
            eprintln!("    DENIED: [{gate}] {msg}", gate = d.gate, msg = d.message);
        }
        panic!(
            "PROJECTION COLD-PATH GATE FAILED: {:.1}ms > 50ms max.\n\
             Investigate: src/store/projection/flow.rs, src/store/segment/scan.rs read_events_batch.",
            projection_ms
        );
    }

    store.close().expect("close");
}

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
        matches!(report.path, batpak::store::OpenIndexPath::Mmap),
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
        matches!(report.path, batpak::store::OpenIndexPath::Rebuild),
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

/// Verify the correctness gates actually FIRE when properties are violated.
/// Without this, a broken gate that always passes would be invisible.
#[test]
fn correctness_gates_fire_on_violations() {
    let broken_ctx = CorrectnessContext {
        fd_eviction_round_trips: false,
        cross_segment_reads_ok: false,
        cas_rejects_stale: false,
        idempotency_deduplicates: false,
        cursor_sees_all_events: false,
        snapshot_boots: false,
    };

    let mut gates = GateSet::new();
    gates.push(FdEvictionGate);
    gates.push(CrossSegmentGate);
    gates.push(CasGate);
    gates.push(IdempotencyGate);
    gates.push(CursorCompletenessGate);
    gates.push(SnapshotBootGate);

    let denials = gates.evaluate_all(&broken_ctx);
    assert_eq!(
        denials.len(),
        6,
        "PROPERTY: All 6 correctness gates must fire when all properties are violated.\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all() tests/perf_gates.rs correctness gates.\n\
         Common causes: evaluate_all() stopping early after fewer than 6 denials, or \
         one of the correctness gates returning Ok even when the property is false.\n\
         Run: cargo test --test perf_gates correctness_gates_fire_on_violations"
    );

    // Every denial should contain an investigation pointer
    for d in &denials {
        assert!(
            d.message.contains("Investigate:"),
            "PROPERTY: Every correctness gate denial must include an 'Investigate:' pointer to a source file.\n\
             Investigate: tests/perf_gates.rs [{gate}] Gate::evaluate() denial message: {msg}.\n\
             Common causes: Gate denial message not including the 'Investigate:' keyword, or \
             denial constructed with an empty message string.\n\
             Run: cargo test --test perf_gates correctness_gates_fire_on_violations",
            gate = d.gate,
            msg = d.message
        );
    }
}
