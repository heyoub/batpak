// justifies: INV-TEST-PANIC-AS-ASSERTION; fuzz+chaos feedback policy harness in tests/fuzz_chaos_feedback.rs reports shrink traces to stderr and uses unwrap/panic as assertion style while evaluating proptest-bounded gate metrics.
#![allow(clippy::panic, clippy::print_stderr, clippy::unwrap_used)]
//! Fuzz + Chaos Feedback Loop: the library uses its guard primitives as a
//! reusable harness to evaluate fuzz and chaos testing results.
//! Harness pattern: Property Harness (feedback gate lane).
//! Performance scores here are screening signals for extended load probes,
//! not standalone production authority.
//!
//! PROVES: LAW-001 (No Fake Success), LAW-005 (Resilience Under Chaos)
//! CATCHES: FM-013 (Coverage Mirage), FM-019 (Chaos Gap) — a regression where
//! fuzz/chaos probes pass a gate they should deny (or panic) goes uncaught.
//! SEEDED: deterministic FUZZ_CHAOS_SEED (default 0) drives the extended fuzz loop.
//!
//! DEFENDS: FM-013 (Coverage Mirage), FM-019 (Chaos Gap)
//! INVARIANTS: INV-FAULT-INJECT-GATED (state machine), INV-CONCURRENCY-SCHEDULE-PROOF (concurrent), INV-BATCH-CRASH-RECOVERY (temporal)
//!
//! This is the feedback loop:
//!   1. Run fuzz + chaos probes
//!   2. Feed results through the guard harness
//!   3. If all gates pass, launch extended load fuzz + chaos
//!   4. Extended results feed back through stricter gates
//!
//! Run with: cargo test --test fuzz_chaos_feedback --all-features --release -- --ignored

#[path = "support/fuzz_chaos_feedback.rs"]
mod fcf_support;
mod support;

use fcf_support::{
    ChaosIntegrityGate, ChaosSubscriptionGate, ChaosWriteGate, FuzzChaosContext, FuzzPanicGate,
    FuzzThroughputGate,
};
use support::prelude::*;

// ============================================================
// PHASE 4: The feedback loop test
// ============================================================

#[test]
#[ignore = "long-running fuzz+chaos integration loop (~minutes). Run on demand via `cargo test --test fuzz_chaos_feedback -- --ignored` or via the dedicated chaos CI job. Excluded from `cargo xtask ci` to keep the inner loop under 30 seconds."]
fn fuzz_chaos_feedback_loop() {
    eprintln!("\n  ========================================");
    eprintln!("  FUZZ + CHAOS FEEDBACK LOOP (Phase 1)");
    eprintln!("  ========================================");

    // --- Run fuzz probes ---
    let (frame_ops, wire_ops, combinator_ops, fuzz_panics) = fcf_support::run_fuzz_probes();
    eprintln!("  Fuzz: frame_decode   {frame_ops:.0} ops/sec");
    eprintln!("  Fuzz: wire roundtrip {wire_ops:.0} ops/sec");
    eprintln!("  Fuzz: combinators    {combinator_ops:.0} ops/sec");
    eprintln!("  Fuzz: panics         {fuzz_panics}");

    // --- Run chaos probes ---
    let (write_tp, write_err, cas_ok, integrity_ok, rot_loss, sub_rate, cursor_ok) =
        fcf_support::run_chaos_probes();
    eprintln!("  Chaos: write throughput  {write_tp:.0} events/sec");
    eprintln!("  Chaos: write errors      {write_err}");
    eprintln!("  Chaos: CAS correct       {cas_ok}");
    eprintln!("  Chaos: integrity ok      {integrity_ok}");
    eprintln!("  Chaos: rotation loss     {rot_loss}");
    eprintln!("  Chaos: sub delivery      {:.1}%", sub_rate * 100.0);
    eprintln!("  Chaos: cursor complete   {cursor_ok}");

    let ctx = FuzzChaosContext {
        frame_decode_fuzz_ops_per_sec: frame_ops,
        wire_roundtrip_ops_per_sec: wire_ops,
        outcome_combinator_ops_per_sec: combinator_ops,
        fuzz_panics,
        concurrent_write_throughput: write_tp,
        concurrent_write_errors: write_err,
        cas_contention_correct: cas_ok,
        data_integrity_after_corruption: integrity_ok,
        segment_rotation_data_loss: rot_loss,
        subscription_delivery_rate: sub_rate,
        cursor_completeness: cursor_ok,
    };

    // --- Build gates (generous thresholds for CI) ---
    let mut gates = GateSet::new();
    gates.push(FuzzPanicGate);
    gates.push(FuzzThroughputGate {
        min_frame_ops: 10_000.0,
        min_wire_ops: 10_000.0,
        min_combinator_ops: 50_000.0,
    });
    gates.push(ChaosWriteGate {
        min_throughput: 500.0,
    });
    gates.push(ChaosIntegrityGate);
    gates.push(ChaosSubscriptionGate {
        min_delivery_rate: 0.01,
    }); // lossy, just needs >0

    // --- Evaluate ALL gates ---
    let denials = gates.evaluate_all(&ctx);

    if denials.is_empty() {
        eprintln!("  Result: ALL FUZZ+CHAOS GATES PASSED");
        eprintln!("  ========================================");
        eprintln!("  Launching EXTENDED load fuzz + chaos...");
        eprintln!("  ========================================");

        // PHASE 2: Extended runs — only when Phase 1 passes
        fcf_support::run_extended_fuzz_chaos();
    } else {
        eprintln!("  Result: {} GATES FAILED:", denials.len());
        for d in &denials {
            eprintln!("    [{gate}] {msg}", gate = d.gate, msg = d.message);
            for (k, v) in &d.context {
                eprintln!("      {k} = {v}");
            }
        }
        panic!(
            "FUZZ+CHAOS FEEDBACK LOOP FAILED: {} gate(s) denied.\n\
             Fix the issues above, then re-run. Extended fuzz+chaos will \
             NOT launch until all gates pass.",
            denials.len()
        );
    }
}
