//! DST corpus ledger consumer (Thread #64-B, slug `dst-corpus-currency`).
//!
//! `traceability/dst_corpus.yaml` is the durable graduated-seed corpus. This module
//! validates schema and non-emptiness on every `structural-check`. Digest replay
//! currency is proven by the `dst-corpus-currency` integration gate in
//! `crates/core/tests/dst_corpus_currency.rs` (requires `dangerous-test-hooks`).

use crate::repo_surface::load_yaml;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Repo-relative path to the graduated DST corpus ledger.
pub(crate) const DST_CORPUS_REL: &str = "traceability/dst_corpus.yaml";

/// One graduated corpus row. Mirrors the schema documented in the yaml header.
#[derive(Debug, Deserialize)]
struct DstCorpusRow {
    seed: u64,
    fault_mode: String,
    boundary: Option<String>,
    seam_touched: String,
    assurance_level: String,
    steps: u32,
    op_trace_digest: u64,
    outcome: String,
}

pub(crate) fn manifest_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join(DST_CORPUS_REL)
}

pub(crate) fn load_rows(repo_root: &Path) -> Result<Vec<DstCorpusRow>> {
    load_yaml(&manifest_path(repo_root))
}

fn validate_row(row: &DstCorpusRow, index: usize) -> Result<()> {
    if row.seam_touched.is_empty() {
        bail!("dst-corpus-currency: entry[{index}] has empty seam_touched");
    }
    if row.assurance_level.is_empty() {
        bail!("dst-corpus-currency: entry[{index}] has empty assurance_level");
    }
    if row.steps == 0 {
        bail!("dst-corpus-currency: entry[{index}] steps must be >= 1");
    }
    if row.op_trace_digest == 0 {
        bail!(
            "dst-corpus-currency: entry[{index}] op_trace_digest must be non-zero (run \
             graduation to fill the identity digest)"
        );
    }
    if row.fault_mode != "HonestDiskCrash" {
        bail!(
            "dst-corpus-currency: entry[{index}] fault_mode `{}` is not routed locally yet \
             (only HonestDiskCrash today)",
            row.fault_mode
        );
    }
    match row.outcome.as_str() {
        "CommittedPrefix" | "RolledBack" | "CanonicalRefusal" => {}
        other => bail!(
            "dst-corpus-currency: entry[{index}] outcome `{other}` is not a legal classification"
        ),
    }
    Ok(())
}

/// Structural entry: schema-validates the corpus and requires at least one entry.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let rows = load_rows(repo_root).context("load dst_corpus.yaml")?;
    if rows.is_empty() {
        bail!("dst-corpus-currency: traceability/dst_corpus.yaml must be non-empty");
    }
    for (index, row) in rows.iter().enumerate() {
        validate_row(row, index)?;
    }
    outln!(
        "dst-corpus-currency: ok ({} graduated seed(s) in corpus)",
        rows.len()
    );
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
            fault_mode: "HonestDiskCrash".to_owned(),
            boundary: None,
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

    fn validate_corpus_rows(rows: &[DstCorpusRow]) -> Result<()> {
        if rows.is_empty() {
            bail!("dst-corpus-currency: traceability/dst_corpus.yaml must be non-empty");
        }
        for (index, row) in rows.iter().enumerate() {
            validate_row(row, index)?;
        }
        Ok(())
    }
}
