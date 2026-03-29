#![allow(
    clippy::panic,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::inconsistent_digit_grouping,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::unnecessary_cast,
    clippy::needless_borrows_for_generic_args,
    clippy::disallowed_methods // test harness uses thread::spawn for chaos probes
)]
//! Fuzz + Chaos Feedback Loop: the library dogfoods its own Gate system
//! to evaluate fuzz and chaos testing results. Performance scores gate
//! whether extended load fuzz/chaos runs are launched.
//!
//! PROVES: LAW-001 (No Fake Success), LAW-005 (Resilience Under Chaos)
//! DEFENDS: FM-013 (Coverage Mirage), FM-019 (Chaos Gap)
//! INVARIANTS: INV-STATE (state machine), INV-CONC (concurrent), INV-TEMP (temporal)
//!
//! This is the quadratic feedback loop:
//!   1. Run fuzz + chaos probes
//!   2. Feed results through Gate system
//!   3. If all gates pass, launch extended load fuzz + chaos
//!   4. Extended results feed back through stricter gates
//!
//! Run with: cargo test --test fuzz_chaos_feedback --all-features --release
//! [SPEC:tests/fuzz_chaos_feedback.rs]

use batpak::prelude::*;
use batpak::store::segment::{frame_decode, frame_encode};
use batpak::store::{AppendOptions, Store, StoreConfig};
use rand::Rng;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

// ============================================================
// PHASE 1: Probe Context — collect metrics from fuzz + chaos runs
// ============================================================

struct FuzzChaosContext {
    // Fuzz metrics
    frame_decode_fuzz_ops_per_sec: f64,
    wire_roundtrip_ops_per_sec: f64,
    outcome_combinator_ops_per_sec: f64,
    fuzz_panics: u64,

    // Chaos metrics
    concurrent_write_throughput: f64,
    concurrent_write_errors: u64,
    cas_contention_correct: bool,
    data_integrity_after_corruption: bool,
    segment_rotation_data_loss: u64,
    subscription_delivery_rate: f64,
    cursor_completeness: bool,
}

// ============================================================
// PHASE 2: Gates that evaluate fuzz + chaos results
// ============================================================

struct FuzzPanicGate;
impl Gate<FuzzChaosContext> for FuzzPanicGate {
    fn name(&self) -> &'static str {
        "fuzz_no_panics"
    }
    fn evaluate(&self, ctx: &FuzzChaosContext) -> Result<(), Denial> {
        if ctx.fuzz_panics == 0 {
            Ok(())
        } else {
            Err(Denial::new(
                "fuzz_no_panics",
                format!(
                    "{} panics detected during fuzz testing. \
                    Every fuzz target must handle arbitrary input without panic. \
                    Investigate: run PROPTEST_CASES=100000 to reproduce.",
                    ctx.fuzz_panics
                ),
            ))
        }
    }
}

struct FuzzThroughputGate {
    min_frame_ops: f64,
    min_wire_ops: f64,
    min_combinator_ops: f64,
}
impl Gate<FuzzChaosContext> for FuzzThroughputGate {
    fn name(&self) -> &'static str {
        "fuzz_throughput"
    }
    fn evaluate(&self, ctx: &FuzzChaosContext) -> Result<(), Denial> {
        if ctx.frame_decode_fuzz_ops_per_sec < self.min_frame_ops {
            return Err(Denial::new(
                "fuzz_throughput",
                format!(
                    "frame_decode fuzz {:.0} ops/sec < min {:.0}. \
                    Investigate: src/store/segment.rs frame_decode hot path.",
                    ctx.frame_decode_fuzz_ops_per_sec, self.min_frame_ops
                ),
            ));
        }
        if ctx.wire_roundtrip_ops_per_sec < self.min_wire_ops {
            return Err(Denial::new(
                "fuzz_throughput",
                format!(
                    "wire roundtrip fuzz {:.0} ops/sec < min {:.0}. \
                    Investigate: src/wire.rs serde visitors.",
                    ctx.wire_roundtrip_ops_per_sec, self.min_wire_ops
                ),
            ));
        }
        if ctx.outcome_combinator_ops_per_sec < self.min_combinator_ops {
            return Err(Denial::new(
                "fuzz_throughput",
                format!(
                    "outcome combinator fuzz {:.0} ops/sec < min {:.0}. \
                    Investigate: src/outcome/mod.rs and_then/map Batch recursion.",
                    ctx.outcome_combinator_ops_per_sec, self.min_combinator_ops
                ),
            ));
        }
        Ok(())
    }
}

struct ChaosWriteGate {
    min_throughput: f64,
}
impl Gate<FuzzChaosContext> for ChaosWriteGate {
    fn name(&self) -> &'static str {
        "chaos_write_resilience"
    }
    fn evaluate(&self, ctx: &FuzzChaosContext) -> Result<(), Denial> {
        if ctx.concurrent_write_errors > 0 {
            return Err(Denial::new(
                "chaos_write_resilience",
                format!(
                    "{} errors under concurrent write stress. \
                    Investigate: src/store/writer.rs lock ordering, channel capacity.",
                    ctx.concurrent_write_errors
                ),
            ));
        }
        if ctx.concurrent_write_throughput < self.min_throughput {
            return Err(Denial::new(
                "chaos_write_resilience",
                format!(
                    "Concurrent write throughput {:.0} events/sec < min {:.0}. \
                    Investigate: src/store/writer.rs contention.",
                    ctx.concurrent_write_throughput, self.min_throughput
                ),
            ));
        }
        Ok(())
    }
}

struct ChaosIntegrityGate;
impl Gate<FuzzChaosContext> for ChaosIntegrityGate {
    fn name(&self) -> &'static str {
        "chaos_data_integrity"
    }
    fn evaluate(&self, ctx: &FuzzChaosContext) -> Result<(), Denial> {
        if !ctx.cas_contention_correct {
            return Err(Denial::new(
                "chaos_data_integrity",
                "CAS contention produced incorrect results. \
                 Investigate: src/store/writer.rs CAS under entity lock.",
            ));
        }
        if !ctx.data_integrity_after_corruption {
            return Err(Denial::new(
                "chaos_data_integrity",
                "Store panicked or produced incorrect data after segment corruption. \
                 Investigate: src/store/segment.rs frame_decode, CRC validation.",
            ));
        }
        if ctx.segment_rotation_data_loss > 0 {
            return Err(Denial::new(
                "chaos_data_integrity",
                format!(
                    "{} events lost during rapid segment rotation. \
                    Investigate: src/store/writer.rs STEP 7 rotation.",
                    ctx.segment_rotation_data_loss
                ),
            ));
        }
        if !ctx.cursor_completeness {
            return Err(Denial::new(
                "chaos_data_integrity",
                "Cursor missed events or delivered duplicates. \
                 Investigate: src/store/cursor.rs poll() position tracking.",
            ));
        }
        Ok(())
    }
}

struct ChaosSubscriptionGate {
    min_delivery_rate: f64,
}
impl Gate<FuzzChaosContext> for ChaosSubscriptionGate {
    fn name(&self) -> &'static str {
        "chaos_subscription_health"
    }
    fn evaluate(&self, ctx: &FuzzChaosContext) -> Result<(), Denial> {
        if ctx.subscription_delivery_rate < self.min_delivery_rate {
            Err(Denial::new(
                "chaos_subscription_health",
                format!(
                    "Subscription delivery rate {:.1}% < min {:.1}%. \
                    Investigate: src/store/writer.rs broadcast, channel capacity.",
                    ctx.subscription_delivery_rate * 100.0,
                    self.min_delivery_rate * 100.0
                ),
            ))
        } else {
            Ok(())
        }
    }
}

// ============================================================
// PHASE 3: Run probes and collect metrics
// ============================================================

fn run_fuzz_probes() -> (f64, f64, f64, u64) {
    let mut panics = 0u64;

    // Probe: frame_decode throughput
    let n = 10_000;
    let payloads: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let s = format!("payload-{i}");
            frame_encode(&s).expect("encode")
        })
        .collect();

    let start = Instant::now();
    for p in &payloads {
        if frame_decode(p).is_err() {
            panics += 1;
        }
    }
    let frame_ops = n as f64 / start.elapsed().as_secs_f64();

    // Probe: wire u128 roundtrip throughput
    #[derive(serde::Serialize, serde::Deserialize)]
    struct WireProbe {
        #[serde(with = "batpak::wire::u128_bytes")]
        v: u128,
    }
    let start = Instant::now();
    for i in 0..n as u128 {
        let w = WireProbe { v: i };
        let bytes = rmp_serde::to_vec_named(&w).expect("ser");
        let _: WireProbe = rmp_serde::from_slice(&bytes).expect("de");
    }
    let wire_ops = n as f64 / start.elapsed().as_secs_f64();

    // Probe: outcome combinator throughput
    let start = Instant::now();
    for i in 0..n as i32 {
        let o = Outcome::Ok(i);
        let _ = o
            .and_then(|x| Outcome::Ok(x.wrapping_mul(2)))
            .map(|x| x.wrapping_add(1));
    }
    let combinator_ops = n as f64 / start.elapsed().as_secs_f64();

    (frame_ops, wire_ops, combinator_ops, panics)
}

fn run_chaos_probes() -> (f64, u64, bool, bool, u64, f64, bool) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 2048,
        sync_every_n_events: 10,
        fd_budget: 4,
        broadcast_capacity: 128,
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let kind = EventKind::custom(0xF, 1);

    // --- Concurrent write stress ---
    let n_threads = 4;
    let writes_per_thread = 100;
    let start = Instant::now();
    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let coord = Coordinate::new(&format!("probe:t{t}"), "probe:scope").expect("valid");
                let mut errors = 0u64;
                for i in 0..writes_per_thread {
                    if store
                        .append(&coord, kind, &serde_json::json!({"i": i}))
                        .is_err()
                    {
                        errors += 1;
                    }
                }
                errors
            })
        })
        .collect();

    let mut write_errors = 0u64;
    for h in handles {
        write_errors += h.join().expect("join");
    }
    let elapsed = start.elapsed().as_secs_f64();
    let total_writes = (n_threads * writes_per_thread) as f64;
    let write_throughput = (total_writes - write_errors as f64) / elapsed;

    // --- CAS contention ---
    let cas_coord = Coordinate::new("probe:cas", "probe:scope").expect("valid");
    store
        .append(&cas_coord, kind, &serde_json::json!({"seed": true}))
        .expect("seed");

    let cas_handles: Vec<_> = (0..4)
        .map(|t| {
            let store = Arc::clone(&store);
            let coord = cas_coord.clone();
            std::thread::spawn(move || {
                let opts = AppendOptions {
                    expected_sequence: Some(0), // expect latest clock=0 after seed
                    ..Default::default()
                };
                store
                    .append_with_options(&coord, kind, &serde_json::json!({"t": t}), opts)
                    .is_ok()
            })
        })
        .collect();

    let winners: usize = cas_handles
        .into_iter()
        .map(|h| if h.join().expect("join") { 1 } else { 0 })
        .sum();
    let cas_correct = winners == 1;

    // --- Data integrity after corruption simulation ---
    // (We don't actually corrupt files here since other tests do that;
    // we verify the CRC path rejects bad data)
    let good_frame = frame_encode(&"test").expect("encode");
    let mut bad_frame = good_frame.clone();
    if bad_frame.len() > 8 {
        bad_frame[8] ^= 0xFF; // corrupt msgpack
    }
    let integrity_ok = frame_decode(&good_frame).is_ok() && frame_decode(&bad_frame).is_err();

    // --- Segment rotation data loss ---
    let rot_coord = Coordinate::new("probe:rotation", "probe:scope").expect("valid");
    let rot_n = 50;
    for i in 0..rot_n {
        store
            .append(&rot_coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    let rot_entries = store.stream("probe:rotation");
    let rotation_loss = rot_n as u64 - rot_entries.len() as u64;

    // --- Subscription delivery ---
    let sub_coord = Coordinate::new("probe:sub", "probe:scope").expect("valid");
    let region = Region::entity("probe:sub");
    let sub = store.subscribe(&region);
    let sub_n = 50;
    for i in 0..sub_n {
        store
            .append(&sub_coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut received = 0;
    while sub.receiver().try_recv().is_ok() {
        received += 1;
    }
    let delivery_rate = received as f64 / sub_n as f64;

    // --- Cursor completeness ---
    let cur_coord = Coordinate::new("probe:cursor", "probe:scope").expect("valid");
    let cur_n = 30;
    for i in 0..cur_n {
        store
            .append(&cur_coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    let cur_region = Region::entity("probe:cursor");
    let mut cursor = store.cursor(&cur_region);
    let mut cursor_count = 0;
    while cursor.poll().is_some() {
        cursor_count += 1;
    }
    let cursor_ok = cursor_count == cur_n;

    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }

    (
        write_throughput,
        write_errors,
        cas_correct,
        integrity_ok,
        rotation_loss,
        delivery_rate,
        cursor_ok,
    )
}

// ============================================================
// PHASE 4: The feedback loop test
// ============================================================

#[test]
#[ignore] // Heavy: run with `cargo test --test fuzz_chaos_feedback -- --ignored`
fn fuzz_chaos_feedback_loop() {
    eprintln!("\n  ========================================");
    eprintln!("  FUZZ + CHAOS FEEDBACK LOOP (Phase 1)");
    eprintln!("  ========================================");

    // --- Run fuzz probes ---
    let (frame_ops, wire_ops, combinator_ops, fuzz_panics) = run_fuzz_probes();
    eprintln!("  Fuzz: frame_decode   {frame_ops:.0} ops/sec");
    eprintln!("  Fuzz: wire roundtrip {wire_ops:.0} ops/sec");
    eprintln!("  Fuzz: combinators    {combinator_ops:.0} ops/sec");
    eprintln!("  Fuzz: panics         {fuzz_panics}");

    // --- Run chaos probes ---
    let (write_tp, write_err, cas_ok, integrity_ok, rot_loss, sub_rate, cursor_ok) =
        run_chaos_probes();
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
        run_extended_fuzz_chaos();
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

// ============================================================
// PHASE 5: Extended load fuzz + chaos (only runs if Phase 1 passes)
// ============================================================

fn run_extended_fuzz_chaos() {
    eprintln!("  EXTENDED: High-volume frame_decode fuzz...");
    let n = 50_000;
    let mut rng = rand::thread_rng();
    let start = Instant::now();
    for _ in 0..n {
        let len: usize = rng.gen_range(0..4096);
        let data: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
        // catch_unwind would be ideal but proptest handles this;
        // we just verify no panics by continuing
        let _ = frame_decode(&data);
    }
    let frame_extended_ops = n as f64 / start.elapsed().as_secs_f64();
    eprintln!("    {frame_extended_ops:.0} ops/sec over {n} iterations");

    eprintln!("  EXTENDED: High-volume outcome combinator fuzz...");
    let start = Instant::now();
    for i in 0..n as i32 {
        let batch = Outcome::Batch(vec![
            Outcome::Ok(i),
            Outcome::Ok(i.wrapping_add(1)),
            Outcome::Batch(vec![Outcome::Ok(i.wrapping_mul(2))]),
        ]);
        let _ = batch
            .and_then(|x| Outcome::Ok(x.wrapping_add(1)))
            .map(|x| x.wrapping_mul(3));
    }
    let combinator_extended_ops = n as f64 / start.elapsed().as_secs_f64();
    eprintln!("    {combinator_extended_ops:.0} ops/sec over {n} iterations");

    eprintln!("  EXTENDED: Concurrent chaos storm...");
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 1024,
        fd_budget: 4,
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let kind = EventKind::custom(0xF, 1);
    let n_threads = 8;
    let writes_per = 200;

    let start = Instant::now();
    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let coord = Coordinate::new(&format!("ext:t{t}"), "ext:scope").expect("valid");
                let mut ok = 0u64;
                for i in 0..writes_per {
                    if store
                        .append(&coord, kind, &serde_json::json!({"i": i}))
                        .is_ok()
                    {
                        ok += 1;
                    }
                }
                ok
            })
        })
        .collect();

    let total_ok: u64 = handles.into_iter().map(|h| h.join().expect("join")).sum();
    let elapsed = start.elapsed().as_secs_f64();
    let ext_throughput = total_ok as f64 / elapsed;
    eprintln!("    {total_ok} events in {elapsed:.2}s = {ext_throughput:.0} events/sec");

    // Verify all events readable
    let store_ref = &*store;
    let mut total_entries = 0;
    for t in 0..n_threads {
        total_entries += store_ref.stream(&format!("ext:t{t}")).len();
    }
    assert_eq!(
        total_entries, total_ok as usize,
        "EXTENDED CHAOS: index has {total_entries} entries but {total_ok} events written. \
         Data loss detected. Investigate: src/store/writer.rs + src/store/index.rs."
    );

    // Close and reopen to verify durability (cold-start verification).
    // Without this, we only verified in-memory state — events could be lost on disk.
    match Arc::try_unwrap(store) {
        Ok(s) => s.close().expect("close"),
        Err(_) => panic!("Arc still has multiple owners"),
    }
    let config2 = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store2 = Store::open(config2).expect("cold start reopen");
    let mut cold_entries = 0;
    for t in 0..n_threads {
        cold_entries += store2.stream(&format!("ext:t{t}")).len();
    }
    assert_eq!(
        cold_entries, total_ok as usize,
        "COLD START DATA LOSS: wrote {total_ok} events, but only {cold_entries} survived cold start.\n\
         This means events were in-memory but not durable on disk.\n\
         Investigate: src/store/writer.rs sync paths, segment rotation durability.\n\
         Run: cargo test --test fuzz_chaos_feedback"
    );
    store2.close().expect("close cold start store");

    // --- Extended gates (stricter thresholds) ---
    struct ExtendedContext {
        frame_ops: f64,
        combinator_ops: f64,
        store_throughput: f64,
        data_loss: u64,
    }
    struct ExtFrameGate;
    impl Gate<ExtendedContext> for ExtFrameGate {
        fn name(&self) -> &'static str {
            "ext_frame_throughput"
        }
        fn evaluate(&self, ctx: &ExtendedContext) -> Result<(), Denial> {
            if ctx.frame_ops >= 50_000.0 {
                Ok(())
            } else {
                Err(Denial::new(
                    "ext_frame_throughput",
                    format!(
                        "Extended frame fuzz {:.0} ops/sec < 50K. \
                    Investigate: src/store/segment.rs",
                        ctx.frame_ops
                    ),
                ))
            }
        }
    }
    struct ExtCombinatorGate;
    impl Gate<ExtendedContext> for ExtCombinatorGate {
        fn name(&self) -> &'static str {
            "ext_combinator_throughput"
        }
        fn evaluate(&self, ctx: &ExtendedContext) -> Result<(), Denial> {
            if ctx.combinator_ops >= 100_000.0 {
                Ok(())
            } else {
                Err(Denial::new(
                    "ext_combinator_throughput",
                    format!(
                        "Extended combinator fuzz {:.0} ops/sec < 100K. \
                    Investigate: src/outcome/mod.rs",
                        ctx.combinator_ops
                    ),
                ))
            }
        }
    }
    struct ExtStoreGate;
    impl Gate<ExtendedContext> for ExtStoreGate {
        fn name(&self) -> &'static str {
            "ext_store_throughput"
        }
        fn evaluate(&self, ctx: &ExtendedContext) -> Result<(), Denial> {
            if ctx.store_throughput >= 1_000.0 {
                Ok(())
            } else {
                Err(Denial::new(
                    "ext_store_throughput",
                    format!(
                        "Extended store throughput {:.0} events/sec < 1K. \
                    Investigate: src/store/writer.rs",
                        ctx.store_throughput
                    ),
                ))
            }
        }
    }
    struct ExtDataLossGate;
    impl Gate<ExtendedContext> for ExtDataLossGate {
        fn name(&self) -> &'static str {
            "ext_zero_data_loss"
        }
        fn evaluate(&self, ctx: &ExtendedContext) -> Result<(), Denial> {
            if ctx.data_loss == 0 {
                Ok(())
            } else {
                Err(Denial::new(
                    "ext_zero_data_loss",
                    format!(
                        "{} events lost in extended chaos. \
                    Investigate: src/store/writer.rs + src/store/index.rs",
                        ctx.data_loss
                    ),
                ))
            }
        }
    }

    let ext_ctx = ExtendedContext {
        frame_ops: frame_extended_ops,
        combinator_ops: combinator_extended_ops,
        store_throughput: ext_throughput,
        data_loss: (n_threads as u64 * writes_per as u64) - total_ok,
    };

    let mut ext_gates = GateSet::new();
    ext_gates.push(ExtFrameGate);
    ext_gates.push(ExtCombinatorGate);
    ext_gates.push(ExtStoreGate);
    ext_gates.push(ExtDataLossGate);

    let ext_denials = ext_gates.evaluate_all(&ext_ctx);

    eprintln!("\n  ========================================");
    eprintln!("  EXTENDED FUZZ+CHAOS GATE REPORT");
    eprintln!("  ========================================");
    eprintln!("    Frame fuzz:       {frame_extended_ops:.0} ops/sec");
    eprintln!("    Combinator fuzz:  {combinator_extended_ops:.0} ops/sec");
    eprintln!("    Store throughput: {ext_throughput:.0} events/sec");
    eprintln!("    Data loss:        {}", ext_ctx.data_loss);

    if ext_denials.is_empty() {
        eprintln!("    Result: ALL EXTENDED GATES PASSED");
        eprintln!("    The full fuzz+chaos feedback loop is GREEN.");
    } else {
        eprintln!("    Result: {} EXTENDED GATES FAILED:", ext_denials.len());
        for d in &ext_denials {
            eprintln!("      [{gate}] {msg}", gate = d.gate, msg = d.message);
        }
        panic!(
            "EXTENDED FUZZ+CHAOS FAILED: {} gate(s) denied.\n\
             Phase 1 passed but extended load testing revealed regressions.",
            ext_denials.len()
        );
    }
}
