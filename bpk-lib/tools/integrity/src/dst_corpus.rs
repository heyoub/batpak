//! DST corpus ledger consumer (Thread #64-B, slug `dst-corpus-currency`).
//!
//! `traceability/dst_corpus.yaml` is the durable graduated-seed corpus. This module
//! validates schema and non-emptiness on every `structural-check`. Digest replay
//! currency is proven by the `dst-corpus-currency` integration gate in
//! `crates/core/tests/dst_corpus_currency.rs` (requires `dangerous-test-hooks`).

use crate::assurance::{load_seam_registry, SeamRegistryEntry};
use crate::repo_surface::load_yaml;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

/// Repo-relative path to the graduated DST corpus ledger.
pub(crate) const DST_CORPUS_REL: &str = "traceability/dst_corpus.yaml";

const ORACLE_LABELS: [&str; 3] = ["StoreRecovery", "ForkIsolation", "ImportReapply"];
const IMPORT_CEILING_BOUNDARY: &str = "SameStoreCeiling";

/// One graduated corpus row. Mirrors the schema documented in the yaml header.
#[derive(Debug, Deserialize)]
struct DstCorpusRow {
    seed: u64,
    oracle: String,
    fault_mode: String,
    #[serde(default)]
    boundary: Option<String>,
    #[serde(default)]
    fsync_drop_one_in: Option<u32>,
    seam_touched: String,
    assurance_level: String,
    steps: u32,
    op_trace_digest: u64,
    outcome: String,
}

/// The durability-boundary labels a `CrashBeforeFsync` row may declare. Mirrors
/// `store::sim::recovery_matrix::Boundary`.
const BOUNDARY_LABELS: [&str; 4] = [
    "SingleAppendFrame",
    "BatchCommitMarker",
    "BatchPostFsyncPrePublish",
    "SegmentRotationCreate",
];

fn manifest_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join(DST_CORPUS_REL)
}

fn load_rows(repo_root: &Path) -> Result<Vec<DstCorpusRow>> {
    load_yaml(&manifest_path(repo_root))
}

fn validate_store_recovery_row(row: &DstCorpusRow, index: usize) -> Result<()> {
    let seed = row.seed;
    match row.fault_mode.as_str() {
        "HonestDiskCrash" => {
            if row.boundary.is_some() {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) HonestDiskCrash must leave \
                     boundary null"
                );
            }
            if row.fsync_drop_one_in.is_some() {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) HonestDiskCrash must leave \
                     fsync_drop_one_in null"
                );
            }
        }
        "LyingDiskFsyncDrop" => {
            if row.boundary.is_some() {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) LyingDiskFsyncDrop must \
                     leave boundary null"
                );
            }
            match row.fsync_drop_one_in {
                Some(rate) if rate >= 1 => {}
                _ => bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) LyingDiskFsyncDrop requires \
                     fsync_drop_one_in >= 1"
                ),
            }
        }
        "CrashBeforeFsync" => {
            if row.fsync_drop_one_in.is_some() {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) CrashBeforeFsync must leave \
                     fsync_drop_one_in null"
                );
            }
            match row.boundary.as_deref() {
                Some(label) if BOUNDARY_LABELS.contains(&label) => {}
                Some(other) => bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) unknown boundary `{other}`"
                ),
                None => bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) CrashBeforeFsync requires a \
                     boundary label"
                ),
            }
        }
        other => bail!(
            "dst-corpus-currency: entry[{index}] (seed={seed}) fault_mode `{other}` is not a \
             routed mode (HonestDiskCrash | LyingDiskFsyncDrop | CrashBeforeFsync)"
        ),
    }
    Ok(())
}

fn validate_oracle_seam_pairing(row: &DstCorpusRow, index: usize) -> Result<()> {
    let seed = row.seed;
    match row.oracle.as_str() {
        "ForkIsolation" if row.seam_touched != "fork-isolation" => bail!(
            "dst-corpus-currency: entry[{index}] (seed={seed}) ForkIsolation oracle requires \
             seam_touched fork-isolation"
        ),
        "ImportReapply" if row.seam_touched != "import-reapply" => bail!(
            "dst-corpus-currency: entry[{index}] (seed={seed}) ImportReapply oracle requires \
             seam_touched import-reapply"
        ),
        "StoreRecovery"
            if matches!(
                row.seam_touched.as_str(),
                "fork-isolation" | "import-reapply"
            ) =>
        {
            bail!(
                "dst-corpus-currency: entry[{index}] (seed={seed}) seam `{}` must not route through \
                 StoreRecovery",
                row.seam_touched
            );
        }
        _ => {}
    }
    Ok(())
}

fn validate_row(row: &DstCorpusRow, index: usize) -> Result<()> {
    let seed = row.seed;
    if row.seam_touched.is_empty() {
        bail!("dst-corpus-currency: entry[{index}] (seed={seed}) has empty seam_touched");
    }
    if row.assurance_level.is_empty() {
        bail!("dst-corpus-currency: entry[{index}] (seed={seed}) has empty assurance_level");
    }
    if row.steps == 0 {
        bail!("dst-corpus-currency: entry[{index}] (seed={seed}) steps must be >= 1");
    }
    if row.op_trace_digest == 0 {
        bail!(
            "dst-corpus-currency: entry[{index}] (seed={seed}) op_trace_digest must be non-zero \
             (run graduation to fill the identity digest)"
        );
    }
    if !ORACLE_LABELS.contains(&row.oracle.as_str()) {
        bail!(
            "dst-corpus-currency: entry[{index}] (seed={seed}) oracle `{}` is not routed \
             ({ORACLE_LABELS:?})",
            row.oracle
        );
    }
    validate_oracle_seam_pairing(row, index)?;
    match row.oracle.as_str() {
        "StoreRecovery" => validate_store_recovery_row(row, index)?,
        "ForkIsolation" => {
            if row.fault_mode != "HonestDiskCrash" {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) ForkIsolation rows use \
                     fault_mode HonestDiskCrash as an inert placeholder"
                );
            }
            if row.boundary.is_some() {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) ForkIsolation must leave \
                     boundary null"
                );
            }
            if row.fsync_drop_one_in.is_some() {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) ForkIsolation must leave \
                     fsync_drop_one_in null"
                );
            }
        }
        "ImportReapply" => {
            if row.fault_mode != "HonestDiskCrash" {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) ImportReapply rows use \
                     fault_mode HonestDiskCrash as an inert placeholder"
                );
            }
            if row.fsync_drop_one_in.is_some() {
                bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) ImportReapply must leave \
                     fsync_drop_one_in null"
                );
            }
            match row.boundary.as_deref() {
                None | Some(IMPORT_CEILING_BOUNDARY) => {}
                Some(other) => bail!(
                    "dst-corpus-currency: entry[{index}] (seed={seed}) ImportReapply boundary \
                     must be null or `{IMPORT_CEILING_BOUNDARY}`, got `{other}`"
                ),
            }
        }
        _ => {}
    }
    match row.outcome.as_str() {
        "CommittedPrefix" | "RolledBack" | "CanonicalRefusal" => {}
        other => bail!(
            "dst-corpus-currency: entry[{index}] outcome `{other}` is not a legal classification"
        ),
    }
    Ok(())
}

fn check_dst_coverage_lockstep(
    rows: &[DstCorpusRow],
    seam_registry: &[SeamRegistryEntry],
) -> Result<()> {
    let mut covered = HashSet::new();
    for row in rows {
        covered.insert(row.seam_touched.as_str());
    }
    for entry in seam_registry {
        if entry.dst_coverage && !covered.contains(entry.slug.as_str()) {
            bail!(
                "dst-corpus-currency: seam `{}` declares dst_coverage but has no corpus row with \
                 seam_touched `{}`",
                entry.slug,
                entry.slug
            );
        }
    }
    Ok(())
}

/// Structural entry: schema-validates the corpus and requires at least one entry.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let rows = load_rows(repo_root).context("load dst_corpus.yaml")?;
    validate_corpus_rows(&rows)?;
    let seam_registry = load_seam_registry(repo_root).context("load seam_registry.yaml")?;
    check_dst_coverage_lockstep(&rows, &seam_registry)?;
    outln!(
        "dst-corpus-currency: ok ({} graduated seed(s) in corpus)",
        rows.len()
    );
    Ok(())
}

fn validate_corpus_rows(rows: &[DstCorpusRow]) -> Result<()> {
    if rows.is_empty() {
        bail!("dst-corpus-currency: traceability/dst_corpus.yaml must be non-empty");
    }
    for (index, row) in rows.iter().enumerate() {
        validate_row(row, index)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_surface::repo_root;

    fn repo() -> std::path::PathBuf {
        repo_root().expect("repo root resolves from tools/integrity")
    }

    #[test]
    fn committed_corpus_passes_schema_check() {
        check(&repo()).expect("committed dst_corpus.yaml must pass schema check");
    }

    #[test]
    fn empty_corpus_fails_schema_check() {
        let err = validate_corpus_rows(&[]).expect_err("empty corpus must fail");
        assert!(
            err.to_string().contains("non-empty"),
            "error must mention non-empty requirement, got: {err}"
        );
    }

    #[test]
    fn zero_digest_fails_schema_check() {
        let rows = vec![DstCorpusRow {
            seed: 1,
            oracle: "StoreRecovery".to_owned(),
            fault_mode: "HonestDiskCrash".to_owned(),
            boundary: None,
            fsync_drop_one_in: None,
            seam_touched: "writer-commit".to_owned(),
            assurance_level: "L4".to_owned(),
            steps: 32,
            op_trace_digest: 0,
            outcome: "CommittedPrefix".to_owned(),
        }];
        let err = validate_corpus_rows(&rows).expect_err("zero digest must fail");
        assert!(
            err.to_string().contains("op_trace_digest"),
            "error must mention digest, got: {err}"
        );
    }

    fn row(
        oracle: &str,
        seam: &str,
        fault_mode: &str,
        boundary: Option<&str>,
        fsync_drop_one_in: Option<u32>,
    ) -> DstCorpusRow {
        DstCorpusRow {
            seed: 7,
            oracle: oracle.to_owned(),
            fault_mode: fault_mode.to_owned(),
            boundary: boundary.map(str::to_owned),
            fsync_drop_one_in,
            seam_touched: seam.to_owned(),
            assurance_level: "L4".to_owned(),
            steps: 32,
            op_trace_digest: 123,
            outcome: "CommittedPrefix".to_owned(),
        }
    }

    #[test]
    fn crash_before_fsync_requires_a_known_boundary() {
        let err = validate_row(
            &row(
                "StoreRecovery",
                "single-append-frame",
                "CrashBeforeFsync",
                None,
                None,
            ),
            0,
        )
        .expect_err("CrashBeforeFsync without a boundary must fail");
        assert!(
            err.to_string().contains("requires a"),
            "error must demand a boundary, got: {err}"
        );
        let err = validate_row(
            &row(
                "StoreRecovery",
                "single-append-frame",
                "CrashBeforeFsync",
                Some("Nope"),
                None,
            ),
            0,
        )
        .expect_err("unknown boundary must fail");
        assert!(
            err.to_string().contains("unknown boundary"),
            "error must name the unknown boundary, got: {err}"
        );
        validate_row(
            &row(
                "StoreRecovery",
                "batch-commit-marker",
                "CrashBeforeFsync",
                Some("BatchCommitMarker"),
                None,
            ),
            0,
        )
        .expect("known boundary must pass");
    }

    #[test]
    fn lying_disk_requires_a_drop_rate_and_no_boundary() {
        let err = validate_row(
            &row(
                "StoreRecovery",
                "durability-fsync",
                "LyingDiskFsyncDrop",
                None,
                None,
            ),
            0,
        )
        .expect_err("LyingDiskFsyncDrop without a drop rate must fail");
        assert!(
            err.to_string().contains("fsync_drop_one_in"),
            "error must demand a drop rate, got: {err}"
        );
        let err = validate_row(
            &row(
                "StoreRecovery",
                "durability-fsync",
                "LyingDiskFsyncDrop",
                Some("BatchCommitMarker"),
                Some(2),
            ),
            0,
        )
        .expect_err("LyingDiskFsyncDrop with a boundary must fail");
        assert!(
            err.to_string().contains("boundary"),
            "error must reject the boundary, got: {err}"
        );
        validate_row(
            &row(
                "StoreRecovery",
                "durability-fsync",
                "LyingDiskFsyncDrop",
                None,
                Some(2),
            ),
            0,
        )
        .expect("valid lying-disk row must pass");
    }

    #[test]
    fn honest_disk_rejects_boundary_and_drop_rate() {
        let err = validate_row(
            &row(
                "StoreRecovery",
                "writer-commit",
                "HonestDiskCrash",
                Some("BatchCommitMarker"),
                None,
            ),
            0,
        )
        .expect_err("HonestDiskCrash with a boundary must fail");
        assert!(
            err.to_string().contains("boundary"),
            "error must reject the boundary, got: {err}"
        );
        let err = validate_row(
            &row(
                "StoreRecovery",
                "writer-commit",
                "HonestDiskCrash",
                None,
                Some(2),
            ),
            0,
        )
        .expect_err("HonestDiskCrash with a drop rate must fail");
        assert!(
            err.to_string().contains("fsync_drop_one_in"),
            "error must reject the drop rate, got: {err}"
        );
    }

    #[test]
    fn unknown_fault_mode_is_rejected() {
        let err = validate_row(
            &row("StoreRecovery", "writer-commit", "Teleport", None, None),
            0,
        )
        .expect_err("unknown fault_mode must fail");
        assert!(
            err.to_string().contains("routed mode"),
            "error must reject the unknown mode, got: {err}"
        );
    }

    #[test]
    fn fork_isolation_requires_matching_oracle_and_seam() {
        validate_row(
            &row(
                "ForkIsolation",
                "fork-isolation",
                "HonestDiskCrash",
                None,
                None,
            ),
            0,
        )
        .expect("valid fork row must pass");
        let err = validate_row(
            &row(
                "StoreRecovery",
                "fork-isolation",
                "HonestDiskCrash",
                None,
                None,
            ),
            0,
        )
        .expect_err("fork seam through StoreRecovery must fail");
        assert!(
            err.to_string()
                .contains("must not route through StoreRecovery"),
            "error must reject ghost routing, got: {err}"
        );
        let err = validate_row(
            &row(
                "ForkIsolation",
                "writer-commit",
                "HonestDiskCrash",
                None,
                None,
            ),
            0,
        )
        .expect_err("ForkIsolation with wrong seam must fail");
        assert!(
            err.to_string()
                .contains("requires seam_touched fork-isolation"),
            "error must demand fork-isolation seam, got: {err}"
        );
    }

    #[test]
    fn import_reapply_requires_matching_oracle_and_seam() {
        validate_row(
            &row(
                "ImportReapply",
                "import-reapply",
                "HonestDiskCrash",
                None,
                None,
            ),
            0,
        )
        .expect("valid import row must pass");
        validate_row(
            &row(
                "ImportReapply",
                "import-reapply",
                "HonestDiskCrash",
                Some(IMPORT_CEILING_BOUNDARY),
                None,
            ),
            0,
        )
        .expect("valid import ceiling row must pass");
    }

    #[test]
    fn dst_coverage_true_requires_matching_corpus_row() {
        let rows = vec![row(
            "StoreRecovery",
            "writer-commit",
            "HonestDiskCrash",
            None,
            None,
        )];
        let registry = vec![SeamRegistryEntry {
            slug: "fork-isolation".to_owned(),
            assurance_level: "L4".to_owned(),
            dst_coverage: true,
            globs: vec!["crates/core/src/store/fork_report.rs".to_owned()],
        }];
        let err = check_dst_coverage_lockstep(&rows, &registry)
            .expect_err("dst_coverage without row must fail");
        assert!(
            err.to_string().contains("fork-isolation"),
            "error must name the uncovered seam, got: {err}"
        );
    }

    #[test]
    fn missing_oracle_is_rejected() {
        let mut bad = row(
            "StoreRecovery",
            "writer-commit",
            "HonestDiskCrash",
            None,
            None,
        );
        bad.oracle = "Teleport".to_owned();
        let err = validate_row(&bad, 0).expect_err("unknown oracle must fail");
        assert!(
            err.to_string().contains("oracle"),
            "error must mention oracle, got: {err}"
        );
    }
}
