//! PROVES: LAW-004 (Composition Over Construction — quadratic dogfooding) for the
//! write-throughput, query-latency, and projection-replay catastrophic-regression
//! gates, plus the multi-gate `evaluate_all` ordering/feedback contract.
//! Enforces INV-PERFORMANCE-GATES-ENFORCED (catastrophic-regression gates fire)
//! and INV-FRONTIER-APPEND-GATE-HONORED (the append-throughput gate path).
//! CATCHES: gross write/query/projection regressions and `evaluate_all` drift
//! (short-circuiting, gate-order loss, or missing investigation pointers).
//! SEEDED: deterministic 1K/5K event corpora appended in-loop; thresholds are
//! intentionally generous CI floors, not precision benchmark authority.
//!
//! Split out of the original 1322-line `tests/perf_gates.rs`. This binary owns
//! every test that constructs a [`PerfContext`]: all three gate types
//! (`WriteThroughputGate`, `QueryLatencyGate`, `ProjectionGate`) read distinct
//! `PerfContext` fields, so they MUST share a binary to keep every field live.
//! Harness pattern: Property Harness (catastrophic threshold lane).

use batpak::store::{Store, StoreConfig};
use batpak_testkit::prelude::*;
use std::io::Write;
use std::time::Instant;
use tempfile::TempDir;

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
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("perf-gates-bench-counter");

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

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Uses Instant::now() for write/query/projection timing; flakes on shared CI runners."]
fn multi_gate_performance_feedback() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("perf:entity", "perf:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 1_000u64;

    // Measure write throughput
    let write_start = Instant::now();
    for i in 0..n {
        let _ = store
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
    let mut report = format!("\n  SELF-BENCHMARK REPORT ({n} events):");
    report.push_str(&format!(
        "\n    Write throughput:  {events_per_sec:.0} events/sec"
    ));
    report.push_str(&format!("\n    Query latency:     {query_us:.1} µs/query"));
    report.push_str(&format!("\n    Projection replay: {projection_ms:.1} ms"));
    for d in &denials {
        report.push_str(&format!(
            "\n      [{gate}] {msg}",
            gate = d.gate,
            msg = d.message
        ));
        for (k, v) in &d.context {
            report.push_str(&format!("\n        {k} = {v}"));
        }
    }
    let _ = writeln!(std::io::stderr(), "{report}");

    assert!(
        denials.is_empty(),
        "SELF-BENCHMARK FAILED: {} performance gate(s) denied.\n\
         The denials above point to the likely investigation sites.\n\
         This is the library using the shared guard primitives to catch gross regressions.{report}",
        denials.len()
    );

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
         Run: cargo test --test perf_gates_throughput_latency multi_gate_collects_all_denials"
    );

    // Verify each denial points to the right gate and has actionable context
    assert_eq!(
        denials[0].gate, "write_throughput",
        "PROPERTY: First denial gate name must be 'write_throughput' (gates evaluated in order).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all() gate ordering.\n\
         Common causes: evaluate_all() not preserving insertion order, or \
         gate names being overwritten with a generic label.\n\
         Run: cargo test --test perf_gates_throughput_latency multi_gate_collects_all_denials"
    );
    assert_eq!(
        denials[1].gate, "query_latency",
        "PROPERTY: Second denial gate name must be 'query_latency' (gates evaluated in order).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all() gate ordering.\n\
         Common causes: evaluate_all() not preserving insertion order of gates.\n\
         Run: cargo test --test perf_gates_throughput_latency multi_gate_collects_all_denials"
    );
    assert_eq!(
        denials[2].gate,
        "projection_replay",
        "PROPERTY: Third denial gate name must be 'projection_replay' (gates evaluated in order).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate_all() gate ordering.\n\
         Common causes: evaluate_all() not preserving insertion order of gates.\n\
         Run: cargo test --test perf_gates_throughput_latency multi_gate_collects_all_denials"
    );

    // Verify context has the "investigate" pointers
    assert!(
        denials[0].message.contains("writer.rs"),
        "PROPERTY: WriteThroughputGate denial must point to src/store/write/writer.rs for investigation.\n\
         Investigate: WriteThroughputGate::evaluate() denial message in tests/perf_gates_throughput_latency.rs.\n\
         Common causes: Gate message missing 'writer.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates_throughput_latency multi_gate_collects_all_denials"
    );
    assert!(
        denials[1].message.contains("index/mod.rs"),
        "PROPERTY: QueryLatencyGate denial must point to src/store/index/mod.rs for investigation.\n\
         Investigate: QueryLatencyGate::evaluate() denial message in tests/perf_gates_throughput_latency.rs.\n\
         Common causes: Gate message missing 'index.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates_throughput_latency multi_gate_collects_all_denials"
    );
    assert!(
        denials[2].message.contains("scan.rs"),
        "PROPERTY: ProjectionGate denial must point to src/store/segment/scan.rs for investigation.\n\
         Investigate: ProjectionGate::evaluate() denial message in tests/perf_gates_throughput_latency.rs.\n\
         Common causes: Gate message missing 'scan.rs' investigation pointer, or \
         message format changed without updating this assertion.\n\
         Run: cargo test --test perf_gates_throughput_latency multi_gate_collects_all_denials"
    );
}

/// Append throughput gate: dedicated catastrophic-regression harness.
#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Asserts events/sec on shared hardware."]
fn append_throughput_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("gate:append", "gate:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 5_000u64;

    let start = Instant::now();
    for i in 0..n {
        let _ = store
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

    let mut report = format!("\n  APPEND THROUGHPUT GATE ({n} events):");
    report.push_str(&format!("\n    Throughput: {events_per_sec:.0} events/sec"));
    for d in &denials {
        report.push_str(&format!(
            "\n    DENIED: [{gate}] {msg}",
            gate = d.gate,
            msg = d.message
        ));
    }
    let _ = writeln!(std::io::stderr(), "{report}");

    assert!(
        denials.is_empty(),
        "APPEND THROUGHPUT GATE FAILED: {events_per_sec:.0} events/sec < 5000 minimum.\n\
         Investigate: src/store/write/writer.rs handle_append.{report}"
    );

    store.close().expect("close");
}

/// Projection latency gate: dedicated catastrophic-regression harness.
#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Asserts projection latency in ms on shared hardware."]
fn projection_latency_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("gate:proj", "gate:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 1_000u64;

    for i in 0..n {
        let _ = store
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

    let mut report = format!("\n  PROJECTION LATENCY GATE ({n} events):");
    report.push_str(&format!("\n    Replay: {projection_ms:.1} ms"));
    for d in &denials {
        report.push_str(&format!(
            "\n    DENIED: [{gate}] {msg}",
            gate = d.gate,
            msg = d.message
        ));
    }
    let _ = writeln!(std::io::stderr(), "{report}");

    assert!(
        denials.is_empty(),
        "PROJECTION LATENCY GATE FAILED: {projection_ms:.1}ms > 5000ms max.\n\
         Investigate: src/store/projection/flow.rs project(), src/store/segment/scan.rs.{report}"
    );

    store.close().expect("close");
}

/// Projection cold-path gate: measures first-pass projection on a freshly
/// reopened store. This isolates the cold projection cost from warm caches.
#[test]
#[ignore = "hardware-dependent perf gate — run via `cargo xtask perf-gates`. Asserts cold-path projection latency."]
fn projection_cold_path_gate() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("gate:cold-proj", "gate:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 1_000u64;

    for i in 0..n {
        let _ = store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.close().expect("close");

    // Reopen for a true cold path
    let config = StoreConfig::new(dir.path());
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

    let mut report = format!("\n  PROJECTION COLD-PATH GATE ({n} events):");
    report.push_str(&format!("\n    First-pass replay: {projection_ms:.1} ms"));
    for d in &denials {
        report.push_str(&format!(
            "\n    DENIED: [{gate}] {msg}",
            gate = d.gate,
            msg = d.message
        ));
    }
    let _ = writeln!(std::io::stderr(), "{report}");

    assert!(
        denials.is_empty(),
        "PROJECTION COLD-PATH GATE FAILED: {projection_ms:.1}ms > 50ms max.\n\
         Investigate: src/store/projection/flow.rs, src/store/segment/scan.rs read_events_batch.{report}"
    );

    store.close().expect("close");
}
