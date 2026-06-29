//! `InheritedFds::None` lowering for the Linux backend (proof-spine §5/S5), split out
//! of `backend_impl.rs` to hold it under the non-overridable file-size cap.
//!
//! THE LOWERING (the seam S4 set for `EnvPolicy::Exact`): the admitted [`FdPolicy`]
//! DRIVES the launcher's fd-scrub close-list. For [`FdPolicy::None`] the workload
//! inherits ONLY the launcher's own protocol/stdio fds plus the descriptor-table
//! authority slots (exe + confinement roots) — the launcher's child-side scrub closes
//! EVERY other inherited fd before `fexecve` (the allowlist complement). So the admitted
//! `None` policy is realized by the descriptor-table-driven scrub already built in
//! `plan_build` + `launcher::imp`; this module is the CONTRACT-LEVEL gate that confirms
//! the admitted policy is one the lowering actually realizes, and FAILS CLOSED otherwise
//! (the workload never runs under an unrealized fd policy).
//!
//! `FdPolicy::Only(..)` is OUT OF SCOPE (S5) — it is absent from the ceiling so it never
//! admits, but defense-in-depth this gate ALSO refuses it here: were it ever to reach
//! `execute()` (a future ceiling change without a lowering), the workload must NOT run
//! with the scrub silently realizing the wrong policy (which would leak no fd, but would
//! ALSO not honor the `Only` allowlist — an unrealized guarantee). SAFE std; the OS work
//! (the scrub itself) is the launcher's.

use super::LinuxBackend;
use crate::contract::capability::{Capability, FdPolicy};
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::ObservedFact;

/// LOWER the plan's admitted [`Capability::InheritedFds`] policy onto the launcher
/// scrub. The scrub close-list itself is built (descriptor-table-driven) in
/// `plan_build`/`launcher::imp`; this is the contract gate that confirms the admitted
/// policy is REALIZED by that scrub. On a policy the lowering does not realize it
/// returns `Err(observed)` with an `inherited_fds_lowering_failed` fact appended, so the
/// caller FAILS CLOSED — the workload never runs.
///
/// `_backend` is unused today (the scrub needs no host resolver, unlike the env lease
/// resolver) but kept in the signature so the seam matches `lower_environment` and a
/// future `Only` realization can consult host state.
pub(super) fn lower_inherited_fds(
    _backend: &LinuxBackend,
    plan: &BoundaryPlan,
    mut observed: Vec<ObservedFact>,
) -> Result<Vec<ObservedFact>, Vec<ObservedFact>> {
    match inherited_fds_policy(plan) {
        // `None` (declared or defaulted): the scrub closes every undeclared inherited
        // fd; only the descriptor-table authority + the launcher's stdio/protocol fds
        // survive. The descriptor-table-driven scrub REALIZES this exactly.
        FdPolicy::None => {
            observed.push(ObservedFact {
                kind: "inherited_fds_lowered".to_string(),
                detail: "FdPolicy::None: the launcher fd-scrub closes every undeclared \
                         inherited fd before fexecve (only the declared descriptor-table \
                         authority + stdio survive)"
                    .to_string(),
            });
            Ok(observed)
        }
        // `Only(..)` is out of scope (S5): the descriptor-table scrub does not realize a
        // selective-keep allowlist, so admitting it would be an unrealized guarantee.
        // It is absent from the ceiling (never admits), but fail CLOSED here too.
        FdPolicy::Only(fds) => {
            observed.push(ObservedFact {
                kind: "inherited_fds_lowering_failed".to_string(),
                detail: format!(
                    "refusing to launch: FdPolicy::Only({} fd(s)) is not realized by this \
                     backend (only FdPolicy::None is lowered); the target never runs",
                    fds.len()
                ),
            });
            Err(observed)
        }
    }
}

/// The admitted [`FdPolicy`] to realize: the admitted `InheritedFds` capability's
/// policy, or [`FdPolicy::None`] when the spec declared no `InheritedFds` capability
/// (⇒ the default is no inherited fds — nothing survives the scrub but the declared
/// authority + stdio). The plan was admitted against our ceiling, so any admitted
/// `InheritedFds` capability whose key is `InheritedFdsNone` is the `None` variant.
fn inherited_fds_policy(plan: &BoundaryPlan) -> FdPolicy {
    plan.admitted
        .iter()
        .find_map(|a| match &a.requirement {
            BoundaryRequirement::Capability(Capability::InheritedFds { policy }) => {
                Some(policy.clone())
            }
            BoundaryRequirement::Capability(_) | BoundaryRequirement::HostControl(_) => None,
        })
        .unwrap_or(FdPolicy::None)
}
