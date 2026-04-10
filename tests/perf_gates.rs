#![allow(clippy::panic, clippy::print_stderr, clippy::cast_possible_truncation)] // benchmark reporting uses eprintln; gate failures use panic
//! Performance gate tests: the library dogfoods its own Gate/Pipeline system
//! to enforce its own throughput, latency, and correctness thresholds.
//! [SPEC:tests/perf_gates.rs]
//!
//! PROVES: LAW-004 (Composition Over Construction — quadratic dogfooding)
//! DEFENDS: FM-013 (Coverage Mirage — gates test themselves), FM-007 (Island Syndrome)
//! INVARIANTS: INV-PERF (performance thresholds), INV-STATE (gate evaluation)
//!
//! This IS the "free battery factory" philosophy: the same Gate/Pipeline system
//! that products use to enforce business rules, the library uses to enforce
//! its own performance AND correctness characteristics.
//! If gates work, this test passes. If this test passes, gates work.
//! Quadratic feedback — the deepest kind of dogfood.

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use std::time::Instant;
use tempfile::TempDir;

/// A Gate that checks cold-start performance.
/// This is not a unit test — it's the library testing itself with its own tools.
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
                     src/store/reader.rs scan_segment.",
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

    // Dogfood: use our own Gate system to validate performance
    let mut gates = GateSet::new();
    // SPEC target: cold start < 200ms for 1K events on production hardware.
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
                    The library's own Gate system detected a performance regression.\n\
                    Context: {:?}",
                denial, denial.context
            );
        }
    }

    store.sync().expect("sync");
}

/// Verify the Gate system correctly rejects slow cold starts.
/// This tests that the dogfood mechanism itself works — it would catch
/// a broken Gate that always passes.
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
                     Investigate: src/store/writer.rs handle_append (10-step commit), \
                     src/store/segment.rs write_frame, CRC overhead.",
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
                     Investigate: src/store/index.rs query() DashMap scan, \
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
                     Investigate: src/store/projection_flow.rs project(), \
                     src/store/reader.rs read_entry deserialization.",
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

impl EventSourced<serde_json::Value> for BenchCounter {
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
             The denials above tell you exactly where to look.\n\
             This is the library using its own Gate system to enforce its own quality.",
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
                     Investigate: src/store/writer.rs handle_append_batch, \
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
fn batch_throughput_performance_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync_every_n_events: 1, // Each batch is a sync
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

    // Batch should be significantly faster than single append
    // Threshold: 5K events/sec (generous for CI)
    let mut gates = GateSet::new();
    gates.push(BatchThroughputGate {
        min_events_per_sec: 5_000.0,
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
        "PROPERTY: WriteThroughputGate denial must point to src/store/writer.rs for investigation.\n\
         Investigate: WriteThroughputGate::evaluate() denial message in tests/perf_gates.rs.\n\
         Common causes: Gate message missing 'writer.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
    assert!(
        denials[1].message.contains("index.rs"),
        "PROPERTY: QueryLatencyGate denial must point to src/store/index.rs for investigation.\n\
         Investigate: QueryLatencyGate::evaluate() denial message in tests/perf_gates.rs.\n\
         Common causes: Gate message missing 'index.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
    assert!(
        denials[2].message.contains("reader.rs"),
        "PROPERTY: ProjectionGate denial must point to src/store/reader.rs for investigation.\n\
         Investigate: ProjectionGate::evaluate() denial message in tests/perf_gates.rs.\n\
         Common causes: Gate message missing 'reader.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates multi_gate_collects_all_denials"
    );
}

// ================================================================
// CORRECTNESS GATES: the library uses its own Gate system to verify
// its own resilience properties. Not just "does it work?" but
// "does it KEEP working when things go wrong?"
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
                 Investigate: src/store/reader.rs get_fd() LRU eviction, \
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
                 Investigate: src/store/writer.rs STEP 7 rotation, \
                 src/store/reader.rs read_entry offset calculation.",
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
                 Investigate: src/store/cursor.rs poll() started flag.",
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
                 Investigate: src/store/mod.rs snapshot(), src/store/reader.rs scan_segment.",
            ))
        }
    }
}

/// THE CORRECTNESS SELF-TEST.
/// The library exercises itself under adversarial conditions, collects
/// the results, then feeds them through its own Gate system.
/// Every denial tells you EXACTLY where the bug is.
#[test]
fn correctness_gates_self_validate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512, // tiny → many segments
        sync_every_n_events: 1,
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
    let mut cursor = store.cursor(&region);
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

    // --- Feed through our own Gate system ---
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
             Each denial above tells you the exact file + function to investigate.\n\
             This is the library stress-testing itself with its own Gate system.",
            denials.len()
        );
    }

    store.close().expect("close");
}

/// Append throughput gate: dedicated test using the library's own Gate system.
/// [SPEC:tests/perf_gates.rs — BN5 append throughput gate]
#[test]
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
             Investigate: src/store/writer.rs handle_append.",
            events_per_sec
        );
    }

    store.close().expect("close");
}

/// Projection latency gate: dedicated test using the library's own Gate system.
/// [SPEC:tests/perf_gates.rs — BN5 projection latency gate]
#[test]
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
             Investigate: src/store/projection_flow.rs project(), src/store/reader.rs.",
            projection_ms
        );
    }

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
