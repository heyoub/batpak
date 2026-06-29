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
//! Four formats are governed: mmap-index, checkpoint, idempotency-index,
//! visibility-ranges, fork-evidence, and import-provenance; each has an OpensOK
//! self-row and a forged future-version `CanonicalRefusal` row.
//!
//! Adding a covered (format, version) pair is a YAML edit (a new row) plus, for
//! a genuinely new FORMAT, a new arm in each of `config_for`,
//! `forge_artifact_version`, `open_outcome`, and `live_supported_version`. The
//! segment/.fbat format is an honest skip: its version is msgpack-encoded (no
//! fixed-offset forge), and a future-version segment already fails closed via
//! `CorruptSegment`.
//!
//! The fixture scaffolding (version mirrors, fixture seeding, the version-field
//! forge primitive, and the open/decode outcome classifier) lives in the
//! `support` submodule so this doctrine-bearing harness stays under the absolute
//! harness line cap — split-don't-bump. The three table-driven gate tests below
//! are the contract executor.

#[path = "compat_matrix/support.rs"]
mod support;

use support::{
    forge_artifact_version, live_supported_version, load_matrix, open_outcome, parse_outcome,
    Outcome,
};
use tempfile::TempDir;

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
    "fork-evidence",
    "import-provenance",
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
