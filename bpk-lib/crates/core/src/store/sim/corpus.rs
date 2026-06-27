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

use super::fork_recovery;
use super::import_recovery;
use super::recovery::{run, RecoveryOutcome};
use super::recovery_matrix::{self, Boundary, Classification, FaultMode};

/// Corpus YAML label selecting which replay oracle owns a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CorpusOracle {
    /// Writer crash-recovery oracle (`recovery` / `recovery_matrix`).
    StoreRecovery,
    /// Fork-under-fault isolation oracle (`fork_recovery`).
    ForkIsolation,
    /// Import re-apply oracle (`import_recovery`).
    ImportReapply,
}

impl CorpusOracle {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "StoreRecovery" => Some(Self::StoreRecovery),
            "ForkIsolation" => Some(Self::ForkIsolation),
            "ImportReapply" => Some(Self::ImportReapply),
            _ => None,
        }
    }
}

/// Import-reapply corpus variant carried in the optional `boundary` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportReapplyKind {
    /// Crash mid-import, reopen, re-import with dedup.
    UnderFault,
    /// Same-store import terminates at the call-time frontier.
    SameStoreCeiling,
}

pub(crate) const IMPORT_CEILING_BOUNDARY: &str = "SameStoreCeiling";

fn parse_import_kind(boundary: Option<&str>) -> ImportReapplyKind {
    match boundary {
        Some(IMPORT_CEILING_BOUNDARY) => ImportReapplyKind::SameStoreCeiling,
        _ => ImportReapplyKind::UnderFault,
    }
}

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
    /// Replay oracle that owns this row.
    pub oracle: CorpusOracle,
    /// Hostile-fs mode exercised (honest-disk crash, lying-disk fsync-drop, or
    /// crash-before-fsync at a durability boundary).
    pub fault_mode: FaultModeLabel,
    /// Durability boundary when `fault_mode` is crash-before-fsync; absent otherwise.
    /// For `ImportReapply`, may carry `SameStoreCeiling` instead.
    pub boundary: Option<Boundary>,
    /// Import-reapply variant when `oracle == ImportReapply`.
    pub import_kind: Option<ImportReapplyKind>,
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
    /// FNV-1a digest identity for this seed + oracle outcome.
    pub digest: u64,
    /// Recovered-state classification recorded at graduation time.
    pub outcome: CorpusOutcome,
}

/// Doc-hidden public mirror of [`CorpusReplay`] for integration gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusReplayPublic {
    /// FNV-1a digest identity for this seed + oracle outcome.
    pub digest: u64,
    /// Recovered classification label (`CommittedPrefix` / `RolledBack` / `CanonicalRefusal`).
    pub outcome: &'static str,
}

impl From<CorpusReplay> for CorpusReplayPublic {
    fn from(replay: CorpusReplay) -> Self {
        Self {
            digest: replay.digest,
            outcome: replay.outcome.as_str(),
        }
    }
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

/// Replay one fork-isolation corpus cell through [`fork_recovery::run_seeded_fork_fault`].
fn replay_fork_isolation_cell(seed: u64) -> Result<CorpusReplay, String> {
    let outcome = fork_recovery::run_seeded_fork_fault(seed)?;
    Ok(CorpusReplay {
        digest: outcome.digest,
        outcome: classification_to_outcome(outcome.classification),
    })
}

/// Replay one import-reapply corpus cell through the import oracle that owns the
/// variant (`UnderFault` vs `SameStoreCeiling`).
fn replay_import_reapply_cell(seed: u64, kind: ImportReapplyKind) -> Result<CorpusReplay, String> {
    match kind {
        ImportReapplyKind::UnderFault => {
            let outcome = import_recovery::run_seeded_import_fault(seed)?;
            Ok(CorpusReplay {
                digest: outcome.digest,
                outcome: CorpusOutcome::CommittedPrefix,
            })
        }
        ImportReapplyKind::SameStoreCeiling => {
            let outcome = import_recovery::run_seeded_import_same_store_ceiling(seed)?;
            Ok(CorpusReplay {
                digest: outcome.digest,
                outcome: CorpusOutcome::CommittedPrefix,
            })
        }
    }
}

/// Route one corpus entry through the oracle declared on the row.
fn replay_entry(entry: &CorpusEntry) -> Result<CorpusReplay, String> {
    match entry.oracle {
        CorpusOracle::StoreRecovery => {
            replay_cell(entry.seed, entry.steps, entry.fault_mode, entry.boundary)
        }
        CorpusOracle::ForkIsolation => replay_fork_isolation_cell(entry.seed),
        CorpusOracle::ImportReapply => {
            let kind = entry.import_kind.ok_or_else(|| {
                format!(
                    "corpus replay (seed=0x{:X}): ImportReapply row missing import_kind",
                    entry.seed
                )
            })?;
            replay_import_reapply_cell(entry.seed, kind)
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
    replay_entry(entry)
}

/// Graduation criterion (#64-B): (a) deterministic across two runs, (b) names a
/// target seam, (c) the legality oracle passes on both runs. Routes the seed
/// through the oracle that owns `fault_mode` (honest-disk or recovery-matrix).
fn check_graduation_for(
    cell: GraduationCell<'_>,
) -> Result<GraduationCandidate, GraduationRefusal> {
    if cell.seam_touched.is_empty() {
        return Err(GraduationRefusal::EmptySeam { seed: cell.seed });
    }

    let entry = CorpusEntry {
        seed: cell.seed,
        oracle: cell.oracle,
        fault_mode: cell.fault_mode,
        boundary: cell.boundary,
        import_kind: cell.import_kind,
        seam_touched: cell.seam_touched.to_owned(),
        assurance_level: cell.assurance_level.to_owned(),
        steps: cell.steps,
        op_trace_digest: 0,
        outcome: CorpusOutcome::CommittedPrefix,
    };

    let first = replay_entry(&entry).map_err(|reason| GraduationRefusal::IllegalRecovery {
        seed: cell.seed,
        reason,
    })?;
    let second = replay_entry(&entry).map_err(|reason| GraduationRefusal::IllegalRecovery {
        seed: cell.seed,
        reason,
    })?;

    if first.digest != second.digest {
        return Err(GraduationRefusal::NonDeterministic {
            seed: cell.seed,
            first: first.digest,
            second: second.digest,
        });
    }

    Ok(GraduationCandidate {
        entry: CorpusEntry {
            op_trace_digest: first.digest,
            outcome: first.outcome,
            ..entry
        },
    })
}

#[derive(Clone, Copy)]
struct GraduationCell<'a> {
    seed: u64,
    steps: usize,
    fault_mode: FaultModeLabel,
    boundary: Option<Boundary>,
    seam_touched: &'a str,
    assurance_level: &'a str,
    oracle: CorpusOracle,
    import_kind: Option<ImportReapplyKind>,
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
    verify_corpus_row_cell(
        seed,
        steps,
        fault_mode,
        None,
        None,
        "StoreRecovery",
        expected_digest,
    )
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
    oracle: &str,
    expected_digest: u64,
) -> Result<(), String> {
    let oracle = CorpusOracle::parse(oracle)
        .ok_or_else(|| format!("corpus row (seed=0x{seed:X}): unknown oracle `{oracle}`"))?;
    let mode = FaultModeLabel::parse_with_rate(fault_mode, one_in).ok_or_else(|| {
        format!("corpus row (seed=0x{seed:X}): unknown fault_mode `{fault_mode}`")
    })?;
    let (boundary, import_kind) = parse_row_labels(seed, oracle, boundary)?;
    let entry = CorpusEntry {
        seed,
        oracle,
        fault_mode: mode,
        boundary,
        import_kind,
        seam_touched: String::new(),
        assurance_level: String::new(),
        steps,
        op_trace_digest: expected_digest,
        outcome: CorpusOutcome::CommittedPrefix,
    };
    let live = replay_entry(&entry)?;
    if live.digest != expected_digest {
        return Err(format!(
            "corpus currency (seed=0x{seed:X}): expected digest 0x{expected_digest:X}, replay 0x{:X}",
            live.digest
        ));
    }
    Ok(())
}

/// Replay one fork-isolation corpus cell and return its digest + outcome.
///
/// # Errors
/// Seed-tagged violation when the fork oracle fails.
pub fn run_fork_isolation_corpus_cell(seed: u64) -> Result<CorpusReplayPublic, String> {
    replay_fork_isolation_cell(seed).map(Into::into)
}

/// Replay one import-reapply corpus cell and return its digest + outcome.
///
/// # Errors
/// Seed-tagged violation when the import oracle fails.
pub fn run_import_reapply_corpus_cell(
    seed: u64,
    boundary: Option<&str>,
) -> Result<CorpusReplayPublic, String> {
    let kind = parse_import_kind(boundary);
    replay_import_reapply_cell(seed, kind).map(Into::into)
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

/// Parse boundary/import labels for one row, honoring the declared oracle.
fn parse_row_labels(
    seed: u64,
    oracle: CorpusOracle,
    boundary: Option<&str>,
) -> Result<(Option<Boundary>, Option<ImportReapplyKind>), String> {
    match oracle {
        CorpusOracle::StoreRecovery => Ok((parse_boundary(seed, boundary)?, None)),
        CorpusOracle::ForkIsolation => {
            if boundary.is_some() {
                return Err(format!(
                    "corpus row (seed=0x{seed:X}): ForkIsolation must leave boundary null"
                ));
            }
            Ok((None, None))
        }
        CorpusOracle::ImportReapply => {
            let kind = parse_import_kind(boundary);
            if let (ImportReapplyKind::UnderFault, Some(boundary)) = (kind, boundary) {
                return Err(format!(
                    "corpus row (seed=0x{seed:X}): unknown import boundary `{}`",
                    boundary
                ));
            }
            Ok((None, Some(kind)))
        }
    }
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
        oracle: "StoreRecovery",
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
    /// Replay oracle label (`StoreRecovery` / `ForkIsolation` / `ImportReapply`).
    pub oracle: &'a str,
}

/// Boundary/lying-disk-aware graduation: drives the full graduation engine for an
/// arbitrary matrix cell and returns its deterministic op-trace digest. The gate
/// uses this to re-graduate every committed corpus row, honest-disk or otherwise.
///
/// # Errors
/// Returns the refusal string when the labels are unknown or the seed fails
/// determinism, legality, or names an empty seam.
pub fn graduate_corpus_cell(req: &GraduationRequest<'_>) -> Result<u64, String> {
    let oracle = CorpusOracle::parse(req.oracle).ok_or_else(|| {
        format!(
            "corpus row (seed=0x{:X}): unknown oracle `{}`",
            req.seed, req.oracle
        )
    })?;
    let mode = FaultModeLabel::parse_with_rate(req.fault_mode, req.one_in).ok_or_else(|| {
        format!(
            "corpus row (seed=0x{:X}): unknown fault_mode `{}`",
            req.seed, req.fault_mode
        )
    })?;
    let (boundary, import_kind) = parse_row_labels(req.seed, oracle, req.boundary)?;
    let candidate = check_graduation_for(GraduationCell {
        seed: req.seed,
        steps: req.steps,
        fault_mode: mode,
        boundary,
        seam_touched: req.seam_touched,
        assurance_level: req.assurance_level,
        oracle,
        import_kind,
    })
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
        let oracle = CorpusOracle::parse(row.oracle).ok_or_else(|| {
            format!(
                "corpus row (seed=0x{:X}): unknown oracle `{}`",
                row.seed, row.oracle
            )
        })?;
        let fault_mode =
            FaultModeLabel::parse_with_rate(row.fault_mode, row.one_in).ok_or_else(|| {
                format!(
                    "corpus row (seed=0x{:X}): unknown fault_mode `{}`",
                    row.seed, row.fault_mode
                )
            })?;
        let (boundary, import_kind) = parse_row_labels(row.seed, oracle, row.boundary)?;
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
            oracle,
            fault_mode,
            boundary,
            import_kind,
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
    /// Replay oracle label.
    pub oracle: &'a str,
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
    check_graduation_for(GraduationCell {
        seed,
        steps,
        fault_mode: FaultModeLabel::HonestDiskCrash,
        boundary: None,
        seam_touched,
        assurance_level,
        oracle: CorpusOracle::StoreRecovery,
        import_kind: None,
    })
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
#[path = "corpus_tests.rs"]
mod tests;
