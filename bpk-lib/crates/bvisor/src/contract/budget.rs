//! The seven-dimensional budget model with THREE separated authorities (kernel
//! plan §7). A caller must never author enforcement or availability — that would
//! let a spec submit a fantasy guarantee, which is architectural poison. So:
//!
//! - [`BudgetRequirements`] (in `BoundarySpec`) — what is *required*: per dimension
//!   `(limit, min-guarantee, required-evidence)`.
//! - [`BudgetProfile`] (in `BackendProfileSnapshot`) — what the machine *provides*:
//!   per dimension `(available, actual-guarantee, available-evidence, mechanism)`,
//!   probed and declared by the backend ONLY.
//! - [`AdmittedBudgets`] (in `BoundaryPlan`) — the *adjudicated contract*.
//!
//! Admission for dimension `d` succeeds exactly when `L_d ≤ A_d ∧ E_d ≥ G_d ∧
//! Q_d ⊆ C_d ∧ L_d ≥ DerivedMinimum_d`. The seven dimensions are fixed and finite —
//! zero is a legitimate deny-all bound; there is no sentinel and no implicit
//! "unlimited".

use crate::contract::capability::{Enforcement, EvidenceSet};
use crate::contract::ids::Digest32;
use serde::{Deserialize, Serialize};

/// The minimum acceptable guarantee a requirement may demand. A caller may demand
/// `Mediated` or `Enforced` — never `Unsupported` (that is only a backend answer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MinGuarantee {
    /// At least supervised mediation per attempt.
    Mediated,
    /// A structural guarantee.
    Enforced,
}

/// The fixed seven budget dimensions, in canonical order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BudgetDimension {
    /// Wall-clock time (µs).
    Wall,
    /// Aggregate CPU time across the run tree (µs).
    Cpu,
    /// Peak aggregate resident memory (bytes).
    ResidentMemory,
    /// Maximum live process-tree members, root included.
    ProcessCount,
    /// Maximum open descriptors/handles across the run tree.
    HandleCount,
    /// Maximum bytes materialized in boundary-owned writable storage.
    Storage,
    /// Maximum network transfer bytes.
    Network,
}

/// One dimension's REQUEST: `R_d = (limit, min-guarantee, required-evidence)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetRequest {
    /// The finite requested limit (unit per dimension; `0` = deny-all).
    pub limit: u64,
    /// The minimum acceptable guarantee for this dimension.
    pub guarantee: MinGuarantee,
    /// Evidence claims the report must carry for this dimension.
    pub evidence: EvidenceSet,
}

/// The complete request `B_R` — seven branded, fixed-unit dimensions. `ProcessCount`
/// and `HandleCount` carry `u32`-valued counts in their `limit`; the rest carry the
/// `u64` unit named on the field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetRequirements {
    /// Wall-clock µs.
    pub wall_micros: BudgetRequest,
    /// Aggregate CPU µs.
    pub cpu_micros: BudgetRequest,
    /// Peak resident bytes.
    pub resident_bytes: BudgetRequest,
    /// Live process-tree members (u32-valued).
    pub process_count: BudgetRequest,
    /// Open descriptors/handles (u32-valued).
    pub handle_count: BudgetRequest,
    /// Boundary-owned writable storage bytes.
    pub storage_bytes: BudgetRequest,
    /// Network transfer bytes.
    pub network_bytes: BudgetRequest,
}

/// One dimension's PROFILE: `P_d = (available, actual-guarantee, available-evidence,
/// mechanism)`. Probed and declared by the backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetAvailability {
    /// The maximum limit this profile can support.
    pub available: u64,
    /// The actual guarantee strength this machine provides.
    pub enforcement: Enforcement,
    /// The evidence claims the backend can produce for this dimension.
    pub evidence: EvidenceSet,
    /// Stable mechanism identity (and parameters) backing the dimension.
    pub mechanism: String,
}

/// The complete profile `B_P` — seven dimensions of host capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetProfile {
    /// Wall-clock capacity.
    pub wall_micros: BudgetAvailability,
    /// CPU capacity.
    pub cpu_micros: BudgetAvailability,
    /// Resident-memory capacity.
    pub resident_bytes: BudgetAvailability,
    /// Process-count capacity.
    pub process_count: BudgetAvailability,
    /// Handle-count capacity.
    pub handle_count: BudgetAvailability,
    /// Storage capacity.
    pub storage_bytes: BudgetAvailability,
    /// Network capacity.
    pub network_bytes: BudgetAvailability,
}

/// One dimension's ADMITTED contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedBudget {
    /// The effective (admitted) limit.
    pub effective_limit: u64,
    /// The guarantee the caller required.
    pub required_guarantee: MinGuarantee,
    /// The guarantee the backend actually provides.
    pub selected_guarantee: Enforcement,
    /// The evidence the caller required.
    pub required_evidence: EvidenceSet,
    /// The evidence the backend promises for this dimension.
    pub promised_evidence: EvidenceSet,
    /// The backing mechanism identity.
    pub mechanism: String,
    /// Digest of the source profile this admission was adjudicated against.
    pub profile_digest: Digest32,
}

/// The complete admitted budget `B_A` bound into a `BoundaryPlan`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedBudgets {
    /// Wall-clock contract.
    pub wall_micros: AdmittedBudget,
    /// CPU contract.
    pub cpu_micros: AdmittedBudget,
    /// Resident-memory contract.
    pub resident_bytes: AdmittedBudget,
    /// Process-count contract.
    pub process_count: AdmittedBudget,
    /// Handle-count contract.
    pub handle_count: AdmittedBudget,
    /// Storage contract.
    pub storage_bytes: AdmittedBudget,
    /// Network contract.
    pub network_bytes: AdmittedBudget,
}

/// The cross-dimensional derived structural minimums `DerivedMinimum_d(S,L)` — the
/// floor each dimension's limit must meet (e.g. a native launch ⇒ `process ≥ 1`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DerivedMinimums {
    /// Minimum wall µs.
    pub wall_micros: u64,
    /// Minimum CPU µs.
    pub cpu_micros: u64,
    /// Minimum resident bytes.
    pub resident_bytes: u64,
    /// Minimum process count.
    pub process_count: u64,
    /// Minimum handle count.
    pub handle_count: u64,
    /// Minimum storage bytes.
    pub storage_bytes: u64,
    /// Minimum network bytes.
    pub network_bytes: u64,
}

/// Why one budget dimension was refused. The canonical check ORDER is fixed (and
/// must match the circuit): limit, then guarantee, then evidence, then
/// structural-minimum — so the first-failing reason is deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BudgetFailure {
    /// Requested limit exceeds the backend's available limit (`L_d > A_d`).
    Limit,
    /// The backend's guarantee is weaker than required (`E_d < G_d`).
    Guarantee,
    /// Required evidence is not a subset of available evidence (`Q_d ⊄ C_d`).
    Evidence,
    /// The requested limit is below the derived structural minimum (`L_d < min`).
    StructuralMinimum,
}

/// A budget refusal: the first failing dimension and its reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetRefusal {
    /// The first dimension (in canonical order) that failed.
    pub dimension: BudgetDimension,
    /// Why it failed.
    pub failure: BudgetFailure,
}

/// Numeric strength of an enforcement level (security order, NOT the derived `Ord`).
fn enforcement_strength(enforcement: Enforcement) -> u8 {
    match enforcement {
        Enforcement::Unsupported => 0,
        Enforcement::Mediated => 1,
        Enforcement::Enforced => 2,
    }
}

/// Numeric strength of a required minimum guarantee.
fn guarantee_strength(guarantee: MinGuarantee) -> u8 {
    match guarantee {
        MinGuarantee::Mediated => 1,
        MinGuarantee::Enforced => 2,
    }
}

/// Adjudicate ONE dimension against its profile + derived minimum, FAIL-CLOSED in
/// the canonical reason order (limit → guarantee → evidence → structural-minimum).
///
/// # Errors
/// The first [`BudgetFailure`] in canonical order.
pub fn admit_dimension(
    request: &BudgetRequest,
    availability: &BudgetAvailability,
    derived_minimum: u64,
    profile_digest: Digest32,
) -> Result<AdmittedBudget, BudgetFailure> {
    if request.limit > availability.available {
        return Err(BudgetFailure::Limit);
    }
    if enforcement_strength(availability.enforcement) < guarantee_strength(request.guarantee) {
        return Err(BudgetFailure::Guarantee);
    }
    if !request.evidence.is_subset(&availability.evidence) {
        return Err(BudgetFailure::Evidence);
    }
    if request.limit < derived_minimum {
        return Err(BudgetFailure::StructuralMinimum);
    }
    Ok(AdmittedBudget {
        effective_limit: request.limit,
        required_guarantee: request.guarantee,
        selected_guarantee: availability.enforcement,
        required_evidence: request.evidence.clone(),
        promised_evidence: availability.evidence.clone(),
        mechanism: availability.mechanism.clone(),
        profile_digest,
    })
}

/// Adjudicate all seven dimensions in canonical order, returning the admitted
/// contract or the FIRST failing dimension + reason.
///
/// # Errors
/// The first [`BudgetRefusal`] (earliest dimension in canonical order to fail).
pub fn budget_admit(
    requirements: &BudgetRequirements,
    profile: &BudgetProfile,
    derived: &DerivedMinimums,
    profile_digest: Digest32,
) -> Result<AdmittedBudgets, BudgetRefusal> {
    let admit = |dimension: BudgetDimension,
                 request: &BudgetRequest,
                 availability: &BudgetAvailability,
                 minimum: u64|
     -> Result<AdmittedBudget, BudgetRefusal> {
        admit_dimension(request, availability, minimum, profile_digest)
            .map_err(|failure| BudgetRefusal { dimension, failure })
    };
    // Struct fields evaluate in written (canonical) order, so the first `?` failure
    // is the earliest failing dimension.
    Ok(AdmittedBudgets {
        wall_micros: admit(
            BudgetDimension::Wall,
            &requirements.wall_micros,
            &profile.wall_micros,
            derived.wall_micros,
        )?,
        cpu_micros: admit(
            BudgetDimension::Cpu,
            &requirements.cpu_micros,
            &profile.cpu_micros,
            derived.cpu_micros,
        )?,
        resident_bytes: admit(
            BudgetDimension::ResidentMemory,
            &requirements.resident_bytes,
            &profile.resident_bytes,
            derived.resident_bytes,
        )?,
        process_count: admit(
            BudgetDimension::ProcessCount,
            &requirements.process_count,
            &profile.process_count,
            derived.process_count,
        )?,
        handle_count: admit(
            BudgetDimension::HandleCount,
            &requirements.handle_count,
            &profile.handle_count,
            derived.handle_count,
        )?,
        storage_bytes: admit(
            BudgetDimension::Storage,
            &requirements.storage_bytes,
            &profile.storage_bytes,
            derived.storage_bytes,
        )?,
        network_bytes: admit(
            BudgetDimension::Network,
            &requirements.network_bytes,
            &profile.network_bytes,
            derived.network_bytes,
        )?,
    })
}

#[cfg(test)]
mod budget_tests {
    use super::{
        admit_dimension, budget_admit, BudgetAvailability, BudgetDimension, BudgetFailure,
        BudgetProfile, BudgetRefusal, BudgetRequest, BudgetRequirements, DerivedMinimums,
        MinGuarantee,
    };
    use crate::contract::capability::{Enforcement, EvidenceClaim, EvidenceSet};

    fn evidence(claims: &[EvidenceClaim]) -> EvidenceSet {
        let mut set = EvidenceSet::new();
        for claim in claims {
            set.insert(*claim);
        }
        set
    }

    fn request(limit: u64) -> BudgetRequest {
        BudgetRequest {
            limit,
            guarantee: MinGuarantee::Mediated,
            evidence: evidence(&[EvidenceClaim::ResourceUsage]),
        }
    }

    fn availability(available: u64) -> BudgetAvailability {
        BudgetAvailability {
            available,
            enforcement: Enforcement::Enforced,
            evidence: evidence(&[EvidenceClaim::ResourceUsage, EvidenceClaim::TerminalOutcome]),
            mechanism: "cgroup".to_string(),
        }
    }

    #[test]
    fn admits_a_dimension_within_capacity_guarantee_and_evidence() {
        let admitted =
            admit_dimension(&request(10), &availability(20), 1, [0u8; 32]).expect("admit");
        assert_eq!(admitted.effective_limit, 10);
        assert_eq!(admitted.selected_guarantee, Enforcement::Enforced);
        assert_eq!(admitted.required_guarantee, MinGuarantee::Mediated);
    }

    #[test]
    fn zero_is_a_legitimate_deny_all_bound() {
        // limit 0, available 0, derived min 0 -> admits (0 <= 0, 0 >= 0).
        assert!(admit_dimension(&request(0), &availability(0), 0, [0u8; 32]).is_ok());
    }

    #[test]
    fn failure_reasons_follow_the_canonical_order() {
        // Over capacity -> Limit (even though derived-min would also fail).
        assert_eq!(
            admit_dimension(&request(100), &availability(20), 200, [0u8; 32]),
            Err(BudgetFailure::Limit)
        );
        // Within capacity, but guarantee too weak.
        let weak = BudgetAvailability {
            enforcement: Enforcement::Unsupported,
            ..availability(20)
        };
        let strict = BudgetRequest {
            guarantee: MinGuarantee::Enforced,
            ..request(10)
        };
        assert_eq!(
            admit_dimension(&strict, &weak, 1, [0u8; 32]),
            Err(BudgetFailure::Guarantee)
        );
        // Capacity + guarantee OK, evidence not a subset.
        let demanding = BudgetRequest {
            evidence: evidence(&[EvidenceClaim::NetworkActivity]),
            ..request(10)
        };
        assert_eq!(
            admit_dimension(&demanding, &availability(20), 1, [0u8; 32]),
            Err(BudgetFailure::Evidence)
        );
        // Everything else OK, but below the derived structural minimum.
        assert_eq!(
            admit_dimension(&request(2), &availability(20), 5, [0u8; 32]),
            Err(BudgetFailure::StructuralMinimum)
        );
    }

    fn uniform_requirements(limit: u64) -> BudgetRequirements {
        BudgetRequirements {
            wall_micros: request(limit),
            cpu_micros: request(limit),
            resident_bytes: request(limit),
            process_count: request(limit),
            handle_count: request(limit),
            storage_bytes: request(limit),
            network_bytes: request(limit),
        }
    }

    fn uniform_profile(available: u64) -> BudgetProfile {
        BudgetProfile {
            wall_micros: availability(available),
            cpu_micros: availability(available),
            resident_bytes: availability(available),
            process_count: availability(available),
            handle_count: availability(available),
            storage_bytes: availability(available),
            network_bytes: availability(available),
        }
    }

    #[test]
    fn admits_all_seven_dimensions() {
        let admitted = budget_admit(
            &uniform_requirements(10),
            &uniform_profile(20),
            &DerivedMinimums::default(),
            [0u8; 32],
        )
        .expect("admit all seven");
        assert_eq!(admitted.network_bytes.effective_limit, 10);
    }

    #[test]
    fn refusal_names_the_first_failing_dimension_in_canonical_order() {
        // Break a LATER dimension (network) and an EARLIER one (cpu); the earliest
        // (cpu) must own the refusal.
        let mut reqs = uniform_requirements(10);
        reqs.cpu_micros.limit = 999; // over capacity
        reqs.network_bytes.limit = 999; // also over capacity, but later
        assert_eq!(
            budget_admit(
                &reqs,
                &uniform_profile(20),
                &DerivedMinimums::default(),
                [0u8; 32]
            ),
            Err(BudgetRefusal {
                dimension: BudgetDimension::Cpu,
                failure: BudgetFailure::Limit,
            })
        );
    }
}
