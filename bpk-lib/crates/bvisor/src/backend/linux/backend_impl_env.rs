//! `Environment::Exact` lowering for the Linux backend (proof-spine §5 D2), split out
//! of `backend_impl.rs` to hold it under the non-overridable file-size cap.
//!
//! The admitted `Environment::Exact` policy is LOWERED to the concrete envp the
//! launcher serves to `fexecve`: literals pass through, and every `SecretLease` is
//! resolved in the PARENT through the backend's host [`crate::SecretResolver`]
//! immediately before launch. The resolved value goes ONLY into the child's env — the
//! durable plan/report keep the policy with lease REFS, never the value. A lowering
//! fault (invalid policy, or an unresolvable lease) fails CLOSED: the workload never
//! runs. SAFE std; the OS work is the launcher's.

use super::LinuxBackend;
use crate::contract::capability::{Capability, EnvPolicy};
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::ObservedFact;
use crate::contract::secret::lower_env;

/// The lowered envp paired with the accumulated observed facts (the `Ok` of
/// [`lower_environment`]). Named to keep the signature readable.
type LoweredEnv = (Vec<(String, String)>, Vec<ObservedFact>);

/// LOWER the plan's admitted `Environment::Exact` policy to the concrete envp the
/// launcher serves. On a lowering fault returns `Err(observed)` with a
/// `environment_lowering_failed` fact appended, so the caller FAILS CLOSED — the
/// workload never runs.
pub(super) fn lower_environment(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    mut observed: Vec<ObservedFact>,
) -> Result<LoweredEnv, Vec<ObservedFact>> {
    let policy = environment_policy(plan);
    match lower_env(&policy, backend.secret_resolver.as_ref()) {
        Ok(envp) => Ok((envp, observed)),
        Err(error) => {
            observed.push(ObservedFact {
                kind: "environment_lowering_failed".to_string(),
                detail: format!("refusing to launch: {error}"),
            });
            Err(observed)
        }
    }
}

/// The admitted [`EnvPolicy`] to lower into the child env: the admitted `Environment`
/// capability's policy, or an EMPTY `Exact` table when the spec declared no Environment
/// capability (⇒ the child gets a genuinely empty env — nothing inherited, no ambient
/// leak). The plan was admitted against our ceiling, so any Environment capability is
/// the `Exact` variant.
fn environment_policy(plan: &BoundaryPlan) -> EnvPolicy {
    plan.admitted
        .iter()
        .find_map(|a| match &a.requirement {
            BoundaryRequirement::Capability(Capability::Environment { policy }) => {
                Some(policy.clone())
            }
            BoundaryRequirement::Capability(_) | BoundaryRequirement::HostControl(_) => None,
        })
        .unwrap_or_else(|| EnvPolicy::Exact(Vec::new()))
}
