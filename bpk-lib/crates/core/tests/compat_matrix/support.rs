//! Support scaffolding for the COMPAT MATRIX harness (`tests/compat_matrix.rs`).
//!
//! Extracted from the harness so the doctrine-bearing test file stays under the
//! absolute harness line cap (split-don't-bump). This module carries the
//! version mirrors, fixture seeding, version-field forge primitive, and the
//! open/decode outcome classifier; the three table-driven gate tests live in the
//! parent harness file and drive these helpers.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{
    decode_fork_evidence_wire, decode_import_provenance_wire, encode_fork_evidence_wire,
    encode_import_provenance_wire, fork_report_body_hash, provenance_from_extensions, ForkOptions,
    ImportOptions, ImportSelector, Store, StoreConfig, StoreError,
    FORK_EVIDENCE_REPORT_SCHEMA_VERSION, IMPORT_PROVENANCE_SCHEMA_VERSION,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
/// * `SUPPORTED_FORK_EVIDENCE_VERSION` ← `store::fork_report::FORK_EVIDENCE_REPORT_SCHEMA_VERSION`
/// * `SUPPORTED_IMPORT_PROVENANCE_VERSION` ← `store::import::IMPORT_PROVENANCE_SCHEMA_VERSION`
pub const SUPPORTED_MMAP_VERSION: u16 = 5;
pub const SUPPORTED_CHECKPOINT_VERSION: u16 = 6;
pub const SUPPORTED_IDEMP_VERSION: u16 = 1;
pub const SUPPORTED_VISIBILITY_VERSION: u16 = 2;
pub const SUPPORTED_FORK_EVIDENCE_VERSION: u16 = FORK_EVIDENCE_REPORT_SCHEMA_VERSION;
pub const SUPPORTED_IMPORT_PROVENANCE_VERSION: u16 = IMPORT_PROVENANCE_SCHEMA_VERSION;

const MMAP_ARTIFACT: &str = "index.fbati";
const CHECKPOINT_ARTIFACT: &str = "index.ckpt";
const IDEMP_ARTIFACT: &str = "index.idemp";
const VISIBILITY_ARTIFACT: &str = "visibility_ranges.fbv";
const FORK_EVIDENCE_ARTIFACT: &str = "fork_evidence.fbev";
const IMPORT_PROVENANCE_ARTIFACT: &str = "import_provenance.fbip";

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
pub struct CompatMatrix {
    pub rows: Vec<CompatRow>,
}

#[derive(Debug, Deserialize)]
pub struct CompatRow {
    pub format: String,
    pub writer_version: u16,
    pub reader_version: u16,
    // Reserved capability bitset (0 today). Read in the row-failure message and
    // the self-row tripwire so it carries weight rather than being dead.
    pub feature_bits: u64,
    pub expected_outcome: String,
    pub fixture_path: String,
}

/// The legal outcomes a row may declare. `CanonicalRefusal` carries the typed
/// error tag so a row can pin WHICH canonical refusal, not merely "is_err".
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    OpensOk,
    CanonicalRefusal(String),
}

pub fn parse_outcome(raw: &str) -> Outcome {
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
        // Wire-framed report bodies are decoded outside `Store::open`; keep a
        // minimal config arm so matrix rows remain structurally valid.
        "fork-evidence" | "import-provenance" => Some(
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

/// Seed a live-version fork evidence wire artifact from a real fork report body.
fn seed_fork_evidence(dir: &Path) {
    let source_dir = dir.join("_seed_source");
    let dest_dir = dir.join("_seed_dest");
    let store = Store::open(config_for(&source_dir, "fork-evidence")).expect("open fork source");
    let coord =
        Coordinate::new("entity:fork-evidence", "scope:compat-matrix").expect("valid coordinate");
    let kind = EventKind::custom(0xF, 8);
    for i in 0..3 {
        let _ = store
            .append(&coord, kind, &serde_json::json!({ "i": i }))
            .expect("append fork seed event");
    }
    let report = store
        .fork_with_evidence(&dest_dir, ForkOptions::default())
        .expect("fork with evidence");
    store.close().expect("close fork source");
    let bytes = encode_fork_evidence_wire(&report.body).expect("encode fork evidence wire");
    std::fs::write(dir.join(FORK_EVIDENCE_ARTIFACT), &bytes)
        .expect("write seeded fork evidence artifact");
    let _decoded = decode_fork_evidence_wire(&bytes).expect("round-trip fork evidence wire");
    let _ = fork_report_body_hash(&report.body).expect("fork body hash");
}

/// Seed a live-version import provenance wire artifact from a real import.
fn seed_import_provenance(dir: &Path) {
    let source_dir = dir.join("_import_source");
    let dest_dir = dir.join("_import_dest");
    let source =
        Store::open(config_for(&source_dir, "import-provenance")).expect("open import source");
    let dest = Store::open(config_for(&dest_dir, "import-provenance")).expect("open import dest");
    let coord =
        Coordinate::new("entity:import-prov", "scope:compat-matrix").expect("valid coordinate");
    let kind = EventKind::custom(0xF, 9);
    let _ = source
        .append(&coord, kind, &serde_json::json!({ "n": 1 }))
        .expect("append import provenance seed event");
    let options = ImportOptions::new("compat-import").expect("import options");
    let report = dest
        .import_events(&source, &ImportSelector::all(), &options)
        .expect("seed import");
    assert_eq!(report.imported, 1, "seed import must import one event");
    let dest_entry = dest.by_entity("entity:import-prov")[0].clone();
    let provenance = provenance_from_extensions(dest_entry.receipt_extensions())
        .expect("seed import must record provenance on the destination receipt");
    source.close().expect("close import source");
    dest.close().expect("close import dest");
    let bytes = encode_import_provenance_wire(&provenance).expect("encode import provenance wire");
    std::fs::write(dir.join(IMPORT_PROVENANCE_ARTIFACT), &bytes)
        .expect("write seeded import provenance artifact");
    decode_import_provenance_wire(&bytes).expect("round-trip import provenance wire");
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
pub fn forge_artifact_version(dir: &Path, format: &str, writer_version: u16) -> PathBuf {
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
        "fork-evidence" => {
            seed_fork_evidence(dir);
            Some((FORK_EVIDENCE_ARTIFACT, SUPPORTED_FORK_EVIDENCE_VERSION))
        }
        "import-provenance" => {
            seed_import_provenance(dir);
            Some((
                IMPORT_PROVENANCE_ARTIFACT,
                SUPPORTED_IMPORT_PROVENANCE_VERSION,
            ))
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
pub fn open_outcome(dir: &Path, format: &str) -> Outcome {
    if format == "fork-evidence" || format == "import-provenance" {
        let artifact_name = match format {
            "fork-evidence" => FORK_EVIDENCE_ARTIFACT,
            "import-provenance" => IMPORT_PROVENANCE_ARTIFACT,
            _ => unreachable!("wire decode formats are fork-evidence and import-provenance"),
        };
        let bytes = std::fs::read(dir.join(artifact_name)).expect("read wire artifact");
        let classified: Result<Outcome, String> = match (format, bytes.as_slice()) {
            ("fork-evidence", bytes) => match decode_fork_evidence_wire(bytes) {
                Ok(_) => Ok(Outcome::OpensOk),
                Err(StoreError::ForkEvidenceFutureVersion { .. }) => Ok(Outcome::CanonicalRefusal(
                    "ForkEvidenceFutureVersion".to_string(),
                )),
                Err(other) => Err(format!(
                    "PROPERTY: forward-compat for {format:?} must yield OpensOK or \
                     ForkEvidenceFutureVersion; got {other:?}"
                )),
            },
            ("import-provenance", bytes) => match decode_import_provenance_wire(bytes) {
                Ok(_) => Ok(Outcome::OpensOk),
                Err(StoreError::ImportProvenanceFutureVersion { .. }) => Ok(
                    Outcome::CanonicalRefusal("ImportProvenanceFutureVersion".to_string()),
                ),
                Err(other) => Err(format!(
                    "PROPERTY: forward-compat for {format:?} must yield OpensOK or \
                     ImportProvenanceFutureVersion; got {other:?}"
                )),
            },
            _ => unreachable!(),
        };
        return classified.expect(
            "wire forward-compat decode must yield OpensOK or a canonical future-version refusal",
        );
    }

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

pub fn live_supported_version(format: &str) -> u16 {
    let version = match format {
        "mmap-index" => Some(SUPPORTED_MMAP_VERSION),
        "checkpoint" => Some(SUPPORTED_CHECKPOINT_VERSION),
        "idempotency-index" => Some(SUPPORTED_IDEMP_VERSION),
        "visibility-ranges" => Some(SUPPORTED_VISIBILITY_VERSION),
        "fork-evidence" => Some(SUPPORTED_FORK_EVIDENCE_VERSION),
        "import-provenance" => Some(SUPPORTED_IMPORT_PROVENANCE_VERSION),
        _ => None,
    };
    version
        .ok_or_else(|| format!("compat_matrix.yaml: no live version mirror for format {format:?}"))
        .expect("every COMPAT_FORMATS entry must have a live version mirror")
}

pub fn load_matrix() -> CompatMatrix {
    let path = matrix_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read compat matrix {}: {e}", path.display()))
        .expect("compat_matrix.yaml fixture must be readable");
    yaml_serde::from_str(&text).expect("parse compat_matrix.yaml")
}
