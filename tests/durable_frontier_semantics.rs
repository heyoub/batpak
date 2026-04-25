// justifies: INV-TEST-PANIC-AS-ASSERTION; this frontier bootstrap harness uses panic! through assert macros for crisp invariant failures.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]

//! PROVES:
//!   - Step-1 frontier scaffolding compiles and exposes a coherent dangerous snapshot.
//!   - Immediately after mutable `Store::open`, the lifecycle open event seeds
//!     accepted, written, durable, visible, and emitted to the same HLC point.
//! CATCHES: missing handle plumbing, missing public accessor coverage, or a
//! bootstrap snapshot that does not reflect `SYSTEM_OPEN_COMPLETED`.
//! SEEDED: deterministic tempdir-based open.

use batpak::store::{FrontierView, HlcPoint, Store, StoreConfig, WatermarkSnapshot};
use tempfile::TempDir;

#[test]
fn bootstrap_watermark_snapshot_matches_lifecycle_open_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");

    let snapshot: WatermarkSnapshot = store.dangerous_watermark_snapshot();
    let frontier: FrontierView = store.diagnostics().frontier;
    let open_hlc = snapshot.durable_hlc;

    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(snapshot.accepted_hlc, open_hlc);
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, HlcPoint::ORIGIN);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);

    assert_eq!(frontier.durable_hlc, open_hlc);
    assert_eq!(frontier.current_visible_hlc, open_hlc);
    assert_eq!(frontier.visible_minus_durable_seq, 0);
    assert_eq!(frontier.oldest_pending_write_age_ms, None);
}
