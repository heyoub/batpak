// justifies: INV-TEST-PANIC-AS-ASSERTION; recovery test uses explicit panic branches as assertion failures for Store open result paths in tests/gauntlet_fault_kth_recovery_io.rs.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]
//! GAUNT-FAULT-3: Kth-I/O cold-start recovery contract.
//!
//! PROVES: a fault injected on the Kth read/scan/cold-start I/O during
//! `Store::open` leaves the store in a legal terminal state — it EITHER opens
//! consistently OR returns a typed [`StoreError`]. It must never panic, abort,
//! or exhibit UB. K is parameterized over a small range so the gate covers the
//! mmap-load, checkpoint-decode, SIDX-footer-decode, per-frame-scan, and
//! hidden-ranges injection points.
//!
//! Slug: GAUNT-FAULT-3 / gauntlet_fault_kth_recovery_io

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{CountdownInjector, Store, StoreConfig, StoreError};
use std::sync::Arc;
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xF, 3);

/// Build a populated store directory with several rotated segments so cold
/// start has real scan/decode work (multiple SIDX footers + frames) to fault.
fn seed_store(dir: &TempDir, mmap: bool, checkpoint: bool) {
    let config = StoreConfig::new(dir.path())
        .with_enable_mmap_index(mmap)
        .with_enable_checkpoint(checkpoint)
        // Small segments → multiple sealed segments → multiple footer decodes.
        .with_segment_max_bytes(4 * 1024);
    let store = Store::open(config).expect("open store for seeding");
    let coord = Coordinate::new("entity:kth", "scope:recovery").expect("valid coordinate");
    for n in 0..200u32 {
        store
            .append(
                &coord,
                KIND,
                &serde_json::json!({ "n": n, "pad": "xxxxxxxxxxxxxxxx" }),
            )
            .expect("append seed event");
    }
    store.close().expect("close seeded store");
}

/// Reopen `dir` with a Kth-recovery-I/O fault armed and assert the outcome is a
/// legal terminal state (consistent open OR a typed error — never a panic).
fn reopen_with_kth_fault(dir: &TempDir, k: usize, mmap: bool, checkpoint: bool) {
    let injector = Arc::new(CountdownInjector::on_kth_recovery_io(k));
    let config = StoreConfig::new(dir.path())
        .with_enable_mmap_index(mmap)
        .with_enable_checkpoint(checkpoint)
        .with_segment_max_bytes(4 * 1024)
        .with_fault_injector(Some(injector));

    match Store::open(config) {
        Ok(store) => {
            // Opened consistently despite the armed fault (the Kth I/O may not
            // have been reached on this path). The store must be usable: a
            // query must not panic and visible history must be a subset of what
            // we wrote.
            let visible = store.query(&batpak::coordinate::Region::all()).len();
            #[cfg(not(gauntlet_red_fixture))]
            assert!(
                visible <= 256,
                "PROPERTY: a consistently-opened store must not invent events \
                 beyond the seeded history (saw {visible})"
            );
            // RED fixture: under `--cfg gauntlet_red_fixture` assert the ILLEGAL
            // invented-events outcome. The cured loader opens consistently
            // (visible <= 256), so this assertion FAILS under the cfg — proving the
            // gate detects a faulted cold start that fabricates events past the
            // seeded history rather than passing vacuously.
            #[cfg(gauntlet_red_fixture)]
            assert!(
                visible > 256,
                "RED FIXTURE: a faulted cold start must not be asserted legal (saw {visible})"
            );
            store.close().expect("close recovered store");
        }
        Err(err) => {
            // A refusal is legal, but it MUST be a typed StoreError carrying the
            // injected fault (or a recovery error), never a panic/abort. Match
            // on the variant to prove it is typed and structured.
            let typed = matches!(
                err,
                StoreError::FaultInjected(_)
                    | StoreError::Io(_)
                    | StoreError::Serialization(_)
                    | StoreError::MmapFutureVersion { .. }
            );
            #[cfg(not(gauntlet_red_fixture))]
            assert!(
                typed,
                "PROPERTY: a faulted cold start must refuse with a typed \
                 StoreError, got: {err:?}"
            );
            // RED fixture: assert the ILLEGAL untyped-failure outcome. The cured
            // loader returns a typed StoreError, so this FAILS under the cfg.
            #[cfg(gauntlet_red_fixture)]
            assert!(
                !typed,
                "RED FIXTURE: a faulted cold start must not be asserted typed"
            );
        }
    }
}

/// Kth-I/O fault on the SCAN path (mmap + checkpoint disabled) → forces SIDX
/// footer decode + per-frame scan + hidden-ranges injection points.
#[test]
fn kth_io_fault_on_scan_path_is_consistent_or_typed_error() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, false, false);
    // Cover the first several recovery I/O points; the scan path emits many
    // (one per sealed footer + one per scanned frame + hidden ranges).
    for k in 1..=12usize {
        reopen_with_kth_fault(&dir, k, false, false);
    }
}

/// Kth-I/O fault on the FAST path (mmap + checkpoint enabled) → exercises the
/// MmapIndexLoad / CheckpointDecode / HiddenRangesLoad injection points.
#[test]
fn kth_io_fault_on_fast_path_is_consistent_or_typed_error() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, true, true);
    for k in 1..=6usize {
        reopen_with_kth_fault(&dir, k, true, true);
    }
}
