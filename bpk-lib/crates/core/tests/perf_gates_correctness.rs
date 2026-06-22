//! PROVES: LAW-004 (Composition Over Construction — quadratic dogfooding) for the
//! six correctness gates (fd-eviction round-trip, cross-segment reads, CAS,
//! idempotency, cursor completeness, snapshot bootability) plus the tripwire
//! that all six gates FIRE when their properties are violated.
//! CATCHES: resilience regressions surfaced by adversarial store exercise, and a
//! vacuous gate set where a broken gate silently always passes.
//! SEEDED: deterministic 50-event corpus under a 512-byte segment cap and a
//! 2-fd budget to force segment rotation + LRU eviction.
//!
//! Split out of the original 1322-line `tests/perf_gates.rs`. These are
//! code-level tripwires, not an independent production certification system.
//! Harness pattern: Property Harness (catastrophic threshold lane).

use batpak::store::{Store, StoreConfig};
use batpak_testkit::prelude::*;
use std::io::Write;
use tempfile::TempDir;

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
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(512) // tiny → many segments
        .with_sync_every_n_events(1)
        .with_fd_budget(2); // tiny → forces LRU eviction
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("correctness:entity", "correctness:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let n = 50u64;

    // Populate with enough events to trigger segment rotation + fd eviction
    for i in 0..n {
        let _ = store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");

    // --- Probe 1: FD eviction round-trip ---
    let entries = store.by_entity("correctness:entity");
    let first = store.get(entries[0].event_id());
    let last = store.get(entries[entries.len() - 1].event_id());
    let first_again = store.get(entries[0].event_id()); // re-read after eviction
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
    let cross_segment_reads_ok = segment_count > 1
        && entries.len() == usize::try_from(n).expect("bounded test event count fits usize");

    // --- Probe 3: CAS rejection ---
    let _ = store
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
        idempotency_key: Some(batpak::id::IdempotencyKey::from(idem_key)),
        ..Default::default()
    };
    let r1 = store
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"x": 1}),
            idem_opts.clone(),
        )
        .expect("first idem");
    let r2 = store
        .append_with_options(&coord, kind, &serde_json::json!({"x": 2}), idem_opts)
        .expect("second idem");
    let idempotency_deduplicates = r1.event_id == r2.event_id;

    // --- Probe 5: Cursor completeness ---
    let coord2 = Coordinate::new("cursor:test", "correctness:scope").expect("valid");
    for i in 0..5 {
        let _ = store
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
    store
        .snapshot_with_evidence(snap_dir.path())
        .expect("snapshot");
    let snap_config = StoreConfig::new(snap_dir.path());
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

    let mut report = String::from("\n  CORRECTNESS GATE REPORT:");
    report.push_str(&format!(
        "\n    fd_eviction_round_trips:   {fd_eviction_round_trips}"
    ));
    report.push_str(&format!(
        "\n    cross_segment_reads:        {cross_segment_reads_ok}"
    ));
    report.push_str(&format!(
        "\n    cas_rejects_stale:          {cas_rejects_stale}"
    ));
    report.push_str(&format!(
        "\n    idempotency_deduplicates:   {idempotency_deduplicates}"
    ));
    report.push_str(&format!(
        "\n    cursor_sees_all_events:     {cursor_sees_all_events}"
    ));
    report.push_str(&format!(
        "\n    snapshot_boots:             {snapshot_boots}"
    ));
    for d in &denials {
        report.push_str(&format!(
            "\n      [{gate}] {msg}",
            gate = d.gate,
            msg = d.message
        ));
    }
    let _ = writeln!(std::io::stderr(), "{report}");

    assert!(
        denials.is_empty(),
        "CORRECTNESS SELF-TEST FAILED: {} gate(s) denied.\n\
         Each denial above points to the likely file + function to investigate.\n\
         This is the library stress-testing itself with the shared guard primitives.{report}",
        denials.len()
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
         Investigate: src/guard/mod.rs GateSet::evaluate_all() tests/perf_gates_correctness.rs correctness gates.\n\
         Common causes: evaluate_all() stopping early after fewer than 6 denials, or \
         one of the correctness gates returning Ok even when the property is false.\n\
         Run: cargo test --test perf_gates_correctness correctness_gates_fire_on_violations"
    );

    // Every denial should contain an investigation pointer
    for d in &denials {
        assert!(
            d.message.contains("Investigate:"),
            "PROPERTY: Every correctness gate denial must include an 'Investigate:' pointer to a source file.\n\
             Investigate: tests/perf_gates_correctness.rs [{gate}] Gate::evaluate() denial message: {msg}.\n\
             Common causes: Gate denial message not including the 'Investigate:' keyword, or \
             denial constructed with an empty message string.\n\
             Run: cargo test --test perf_gates_correctness correctness_gates_fire_on_violations",
            gate = d.gate,
            msg = d.message
        );
    }
}
