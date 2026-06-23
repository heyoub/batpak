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

impl BudgetRequest {
    /// A deny-all request for one dimension: a zero limit, the minimal guarantee,
    /// and no required evidence.
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            limit: 0,
            guarantee: MinGuarantee::Mediated,
            evidence: EvidenceSet::new(),
        }
    }
}

impl BudgetRequirements {
    /// A deny-all request across all seven dimensions (every limit `0`). The
    /// honest neutral default: it constrains the workload to nothing until the
    /// caller asks for resources explicitly.
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            wall_micros: BudgetRequest::deny_all(),
            cpu_micros: BudgetRequest::deny_all(),
            resident_bytes: BudgetRequest::deny_all(),
            process_count: BudgetRequest::deny_all(),
            handle_count: BudgetRequest::deny_all(),
            storage_bytes: BudgetRequest::deny_all(),
            network_bytes: BudgetRequest::deny_all(),
        }
    }
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

impl BudgetAvailability {
    /// A dimension the backend does NOT enforce: it imposes no ceiling
    /// (`available = u64::MAX`), witnesses nothing, and names no mechanism. The
    /// honest declaration for a no-confinement reference backend.
    #[must_use]
    pub fn unenforced() -> Self {
        Self {
            available: u64::MAX,
            enforcement: Enforcement::Unsupported,
            evidence: EvidenceSet::new(),
            mechanism: "none/no-budget-enforcement".to_string(),
        }
    }
}

impl BudgetProfile {
    /// A profile that enforces NO budget dimension — every dimension unenforced.
    /// A spec requiring any budget guarantee (`≥ Mediated`) is therefore refused
    /// once the budget membrane lands; the dimensions stay first-class regardless.
    #[must_use]
    pub fn all_unenforced() -> Self {
        Self {
            wall_micros: BudgetAvailability::unenforced(),
            cpu_micros: BudgetAvailability::unenforced(),
            resident_bytes: BudgetAvailability::unenforced(),
            process_count: BudgetAvailability::unenforced(),
            handle_count: BudgetAvailability::unenforced(),
            storage_bytes: BudgetAvailability::unenforced(),
            network_bytes: BudgetAvailability::unenforced(),
        }
    }
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

/// Why one budget dimension was refused. The canonical reason ORDER is fixed (and
/// must match the circuit + shadow + JSON projection + solver model): the intrinsic
/// derived-minimum check comes FIRST (the request is internally incoherent,
/// independent of any backend), THEN backend adjudication — capacity, then
/// guarantee, then evidence — so the first-failing reason is deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BudgetFailure {
    /// The requested limit is below the derived structural minimum (`L_d < D_d`) —
    /// the request is internally incoherent, refused BEFORE any backend is asked.
    BelowDerivedMinimum,
    /// The requested limit exceeds the backend's available capacity (`L_d > A_d`).
    CapacityExceeded,
    /// The backend's guarantee is weaker than required (`E_d < G_d`).
    GuaranteeInsufficient,
    /// Required evidence is not a subset of available evidence (`Q_d ⊄ C_d`).
    EvidenceMissing,
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

/// Adjudicate ONE dimension's coherent request against the backend's available
/// capacity, FAIL-CLOSED in canonical reason order (capacity → guarantee →
/// evidence). The intrinsic derived-minimum check (`L_d ≥ D_d`) is a SEPARATE,
/// EARLIER phase in [`budget_admit`]; a dimension reaching here has already passed
/// it, so this never returns [`BudgetFailure::BelowDerivedMinimum`].
///
/// # Errors
/// The first of [`BudgetFailure::CapacityExceeded`],
/// [`BudgetFailure::GuaranteeInsufficient`], [`BudgetFailure::EvidenceMissing`].
pub fn adjudicate_dimension(
    request: &BudgetRequest,
    availability: &BudgetAvailability,
    profile_digest: Digest32,
) -> Result<AdmittedBudget, BudgetFailure> {
    if request.limit > availability.available {
        return Err(BudgetFailure::CapacityExceeded);
    }
    if enforcement_strength(availability.enforcement) < guarantee_strength(request.guarantee) {
        return Err(BudgetFailure::GuaranteeInsufficient);
    }
    if !request.evidence.is_subset(&availability.evidence) {
        return Err(BudgetFailure::EvidenceMissing);
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

/// Admit all seven dimensions in two phases, returning the admitted contract or the
/// FIRST failing dimension + reason in canonical order.
///
/// PHASE 1 — intrinsic request validation: every dimension must be internally
/// coherent (`L_d ≥ D_d`) BEFORE any backend is asked to satisfy it. The request is
/// REFUSED with the offending dimension, never silently clamped up to the minimum.
///
/// PHASE 2 — backend adjudication: a coherent request is matched per dimension
/// against the profile's capacity, guarantee, and evidence.
///
/// Both phases walk the canonical dimension order (wall, cpu, resident memory,
/// process, handle, storage, network); phase 1 fully precedes phase 2, so a
/// below-minimum dimension always out-ranks any capacity/guarantee/evidence failure.
///
/// # Errors
/// The first [`BudgetRefusal`] — phase 1 (`BelowDerivedMinimum`) before phase 2.
pub fn budget_admit(
    requirements: &BudgetRequirements,
    profile: &BudgetProfile,
    derived: &DerivedMinimums,
    profile_digest: Digest32,
) -> Result<AdmittedBudgets, BudgetRefusal> {
    // PHASE 1 — intrinsic coherence, canonical dimension order.
    let intrinsic = [
        (
            BudgetDimension::Wall,
            requirements.wall_micros.limit,
            derived.wall_micros,
        ),
        (
            BudgetDimension::Cpu,
            requirements.cpu_micros.limit,
            derived.cpu_micros,
        ),
        (
            BudgetDimension::ResidentMemory,
            requirements.resident_bytes.limit,
            derived.resident_bytes,
        ),
        (
            BudgetDimension::ProcessCount,
            requirements.process_count.limit,
            derived.process_count,
        ),
        (
            BudgetDimension::HandleCount,
            requirements.handle_count.limit,
            derived.handle_count,
        ),
        (
            BudgetDimension::Storage,
            requirements.storage_bytes.limit,
            derived.storage_bytes,
        ),
        (
            BudgetDimension::Network,
            requirements.network_bytes.limit,
            derived.network_bytes,
        ),
    ];
    for (dimension, limit, minimum) in intrinsic {
        if limit < minimum {
            return Err(BudgetRefusal {
                dimension,
                failure: BudgetFailure::BelowDerivedMinimum,
            });
        }
    }

    // PHASE 2 — backend adjudication. Struct fields evaluate in written (canonical)
    // order, so the first `?` failure is the earliest failing dimension.
    let adjudicate = |dimension: BudgetDimension,
                      request: &BudgetRequest,
                      availability: &BudgetAvailability|
     -> Result<AdmittedBudget, BudgetRefusal> {
        adjudicate_dimension(request, availability, profile_digest)
            .map_err(|failure| BudgetRefusal { dimension, failure })
    };
    Ok(AdmittedBudgets {
        wall_micros: adjudicate(
            BudgetDimension::Wall,
            &requirements.wall_micros,
            &profile.wall_micros,
        )?,
        cpu_micros: adjudicate(
            BudgetDimension::Cpu,
            &requirements.cpu_micros,
            &profile.cpu_micros,
        )?,
        resident_bytes: adjudicate(
            BudgetDimension::ResidentMemory,
            &requirements.resident_bytes,
            &profile.resident_bytes,
        )?,
        process_count: adjudicate(
            BudgetDimension::ProcessCount,
            &requirements.process_count,
            &profile.process_count,
        )?,
        handle_count: adjudicate(
            BudgetDimension::HandleCount,
            &requirements.handle_count,
            &profile.handle_count,
        )?,
        storage_bytes: adjudicate(
            BudgetDimension::Storage,
            &requirements.storage_bytes,
            &profile.storage_bytes,
        )?,
        network_bytes: adjudicate(
            BudgetDimension::Network,
            &requirements.network_bytes,
            &profile.network_bytes,
        )?,
    })
}

#[cfg(test)]
mod budget_tests {
    use super::{
        adjudicate_dimension, budget_admit, BudgetAvailability, BudgetDimension, BudgetFailure,
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
    fn unenforced_profile_imposes_no_ceiling_and_no_guarantee() {
        let avail = BudgetAvailability::unenforced();
        assert_eq!(avail.available, u64::MAX);
        assert_eq!(avail.enforcement, Enforcement::Unsupported);
        let profile = BudgetProfile::all_unenforced();
        assert_eq!(profile.network_bytes.enforcement, Enforcement::Unsupported);
        assert!(profile.wall_micros.evidence.is_empty());
    }

    #[test]
    fn deny_all_is_every_limit_zero() {
        let reqs = BudgetRequirements::deny_all();
        assert_eq!(BudgetRequest::deny_all().limit, 0);
        assert_eq!(reqs.wall_micros.limit, 0);
        assert_eq!(reqs.network_bytes.limit, 0);
        assert_eq!(reqs.process_count.guarantee, MinGuarantee::Mediated);
    }

    #[test]
    fn admits_a_dimension_within_capacity_guarantee_and_evidence() {
        let admitted =
            adjudicate_dimension(&request(10), &availability(20), [0u8; 32]).expect("admit");
        assert_eq!(admitted.effective_limit, 10);
        assert_eq!(admitted.selected_guarantee, Enforcement::Enforced);
        assert_eq!(admitted.required_guarantee, MinGuarantee::Mediated);
    }

    #[test]
    fn zero_is_a_legitimate_deny_all_bound() {
        // limit 0, available 0 -> adjudicates OK (0 <= 0). The derived-minimum
        // check is a separate, earlier phase in budget_admit.
        assert!(adjudicate_dimension(&request(0), &availability(0), [0u8; 32]).is_ok());
    }

    #[test]
    fn adjudication_failures_follow_the_canonical_order() {
        // Over capacity.
        assert_eq!(
            adjudicate_dimension(&request(100), &availability(20), [0u8; 32]),
            Err(BudgetFailure::CapacityExceeded)
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
            adjudicate_dimension(&strict, &weak, [0u8; 32]),
            Err(BudgetFailure::GuaranteeInsufficient)
        );
        // Capacity + guarantee OK, evidence not a subset.
        let demanding = BudgetRequest {
            evidence: evidence(&[EvidenceClaim::NetworkActivity]),
            ..request(10)
        };
        assert_eq!(
            adjudicate_dimension(&demanding, &availability(20), [0u8; 32]),
            Err(BudgetFailure::EvidenceMissing)
        );
    }

    #[test]
    fn derived_minimum_is_checked_before_backend_adjudication() {
        // Wall: requested 2, below derived 5 AND over capacity 1. The intrinsic
        // BelowDerivedMinimum (phase 1) out-ranks the capacity failure (phase 2),
        // and the request is refused — never clamped up to the minimum.
        let derived = DerivedMinimums {
            wall_micros: 5,
            ..DerivedMinimums::default()
        };
        let profile = BudgetProfile {
            wall_micros: availability(1),
            ..uniform_profile(20)
        };
        assert_eq!(
            budget_admit(&uniform_requirements(2), &profile, &derived, [0u8; 32]),
            Err(BudgetRefusal {
                dimension: BudgetDimension::Wall,
                failure: BudgetFailure::BelowDerivedMinimum,
            })
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
                failure: BudgetFailure::CapacityExceeded,
            })
        );
    }
}
