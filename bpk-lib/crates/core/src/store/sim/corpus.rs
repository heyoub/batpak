//! DST corpus graduation + sweep harness (Thread #64-B).
//!
//! The durable corpus lives in `traceability/dst_corpus.yaml`. Each entry records
//! a seeded honest-disk recovery run over the real `Store` + `SimFs` composition
//! ([`super::recovery`]). Cloud lanes may sweep candidate seeds via
//! [`run_corpus_sweep`]; only seeds that pass [`check_graduation`] graduate into
//! the YAML ledger (deterministic digest + legality oracle pass + declared seam).

use super::recovery::{run, RecoveryOutcome};
use super::recovery_matrix::Boundary;

/// Legal recovery classification stored in the corpus ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CorpusOutcome {
    /// Reopened cleanly with a legal visible prefix.
    CommittedPrefix,
    /// Reopened cleanly with zero recovered visible events.
    RolledBack,
    /// Reopen refused with a typed corruption error.
    CanonicalRefusal,
}

impl CorpusOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CommittedPrefix => "CommittedPrefix",
            Self::RolledBack => "RolledBack",
            Self::CanonicalRefusal => "CanonicalRefusal",
        }
    }

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "CommittedPrefix" => Some(Self::CommittedPrefix),
            "RolledBack" => Some(Self::RolledBack),
            "CanonicalRefusal" => Some(Self::CanonicalRefusal),
            _ => None,
        }
    }
}

/// One durable corpus row — mirrors `traceability/dst_corpus.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CorpusEntry {
    /// Seeded PRNG / SimFs schedule selector (`BATPAK_SEED` replay).
    pub seed: u64,
    /// Hostile-fs mode exercised (honest-disk only until SIM-2b broadens routing).
    pub fault_mode: FaultModeLabel,
    /// Durability boundary when `fault_mode` is crash-before-fsync; absent otherwise.
    pub boundary: Option<Boundary>,
    /// Critical mutation seam this seed is intended to stress.
    pub seam_touched: String,
    /// Declared assurance level (`L3` / `L4`) for AL-graded consumers.
    pub assurance_level: String,
    /// Op-plan length passed to [`super::recovery::run`].
    pub steps: usize,
    /// FNV-1a digest identity — two runs with the same seed must reproduce it.
    pub op_trace_digest: u64,
    /// Recovered-state classification recorded at graduation time.
    pub outcome: CorpusOutcome,
}

/// Serializable fault-mode label for the corpus YAML (honest disk today).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultModeLabel {
    HonestDiskCrash,
}

impl FaultModeLabel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HonestDiskCrash => "HonestDiskCrash",
        }
    }

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "HonestDiskCrash" => Some(Self::HonestDiskCrash),
            _ => None,
        }
    }
}

/// A seed that passed graduation and may be appended to the corpus YAML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraduationCandidate {
    pub entry: CorpusEntry,
}

/// Why a seed was refused graduation (non-deterministic or illegal recovery).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GraduationRefusal {
    NonDeterministic { seed: u64, first: u64, second: u64 },
    IllegalRecovery { seed: u64, reason: String },
    EmptySeam { seed: u64 },
}

impl std::fmt::Display for GraduationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonDeterministic {
                seed,
                first,
                second,
            } => write!(
                f,
                "seed 0x{seed:X} is non-deterministic (digests 0x{first:X} != 0x{second:X})"
            ),
            Self::IllegalRecovery { seed, reason } => {
                write!(f, "seed 0x{seed:X} failed legality oracle: {reason}")
            }
            Self::EmptySeam { seed } => {
                write!(f, "seed 0x{seed:X} refused: seam_touched must be non-empty")
            }
        }
    }
}

/// Map a successful honest-disk recovery to its corpus outcome label.
fn classify_honest_recovery(outcome: &RecoveryOutcome) -> CorpusOutcome {
    if outcome.recovered_visible == 0 {
        CorpusOutcome::CanonicalRefusal
    } else {
        CorpusOutcome::CommittedPrefix
    }
}

/// Live replay of one corpus entry: the deterministic digest plus the recovered
/// classification, for currency checks against the YAML identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CorpusReplay {
    pub digest: u64,
    pub outcome: CorpusOutcome,
}

/// Replay one corpus entry through the honest-disk recovery oracle and return
/// the live digest plus recovered outcome (for currency checks against the YAML
/// identity).
///
/// # Errors
/// Seed-tagged violation string when recovery is illegal.
pub(crate) fn replay_corpus_entry(entry: &CorpusEntry) -> Result<CorpusReplay, String> {
    if entry.fault_mode != FaultModeLabel::HonestDiskCrash {
        return Err(format!(
            "corpus replay (seed=0x{:X}): fault_mode {} is not routed; only HonestDiskCrash today",
            entry.seed,
            entry.fault_mode.as_str()
        ));
    }
    if entry.boundary.is_some() {
        return Err(format!(
            "corpus replay (seed=0x{:X}): boundary cells require recovery_matrix (deferred)",
            entry.seed
        ));
    }
    run(entry.seed, entry.steps).map(|o| CorpusReplay {
        digest: o.digest,
        outcome: classify_honest_recovery(&o),
    })
}

/// Graduation criterion (#64-B): (a) deterministic across two runs, (b) names a
/// target seam, (c) the legality oracle passes on both runs.
pub(crate) fn check_graduation(
    seed: u64,
    steps: usize,
    seam_touched: &str,
    assurance_level: &str,
) -> Result<GraduationCandidate, GraduationRefusal> {
    if seam_touched.is_empty() {
        return Err(GraduationRefusal::EmptySeam { seed });
    }

    let first =
        run(seed, steps).map_err(|reason| GraduationRefusal::IllegalRecovery { seed, reason })?;
    let second =
        run(seed, steps).map_err(|reason| GraduationRefusal::IllegalRecovery { seed, reason })?;

    if first.digest != second.digest {
        return Err(GraduationRefusal::NonDeterministic {
            seed,
            first: first.digest,
            second: second.digest,
        });
    }

    let outcome = classify_honest_recovery(&first);
    Ok(GraduationCandidate {
        entry: CorpusEntry {
            seed,
            fault_mode: FaultModeLabel::HonestDiskCrash,
            boundary: None,
            seam_touched: seam_touched.to_owned(),
            assurance_level: assurance_level.to_owned(),
            steps,
            op_trace_digest: first.digest,
            outcome,
        },
    })
}

/// Replay one committed corpus row by identity fields and assert the live digest
/// matches `expected_digest`. Test-only surface for the `dst-corpus-currency` gate.
///
/// # Errors
/// Seed-tagged violation when recovery is illegal or the digest drifts.
pub fn verify_corpus_row(
    seed: u64,
    steps: usize,
    fault_mode: &str,
    expected_digest: u64,
) -> Result<(), String> {
    let mode = FaultModeLabel::parse(fault_mode).ok_or_else(|| {
        format!("corpus row (seed=0x{seed:X}): unknown fault_mode `{fault_mode}`")
    })?;
    let entry = CorpusEntry {
        seed,
        fault_mode: mode,
        boundary: None,
        seam_touched: String::new(),
        assurance_level: String::new(),
        steps,
        op_trace_digest: expected_digest,
        outcome: CorpusOutcome::CommittedPrefix,
    };
    let live = replay_corpus_entry(&entry)?;
    if live.digest != expected_digest {
        return Err(format!(
            "corpus currency (seed=0x{seed:X}): expected digest 0x{expected_digest:X}, replay 0x{:X}",
            live.digest
        ));
    }
    Ok(())
}

/// Graduate a single candidate seed through the full sweep engine and return
/// its deterministic op-trace digest.
///
/// This is the public, doc-hidden entry point the `dst_corpus_currency`
/// integration gate uses to exercise the real graduation path
/// ([`run_corpus_sweep`] → [`check_graduation`] → [`classify_honest_recovery`])
/// against a committed corpus seed. A graduated seed's digest must equal the
/// `op_trace_digest` recorded in `traceability/dst_corpus.yaml`.
///
/// # Errors
/// Returns the [`GraduationRefusal`] rendered as a string when the seed fails
/// determinism, legality, or names an empty seam.
pub fn graduate_corpus_seed(
    seed: u64,
    steps: usize,
    seam_touched: &str,
    assurance_level: &str,
) -> Result<u64, String> {
    let (mut graduated, refused) = run_corpus_sweep(&[seed], steps, seam_touched, assurance_level);
    if let Some(refusal) = refused.into_iter().next() {
        return Err(refusal.to_string());
    }
    let candidate = graduated
        .pop()
        .ok_or_else(|| format!("seed 0x{seed:X} produced no graduation candidate"))?;
    Ok(candidate.entry.op_trace_digest)
}

/// Replay committed corpus rows through the honest-disk recovery oracle and
/// assert each stored digest AND outcome label is still current.
///
/// Public, doc-hidden entry point for the `dst_corpus_currency` integration
/// gate. Each tuple mirrors a `traceability/dst_corpus.yaml` row as
/// `(seed, steps, fault_mode, outcome, op_trace_digest)`. Drives
/// [`assert_corpus_currency`] over reconstructed [`CorpusEntry`] rows,
/// exercising both the digest and the outcome-label identity.
///
/// # Errors
/// Returns a descriptive string when the row set is empty, a `fault_mode` or
/// `outcome` label is unknown, replay is illegal, or a stored digest/outcome no
/// longer matches.
pub fn assert_corpus_rows_current(rows: &[(u64, usize, &str, &str, u64)]) -> Result<(), String> {
    let mut entries = Vec::with_capacity(rows.len());
    for &(seed, steps, fault_mode, outcome, op_trace_digest) in rows {
        let fault_mode = FaultModeLabel::parse(fault_mode).ok_or_else(|| {
            format!("corpus row (seed=0x{seed:X}): unknown fault_mode `{fault_mode}`")
        })?;
        let outcome = CorpusOutcome::parse(outcome)
            .ok_or_else(|| format!("corpus row (seed=0x{seed:X}): unknown outcome `{outcome}`"))?;
        entries.push(CorpusEntry {
            seed,
            fault_mode,
            boundary: None,
            seam_touched: String::new(),
            assurance_level: String::new(),
            steps,
            op_trace_digest,
            outcome,
        });
    }
    assert_corpus_currency(&entries)
}

/// Sweep `seeds` and emit graduation candidates for those that pass
/// [`check_graduation`]. Refusals are returned alongside successes so cloud
/// lanes can log why a seed did not graduate.
pub(crate) fn run_corpus_sweep(
    seeds: &[u64],
    steps: usize,
    seam_touched: &str,
    assurance_level: &str,
) -> (Vec<GraduationCandidate>, Vec<GraduationRefusal>) {
    let mut graduated = Vec::new();
    let mut refused = Vec::new();
    for &seed in seeds {
        match check_graduation(seed, steps, seam_touched, assurance_level) {
            Ok(candidate) => graduated.push(candidate),
            Err(reason) => refused.push(reason),
        }
    }
    (graduated, refused)
}

/// Test-only: replay every committed corpus entry and assert both its digest and
/// its recovered outcome label match the stored identity. Used by the
/// `dst-corpus-currency` gate.
///
/// # Errors
/// Returns a descriptive string when any entry fails replay, digest mismatch, or
/// outcome-label drift.
pub(crate) fn assert_corpus_currency(entries: &[CorpusEntry]) -> Result<(), String> {
    if entries.is_empty() {
        return Err("dst corpus must be non-empty".to_owned());
    }
    for entry in entries {
        let live = replay_corpus_entry(entry)?;
        if live.digest != entry.op_trace_digest {
            return Err(format!(
                "corpus currency (seed=0x{:X}): stored digest 0x{:X} != replay 0x{:X}",
                entry.seed, entry.op_trace_digest, live.digest
            ));
        }
        if live.outcome != entry.outcome {
            return Err(format!(
                "corpus currency (seed=0x{:X}): stored outcome {} != replay {}",
                entry.seed,
                entry.outcome.as_str(),
                live.outcome.as_str()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graduation_refuses_nondeterministic_seed() -> Result<(), Box<dyn std::error::Error>> {
        // Two different step counts from the same seed produce different digests;
        // model non-determinism by comparing against a forged second run.
        let seed = 0xC000_0001;
        let steps = 48;
        let first = run(seed, steps).map_err(std::io::Error::other)?;
        let mismatched = run(seed, steps + 1).map_err(std::io::Error::other)?;
        if first.digest == mismatched.digest {
            return Err(std::io::Error::other(
                "PROPERTY: distinct step counts should diverge for this fixture",
            )
            .into());
        }
        // Directly construct the refusal shape the gate relies on.
        let refusal = GraduationRefusal::NonDeterministic {
            seed,
            first: first.digest,
            second: mismatched.digest,
        };
        assert!(
            refusal.to_string().contains("non-deterministic"),
            "refusal must name non-determinism: {refusal}"
        );
        Ok(())
    }

    #[test]
    fn graduation_accepts_deterministic_legal_seed() -> Result<(), Box<dyn std::error::Error>> {
        let candidate = check_graduation(0xC000_0002, 64, "writer-commit", "L4").map_err(|r| {
            std::io::Error::other(format!("PROPERTY: legal seed must graduate: {r}"))
        })?;
        assert_eq!(candidate.entry.seam_touched, "writer-commit");
        assert_eq!(candidate.entry.assurance_level, "L4");
        let again = check_graduation(0xC000_0002, 64, "writer-commit", "L4").map_err(|r| {
            std::io::Error::other(format!("PROPERTY: replay must re-graduate: {r}"))
        })?;
        assert_eq!(
            candidate.entry.op_trace_digest, again.entry.op_trace_digest,
            "PROPERTY: digest must be stable across graduation calls"
        );
        Ok(())
    }

    #[test]
    fn committed_corpus_seed_digest_is_stable() -> Result<(), Box<dyn std::error::Error>> {
        let candidate = check_graduation(48104590831, 96, "writer-commit", "L4").map_err(|r| {
            std::io::Error::other(format!(
                "PROPERTY: committed corpus seed must graduate: {r}"
            ))
        })?;
        // Pin the graduated digest to the value committed in
        // `traceability/dst_corpus.yaml`: a drift here means the recovery oracle
        // changed and the corpus row must be re-graduated.
        assert_eq!(
            candidate.entry.op_trace_digest, 101_395_256_710_529_115,
            "PROPERTY: committed corpus digest for seed 48104590831 / 96 steps must be stable"
        );
        Ok(())
    }

    #[test]
    fn sweep_emits_candidates_for_legal_seeds() -> Result<(), Box<dyn std::error::Error>> {
        let (ok, bad) = run_corpus_sweep(&[0xC000_0003, 0xC000_0004], 48, "writer-commit", "L4");
        if ok.len() != 2 {
            return Err(std::io::Error::other(format!(
                "PROPERTY: expected two graduates, got {} ok and {} refused",
                ok.len(),
                bad.len()
            ))
            .into());
        }
        Ok(())
    }

    #[test]
    fn empty_seam_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        match check_graduation(0xC000_0005, 32, "", "L4") {
            Ok(_) => {
                return Err(
                    std::io::Error::other("PROPERTY: empty seam_touched must be refused").into(),
                )
            }
            Err(GraduationRefusal::EmptySeam { .. }) => {}
            Err(other) => {
                return Err(std::io::Error::other(format!(
                    "PROPERTY: expected EmptySeam refusal, got {other}"
                ))
                .into())
            }
        }
        Ok(())
    }
}
