//! DST corpus graduation + sweep harness (Thread #64-B).
//!
//! The durable corpus lives in `traceability/dst_corpus.yaml` and is a GROWING
//! oracle: each entry records a seeded crash-recovery run over the real `Store` +
//! `SimFs` composition under one cell of the hostile-fs fault matrix —
//! honest-disk crash ([`super::recovery`]), lying-disk fsync-drop, or
//! crash-before-fsync at a durability [`Boundary`] ([`recovery_matrix`]). Only
//! seeds that pass graduation ([`check_graduation_for`]: deterministic digest
//! across two runs + legality-oracle pass + a declared seam) graduate into the
//! YAML ledger, where the `dst_corpus_currency` gate re-graduates every row and
//! checks both the digest AND the recovered outcome label.

use super::recovery::{run, RecoveryOutcome};
use super::recovery_matrix::{self, Boundary, Classification, FaultMode};

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
    /// Hostile-fs mode exercised (honest-disk crash, lying-disk fsync-drop, or
    /// crash-before-fsync at a durability boundary).
    pub fault_mode: FaultModeLabel,
    /// Durability boundary when `fault_mode` is crash-before-fsync; absent otherwise.
    pub boundary: Option<Boundary>,
    /// Critical mutation seam this seed is intended to stress.
    pub seam_touched: String,
    /// Declared assurance level (`L3` / `L4`) for AL-graded consumers.
    pub assurance_level: String,
    /// Op-plan length passed to the recovery oracle that owns `fault_mode`.
    pub steps: usize,
    /// FNV-1a digest identity — two runs with the same seed must reproduce it.
    pub op_trace_digest: u64,
    /// Recovered-state classification recorded at graduation time.
    pub outcome: CorpusOutcome,
}

/// Serializable fault-mode label for the corpus YAML.
///
/// Honest disk routes through [`super::recovery::run`]; the lying-disk and
/// crash-before-fsync modes route through [`recovery_matrix::run`] so the corpus
/// covers the full hostile-fs matrix the recovery oracle models, not just one
/// honest-disk path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultModeLabel {
    /// Honest disk — every fsync honored; a crash loses only the unsynced tail.
    HonestDiskCrash,
    /// Lying disk — roughly one fsync in `one_in` is silently dropped.
    LyingDiskFsyncDrop {
        /// 1-in-N fsync-drop rate (`>= 1`).
        one_in: u32,
    },
    /// Mid-write abort at a durability boundary, leaving a torn/partial frame.
    /// The boundary is carried in the entry's `boundary` field.
    CrashBeforeFsync,
}

impl FaultModeLabel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HonestDiskCrash => "HonestDiskCrash",
            Self::LyingDiskFsyncDrop { .. } => "LyingDiskFsyncDrop",
            Self::CrashBeforeFsync => "CrashBeforeFsync",
        }
    }

    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "HonestDiskCrash" => Some(Self::HonestDiskCrash),
            // The `one_in` rate is carried in the corpus row's dedicated column and
            // supplied to [`parse_with_rate`]; the bare label parses to a rate that
            // [`resolve_matrix_mode`] re-fills from the row, so a `0` placeholder
            // here is never routed directly.
            "LyingDiskFsyncDrop" => Some(Self::LyingDiskFsyncDrop { one_in: 0 }),
            "CrashBeforeFsync" => Some(Self::CrashBeforeFsync),
            _ => None,
        }
    }

    /// Parse a label together with the corpus row's optional drop-rate column,
    /// re-attaching the lying-disk `one_in` rate the bare label cannot carry.
    pub(crate) fn parse_with_rate(raw: &str, one_in: Option<u32>) -> Option<Self> {
        match Self::parse(raw)? {
            Self::LyingDiskFsyncDrop { .. } => Some(Self::LyingDiskFsyncDrop {
                one_in: one_in?.max(1),
            }),
            mode @ (Self::HonestDiskCrash | Self::CrashBeforeFsync) => Some(mode),
        }
    }
}

/// Map a corpus (label, boundary) pair onto the recovery-matrix [`FaultMode`] the
/// non-honest-disk rows replay through. Honest-disk rows do NOT route here (they
/// use [`super::recovery::run`]); `None` signals "not a matrix-routed mode".
fn resolve_matrix_mode(label: FaultModeLabel, boundary: Option<Boundary>) -> Option<FaultMode> {
    match label {
        FaultModeLabel::HonestDiskCrash => None,
        FaultModeLabel::LyingDiskFsyncDrop { one_in } => Some(FaultMode::LyingDiskFsyncDrop {
            one_in: one_in.max(1),
        }),
        FaultModeLabel::CrashBeforeFsync => {
            boundary.map(|boundary| FaultMode::CrashBeforeFsync { boundary })
        }
    }
}

/// Map the recovery-matrix [`Classification`] onto the corpus outcome label.
fn classification_to_outcome(class: Classification) -> CorpusOutcome {
    match class {
        Classification::CommittedPrefix => CorpusOutcome::CommittedPrefix,
        Classification::RolledBack => CorpusOutcome::RolledBack,
        Classification::CanonicalRefusal => CorpusOutcome::CanonicalRefusal,
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

/// Replay one (seed, steps, mode, boundary) cell through the recovery oracle that
/// owns it: honest-disk routes through [`super::recovery::run`]; lying-disk and
/// crash-before-fsync route through [`recovery_matrix::run`]. A
/// `CrashBeforeFsync` mode without a boundary is a malformed row and is refused.
///
/// # Errors
/// Seed-tagged violation string when recovery is illegal or the row is malformed.
fn replay_cell(
    seed: u64,
    steps: usize,
    fault_mode: FaultModeLabel,
    boundary: Option<Boundary>,
) -> Result<CorpusReplay, String> {
    match fault_mode {
        FaultModeLabel::HonestDiskCrash => {
            if boundary.is_some() {
                return Err(format!(
                    "corpus replay (seed=0x{seed:X}): HonestDiskCrash must not declare a boundary"
                ));
            }
            run(seed, steps).map(|o| CorpusReplay {
                digest: o.digest,
                outcome: classify_honest_recovery(&o),
            })
        }
        FaultModeLabel::LyingDiskFsyncDrop { .. } | FaultModeLabel::CrashBeforeFsync => {
            let mode = resolve_matrix_mode(fault_mode, boundary).ok_or_else(|| {
                format!(
                    "corpus replay (seed=0x{seed:X}): {} requires a boundary cell",
                    fault_mode.as_str()
                )
            })?;
            recovery_matrix::run(seed, steps, mode).map(|o| CorpusReplay {
                digest: o.digest,
                outcome: classification_to_outcome(o.classification),
            })
        }
    }
}

/// Replay one corpus entry through the recovery oracle that owns its fault mode
/// and return the live digest plus recovered outcome (for currency checks against
/// the YAML identity).
///
/// # Errors
/// Seed-tagged violation string when recovery is illegal.
pub(crate) fn replay_corpus_entry(entry: &CorpusEntry) -> Result<CorpusReplay, String> {
    replay_cell(entry.seed, entry.steps, entry.fault_mode, entry.boundary)
}

/// Graduation criterion (#64-B): (a) deterministic across two runs, (b) names a
/// target seam, (c) the legality oracle passes on both runs. Routes the seed
/// through the oracle that owns `fault_mode` (honest-disk or recovery-matrix).
pub(crate) fn check_graduation_for(
    seed: u64,
    steps: usize,
    fault_mode: FaultModeLabel,
    boundary: Option<Boundary>,
    seam_touched: &str,
    assurance_level: &str,
) -> Result<GraduationCandidate, GraduationRefusal> {
    if seam_touched.is_empty() {
        return Err(GraduationRefusal::EmptySeam { seed });
    }

    let first = replay_cell(seed, steps, fault_mode, boundary)
        .map_err(|reason| GraduationRefusal::IllegalRecovery { seed, reason })?;
    let second = replay_cell(seed, steps, fault_mode, boundary)
        .map_err(|reason| GraduationRefusal::IllegalRecovery { seed, reason })?;

    if first.digest != second.digest {
        return Err(GraduationRefusal::NonDeterministic {
            seed,
            first: first.digest,
            second: second.digest,
        });
    }

    Ok(GraduationCandidate {
        entry: CorpusEntry {
            seed,
            fault_mode,
            boundary,
            seam_touched: seam_touched.to_owned(),
            assurance_level: assurance_level.to_owned(),
            steps,
            op_trace_digest: first.digest,
            outcome: first.outcome,
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
    verify_corpus_row_cell(seed, steps, fault_mode, None, None, expected_digest)
}

/// Boundary/lying-disk-aware variant of [`verify_corpus_row`]: replays the cell
/// owned by `(fault_mode, boundary, one_in)` and asserts the digest identity.
///
/// `one_in` carries the lying-disk drop rate; `boundary` the crash-before-fsync
/// durability boundary. Both are `None` for an honest-disk row.
///
/// # Errors
/// Seed-tagged violation when the labels are unknown, recovery is illegal, or the
/// digest drifts.
pub fn verify_corpus_row_cell(
    seed: u64,
    steps: usize,
    fault_mode: &str,
    boundary: Option<&str>,
    one_in: Option<u32>,
    expected_digest: u64,
) -> Result<(), String> {
    let mode = FaultModeLabel::parse_with_rate(fault_mode, one_in).ok_or_else(|| {
        format!("corpus row (seed=0x{seed:X}): unknown fault_mode `{fault_mode}`")
    })?;
    let boundary = parse_boundary(seed, boundary)?;
    let live = replay_cell(seed, steps, mode, boundary)?;
    if live.digest != expected_digest {
        return Err(format!(
            "corpus currency (seed=0x{seed:X}): expected digest 0x{expected_digest:X}, replay 0x{:X}",
            live.digest
        ));
    }
    Ok(())
}

/// Parse an optional corpus boundary label into the typed [`Boundary`].
fn parse_boundary(seed: u64, boundary: Option<&str>) -> Result<Option<Boundary>, String> {
    boundary
        .map(|raw| {
            Boundary::parse(raw)
                .ok_or_else(|| format!("corpus row (seed=0x{seed:X}): unknown boundary `{raw}`"))
        })
        .transpose()
}

/// Graduate a single honest-disk candidate seed and return its op-trace digest.
///
/// This is the public, doc-hidden entry point the `dst_corpus_currency`
/// integration gate uses to exercise the real graduation path
/// (`run_corpus_sweep` -> `check_graduation_for`) for honest-disk rows. A
/// graduated seed's digest must equal the `op_trace_digest` recorded in the YAML.
///
/// # Errors
/// Returns the `GraduationRefusal` rendered as a string when the seed fails
/// determinism, legality, or names an empty seam.
pub fn graduate_corpus_seed(
    seed: u64,
    steps: usize,
    seam_touched: &str,
    assurance_level: &str,
) -> Result<u64, String> {
    graduate_corpus_cell(&GraduationRequest {
        seed,
        steps,
        fault_mode: "HonestDiskCrash",
        boundary: None,
        one_in: None,
        seam_touched,
        assurance_level,
    })
}

/// A full graduation request for an arbitrary matrix cell, passed to
/// [`graduate_corpus_cell`]. Bundling the labels avoids a long argument list.
#[derive(Debug, Clone, Copy)]
pub struct GraduationRequest<'a> {
    /// SimFs / op-plan seed.
    pub seed: u64,
    /// Op-plan length.
    pub steps: usize,
    /// Fault-mode label.
    pub fault_mode: &'a str,
    /// Durability boundary label, when `fault_mode == CrashBeforeFsync`.
    pub boundary: Option<&'a str>,
    /// 1-in-N fsync-drop rate, when `fault_mode == LyingDiskFsyncDrop`.
    pub one_in: Option<u32>,
    /// Critical mutation seam slug this seed stresses.
    pub seam_touched: &'a str,
    /// Assurance level at graduation (`L3`/`L4`).
    pub assurance_level: &'a str,
}

/// Boundary/lying-disk-aware graduation: drives the full graduation engine for an
/// arbitrary matrix cell and returns its deterministic op-trace digest. The gate
/// uses this to re-graduate every committed corpus row, honest-disk or otherwise.
///
/// # Errors
/// Returns the refusal string when the labels are unknown or the seed fails
/// determinism, legality, or names an empty seam.
pub fn graduate_corpus_cell(req: &GraduationRequest<'_>) -> Result<u64, String> {
    let mode = FaultModeLabel::parse_with_rate(req.fault_mode, req.one_in).ok_or_else(|| {
        format!(
            "corpus row (seed=0x{:X}): unknown fault_mode `{}`",
            req.seed, req.fault_mode
        )
    })?;
    let boundary = parse_boundary(req.seed, req.boundary)?;
    let candidate = check_graduation_for(
        req.seed,
        req.steps,
        mode,
        boundary,
        req.seam_touched,
        req.assurance_level,
    )
    .map_err(|r| r.to_string())?;
    Ok(candidate.entry.op_trace_digest)
}

/// Replay committed corpus rows through the recovery oracle that owns each fault
/// mode and assert each stored digest AND outcome label is still current.
///
/// Public, doc-hidden entry point for the `dst_corpus_currency` integration gate.
/// Each [`CorpusRowDescriptor`] mirrors a `traceability/dst_corpus.yaml` row.
/// Drives `assert_corpus_currency` over reconstructed `CorpusEntry` rows,
/// exercising both the digest and the outcome-label identity.
///
/// # Errors
/// Returns a descriptive string when the row set is empty, a label is unknown,
/// replay is illegal, or a stored digest/outcome no longer matches.
pub fn assert_corpus_rows_current(rows: &[CorpusRowDescriptor<'_>]) -> Result<(), String> {
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let fault_mode =
            FaultModeLabel::parse_with_rate(row.fault_mode, row.one_in).ok_or_else(|| {
                format!(
                    "corpus row (seed=0x{:X}): unknown fault_mode `{}`",
                    row.seed, row.fault_mode
                )
            })?;
        let boundary = parse_boundary(row.seed, row.boundary)?;
        let outcome = CorpusOutcome::parse(row.outcome).ok_or_else(|| {
            format!(
                "corpus row (seed=0x{:X}): unknown outcome `{}`",
                row.seed, row.outcome
            )
        })?;
        let steps = usize::try_from(row.steps).map_err(|_| {
            format!(
                "corpus row (seed=0x{:X}): steps {} must fit usize",
                row.seed, row.steps
            )
        })?;
        entries.push(CorpusEntry {
            seed: row.seed,
            fault_mode,
            boundary,
            seam_touched: String::new(),
            assurance_level: String::new(),
            steps,
            op_trace_digest: row.op_trace_digest,
            outcome,
        });
    }
    assert_corpus_currency(&entries)
}

/// A public, doc-hidden mirror of one `traceability/dst_corpus.yaml` row, passed
/// to [`assert_corpus_rows_current`]. Carries the boundary + lying-disk drop rate
/// the broadened corpus needs (both `None` for an honest-disk row).
#[derive(Debug, Clone, Copy)]
pub struct CorpusRowDescriptor<'a> {
    /// SimFs / op-plan seed.
    pub seed: u64,
    /// Op-plan length.
    pub steps: u32,
    /// Fault-mode label (`HonestDiskCrash` / `LyingDiskFsyncDrop` / `CrashBeforeFsync`).
    pub fault_mode: &'a str,
    /// Durability boundary label, when `fault_mode == CrashBeforeFsync`.
    pub boundary: Option<&'a str>,
    /// 1-in-N fsync-drop rate, when `fault_mode == LyingDiskFsyncDrop`.
    pub one_in: Option<u32>,
    /// Recovered classification label.
    pub outcome: &'a str,
    /// Stored FNV-1a digest identity.
    pub op_trace_digest: u64,
}

/// Honest-disk graduation shim (`fault_mode == HonestDiskCrash`, no boundary),
/// retained for the in-module unit tests and the honest-disk sweep.
#[cfg(test)]
pub(crate) fn check_graduation(
    seed: u64,
    steps: usize,
    seam_touched: &str,
    assurance_level: &str,
) -> Result<GraduationCandidate, GraduationRefusal> {
    check_graduation_for(
        seed,
        steps,
        FaultModeLabel::HonestDiskCrash,
        None,
        seam_touched,
        assurance_level,
    )
}

/// Sweep `seeds` (honest-disk) and emit graduation candidates for those that pass
/// [`check_graduation`]. Refusals are returned alongside successes so cloud
/// lanes can log why a seed did not graduate.
#[cfg(test)]
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
    fn fault_mode_label_round_trips_through_serialized_form() {
        // The corpus YAML is the wire form; `as_str`/`parse` are its (de)serializer.
        // A drift here would silently mis-route a corpus row to the wrong oracle.
        for (label, expect) in [
            (FaultModeLabel::HonestDiskCrash, "HonestDiskCrash"),
            (
                FaultModeLabel::LyingDiskFsyncDrop { one_in: 3 },
                "LyingDiskFsyncDrop",
            ),
            (FaultModeLabel::CrashBeforeFsync, "CrashBeforeFsync"),
        ] {
            assert_eq!(label.as_str(), expect, "label must serialize stably");
            // Bare-label parse drops the rate (carried in its own column); compare
            // by serialized identity rather than struct equality.
            let parsed = FaultModeLabel::parse(label.as_str()).expect("label must re-parse");
            assert_eq!(
                parsed.as_str(),
                label.as_str(),
                "parse∘as_str must be identity on the label"
            );
        }
        assert!(
            FaultModeLabel::parse("NotARealMode").is_none(),
            "unknown labels must not parse"
        );
        // `parse_with_rate` re-attaches and clamps the lying-disk rate.
        assert_eq!(
            FaultModeLabel::parse_with_rate("LyingDiskFsyncDrop", Some(0)),
            Some(FaultModeLabel::LyingDiskFsyncDrop { one_in: 1 }),
            "a zero drop-rate must clamp to the legal floor of 1"
        );
        assert!(
            FaultModeLabel::parse_with_rate("LyingDiskFsyncDrop", None).is_none(),
            "a lying-disk row without a drop-rate column must be rejected"
        );
    }

    #[test]
    fn boundary_round_trips_through_serialized_form() {
        for boundary in Boundary::ALL {
            let parsed = Boundary::parse(boundary.as_str()).expect("boundary must re-parse");
            assert_eq!(
                parsed, boundary,
                "parse∘as_str must be identity on every boundary"
            );
        }
        assert!(
            Boundary::parse("NotABoundary").is_none(),
            "unknown boundary labels must not parse"
        );
    }

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
