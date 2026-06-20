// justifies: INV-TEST-PANIC-AS-ASSERTION; Phase 0B sentinel uses expect/panic as the assertion style when the future-version refusal contract is violated
#![allow(clippy::panic, clippy::unwrap_used)]
//! Gauntlet Phase 0B — SENTINEL S2: future-version canonical refusal.
//!
//! Harness pattern: Offensive sentinel (always-on, every-PR; ships a RED fixture).
//!
//! PROVES: an on-disk mmap index (`index.fbati`) that declares a format version
//! STRICTLY NEWER than this binary supports is met with a CANONICAL TYPED
//! REFUSAL (`StoreError::MmapFutureVersion`) propagated out of cold-start —
//! NOT silently swallowed into a rebuild-from-scan (silent downgrade), and NOT
//! a panic. A future writer may have written data this reader cannot interpret,
//! so the only legal outcomes are "open the legally-reachable state" or "refuse
//! canonically" — never silent corruption / silent degrade.
//! CATCHES: a regression that reverts the future-version cure back to the old
//! `FileLoad::Invalid` → rebuild-from-scan silent-degrade behavior.
//! SEEDED: deterministic temp-store seed + a byte-forged future version field.
//!
//! The cure: the mmap loader (`cold_start/mmap/load.rs`) classifies
//! `version > MMAP_INDEX_VERSION` as `FileLoad::FutureVersion`, and the
//! cold-start planner (`cold_start/rebuild.rs`) propagates it as
//! `StoreError::MmapFutureVersion` instead of falling through to the rebuild.
//! Corrupt / older artifacts remain `FileLoad::Invalid` and still rebuild — see
//! `mmap_cold_start::corrupt_mmap_index_falls_back_cleanly`.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
#[cfg(not(gauntlet_red_fixture))]
use batpak::store::StoreError;
use batpak::store::{Store, StoreConfig};
use std::path::Path;
use tempfile::TempDir;

/// The maximum mmap-index format version this binary supports. Kept in sync
/// with `cold_start::mmap::format::MMAP_INDEX_VERSION` (a crate-private const;
/// the on-disk artifact is forged through the public store, so this mirror is
/// the only place the value is named in this integration harness).
const SUPPORTED_MMAP_VERSION: u16 = 5;

fn mmap_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(true)
        .with_sync_every_n_events(1)
}

fn seed_store(dir: &TempDir, count: u32) {
    let store = Store::open(mmap_config(dir)).expect("open store to seed mmap artifact");
    let coord = Coordinate::new("entity:s2", "scope:future-version").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    for i in 0..count {
        store
            .append(&coord, kind, &serde_json::json!({ "i": i }))
            .expect("append seed event");
    }
    store.close().expect("close store to flush mmap artifact");
}

/// Forge the on-disk mmap index version field (bytes 6..8, little-endian) to a
/// value STRICTLY GREATER than supported. The CRC at bytes 8..12 covers only
/// bytes 12.., so the version field is OUTSIDE the CRC region; the version check
/// in the loader fires before the CRC check, so no CRC recompute is needed —
/// the only thing that trips is the version. Mirrors the version-forging
/// pattern in `idempotency_corruption_recovery::future_version_is_a_hard_error_at_cold_start`.
fn forge_future_mmap_version(artifact: &Path, future_version: u16) {
    let mut bytes = std::fs::read(artifact).expect("read mmap artifact");
    assert!(
        bytes.len() >= 12,
        "mmap artifact must contain at least the 12-byte prefix"
    );
    let on_disk_version = u16::from_le_bytes(bytes[6..8].try_into().expect("version slice"));
    assert_eq!(
        on_disk_version, SUPPORTED_MMAP_VERSION,
        "test helper expects the live mmap snapshot format on disk before forging"
    );
    assert!(
        future_version > SUPPORTED_MMAP_VERSION,
        "forged version must be strictly newer than supported"
    );
    bytes[6..8].copy_from_slice(&future_version.to_le_bytes());
    std::fs::write(artifact, &bytes).expect("write future-version mmap artifact");
}

/// GREEN (every-PR): a future-version mmap artifact must be a canonical typed
/// refusal that propagates out of cold-start — not a silent rebuild, not a panic.
#[test]
fn future_version_mmap_index_is_canonical_refusal_not_silent_rebuild() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 12);

    let artifact = dir.path().join("index.fbati");
    assert!(
        artifact.exists(),
        "PROPERTY: close() with mmap index enabled must write index.fbati."
    );
    let future_version = SUPPORTED_MMAP_VERSION + 1;
    forge_future_mmap_version(&artifact, future_version);

    let result = Store::open(mmap_config(&dir));

    // RED fixture: under `--cfg gauntlet_red_fixture`, assert the OLD
    // silent-degrade behavior (open succeeds via rebuild-from-scan). That
    // assertion is FALSE against the cured code, so the red fixture FAILS —
    // proving the sentinel detects the illegal silent-rebuild outcome rather
    // than passing vacuously.
    #[cfg(gauntlet_red_fixture)]
    {
        let store = result.expect(
            "RED FIXTURE: this asserts the (illegal) silent rebuild-from-scan \
             behavior; it MUST fail against the cured loader that refuses canonically",
        );
        let report = store
            .diagnostics()
            .open_report
            .clone()
            .expect("open report");
        assert_eq!(
            report.path,
            batpak::store::cold_start::rebuild::OpenIndexPath::Rebuild,
            "RED FIXTURE: future-version artifact must NOT be silently rebuilt",
        );
    }

    // GREEN: the cured behavior — a canonical typed refusal, propagated.
    #[cfg(not(gauntlet_red_fixture))]
    {
        let err = match result {
            Ok(_) => panic!(
                "PROPERTY: a future-version mmap index must be a CANONICAL TYPED REFUSAL, \
                 never silently rebuilt from scan (silent downgrade)."
            ),
            Err(err) => err,
        };
        assert!(
            matches!(
                err,
                StoreError::MmapFutureVersion {
                    found,
                    supported,
                } if found == future_version && supported == SUPPORTED_MMAP_VERSION
            ),
            "PROPERTY: future-version mmap index must surface StoreError::MmapFutureVersion \
             {{ found: {future_version}, supported: {SUPPORTED_MMAP_VERSION} }}, got {err:?}"
        );
    }
}

/// GREEN (every-PR): the cure must NOT break graceful recovery for genuinely
/// corrupt / older artifacts. A corrupt (bad-CRC) mmap artifact must still fall
/// back to a clean segment rebuild without data loss — distinct from the
/// future-version hard refusal above.
#[test]
fn corrupt_mmap_index_still_rebuilds_distinct_from_future_version() {
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 12);

    let artifact = dir.path().join("index.fbati");
    let mut bytes = std::fs::read(&artifact).expect("read mmap artifact");
    // Flip a byte in the CRC-covered body (not the version field): corruption,
    // not a future version. This must rebuild, not refuse.
    let last = bytes.len() - 1;
    bytes[last] ^= 0x5A;
    std::fs::write(&artifact, bytes).expect("rewrite corrupt mmap artifact");

    let store = Store::open(mmap_config(&dir))
        .expect("corrupt (non-future) mmap artifact must still open via clean rebuild");
    let stream = store.by_entity("entity:s2");
    assert_eq!(
        stream.len(),
        12,
        "corrupt mmap artifact must fall back to durable segment rebuild without data loss"
    );
}
