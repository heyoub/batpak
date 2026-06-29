//! Per-run cgroup lifecycle helpers for the Linux backend's `execute()` (split out of
//! `backend_impl.rs` to keep that file under the non-overridable structural-check size
//! cap). These are PURE of any `LinuxBackend` private state — they take the cgroup base /
//! admitted plan / owned leaf explicitly — so they read in isolation and the orchestration
//! (`cgroup_for_run` / `finish`) stays in `backend_impl`. NO `unsafe`: cgroup is SAFE
//! `std::fs` (the cgroup manager itself is `super::cgroup`).

use crate::backend::linux::cgroup;
use crate::backend::linux::LinuxBackend;
use crate::contract::capability::Enforcement;
use crate::contract::host_control::HostControl;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::{BoundaryReportBody, ObservedFact};
use std::path::Path;

/// The cgroup `pids.max` cap to install on the run leaf: the plan's admitted process_count
/// limit, but ONLY when it admitted as `Enforced` (which happens IFF a cgroup base was
/// probed — `observed_budget_profile` offers process_count Enforced only then). A Mediated
/// process_count installs NO cap (Budget stays observed-not-capped); the leaf is then
/// created bare (placement + atomic kill) without a structural pid limit.
pub(super) fn pids_cap_for(plan: &BoundaryPlan) -> Option<u64> {
    (plan.budgets.process_count.selected_guarantee == Enforcement::Enforced)
        .then_some(plan.budgets.process_count.effective_limit)
}

/// Whether the admitted plan REQUIRES cgroup backing — an Enforced process_count budget (a
/// real `pids.max` cap) or an atomic `Kill` control. When true, a per-run cgroup leaf
/// failure is TERMINAL (fail-closed), never a silent uncgrouped run that would leave the
/// report claiming guarantees the workload did not actually run under.
pub(super) fn requires_cgroup_backing(plan: &BoundaryPlan) -> bool {
    pids_cap_for(plan).is_some()
        || plan.admitted.iter().any(|a| {
            matches!(
                a.requirement,
                BoundaryRequirement::HostControl(HostControl::Kill { .. })
            )
        })
}

/// Create the per-run cgroup leaf under the probed confinement `base`. A unique name (pid +
/// a process-local counter, no clock/RNG). `pids_cap` installs a structural `pids.max`
/// limit (the admitted process_count budget, enforced) when `Some`; `None` creates a bare
/// leaf (placement + atomic kill only). `None` return on any create failure (the caller
/// decides whether that is fail-closed or an honest uncgrouped run).
pub(super) fn create_run_leaf(base: &Path, pids_cap: Option<u64>) -> Option<cgroup::CgroupLeaf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);
    let suffix = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("bvisor-run-{}-{suffix}", std::process::id());
    let limits = match pids_cap {
        Some(max) => cgroup::CgroupLimits::with_pids_max(max),
        None => cgroup::CgroupLimits::default(),
    };
    cgroup::CgroupLeaf::create(base, &name, limits).ok()
}

/// Tear down a per-run cgroup leaf (kill → bounded drain → remove). Returns `Some(detail)`
/// describing an INCOMPLETE teardown — so "no leak" stays REPORT-ATTESTABLE (the codex
/// fix) — or `None` on a clean teardown / no leaf. `cgroup.kill` is async, so the bounded
/// drain bridges the SIGKILL window the rmdir would race; a hung workload is atomically
/// killed here (the atomic run-tree teardown the `Kill` ceiling claims).
pub(super) fn teardown_leaf(leaf: Option<cgroup::CgroupLeaf>) -> Option<String> {
    let mut leaf = leaf?;
    let killed = leaf.kill();
    let drained = leaf.wait_until_empty(50, std::time::Duration::from_millis(10));
    let removed = leaf.remove();
    if killed.is_ok() && matches!(drained, Ok(true)) && removed.is_ok() {
        None
    } else {
        Some(format!(
            "cgroup leaf teardown incomplete: kill={killed:?} drained={drained:?} removed={removed:?}"
        ))
    }
}

/// The successful output of [`cgroup_for_run`]: the leaf to tear down, the dir fd for
/// the launcher's CgroupDir slot, and the threaded-back `observed` facts.
pub(super) type CgroupRunPrep = (
    Option<cgroup::CgroupLeaf>,
    Option<std::os::fd::OwnedFd>,
    Vec<ObservedFact>,
);

/// Prepare the per-run cgroup leaf for placement + atomic kill, recording honest
/// evidence and threading `observed` back.
///
/// FAIL-CLOSED (the codex-review fix): if the plan was admitted with cgroup-backed
/// guarantees ([`requires_cgroup_backing`]) but the leaf cannot be created / its dir fd
/// cannot be opened, the workload MUST NOT run uncgrouped while the report claims those
/// guarantees — tear down any partial leaf (so nothing leaks) and return `Err(observed)`.
/// When cgroup is NOT required, a leaf failure is an honest `cgroup_placement_unavailable`
/// (the run proceeds in the launcher's cgroup). On success returns `(leaf, dir fd,
/// observed)`; on the fail-closed path returns `Err(observed)` (the partial leaf already
/// torn down) and the CALLER turns it into a `SupervisorFault` report — so this stays free
/// of the report-building surface.
pub(super) fn cgroup_for_run(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    mut observed: Vec<ObservedFact>,
) -> Result<CgroupRunPrep, Vec<ObservedFact>> {
    let leaf = backend
        .cgroup_base
        .as_ref()
        .and_then(|base| create_run_leaf(base, pids_cap_for(plan)));
    // A SEPARATE dir fd (`File::open` on the leaf) the launcher inherits for the
    // CgroupDir slot; the `CgroupLeaf` retains its path for teardown.
    let dir_fd = leaf.as_ref().and_then(|l| l.dir_fd().ok());
    if dir_fd.is_some() {
        let leaf_path = leaf
            .as_ref()
            .and_then(|l| l.dir().ok())
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        // HONEST timing (codex review #2): this runs BEFORE the launcher — the backend
        // only KNOWS it created the leaf and is passing the dir fd for CLONE_INTO_CGROUP.
        // Actual placement is proven by the launcher's own `cgroup_placement` note
        // (recorded by map_observation); do NOT claim "placed" here or a later launcher
        // fault would leave a stronger claim than the observation point supports.
        observed.push(ObservedFact {
            kind: "cgroup_leaf_prepared".to_string(),
            detail: format!(
                "cgroup leaf {leaf_path} created + dir fd passed to the launcher for \
                 CLONE_INTO_CGROUP placement; atomic run-tree teardown available (cgroup.kill)"
            ),
        });
        return Ok((leaf, dir_fd, observed));
    }
    // No usable placement fd. If the admitted plan REQUIRES cgroup backing, fail closed —
    // running uncgrouped would make the report's Enforced/Kill guarantees a lie.
    if requires_cgroup_backing(plan) {
        if let Some(detail) = teardown_leaf(leaf) {
            observed.push(ObservedFact {
                kind: "cgroup_teardown_incomplete".to_string(),
                detail,
            });
        }
        observed.push(ObservedFact {
            kind: "cgroup_required_but_unavailable".to_string(),
            detail: "plan admitted cgroup-backed guarantees (Enforced process_count and/or \
                     atomic Kill) but the per-run cgroup leaf could not be created; refusing \
                     to run the workload uncgrouped (fail-closed)"
                .to_string(),
        });
        return Err(observed);
    }
    // cgroup NOT required ⇒ honest no-placement; the workload runs in the launcher's
    // cgroup. Keep the (empty) leaf so `finish` removes it.
    if backend.cgroup_base.is_some() {
        observed.push(ObservedFact {
            kind: "cgroup_placement_unavailable".to_string(),
            detail: "cgroup base probed but the per-run leaf could not be created; the \
                     workload runs without cgroup placement this run"
                .to_string(),
        });
    }
    Ok((leaf, None, observed))
}

/// Tear down the per-run cgroup leaf and return the report — recording a teardown-failure
/// fact onto it when cleanup was INCOMPLETE (so "no leak" is attestable from the report,
/// not just asserted). A `None` leaf is a no-op. Called on EVERY post-creation return path
/// so a leaf can never silently leak.
pub(super) fn finish(
    leaf: Option<cgroup::CgroupLeaf>,
    mut report: BoundaryReportBody,
) -> BoundaryReportBody {
    if let Some(detail) = teardown_leaf(leaf) {
        report.observed.push(ObservedFact {
            kind: "cgroup_teardown_incomplete".to_string(),
            detail,
        });
    }
    report
}
