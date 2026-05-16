use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MutationScore {
    pub(super) caught: usize,
    pub(super) missed: usize,
    pub(super) timed_out: usize,
    pub(super) unviable: usize,
    pub(super) executed: usize,
    pub(super) scored: usize,
    pub(super) score_pct: Option<usize>,
}

fn count_mutants_file(output_dir: &Path, filename: &str) -> Result<usize> {
    let path = cargo_mutants_receipt_path(output_dir, filename);
    if !path.exists() {
        return Ok(0);
    }
    let contents = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(contents.lines().filter(|l| !l.trim().is_empty()).count())
}

pub(super) fn cargo_mutants_results_dir(output_dir: &Path) -> PathBuf {
    output_dir.join("mutants.out")
}

pub(super) fn cargo_mutants_receipt_path(output_dir: &Path, filename: &str) -> PathBuf {
    cargo_mutants_results_dir(output_dir).join(filename)
}

pub(super) fn mutation_score(output_dir: &Path) -> Result<MutationScore> {
    let caught = count_mutants_file(output_dir, "caught.txt")?;
    let missed = count_mutants_file(output_dir, "missed.txt")?;
    let timed_out = count_mutants_file(output_dir, "timeout.txt")?;
    let unviable = count_mutants_file(output_dir, "unviable.txt")?;
    let scored = caught + missed;
    let executed = scored + timed_out + unviable;
    let score_pct = if scored == 0 {
        None
    } else {
        Some((caught * 100) / scored)
    };
    Ok(MutationScore {
        caught,
        missed,
        timed_out,
        unviable,
        executed,
        scored,
        score_pct,
    })
}
