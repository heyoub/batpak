// The sim recovery oracle and __sim corpus helpers live behind
// `dangerous-test-hooks`; without the feature the whole file is empty.
#![cfg(feature = "dangerous-test-hooks")]
//! GAUNTLET B6 — DST corpus currency over the graduated seed ledger.
//!
//! justifies: INV-DST-CORPUS-CURRENCY — every row in `traceability/dst_corpus.yaml`
//! must replay through the real Store+SimFs recovery oracle with the stored
//! FNV-1a digest identity. The structural half (`dst_corpus::check`) validates
//! schema + non-emptiness; this gate proves digest replay currency.
//!
//! Requires `--features dangerous-test-hooks`. Replay one seed with
//! `BATPAK_SEED=<seed> cargo nextest run -p batpak --features dangerous-test-hooks
//! -E 'test(dst_corpus_currency_replays_committed_corpus)'`.

use serde::Deserialize;
use std::path::{Path, PathBuf};

fn corpus_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("traceability")
        .join("dst_corpus.yaml")
}

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

fn load_rows() -> Vec<DstCorpusRow> {
    let raw =
        std::fs::read_to_string(corpus_path()).expect("traceability/dst_corpus.yaml must exist");
    yaml_serde::from_str(&raw).expect("dst_corpus.yaml must parse")
}

/// GREEN: every committed corpus row replays with the stored digest identity.
///
/// RED fixture (`--cfg gauntlet_red_fixture`): asserts the first row's digest is
/// zero — false against any graduated entry, so the red half FAILS and proves
/// the currency gate bites.
#[test]
fn dst_corpus_currency_replays_committed_corpus() -> Result<(), Box<dyn std::error::Error>> {
    let rows = load_rows();
    if rows.is_empty() {
        return Err(std::io::Error::other("PROPERTY: dst_corpus.yaml must be non-empty").into());
    }

    // Drive the real graduation engine over every committed seed and assert it
    // re-graduates to the stored digest identity through the oracle declared on
    // the row (StoreRecovery, ForkIsolation, or ImportReapply).
    for row in &rows {
        let steps = usize::try_from(row.steps).map_err(|_| {
            std::io::Error::other(format!("PROPERTY: steps {} must fit usize", row.steps))
        })?;
        let graduated = batpak::__sim::graduate_corpus_cell(&batpak::__sim::GraduationRequest {
            seed: row.seed,
            steps,
            fault_mode: row.fault_mode.as_str(),
            boundary: row.boundary.as_deref(),
            one_in: row.fsync_drop_one_in,
            seam_touched: &row.seam_touched,
            assurance_level: &row.assurance_level,
            oracle: &row.oracle,
        })
        .map_err(std::io::Error::other)?;
        if graduated != row.op_trace_digest {
            return Err(std::io::Error::other(format!(
                "PROPERTY: seed {} re-graduated to digest {graduated}, stored {}",
                row.seed, row.op_trace_digest
            ))
            .into());
        }
    }

    // Replay every committed row through the corpus currency oracle by identity
    // (digest + recovered outcome label).
    let descriptors: Vec<batpak::__sim::CorpusRowDescriptor<'_>> = rows
        .iter()
        .map(|row| batpak::__sim::CorpusRowDescriptor {
            seed: row.seed,
            steps: row.steps,
            oracle: row.oracle.as_str(),
            fault_mode: row.fault_mode.as_str(),
            boundary: row.boundary.as_deref(),
            one_in: row.fsync_drop_one_in,
            outcome: row.outcome.as_str(),
            op_trace_digest: row.op_trace_digest,
        })
        .collect();
    batpak::__sim::assert_corpus_rows_current(&descriptors).map_err(std::io::Error::other)?;

    // Cross-check the single-row replay helpers still agree per row.
    for row in &rows {
        let steps = usize::try_from(row.steps).map_err(|_| {
            std::io::Error::other(format!("PROPERTY: steps {} must fit usize", row.steps))
        })?;
        if row.oracle == "StoreRecovery"
            && row.fault_mode == "HonestDiskCrash"
            && row.boundary.is_none()
        {
            batpak::__sim::verify_corpus_row(row.seed, steps, &row.fault_mode, row.op_trace_digest)
                .map_err(std::io::Error::other)?;
            let honest = batpak::__sim::graduate_corpus_seed(
                row.seed,
                steps,
                &row.seam_touched,
                &row.assurance_level,
            )
            .map_err(std::io::Error::other)?;
            if honest != row.op_trace_digest {
                return Err(std::io::Error::other(format!(
                    "PROPERTY: honest-disk seed {} re-graduated to {honest}, stored {}",
                    row.seed, row.op_trace_digest
                ))
                .into());
            }
        }
        if row.oracle == "ForkIsolation" {
            let live: batpak::__sim::CorpusReplayPublic =
                batpak::__sim::run_fork_isolation_corpus_cell(row.seed)
                    .map_err(std::io::Error::other)?;
            if live.digest != row.op_trace_digest {
                return Err(std::io::Error::other(format!(
                    "PROPERTY: fork seed {} replay digest {} != stored {}",
                    row.seed, live.digest, row.op_trace_digest
                ))
                .into());
            }
            if live.outcome != row.outcome {
                return Err(std::io::Error::other(format!(
                    "PROPERTY: fork seed {} replay outcome {} != stored {}",
                    row.seed, live.outcome, row.outcome
                ))
                .into());
            }
        }
        if row.oracle == "ImportReapply" {
            let live: batpak::__sim::CorpusReplayPublic =
                batpak::__sim::run_import_reapply_corpus_cell(row.seed, row.boundary.as_deref())
                    .map_err(std::io::Error::other)?;
            if live.digest != row.op_trace_digest {
                return Err(std::io::Error::other(format!(
                    "PROPERTY: import seed {} replay digest {} != stored {}",
                    row.seed, live.digest, row.op_trace_digest
                ))
                .into());
            }
            if live.outcome != row.outcome {
                return Err(std::io::Error::other(format!(
                    "PROPERTY: import seed {} replay outcome {} != stored {}",
                    row.seed, live.outcome, row.outcome
                ))
                .into());
            }
        }
        batpak::__sim::verify_corpus_row_cell(
            row.seed,
            steps,
            &row.fault_mode,
            row.boundary.as_deref(),
            row.fsync_drop_one_in,
            &row.oracle,
            row.op_trace_digest,
        )
        .map_err(std::io::Error::other)?;
    }

    #[cfg(gauntlet_red_fixture)]
    assert_eq!(
        rows[0].op_trace_digest, 0,
        "RED FIXTURE: asserts a zero digest — MUST fail against a graduated corpus row"
    );

    Ok(())
}

/// Anti-vacuous wiring: the committed corpus must cover fork/import seams through
/// the dedicated oracles, not ghost StoreRecovery labels.
#[test]
fn dst_corpus_currency_covers_committed_rows() -> Result<(), Box<dyn std::error::Error>> {
    let rows = load_rows();
    if rows.is_empty() {
        return Err(std::io::Error::other(
            "PROPERTY: fuzz-style wiring requires a non-empty corpus",
        )
        .into());
    }
    assert!(
        rows.iter().all(|row| row.op_trace_digest != 0),
        "PROPERTY: every corpus row must carry a non-zero digest identity"
    );
    assert!(
        rows.iter().any(|row| row.seam_touched == "fork-isolation"),
        "PROPERTY: corpus must contain at least one fork-isolation row"
    );
    assert!(
        rows.iter().any(|row| row.seam_touched == "import-reapply"),
        "PROPERTY: corpus must contain at least one import-reapply row"
    );
    assert!(
        rows.iter()
            .any(|row| { row.seam_touched == "fork-isolation" && row.oracle == "ForkIsolation" }),
        "PROPERTY: fork-isolation row must use ForkIsolation oracle"
    );
    assert!(
        rows.iter()
            .any(|row| { row.seam_touched == "import-reapply" && row.oracle == "ImportReapply" }),
        "PROPERTY: import-reapply row must use ImportReapply oracle"
    );
    Ok(())
}
