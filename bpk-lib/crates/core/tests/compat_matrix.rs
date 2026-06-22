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
//! SEEDED: a deterministic temp store per row whose enabled cold-start artifact
//! is forged at bytes `6..8` (the shared version field) to the row's
//! `writer_version` (the forge mirrors
//! `gauntlet_s2_future_version_refusal::forge_future_mmap_version` and
//! `idempotency_corruption_recovery::future_version_is_a_hard_error_at_cold_start`).
//! Four formats are governed: mmap-index, checkpoint, idempotency-index, and
//! visibility-ranges; each has an OpensOK self-row and a forged future-version
//! `CanonicalRefusal` row.
//!
//! Adding a covered (format, version) pair is a YAML edit (a new row) plus, for
//! a genuinely new FORMAT, a new arm in each of `config_for`,
//! `forge_artifact_version`, `open_outcome`, and `live_supported_version`. The
//! segment/.fbat format is an honest skip: its version is msgpack-encoded (no
//! fixed-offset forge), and a future-version segment already fails closed via
//! `CorruptSegment`.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{Store, StoreConfig, StoreError};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The live on-disk format versions this binary supports, one per governed
/// format. Each mirrors a crate-private const (the artifacts are forged through
/// the public store, so these are the only places the values are named in this
/// harness). If an on-disk const is bumped, the matching self-row's
/// `reader_version` must follow — the gate enforces that link.
///
/// * `SUPPORTED_MMAP_VERSION` ← `cold_start::mmap::format::MMAP_INDEX_VERSION`
/// * `SUPPORTED_CHECKPOINT_VERSION` ← `cold_start::checkpoint::format::CHECKPOINT_VERSION`
/// * `SUPPORTED_IDEMP_VERSION` ← `store::index::idemp::IDEMP_VERSION`
/// * `SUPPORTED_VISIBILITY_VERSION` ← `store::hidden_ranges::VISIBILITY_RANGES_VERSION`
const SUPPORTED_MMAP_VERSION: u16 = 5;
const SUPPORTED_CHECKPOINT_VERSION: u16 = 6;
const SUPPORTED_IDEMP_VERSION: u16 = 1;
const SUPPORTED_VISIBILITY_VERSION: u16 = 2;

const MMAP_ARTIFACT: &str = "index.fbati";
const CHECKPOINT_ARTIFACT: &str = "index.ckpt";
const IDEMP_ARTIFACT: &str = "index.idemp";
const VISIBILITY_ARTIFACT: &str = "visibility_ranges.fbv";

/// Every governed format stamps its version as a little-endian `u16` at bytes
/// `6..8` (after a 6-byte magic), OUTSIDE the CRC region (CRC at `8..12` covers
/// only the body at `12..`) and checked before the CRC — so a forged version
/// alone trips the future-version branch in each loader. This is the single
/// shared forge primitive across all four formats.
const VERSION_FIELD: std::ops::Range<usize> = 6..8;

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
        return Outcome::OpensOk;
    }
    let tag = raw
        .strip_prefix("CanonicalRefusal:")
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "compat_matrix.yaml: unknown expected_outcome {raw:?}; \
                 expected `OpensOK` or `CanonicalRefusal:<TypedError>`"
            )
        })
        .expect("compat_matrix.yaml expected_outcome must be OpensOK or CanonicalRefusal:<tag>");
    Outcome::CanonicalRefusal(tag)
}

/// The store config a given `format` row opens through. Each format enables
/// exactly the cold-start artifact it governs so the loader path under test is
/// the one actually exercised (e.g. the checkpoint row enables checkpoints, the
/// mmap row enables the mmap index).
fn config_for(dir: &Path, format: &str) -> StoreConfig {
    let base = StoreConfig::new(dir).with_sync_every_n_events(1);
    let config = match format {
        // The mmap path must be the only fast path so a forged mmap artifact is
        // what cold-start sees first.
        "mmap-index" => Some(
            base.with_enable_checkpoint(false)
                .with_enable_mmap_index(true),
        ),
        // Disable mmap so the checkpoint is the fast path (mmap would otherwise
        // shadow it). Checkpoints are enabled by default; make it explicit.
        "checkpoint" => Some(
            base.with_enable_mmap_index(false)
                .with_enable_checkpoint(true),
        ),
        // The durable idempotency sidecar + hidden-ranges metadata are loaded
        // UNCONDITIONALLY on open, independent of mmap/checkpoint; keep the
        // config minimal and deterministic.
        "idempotency-index" | "visibility-ranges" => Some(
            base.with_enable_mmap_index(false)
                .with_enable_checkpoint(false),
        ),
        _ => None,
    };
    config
        .ok_or_else(|| format!("compat_matrix.yaml: no store config for format {format:?}"))
        .expect("compat_matrix.yaml row must reference a format with a config arm")
}

/// Append `count` plain events through `config`, returning after a clean close
/// so every enabled cold-start artifact is flushed.
fn seed_events(dir: &Path, format: &str, count: u32) {
    let store = Store::open(config_for(dir, format)).expect("open store to seed compat fixture");
    let coord =
        Coordinate::new("entity:compat", "scope:compat-matrix").expect("valid compat coordinate");
    let kind = EventKind::custom(0xF, 7);
    for i in 0..count {
        let _ = store
            .append(&coord, kind, &serde_json::json!({ "i": i }))
            .expect("append compat seed event");
    }
    store.close().expect("close store to flush compat fixture");
}

/// Mirror of the crate-private `hidden_ranges::VisibilityRangesData` body so the
/// forged `visibility_ranges.fbv` is byte-identical to what the store writes
/// (same `to_vec_named` MessagePack surface). Producing a real artifact through
/// the public fence-cancel path requires driving the background writer to a
/// terminal state, which is needlessly heavy here — the loader contract under
/// test is purely the on-disk header (magic + version + CRC), so a faithfully
/// encoded body is the simpler, deterministic fixture.
#[derive(serde::Serialize)]
struct VisibilityRangesBody {
    ranges: Vec<VisibilityRange>,
}

#[derive(serde::Serialize)]
struct VisibilityRange {
    start: u64,
    end: u64,
}

/// Write a valid live-version `visibility_ranges.fbv` directly:
/// `magic(6) | version(2 le) | crc32(4 le over body) | body(to_vec_named)`.
/// Identical layout to `hidden_ranges::write_cancelled_ranges`. The body carries
/// only `ranges`; the v1→v2 bump added an optional `lane_ranges` field with
/// `#[serde(default)]`, so this body decodes cleanly at the live v2 (the bump is
/// version-number-only as far as a global-ranges-only body is concerned).
fn seed_visibility_ranges(dir: &Path) {
    // Seed a real (segment-bearing) store first so the data dir is a valid store
    // the reopen can cold-start, then drop the cancelled-ranges sidecar beside it.
    seed_events(dir, "visibility-ranges", 1);
    // Hide a range FAR beyond any committed global sequence so the restored
    // cancelled ranges hide no real (open-receipt or seeded) event — otherwise
    // cold-start would refuse a now-invisible indexed event. The loader path
    // under test is the header version check, not the range semantics.
    let body = rmp_serde::to_vec_named(&VisibilityRangesBody {
        ranges: vec![VisibilityRange {
            start: 1_000_000,
            end: 1_000_001,
        }],
    })
    .expect("encode visibility-ranges body");
    let crc = crc32fast::hash(&body);
    let mut bytes = Vec::with_capacity(12 + body.len());
    bytes.extend_from_slice(b"FBATVR");
    bytes.extend_from_slice(&SUPPORTED_VISIBILITY_VERSION.to_le_bytes());
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes.extend_from_slice(&body);
    std::fs::write(dir.join(VISIBILITY_ARTIFACT), &bytes)
        .expect("write seeded visibility-ranges artifact");
}

/// Read an on-disk artifact, assert it is on the expected live version, and —
/// when `writer_version` differs — byte-patch the version field. Returns the
/// patched bytes' path. The version field is the single forge point for every
/// format (see `VERSION_FIELD`).
fn forge_version_field(artifact: &Path, live: u16, writer_version: u16) {
    let mut bytes = std::fs::read(artifact).expect("read artifact to forge");
    assert!(
        bytes.len() >= 12,
        "artifact {} must contain at least the 12-byte prefix",
        artifact.display()
    );
    let on_disk = u16::from_le_bytes(bytes[VERSION_FIELD].try_into().expect("version slice"));
    assert_eq!(
        on_disk,
        live,
        "compat forge expects the live on-disk format for {} before forging",
        artifact.display()
    );
    if writer_version != live {
        bytes[VERSION_FIELD].copy_from_slice(&writer_version.to_le_bytes());
        std::fs::write(artifact, &bytes).expect("write forged-version artifact");
    }
}

/// Forge the on-disk version field for `format` to `writer_version`. Returns the
/// artifact path. A self-version forge (writer == live) is a no-op rewrite that
/// still asserts the artifact is on the expected live format.
fn forge_artifact_version(dir: &Path, format: &str, writer_version: u16) -> PathBuf {
    let seeded: Option<(&str, u16)> = match format {
        "mmap-index" => {
            seed_events(dir, format, 4);
            Some((MMAP_ARTIFACT, SUPPORTED_MMAP_VERSION))
        }
        "checkpoint" => {
            seed_events(dir, format, 4);
            Some((CHECKPOINT_ARTIFACT, SUPPORTED_CHECKPOINT_VERSION))
        }
        "idempotency-index" => {
            seed_events(dir, format, 4);
            Some((IDEMP_ARTIFACT, SUPPORTED_IDEMP_VERSION))
        }
        "visibility-ranges" => {
            seed_visibility_ranges(dir);
            Some((VISIBILITY_ARTIFACT, SUPPORTED_VISIBILITY_VERSION))
        }
        _ => None,
    };
    let (artifact_name, live) = seeded
        .ok_or_else(|| {
            format!(
                "compat_matrix.yaml row references format {format:?} with no forge arm; \
                 add a `forge_artifact_version` arm (and an `open_outcome` arm) before \
                 adding the row, or it is a silent-skip gap"
            )
        })
        .expect("compat_matrix.yaml row must reference a format with a forge arm");
    let artifact = dir.join(artifact_name);
    assert!(
        artifact.exists(),
        "PROPERTY: seeding format {format:?} must write {artifact_name}"
    );
    forge_version_field(&artifact, live, writer_version);
    artifact
}

/// Open the forged artifact and classify the result into an `Outcome`, mapping
/// the typed `StoreError` into the canonical-refusal tag namespace the matrix
/// uses. Unknown formats — or unrelated errors — panic (no silent skip).
fn open_outcome(dir: &Path, format: &str) -> Outcome {
    let result = Store::open(config_for(dir, format));
    let classified: Result<Outcome, String> = match (format, result) {
        (_, Ok(_store)) => Ok(Outcome::OpensOk),
        ("mmap-index", Err(StoreError::MmapFutureVersion { .. })) => {
            Ok(Outcome::CanonicalRefusal("MmapFutureVersion".to_string()))
        }
        ("checkpoint", Err(StoreError::CheckpointFutureVersion { .. })) => Ok(
            Outcome::CanonicalRefusal("CheckpointFutureVersion".to_string()),
        ),
        ("idempotency-index", Err(StoreError::IdempotencyFutureVersion { .. })) => Ok(
            Outcome::CanonicalRefusal("IdempotencyFutureVersion".to_string()),
        ),
        ("visibility-ranges", Err(StoreError::HiddenRangesFutureVersion { .. })) => Ok(
            Outcome::CanonicalRefusal("HiddenRangesFutureVersion".to_string()),
        ),
        (format, Err(other)) => Err(format!(
            "PROPERTY: forward-compat for {format:?} must yield OpensOK or its canonical \
             future-version refusal; got an unrelated error {other:?}"
        )),
    };
    classified
        .expect("forward-compat open must yield OpensOK or a canonical future-version refusal")
}

fn live_supported_version(format: &str) -> u16 {
    let version = match format {
        "mmap-index" => Some(SUPPORTED_MMAP_VERSION),
        "checkpoint" => Some(SUPPORTED_CHECKPOINT_VERSION),
        "idempotency-index" => Some(SUPPORTED_IDEMP_VERSION),
        "visibility-ranges" => Some(SUPPORTED_VISIBILITY_VERSION),
        _ => None,
    };
    version
        .ok_or_else(|| format!("compat_matrix.yaml: no live version mirror for format {format:?}"))
        .expect("every COMPAT_FORMATS entry must have a live version mirror")
}

fn load_matrix() -> CompatMatrix {
    let path = matrix_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read compat matrix {}: {e}", path.display()))
        .expect("compat_matrix.yaml fixture must be readable");
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

/// On-disk formats the compat matrix governs. Every entry here MUST have a
/// self-row (OpensOK at the live version) and a forged future-version refusal
/// row in `compat_matrix.yaml`; the staleness tripwire enforces both. The
/// segment/SIDX format is intentionally absent — its version lives in a
/// msgpack-encoded header (not a fixed byte offset) and a future-version
/// segment already fails closed via `CorruptSegment` with no silent-degrade
/// path (segments are the durable bottom layer, never rebuilt from elsewhere).
const COMPAT_FORMATS: &[&str] = &[
    "mmap-index",
    "checkpoint",
    "idempotency-index",
    "visibility-ranges",
];

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
        let self_row = self_row
            .ok_or_else(|| {
                format!(
                    "compat_matrix.yaml has NO self-row for format {format:?} at the live \
                     supported version {live}. A new on-disk version without a row is a \
                     forward-compat gap: add a row \
                     {{ writer_version: {live}, reader_version: {live}, \
                     expected_outcome: OpensOK }}"
                )
            })
            .expect("compat_matrix.yaml must declare a self-row at the live version");
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
        let future = future
            .ok_or_else(|| {
                format!(
                    "compat_matrix.yaml has no forged future-version row for {format:?} \
                     at version {}; the downgrade-refusal branch would be untested",
                    live + 1
                )
            })
            .expect("compat_matrix.yaml must declare a forged future-version row");
        assert!(
            matches!(
                parse_outcome(&future.expected_outcome),
                Outcome::CanonicalRefusal(_)
            ),
            "compat_matrix.yaml future-version row for {format:?} must be a CanonicalRefusal"
        );
    }
}

/// Staleness tripwire (the future-version sibling of
/// `compat_matrix_self_row_tracks_live_version`): every governed format MUST
/// have a forged future-version row at EXACTLY `live + 1` whose
/// `reader_version == live` and whose outcome is a canonical refusal. The
/// self-row tripwire catches a missing/stale SELF-row when the on-disk version
/// is bumped, but nothing pinned the FUTURE-row to the live const — a bump that
/// left a stale future-row (writer no longer == live + 1) silently regressed
/// the downgrade-refusal coverage (the forged "future" artifact was actually a
/// supported version that OpensOK, not the refusal branch). This gate ties the
/// future-row to the live const so a bump without updating it now reds CI.
#[test]
fn compat_matrix_future_row_tracks_live_plus_one() {
    let matrix = load_matrix();

    for &format in COMPAT_FORMATS {
        let live = live_supported_version(format);
        let future_version = live + 1;
        let future_row = matrix.rows.iter().find(|r| {
            r.format == format && r.writer_version == future_version && r.reader_version == live
        });
        let future_row = future_row
            .ok_or_else(|| {
                format!(
                    "compat_matrix.yaml has NO future-version row for format {format:?} at \
                     writer_version {future_version} (live + 1) with reader_version {live}. A \
                     bump to the on-disk version without re-pinning the future-row leaves the \
                     downgrade-refusal branch untested (the old future-row may now name a \
                     supported version that OpensOK): add a row \
                     {{ writer_version: {future_version}, reader_version: {live}, \
                     expected_outcome: CanonicalRefusal:<TypedError> }}"
                )
            })
            .expect("compat_matrix.yaml must declare a future-version row at the live + 1 version");
        assert!(
            matches!(
                parse_outcome(&future_row.expected_outcome),
                Outcome::CanonicalRefusal(_)
            ),
            "compat_matrix.yaml future-version row for {format:?} at writer_version \
             {future_version} (feature_bits={}) must be a CanonicalRefusal, not {:?}",
            future_row.feature_bits,
            future_row.expected_outcome,
        );
    }
}
