//! GAUNTLET B3 — the recovery legality oracle generalized across the FULL
//! hostile-fs fault matrix `SimFs` (plus the durability-boundary fault injector)
//! can model over the REAL [`Store`].
//!
//! B2 ([`super::recovery`]) ran the legality oracle under ONE fault profile:
//! honest disk (`fsync_drop = 0`), crash = truncate the write-but-unsynced tail.
//! B3 keeps the SAME genuine composition — a real [`Store::open`] with
//! [`super::fs::SimFs`] installed via [`crate::store::StoreConfig::with_fs`],
//! driven through the real `append`/`append_batch`/`sync` API, crashed, then
//! REOPENED over the persisted (truncated) tree — and parameterizes it over a
//! `fault_mode × durability_boundary × seed` matrix:
//!
//!   * [`FaultMode::HonestDiskCrash`] — `fsync_drop = 0`. A crash loses only the
//!     unsynced tail, so the SACRED RULE holds: no acknowledged-durable commit
//!     may be lost. The boundary is which ops were synced before the crash.
//!   * [`FaultMode::LyingDiskFsyncDrop`] — `fsync_drop_one_in > 0`. A dropped
//!     fsync returns `Ok` to the store but does NOT advance the durable length,
//!     so a "confirmed" commit MAY be absent after the crash. That loss is the
//!     FS's fault and is LEGAL — but the recovered state must STILL be a prefix,
//!     undead-free, and hash-chain-intact. Losing the dropped commit is allowed;
//!     exposing an undead/corrupt one is not.
//!   * [`FaultMode::CrashBeforeFsync { boundary }`] — a [`CountdownInjector`]
//!     aborts the write at a chosen durability [`Boundary`] (single-append frame
//!     write, batch-commit-marker write, post-fsync-before-publish, or
//!     segment-rotation create), leaving a genuinely torn/partial frame on the
//!     real file; the unsynced tail is then truncated and the store reopened.
//!     A torn frame must recover as a clean prefix or a typed corruption refusal,
//!     never a half-ghost.
//!
//! For EVERY cell the recovered state must be EXACTLY one of
//! {Committed-prefix | RolledBack | CanonicalRefusal} and LEGAL: a prefix of the
//! appended op-log (no invented/undead events), an intact hash chain across the
//! recovered visible events, the honest-disk no-loss rule (or the lying-disk
//! relaxation), and a typed refusal on reopen counts as legal. The same
//! `(seed, fault_mode, boundary)` triple recovers the IDENTICAL classification +
//! digest (determinism).
//!
//! Honest deferral: `SimFs::sync_parent_dir` is modeled as always-durable (a
//! crash truncates file CONTENTS, it never unlinks a created file), so a pure
//! "parent-dir sync dropped" mode is not independently modeled here; the
//! [`Boundary::SegmentRotationCreate`] cell exercises the new-segment-create +
//! dir-sync window via the injector instead. See GAUNTLET_ISSUES.md.

use super::fs::SimFs;
use super::recovery::{
    batch_items, fold, is_canonical_refusal, recovered_user_events, verify_hash_chain, Op, OpPlan,
    Violation, FNV_OFFSET, KIND,
};
use super::seed_from_env;
use crate::coordinate::Coordinate;
use crate::store::fault::{FaultInjector, InjectionPoint};
use crate::store::{Store, StoreConfig, StoreError, SyncMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The entity name `Store::open` appends its `SYSTEM_OPEN_COMPLETED` lifecycle
/// event under (see `crates/core/src/store/open.rs`). The single-append boundary
/// filter excludes it so the fault arms on a driven user op, not on open.
const LIFECYCLE_ENTITY: &str = "batpak:store";

/// A durability boundary at which a [`FaultMode::CrashBeforeFsync`] run aborts
/// the in-flight write, leaving a torn/partial frame on the real file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Boundary {
    /// After a single-event frame is written but before its fsync — the unsynced
    /// frame tail must be truncated cleanly on crash.
    SingleAppendFrame,
    /// After a batch COMMIT marker is written but before its fsync — the
    /// un-durable batch must roll back wholesale, never half-commit.
    BatchCommitMarker,
    /// After the batch fsync succeeded but before the in-memory index publish —
    /// the durable batch must come back Committed (the sacred post-fsync window).
    BatchPostFsyncPrePublish,
    /// During segment rotation, before the new active segment file is created
    /// (the new-segment create + parent-dir-sync window).
    SegmentRotationCreate,
}

impl Boundary {
    /// Every boundary the matrix exercises.
    pub(crate) const ALL: [Boundary; 4] = [
        Boundary::SingleAppendFrame,
        Boundary::BatchCommitMarker,
        Boundary::BatchPostFsyncPrePublish,
        Boundary::SegmentRotationCreate,
    ];

    /// A stable token folded into the determinism digest so each boundary's
    /// recovered classification is distinguishable.
    fn token(self) -> u64 {
        match self {
            Boundary::SingleAppendFrame => 0xB0_01,
            Boundary::BatchCommitMarker => 0xB0_02,
            Boundary::BatchPostFsyncPrePublish => 0xB0_03,
            Boundary::SegmentRotationCreate => 0xB0_04,
        }
    }

    /// Does `point` match this boundary? Used to filter the [`CountdownInjector`].
    ///
    /// The `SingleAppendFrame` boundary deliberately EXCLUDES the `batpak:store`
    /// lifecycle entity: every `Store::open` appends a `SYSTEM_OPEN_COMPLETED`
    /// event through the single-append path, which would otherwise consume the
    /// one-shot fault during initialization rather than on a driven user op.
    fn matches(self, point: &InjectionPoint) -> bool {
        match self {
            Boundary::SingleAppendFrame => {
                matches!(point, InjectionPoint::SingleAppendWritten { entity } if entity != LIFECYCLE_ENTITY)
            }
            Boundary::BatchCommitMarker => {
                matches!(point, InjectionPoint::BatchCommitWritten { .. })
            }
            Boundary::BatchPostFsyncPrePublish => {
                matches!(point, InjectionPoint::BatchPrePublish { .. })
            }
            Boundary::SegmentRotationCreate => {
                matches!(point, InjectionPoint::SegmentRotationCreate { .. })
            }
        }
    }
}

/// One fault mode of the hostile-fs matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultMode {
    /// Honest disk: every fsync honored; a crash loses only the unsynced tail.
    HonestDiskCrash,
    /// Lying disk: roughly one fsync in `1/rate` is silently dropped, so an
    /// "acknowledged" commit may be lost on the crash.
    LyingDiskFsyncDrop {
        /// 1-in-N fsync-drop rate (`>= 1`).
        one_in: u32,
    },
    /// Mid-write abort at a durability [`Boundary`] via the fault injector,
    /// leaving a torn/partial frame; then crash + reopen.
    CrashBeforeFsync {
        /// Boundary at which the write is aborted.
        boundary: Boundary,
    },
}

impl FaultMode {
    /// A stable digest token discriminating the recovered classification per mode.
    fn token(self) -> u64 {
        match self {
            FaultMode::HonestDiskCrash => fold(0xF0_DE_00, 0),
            FaultMode::LyingDiskFsyncDrop { one_in } => fold(0xF0_DE_01, u64::from(one_in)),
            FaultMode::CrashBeforeFsync { boundary } => fold(0xF0_DE_02, boundary.token()),
        }
    }

    /// Whether this mode runs over a lying disk (the no-loss rule is relaxed: a
    /// dropped-fsync commit may legally be absent).
    fn is_lying_disk(self) -> bool {
        matches!(self, FaultMode::LyingDiskFsyncDrop { .. })
    }

    /// The `fsync_drop_one_in` to seed [`SimFs`] with for this mode.
    fn fsync_drop_one_in(self) -> u32 {
        match self {
            FaultMode::LyingDiskFsyncDrop { one_in } => one_in.max(1),
            FaultMode::HonestDiskCrash | FaultMode::CrashBeforeFsync { .. } => 0,
        }
    }
}

/// How the recovered state classifies against the op-log model. EXACTLY one of
/// these is legal; anything else is a half-ghost the oracle rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Classification {
    /// Reopened cleanly and the recovered visible prefix is legal.
    CommittedPrefix,
    /// Reopened cleanly with zero recovered visible events (full roll-back).
    RolledBack,
    /// Reopen refused with a typed corruption error (canonical refusal).
    CanonicalRefusal,
}

/// Outcome of one matrix cell: the determinism digest, the classification, and
/// the counts the legality oracle asserted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellOutcome {
    /// FNV-1a digest folding the cell key, op-trace, and recovered visible state.
    pub(crate) digest: u64,
    /// Recovered classification.
    pub(crate) classification: Classification,
    /// Number of user-visible events recovered after the crash.
    pub(crate) recovered_visible: usize,
    /// Number of ops the driver acknowledged durable (via an honored sync; under
    /// a lying disk an "acked" sync may have been silently dropped).
    pub(crate) durable_acked: usize,
}

/// A legality violation in one matrix cell.
pub(crate) enum CellViolation {
    /// Honest-disk mode lost an acknowledged-durable commit (the sacred rule).
    LostDurableCommit {
        /// Acked-durable count before the crash.
        durable: usize,
        /// Recovered visible count.
        recovered: usize,
    },
    /// An undead/invented event appeared (recovered > appended). Illegal in EVERY
    /// mode, including lying disk — the FS may lose, never resurrect.
    UndeadEvent {
        /// Recovered visible count.
        recovered: usize,
        /// Total ops appended before the crash.
        appended: usize,
    },
    /// The recovered visible events do not form an intact hash chain.
    BrokenHashChain {
        /// Global sequence of the event whose chain link failed.
        global_sequence: u64,
    },
    /// Reopen failed with a non-canonical (untyped) error.
    NonCanonicalReopen(String),
}

/// The matrix reuses [`super::recovery`]'s legality helpers, which return its
/// [`Violation`]; map each arm onto the matrix's own [`CellViolation`] so `?`
/// works through the shared `run_op_plan` / `verify_hash_chain` helpers.
impl From<Violation> for CellViolation {
    fn from(v: Violation) -> Self {
        match v {
            Violation::BrokenHashChain { global_sequence } => {
                CellViolation::BrokenHashChain { global_sequence }
            }
            Violation::LostDurableCommit { durable, recovered } => {
                CellViolation::LostDurableCommit { durable, recovered }
            }
            Violation::UndeadEvent {
                recovered,
                appended,
            } => CellViolation::UndeadEvent {
                recovered,
                appended,
            },
            Violation::NonCanonicalReopen(reason) => CellViolation::NonCanonicalReopen(reason),
        }
    }
}

impl std::fmt::Display for CellViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LostDurableCommit { durable, recovered } => write!(
                f,
                "lost acknowledged-durable commit: recovered {recovered} visible < {durable} durable \
                 (honest disk)"
            ),
            Self::UndeadEvent {
                recovered,
                appended,
            } => write!(
                f,
                "undead/invented event: recovered {recovered} visible > {appended} appended"
            ),
            Self::BrokenHashChain { global_sequence } => write!(
                f,
                "broken hash chain at recovered event global_sequence={global_sequence}"
            ),
            Self::NonCanonicalReopen(reason) => write!(f, "non-canonical reopen: {reason}"),
        }
    }
}

/// Run one matrix cell: drive a seeded op stream over a real `Store` backed by
/// `SimFs` under `mode`, crash, reopen, and classify the recovered state.
///
/// # Errors
/// Returns a seed/mode-tagged [`CellViolation`] string if the recovered state is
/// illegal for the cell's fault mode.
pub(crate) fn run(seed: u64, steps: usize, mode: FaultMode) -> Result<CellOutcome, String> {
    drive(seed, steps, mode).map_err(|v| format!("B3 violation (seed={seed}, mode={mode:?}): {v}"))
}

/// What the driver observed before the crash. `attempted` is the no-undead
/// ceiling (every event whose payload the driver actually submitted, whether or
/// not the op returned `Ok`), and `durable_acked` is the no-loss floor (events an
/// honored `sync` confirmed durable).
struct DriveStats {
    /// Total events the driver SUBMITTED (sum of op sizes, Ok or fault-aborted).
    /// A recovered event with content beyond this is genuinely undead/invented.
    attempted: usize,
    /// Events an honored `sync` acknowledged durable before the crash.
    durable_acked: usize,
    /// Running determinism digest.
    digest: u64,
}

/// Internal driver returning the typed [`CellViolation`].
fn drive(seed: u64, steps: usize, mode: FaultMode) -> Result<CellOutcome, CellViolation> {
    let dir = tempfile::tempdir().map_err(|e| CellViolation::NonCanonicalReopen(e.to_string()))?;
    let sim_fs = Arc::new(SimFs::new(seed, mode.fsync_drop_one_in()));
    let coord = Coordinate::new("entity:b3", "scope:recovery")
        .map_err(|e| CellViolation::NonCanonicalReopen(e.to_string()))?;
    let plan = OpPlan::seeded(seed, steps);

    let digest = fold(fold(FNV_OFFSET, seed), mode.token());

    // ── Phase 1: drive the seeded op stream over the real Store, then crash. ──
    let stats = drive_until_crash(&dir, &sim_fs, &coord, &plan, mode, digest)?;

    // ── Phase 2: reopen the real Store over the truncated tree. ──────────────
    match Store::open(StoreConfig::new(dir.path())) {
        Ok(store) => classify_open(&store, &stats, mode),
        Err(error) if is_canonical_refusal(&error) => Ok(CellOutcome {
            digest: fold(stats.digest, 0xCA11_AB1E),
            classification: Classification::CanonicalRefusal,
            recovered_visible: 0,
            durable_acked: stats.durable_acked,
        }),
        Err(other) => Err(CellViolation::NonCanonicalReopen(format!("{other:?}"))),
    }
}

/// True when `err` is the injected boundary fault (directly, or wrapped by the
/// batch path as `BatchFailed`). The driver treats this as THE crash and stops
/// issuing ops, faithfully modelling a single power-loss at the boundary (rather
/// than continuing — a later `sync` would otherwise physically flush the orphaned
/// torn frame and confound the crash-before-fsync semantics).
fn is_injected_crash(err: &StoreError) -> bool {
    matches!(
        err,
        StoreError::FaultInjected(_) | StoreError::BatchFailed { .. }
    )
}

/// Drive the seeded op plan over `store`, returning the [`DriveStats`]. For an
/// injector-armed cell the loop STOPS at the first injected fault — that op is
/// the crash. The faulted op's frame may or may not have reached disk; either way
/// its payload WAS submitted, so `attempted` (the no-undead ceiling) counts it.
fn drive_ops(store: &Store, coord: &Coordinate, plan: &OpPlan, digest: u64) -> DriveStats {
    let mut attempted = 0usize;
    // Events that returned `Ok` (genuinely appended). The no-loss floor a `sync`
    // acknowledges is built from THESE, never from a fault-aborted op (whose
    // durability the store never confirmed).
    let mut succeeded = 0usize;
    let mut durable_acked = 0usize;
    let mut digest = digest;
    for (idx, op) in plan.ops.iter().enumerate() {
        match op {
            Op::Append => {
                let payload = serde_json::json!({ "seq": attempted, "step": idx });
                attempted += 1;
                digest = fold(digest, attempted as u64);
                match store.append(coord, KIND, &payload) {
                    Ok(_) => succeeded += 1,
                    Err(e) if is_injected_crash(&e) => break,
                    Err(_) => {}
                }
            }
            Op::Batch(n) => {
                let items = batch_items(coord, attempted, *n);
                attempted += *n as usize;
                digest = fold(fold(digest, 0xBA7C), attempted as u64);
                match store.append_batch(items) {
                    Ok(_) => succeeded += *n as usize,
                    Err(e) if is_injected_crash(&e) => break,
                    Err(_) => {}
                }
            }
            Op::Sync => {
                // An honored sync makes every SUCCEEDED append acknowledged
                // durable. Under a lying disk the sync may be silently dropped, so
                // the no-loss rule is relaxed for that mode in `classify_open`.
                if Store::sync(store).is_ok() {
                    durable_acked = succeeded;
                    digest = fold(digest, 0x5_4_C);
                }
            }
        }
    }
    DriveStats {
        attempted,
        durable_acked,
        digest,
    }
}

/// Base config for a driven session: real `Store` over `SimFs`, with an unsynced
/// tail left for the crash to eat (explicit `Sync` ops are the only durability
/// boundary the driver acknowledges).
fn driven_config(dir: &tempfile::TempDir, sim_fs: &Arc<SimFs>) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_fs(Arc::clone(sim_fs) as Arc<dyn crate::store::platform::fs::StoreFs>)
        .with_sync_every_n_events(1_000_000)
        .with_sync_mode(SyncMode::SyncAll)
}

/// Drive the op plan over a fault-configured real `Store`, then crash it,
/// returning `(appended, durable_acked, digest)`.
///
/// For [`FaultMode::CrashBeforeFsync`] a clean BASELINE session is opened+closed
/// first (mirroring the S3 sentinel) so the fault injector arms only for the
/// driven user ops, not for the `Store::open` lifecycle append (whose
/// `SingleAppendWritten`/etc. point would otherwise consume the one-shot fault
/// during initialization).
fn drive_until_crash(
    dir: &tempfile::TempDir,
    sim_fs: &Arc<SimFs>,
    coord: &Coordinate,
    plan: &OpPlan,
    mode: FaultMode,
    digest: u64,
) -> Result<DriveStats, CellViolation> {
    let injector = match mode {
        FaultMode::CrashBeforeFsync { boundary } => {
            // Baseline session: clean open+close so initialization (and its
            // lifecycle appends) completes durably before the fault arms.
            let baseline = Store::open(driven_config(dir, sim_fs))
                .map_err(|e| CellViolation::NonCanonicalReopen(e.to_string()))?;
            baseline
                .close()
                .map_err(|e| CellViolation::NonCanonicalReopen(e.to_string()))?;
            Some(injector_for(boundary))
        }
        FaultMode::HonestDiskCrash | FaultMode::LyingDiskFsyncDrop { .. } => None,
    };

    let store = Store::open(driven_config(dir, sim_fs).with_fault_injector(injector))
        .map_err(|e| CellViolation::NonCanonicalReopen(e.to_string()))?;

    let stats = drive_ops(&store, coord, plan, digest);

    // Crash: abandon the writer without a clean shutdown (no drain/footer/final
    // sync), then truncate the unsynced (and any dropped-fsync) tail.
    store.abandon_without_shutdown();
    sim_fs.crash();
    Ok(stats)
}

/// A fault injector that fires EXACTLY ONCE, at the first injection point
/// matching `boundary`, then permanently disarms. Unlike [`CountdownInjector`]
/// (whose `trigger_after` threshold, once crossed, fires on EVERY subsequent
/// matching point), this models a SINGLE crash at the boundary: the one in-flight
/// write is torn, and every later op proceeds normally so the recovered state is
/// driven by the genuine post-crash reopen, not a fault storm.
struct OneShotInjector {
    /// The durability boundary whose first occurrence fires the fault.
    boundary: Boundary,
    /// `true` until the single fault fires; then permanently `false`.
    armed: AtomicBool,
}

impl FaultInjector for OneShotInjector {
    fn check(&self, point: InjectionPoint) -> Option<StoreError> {
        if !self.boundary.matches(&point) {
            return None;
        }
        // Disarm atomically: only the thread that flips `true`->`false` fires.
        if self
            .armed
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            Some(StoreError::FaultInjected(format!(
                "B3: simulated crash at durability boundary at {point:?}"
            )))
        } else {
            None
        }
    }
}

/// Build the one-shot fault injector that tears the first op reaching `boundary`.
fn injector_for(boundary: Boundary) -> Arc<dyn FaultInjector> {
    Arc::new(OneShotInjector {
        boundary,
        armed: AtomicBool::new(true),
    })
}

/// Classify + legality-check the recovered visible state of a reopened store.
fn classify_open(
    store: &Store,
    stats: &DriveStats,
    mode: FaultMode,
) -> Result<CellOutcome, CellViolation> {
    let recovered = recovered_user_events(store);
    let recovered_visible = recovered.len();

    // Sacred rule (honest disk only): no acknowledged-durable commit may be lost.
    // Lying disk RELAXES this — a dropped-fsync commit may legally be absent.
    if !mode.is_lying_disk() && recovered_visible < stats.durable_acked {
        return Err(CellViolation::LostDurableCommit {
            durable: stats.durable_acked,
            recovered: recovered_visible,
        });
    }
    // Prefix rule (EVERY mode, including lying disk): never resurrect an event
    // whose payload the driver never submitted. The ceiling is `attempted` — the
    // total events submitted, INCLUDING a fault-torn op whose frame may legally
    // have landed on disk (that is recovered history, not an invented event).
    if recovered_visible > stats.attempted {
        return Err(CellViolation::UndeadEvent {
            recovered: recovered_visible,
            appended: stats.attempted,
        });
    }
    // Intact hash chain across the recovered visible prefix (EVERY mode).
    verify_hash_chain(&recovered)?;

    let mut digest = fold(
        fold(stats.digest, recovered_visible as u64),
        stats.durable_acked as u64,
    );
    for ev in &recovered {
        digest = fold(digest, ev.event_hash_token);
    }
    let classification = if recovered_visible == 0 {
        Classification::RolledBack
    } else {
        Classification::CommittedPrefix
    };
    Ok(CellOutcome {
        digest,
        classification,
        recovered_visible,
        durable_acked: stats.durable_acked,
    })
}

/// The full fault-mode matrix the public oracle sweeps (lying-disk rates and
/// crash-before-fsync boundaries included).
pub(crate) fn all_modes() -> Vec<FaultMode> {
    let mut modes = vec![
        FaultMode::HonestDiskCrash,
        FaultMode::LyingDiskFsyncDrop { one_in: 2 },
        FaultMode::LyingDiskFsyncDrop { one_in: 5 },
    ];
    for boundary in Boundary::ALL {
        modes.push(FaultMode::CrashBeforeFsync { boundary });
    }
    modes
}

// ── Test-only public surface, re-exported (doc-hidden) at `batpak::__sim`. ──

/// Doc-hidden public mirror of [`Classification`] for the integration test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveredClass {
    /// Reopened cleanly with a legal non-empty recovered prefix.
    CommittedPrefix,
    /// Reopened cleanly with zero recovered visible events.
    RolledBack,
    /// Reopen refused with a typed corruption error.
    CanonicalRefusal,
}

impl From<Classification> for RecoveredClass {
    fn from(c: Classification) -> Self {
        match c {
            Classification::CommittedPrefix => RecoveredClass::CommittedPrefix,
            Classification::RolledBack => RecoveredClass::RolledBack,
            Classification::CanonicalRefusal => RecoveredClass::CanonicalRefusal,
        }
    }
}

/// One cell of the public matrix sweep: the fault-mode label, the boundary label
/// (if any), the recovered classification, and the determinism digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixCell {
    /// Human-readable fault-mode label (e.g. `"honest-disk-crash"`).
    pub mode: String,
    /// Recovered classification.
    pub class: RecoveredClass,
    /// FNV-1a determinism digest for this cell.
    pub digest: u64,
    /// Recovered visible event count.
    pub recovered_visible: usize,
    /// Acknowledged-durable op count before the crash.
    pub durable_acked: usize,
}

/// A short label for `mode`, stable across runs (used in the public matrix).
fn mode_label(mode: FaultMode) -> String {
    match mode {
        FaultMode::HonestDiskCrash => "honest-disk-crash".to_string(),
        FaultMode::LyingDiskFsyncDrop { one_in } => format!("lying-disk-fsync-drop-1-in-{one_in}"),
        FaultMode::CrashBeforeFsync { boundary } => {
            format!("crash-before-fsync@{boundary:?}")
        }
    }
}

/// Test-only entry point re-exported (doc-hidden) at `batpak::__sim`: sweep the
/// FULL hostile-fs fault matrix for `seed` and return one [`MatrixCell`] per
/// cell. Each cell's legality oracle fail-closes inside [`run`].
///
/// # Errors
/// Returns a seed/mode-tagged violation string on the first illegal cell.
pub fn run_recovery_matrix(seed: u64, steps: usize) -> Result<Vec<MatrixCell>, String> {
    all_modes()
        .into_iter()
        .map(|mode| {
            run(seed, steps, mode).map(|o| MatrixCell {
                mode: mode_label(mode),
                class: o.classification.into(),
                digest: o.digest,
                recovered_visible: o.recovered_visible,
                durable_acked: o.durable_acked,
            })
        })
        .collect()
}

/// Test-only replay-seed helper for `BATPAK_SEED`.
pub fn matrix_replay_seed(default: u64) -> u64 {
    seed_from_env(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_recovers_legally() {
        for mode in all_modes() {
            let result = run(0x5EED_B301, 64, mode);
            assert!(
                result.is_ok(),
                "mode {mode:?} must recover legally: {result:?}"
            );
        }
    }

    #[test]
    fn same_seed_same_classification_per_mode() {
        for mode in all_modes() {
            let a = run(0x5EED_B302, 48, mode).expect("legal");
            let b = run(0x5EED_B302, 48, mode).expect("legal");
            assert_eq!(
                a, b,
                "PROPERTY: identical (seed, mode={mode:?}) must recover identically"
            );
        }
    }

    #[test]
    fn lying_disk_relaxes_no_loss_but_not_undead() {
        // A lying disk MAY lose an acked commit (recovered < durable is legal),
        // but an undead event (recovered > appended) is still rejected. We can
        // only positively assert legality + that the relaxation flag is honored.
        let mode = FaultMode::LyingDiskFsyncDrop { one_in: 2 };
        assert!(
            mode.is_lying_disk(),
            "lying disk must relax the no-loss rule"
        );
        let outcome = run(0x5EED_B303, 64, mode).expect("legal under lying disk");
        assert!(
            outcome.recovered_visible <= 64 * 3,
            "no undead events even under a lying disk"
        );
    }

    #[test]
    fn honest_disk_preserves_durable_prefix() {
        let outcome = run(0x5EED_B304, 64, FaultMode::HonestDiskCrash).expect("legal");
        assert!(
            outcome.recovered_visible >= outcome.durable_acked,
            "SACRED RULE: honest disk never loses an acknowledged-durable commit"
        );
    }
}
