//! `ChildSpawn` child-task lowering for the Linux backend (proof-spine S10), split out of
//! `backend_impl.rs` to hold it under the non-overridable file-size cap.
//!
//! THE LOWERING (mirrors the S4/S5/S9 seam): the admitted [`SpawnPolicy`] DRIVES how the
//! launch plan confines the workload's OWN task creation. The three S6-frozen variants lower
//! to DISTINCT mechanisms (the §8 object-capability attenuation ladder):
//!
//! - [`SpawnPolicy::DenyNewTasks`] → a default-allow seccomp DENYLIST refusing the
//!   `clone`/`clone3`/`fork`/`vfork` family at the SYSCALL-NUMBER level (no `clone3`
//!   arg-deref needed, S6 freeze). ONE composed layer — the broad confinement is
//!   landlock/cgroup/netns/fd-scrub. Drives [`ChildTaskLowering::deny_new_tasks`].
//! - [`SpawnPolicy::AllowDescendantsWithinBoundary`] → NO seccomp deny: the descendant
//!   inherits the cgroup (the S1 Kill / process_count mechanism), so it is killable via
//!   `cgroup.kill`, counted by `pids.max`, and namespace-trapped. Drives no filter — the
//!   cgroup boundary (already engaged when a cgroup base is probed) IS the mechanism.
//! - [`SpawnPolicy::AllowThreadsWithinBoundary`] → FAIL CLOSED (the clone3-pointer /
//!   classic-BPF problem, S6): seccomp cannot deref the `clone3` flags to permit-threads-
//!   but-deny-processes, and denying `clone3` outright breaks modern glibc threads. This is
//!   the OPEN enforcement problem — it stays absent from the ceiling (Unsupported) and any
//!   admitted variant fails closed here too (defense-in-depth), so the workload never runs
//!   under an unrealized child-task guarantee.
//!
//! A seccomp denylist is NOT a standalone sandbox — it is one Swiss-cheese layer. SAFE std;
//! the OS work (`prctl(NO_NEW_PRIVS)` + `seccomp(SET_MODE_FILTER)`) is the launcher's.

use super::LinuxBackend;
use crate::contract::capability::{Capability, SpawnPolicy};
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::ObservedFact;

/// The outcome of lowering the admitted child-task policy: whether the launch plan must
/// install a seccomp denylist denying the task-creation family, carried with the facts.
pub(super) struct ChildTaskLowering {
    /// `true` ⇒ the launch plan must request a seccomp denylist refusing
    /// `clone`/`clone3`/`fork`/`vfork` (`ChildSpawn::DenyNewTasks`); `false` ⇒ no
    /// task-creation deny (either no ChildSpawn admitted, or `AllowDescendants` — cgroup-
    /// confined, no filter).
    pub(super) deny_new_tasks: bool,
    /// The observed facts threaded through (the lowering fact appended).
    pub(super) observed: Vec<ObservedFact>,
}

/// LOWER the plan's admitted [`Capability::ChildSpawn`] policy onto the launcher's child-task
/// confinement. On `AllowThreadsWithinBoundary` (the unenforceable open problem) it returns
/// `Err(observed)` with a fact appended so the caller FAILS CLOSED — the workload never runs
/// under an unrealized child-task guarantee. With NO `ChildSpawn` capability admitted
/// `deny_new_tasks` is `false` (no task-creation deny — the default).
///
/// `_backend` is unused today (the deny needs no host resolver) but kept in the signature so
/// the seam matches `lower_network` / `lower_environment` / `lower_inherited_fds`.
pub(super) fn lower_child_spawn(
    _backend: &LinuxBackend,
    plan: &BoundaryPlan,
    mut observed: Vec<ObservedFact>,
) -> Result<ChildTaskLowering, Vec<ObservedFact>> {
    match admitted_spawn_policy(plan) {
        // No ChildSpawn capability admitted: nothing to lower — the no-filter path runs.
        None => Ok(ChildTaskLowering {
            deny_new_tasks: false,
            observed,
        }),
        // DenyNewTasks: install a seccomp denylist refusing the task-creation family.
        Some(SpawnPolicy::DenyNewTasks) => {
            observed.push(ObservedFact {
                kind: "child_spawn_lowered".to_string(),
                detail: "ChildSpawn::DenyNewTasks: the launcher installs a default-allow seccomp \
                         DENYLIST refusing clone/clone3/fork/vfork at the syscall-number level \
                         (LAST, after landlock, before fexecve; EPERM so the workload's fork \
                         fails observably). ONE composed layer — the broad confinement is \
                         landlock/cgroup/netns/fd-scrub."
                    .to_string(),
            });
            Ok(ChildTaskLowering {
                deny_new_tasks: true,
                observed,
            })
        }
        // AllowDescendantsWithinBoundary: NO seccomp deny — the descendant inherits the cgroup
        // (killable via cgroup.kill, counted by pids.max, namespace-trapped). The cgroup
        // boundary IS the mechanism; the filter stays off.
        Some(SpawnPolicy::AllowDescendantsWithinBoundary) => {
            observed.push(ObservedFact {
                kind: "child_spawn_lowered".to_string(),
                detail: "ChildSpawn::AllowDescendantsWithinBoundary: NO seccomp deny — the \
                         descendant inherits the run cgroup (killable via cgroup.kill, counted \
                         by pids.max, namespace-trapped); the cgroup boundary is the mechanism."
                    .to_string(),
            });
            Ok(ChildTaskLowering {
                deny_new_tasks: false,
                observed,
            })
        }
        // AllowThreadsWithinBoundary: the open clone3-pointer/classic-BPF problem (S6) —
        // seccomp cannot permit-threads-but-deny-processes precisely. It is absent from the
        // ceiling (never admits), but fail CLOSED here too — the workload must never run under
        // an unrealized child-task guarantee.
        Some(SpawnPolicy::AllowThreadsWithinBoundary) => {
            observed.push(ObservedFact {
                kind: "child_spawn_lowering_failed".to_string(),
                detail: "refusing to launch: ChildSpawn::AllowThreadsWithinBoundary is NOT \
                         realized by this backend (the clone3-pointer / classic-BPF problem — \
                         seccomp cannot deref clone3 flags to permit-threads-but-deny-processes; \
                         denying clone3 outright breaks glibc threads). FailClosed; the target \
                         never runs."
                    .to_string(),
            });
            Err(observed)
        }
    }
}

/// The admitted [`SpawnPolicy`] to realize: the admitted `ChildSpawn` capability's policy, or
/// `None` when the spec declared no `ChildSpawn` capability. The plan was admitted against our
/// ceiling, so any admitted `ChildSpawn` whose key is `ChildSpawnDenyNewTasks` is
/// `DenyNewTasks` (and `AllowThreads` never admits — absent from the ceiling).
fn admitted_spawn_policy(plan: &BoundaryPlan) -> Option<SpawnPolicy> {
    plan.admitted.iter().find_map(|a| match &a.requirement {
        BoundaryRequirement::Capability(Capability::ChildSpawn { policy }) => Some(*policy),
        BoundaryRequirement::Capability(_) | BoundaryRequirement::HostControl(_) => None,
    })
}
