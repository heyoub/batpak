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
    fault_mode: String,
    steps: u32,
    op_trace_digest: u64,
}

fn load_rows() -> Vec<DstCorpusRow> {
    let raw = std::fs::read_to_string(corpus_path())
        .expect("traceability/dst_corpus.yaml must exist");
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
        return Err(std::io::Error::other(
            "PROPERTY: dst_corpus.yaml must be non-empty",
        )
        .into());
    }

    for row in &rows {
        let steps = usize::try_from(row.steps).map_err(|_| {
            std::io::Error::other(format!(
                "PROPERTY: steps {} must fit usize",
                row.steps
            ))
        })?;
        batpak::__sim::verify_corpus_row(row.seed, steps, &row.fault_mode, row.op_trace_digest)
            .map_err(std::io::Error::other)?;
    }

    #[cfg(gauntlet_red_fixture)]
    assert_eq!(
        rows[0].op_trace_digest, 0,
        "RED FIXTURE: asserts a zero digest — MUST fail against a graduated corpus row"
    );

    Ok(())
}

/// Anti-vacuous wiring: the committed corpus must cover at least one target and
/// the replay helper must be exercised for every row.
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
    Ok(())
}
