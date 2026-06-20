// justifies: INV-TEST-PANIC-AS-ASSERTION; a data-driven downgrade gate uses expect/panic as the assertion style when a matrix row's contract is violated
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
//! Gauntlet Phase 3 — COMPAT MATRIX: on-disk forward-compat downgrade discipline.
//!
//! Harness pattern: Oracle / Fault harness (table-driven, every-PR).
//! Witnesses: INV-ONDISK-FORWARD-COMPAT-CANONICAL, INV-IDEMPOTENCY-DURABLE-WINDOW.
//!
//! PROVES: every (writer_version, reader_version, feature_bits) row declared in
//! `traceability/compat_matrix.yaml` produces EXACTLY its declared outcome —
//! either `OpensOK` (the artifact opens through the real `Store::open`
//! lifecycle) or `CanonicalRefusal:<TypedError>` (a single canonical typed
//! refusal, never silent corruption and never a silent rebuild-from-scan
//! downgrade). The matrix is the live contract; this gate is its executor.
//!
//! CATCHES: (a) a regression that reverts a future-version cure back to silent
//! degrade; (b) an on-disk version bump with NO matching matrix row — the
//! self-row's `reader_version` is cross-checked against the live supported
//! version, so a forgotten row trips this gate rather than shipping an
//! uncovered format version.
//!
//! SEEDED: a deterministic 4-event temp store per row, with the on-disk version
//! field byte-forged to the row's `writer_version` (the forge mirrors
//! `gauntlet_s2_future_version_refusal::forge_future_mmap_version` and
//! `idempotency_corruption_recovery::future_version_is_a_hard_error_at_cold_start`).
//!
//! Adding a covered (format, version) pair is a YAML edit (a new row) plus, for
//! a genuinely new FORMAT, a new arm in `forge_artifact_version` + `open_outcome`.
//! Deferred formats are logged in `GAUNTLET_ISSUES.md`.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{Store, StoreConfig, StoreError};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The live mmap-index format version this binary supports. Mirrors the
/// crate-private `cold_start::mmap::format::MMAP_INDEX_VERSION`; the artifact is
/// forged through the public store, so this is the only place the value is named
/// in this harness. If the on-disk const is bumped, the self-row's
/// `reader_version` in the matrix must follow — the gate enforces that link.
const SUPPORTED_MMAP_VERSION: u16 = 5;

const MMAP_ARTIFACT: &str = "index.fbati";

fn matrix_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("traceability")
        .join("compat_matrix.yaml")
}

#[derive(Debug, Deserialize)]
struct CompatMatrix {
    rows: Vec<CompatRow>,
}

#[derive(Debug, Deserialize)]
struct CompatRow {
    format: String,
    writer_version: u16,
    reader_version: u16,
    // Reserved capability bitset (0 today). Read in the row-failure message and
    // the self-row tripwire so it carries weight rather than being dead.
    feature_bits: u64,
    expected_outcome: String,
    fixture_path: String,
}

/// The legal outcomes a row may declare. `CanonicalRefusal` carries the typed
/// error tag so a row can pin WHICH canonical refusal, not merely "is_err".
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    OpensOk,
    CanonicalRefusal(String),
}

fn parse_outcome(raw: &str) -> Outcome {
    if raw == "OpensOK" {
        Outcome::OpensOk
    } else if let Some(tag) = raw.strip_prefix("CanonicalRefusal:") {
        Outcome::CanonicalRefusal(tag.to_string())
    } else {
        panic!(
            "compat_matrix.yaml: unknown expected_outcome {raw:?}; \
             expected `OpensOK` or `CanonicalRefusal:<TypedError>`"
        );
    }
}

fn mmap_config(dir: &Path) -> StoreConfig {
    StoreConfig::new(dir)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(true)
        .with_sync_every_n_events(1)
}

/// Seed a tiny store (kept small: 4 events) and flush a live on-disk artifact.
fn seed_mmap_store(dir: &Path) {
    let store = Store::open(mmap_config(dir)).expect("open store to seed compat fixture");
    let coord =
        Coordinate::new("entity:compat", "scope:compat-matrix").expect("valid compat coordinate");
    let kind = EventKind::custom(0xF, 7);
    for i in 0..4u32 {
        store
            .append(&coord, kind, &serde_json::json!({ "i": i }))
            .expect("append compat seed event");
    }
    store.close().expect("close store to flush compat fixture");
}

/// Forge the on-disk version field for `format` to `writer_version`. Returns the
/// artifact path. For mmap-index the version lives in bytes 6..8 (LE), OUTSIDE
/// the CRC region and checked before the CRC, so a bump alone trips only the
/// version branch. A self-version forge (writer == live) is a no-op rewrite
/// that asserts the artifact is on the expected live format.
fn forge_artifact_version(dir: &Path, format: &str, writer_version: u16) -> PathBuf {
    match format {
        "mmap-index" => {
            seed_mmap_store(dir);
            let artifact = dir.join(MMAP_ARTIFACT);
            assert!(
                artifact.exists(),
                "PROPERTY: close() with mmap index enabled must write {MMAP_ARTIFACT}"
            );
            let mut bytes = std::fs::read(&artifact).expect("read mmap artifact");
            assert!(
                bytes.len() >= 12,
                "mmap artifact must contain at least the 12-byte prefix"
            );
            let on_disk = u16::from_le_bytes(bytes[6..8].try_into().expect("version slice"));
            assert_eq!(
                on_disk, SUPPORTED_MMAP_VERSION,
                "compat forge expects the live mmap snapshot format on disk before forging"
            );
            if writer_version != SUPPORTED_MMAP_VERSION {
                bytes[6..8].copy_from_slice(&writer_version.to_le_bytes());
                std::fs::write(&artifact, &bytes).expect("write forged-version mmap artifact");
            }
            artifact
        }
        other => panic!(
            "compat_matrix.yaml row references format {other:?} with no forge arm; \
             add a `forge_artifact_version` arm (and an `open_outcome` arm) before \
             adding the row, or it is a silent-skip gap"
        ),
    }
}

/// Open the forged artifact and classify the result into an `Outcome`, mapping
/// the typed `StoreError` into the canonical-refusal tag namespace the matrix
/// uses. Unknown formats panic (no silent skip).
fn open_outcome(dir: &Path, format: &str) -> Outcome {
    match format {
        "mmap-index" => match Store::open(mmap_config(dir)) {
            Ok(_store) => Outcome::OpensOk,
            Err(StoreError::MmapFutureVersion { .. }) => {
                Outcome::CanonicalRefusal("MmapFutureVersion".to_string())
            }
            Err(other) => panic!(
                "PROPERTY: mmap forward-compat must yield OpensOK or the canonical \
                 MmapFutureVersion refusal; got an unrelated error {other:?}"
            ),
        },
        other => panic!("compat_matrix.yaml: no open_outcome arm for format {other:?}"),
    }
}

fn live_supported_version(format: &str) -> u16 {
    match format {
        "mmap-index" => SUPPORTED_MMAP_VERSION,
        other => panic!("compat_matrix.yaml: no live version mirror for format {other:?}"),
    }
}

fn load_matrix() -> CompatMatrix {
    let path = matrix_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read compat matrix {}: {e}", path.display()));
    yaml_serde::from_str(&text).expect("parse compat_matrix.yaml")
}

/// Table-driven gate: drive EVERY matrix row and assert the declared outcome.
#[test]
fn compat_matrix_rows_match_declared_outcomes() {
    let matrix = load_matrix();
    assert!(
        !matrix.rows.is_empty(),
        "compat_matrix.yaml must declare at least one row"
    );

    for row in &matrix.rows {
        let expected = parse_outcome(&row.expected_outcome);
        let dir = TempDir::new().expect("temp dir for compat row");
        let _artifact = forge_artifact_version(dir.path(), &row.format, row.writer_version);
        let actual = open_outcome(dir.path(), &row.format);
        assert_eq!(
            actual,
            expected,
            "compat row (format={}, writer={}, reader={}, feature_bits={}, fixture={}) \
             expected {:?} but the live store produced {:?}",
            row.format,
            row.writer_version,
            row.reader_version,
            row.feature_bits,
            row.fixture_path,
            expected,
            actual,
        );
    }
}

/// On-disk formats the compat matrix governs. Grows as typed `*FutureVersion`
/// refusal errors land for the deferred formats (checkpoint, segment,
/// idempotency-index, visibility-ranges — see GAUNTLET_ISSUES.md).
const COMPAT_FORMATS: &[&str] = &["mmap-index"];

/// Staleness tripwire: a NEW on-disk version with no matrix row must be
/// catchable. Each format MUST have a self-row whose `writer_version ==
/// reader_version == live supported version`. If the on-disk version const is
/// bumped and no row follows, this gate fails — the row is the contract.
#[test]
fn compat_matrix_self_row_tracks_live_version() {
    let matrix = load_matrix();

    for &format in COMPAT_FORMATS {
        let live = live_supported_version(format);
        let self_row = matrix
            .rows
            .iter()
            .find(|r| r.format == format && r.writer_version == live && r.reader_version == live);
        let self_row = self_row.unwrap_or_else(|| {
            panic!(
                "compat_matrix.yaml has NO self-row for format {format:?} at the live \
                 supported version {live}. A new on-disk version without a row is a \
                 forward-compat gap: add a row \
                 {{ writer_version: {live}, reader_version: {live}, \
                 expected_outcome: OpensOK }}"
            )
        });
        assert_eq!(
            parse_outcome(&self_row.expected_outcome),
            Outcome::OpensOk,
            "compat_matrix.yaml self-row for {format:?} must declare OpensOK"
        );

        // The future row (live + 1) must exist and be a canonical refusal: this
        // proves the matrix exercises the downgrade-refusal branch, not just the
        // happy self-open.
        let future = matrix
            .rows
            .iter()
            .find(|r| r.format == format && r.writer_version == live + 1);
        let future = future.unwrap_or_else(|| {
            panic!(
                "compat_matrix.yaml has no forged future-version row for {format:?} \
                 at version {}; the downgrade-refusal branch would be untested",
                live + 1
            )
        });
        assert!(
            matches!(
                parse_outcome(&future.expected_outcome),
                Outcome::CanonicalRefusal(_)
            ),
            "compat_matrix.yaml future-version row for {format:?} must be a CanonicalRefusal"
        );
    }
}
