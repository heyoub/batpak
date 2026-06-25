//! Execution-budget WITNESS contract (kernel plan §7, execution slice): the
//! per-dimension `W_d = (L, G, E, M, O, R, F)` the [`crate::BoundaryReport`] carries
//! for every budget dimension. This FREEZES the shape before Spawn so the public
//! execution model serves it, rather than bending around a published seam.
//!
//! Real witnessing (populating `O`/`F` from simulated or native counters, tripping
//! each limit) is the execution vertical slice. Until a backend wires real counters
//! for a dimension it emits an UNWITNESSED echo: the admitted contract `(L,G,E,M)` is
//! known from the plan, but usage is unobserved and the finding is
//! [`BudgetFinding::ObservationUnavailable`] — uncertainty preserved honestly, never
//! a fabricated measurement.

use crate::contract::budget::{AdmittedBudget, AdmittedBudgets, MinGuarantee};
use crate::contract::capability::{Enforcement, EvidenceSet};
use serde::{Deserialize, Serialize};

/// The quantitative enforcement semantics actually supplied for a dimension (`E_d`).
/// "Enforced" without overshoot numbers is too vague for timers, sampled memory, CPU
/// accounting, and network mediation — so a mediated guarantee carries them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuaranteeProfile {
    /// HARD: the limit structurally cannot be exceeded (e.g. `cgroup.kill`, `rlimit`)
    /// — no sampling, no overshoot.
    Hard,
    /// MEDIATED: enforced by SAMPLING, so a bounded overshoot is possible between
    /// samples.
    Mediated {
        /// Sampling interval (µs) between enforcement checks.
        sample_interval_micros: u64,
        /// Maximum overshoot tolerated between samples (the dimension's unit).
        max_overshoot: u64,
    },
}

impl GuaranteeProfile {
    /// The quantitative profile a qualitative [`Enforcement`] maps to BEFORE execution
    /// declares real sampling numbers: `Enforced` → `Hard`; `Mediated` → sampling with
    /// as-yet-undeclared interval/overshoot (`0, 0`). `Unsupported` never reaches an
    /// admitted dimension; it maps to the same conservative mediated placeholder.
    #[must_use]
    pub fn from_enforcement(enforcement: Enforcement) -> Self {
        match enforcement {
            Enforcement::Enforced => Self::Hard,
            Enforcement::Mediated | Enforcement::Unsupported => Self::Mediated {
                sample_interval_micros: 0,
                max_overshoot: 0,
            },
        }
    }
}

/// The terminal budget finding for a dimension (`F_d`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetFinding {
    /// Observed usage stayed within the admitted limit.
    WithinLimit,
    /// The limit was reached and enforcement ACTIVATED (the workload was held or
    /// killed at the boundary).
    LimitReachedEnforced,
    /// The limit was exceeded but within the DECLARED mediated overshoot bound —
    /// acceptable for a `Mediated` guarantee.
    ExceededWithinMediatedOvershoot,
    /// The enforcement MECHANISM itself faulted (could not enforce) — fail-closed.
    EnforcementFault,
    /// Observation is UNAVAILABLE — uncertainty preserved honestly, never a fabricated
    /// measurement. The canonical case is an uncontrolled host loss (crash) after
    /// which CPU/memory cannot be reconstructed; it also covers a backend that does
    /// not yet witness this dimension.
    ObservationUnavailable,
}

/// One dimension's execution witness: `W_d = (L, G, E, M, O, R, F)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetWitness {
    /// `L_d` — the admitted limit (echoed from the plan).
    pub admitted_limit: u64,
    /// `G_d` — the guarantee the caller required.
    pub required_guarantee: MinGuarantee,
    /// `E_d` — the guarantee actually supplied, with quantitative semantics.
    pub supplied_guarantee: GuaranteeProfile,
    /// `M_d` — the enforcing/mediating mechanism identity.
    pub mechanism: String,
    /// `O_d` — the observed usage (the dimension's unit). Meaningless unless `finding`
    /// is a MEASURED state (not [`BudgetFinding::ObservationUnavailable`]).
    pub observed_usage: u64,
    /// `R_d` — the evidence claims backing this witness.
    pub evidence: EvidenceSet,
    /// `F_d` — the terminal budget finding.
    pub finding: BudgetFinding,
}

impl BudgetWitness {
    /// Classify a MEASURED usage against the admitted limit — the simple
    /// within/at-limit finding. (The richer `ExceededWithinMediatedOvershoot` and
    /// `EnforcementFault` cases are decided by the enforcing mechanism, not here.)
    #[must_use]
    pub fn classify(admitted_limit: u64, observed_usage: u64) -> BudgetFinding {
        if observed_usage <= admitted_limit {
            BudgetFinding::WithinLimit
        } else {
            BudgetFinding::LimitReachedEnforced
        }
    }

    /// A WITNESSED dimension: the contract `(L, G, E, M)` echoed from the plan PLUS a
    /// real `observed_usage` measured by a counter, with the within/at-limit finding.
    /// Used for dimensions an execution backend actually measures — e.g. wall time
    /// from the [`batpak::store::Clock`] seam.
    #[must_use]
    pub fn witnessed(admitted: &AdmittedBudget, observed_usage: u64) -> Self {
        Self {
            admitted_limit: admitted.effective_limit,
            required_guarantee: admitted.required_guarantee,
            supplied_guarantee: GuaranteeProfile::from_enforcement(admitted.selected_guarantee),
            mechanism: admitted.mechanism.clone(),
            observed_usage,
            evidence: admitted.promised_evidence.clone(),
            finding: Self::classify(admitted.effective_limit, observed_usage),
        }
    }

    /// An UNWITNESSED echo of an admitted dimension: the contract `(L, G, E, M)` is
    /// known from the plan, but usage is genuinely NOT measured
    /// ([`BudgetFinding::ObservationUnavailable`], `observed_usage = 0` as a
    /// non-measurement). EARNED, not lazy: a dimension is unwitnessed only when its
    /// counter is genuinely absent (cpu needs per-tree CPU accounting; resident /
    /// process / handle / storage / network need OS counters). Time dimensions that
    /// have a [`batpak::store::Clock`] use [`Self::witnessed`] instead.
    #[must_use]
    pub fn unwitnessed(admitted: &AdmittedBudget) -> Self {
        Self {
            admitted_limit: admitted.effective_limit,
            required_guarantee: admitted.required_guarantee,
            supplied_guarantee: GuaranteeProfile::from_enforcement(admitted.selected_guarantee),
            mechanism: admitted.mechanism.clone(),
            observed_usage: 0,
            evidence: admitted.promised_evidence.clone(),
            finding: BudgetFinding::ObservationUnavailable,
        }
    }
}

/// The seven execution witnesses the report carries — one per fixed budget dimension,
/// in canonical order, with FIXED observed-usage semantics (per field).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetWitnesses {
    /// Wall-clock: elapsed microseconds from workload launch to terminal classification.
    pub wall_micros: BudgetWitness,
    /// CPU: cumulative microseconds across the entire run tree.
    pub cpu_micros: BudgetWitness,
    /// Resident memory: peak aggregate resident bytes across the run tree.
    pub resident_bytes: BudgetWitness,
    /// Process count: peak simultaneously live processes, including the root.
    pub process_count: BudgetWitness,
    /// Handle count: peak aggregate open descriptors/handles across the run tree.
    pub handle_count: BudgetWitness,
    /// Storage: peak bytes present in boundary-owned writable AND quarantine storage.
    pub storage_bytes: BudgetWitness,
    /// Network: cumulative ingress plus egress bytes.
    pub network_bytes: BudgetWitness,
}

impl BudgetWitnesses {
    /// The UNWITNESSED echo of an admitted contract — every dimension's `(L, G, E, M)`
    /// known, usage unobserved. The execution slice replaces these with real findings.
    #[must_use]
    pub fn unwitnessed(admitted: &AdmittedBudgets) -> Self {
        Self {
            wall_micros: BudgetWitness::unwitnessed(&admitted.wall_micros),
            cpu_micros: BudgetWitness::unwitnessed(&admitted.cpu_micros),
            resident_bytes: BudgetWitness::unwitnessed(&admitted.resident_bytes),
            process_count: BudgetWitness::unwitnessed(&admitted.process_count),
            handle_count: BudgetWitness::unwitnessed(&admitted.handle_count),
            storage_bytes: BudgetWitness::unwitnessed(&admitted.storage_bytes),
            network_bytes: BudgetWitness::unwitnessed(&admitted.network_bytes),
        }
    }

    /// Witnesses with WALL TIME measured (`wall_micros_observed` = the elapsed wall a
    /// [`batpak::store::Clock`] read from launch to terminal) and the other six
    /// dimensions genuinely unwitnessed until their counters land. This is the first
    /// execution-slice step: the one budget dimension a clock gives directly stops
    /// defaulting to `ObservationUnavailable`.
    #[must_use]
    pub fn with_wall(admitted: &AdmittedBudgets, wall_micros_observed: u64) -> Self {
        Self {
            wall_micros: BudgetWitness::witnessed(&admitted.wall_micros, wall_micros_observed),
            cpu_micros: BudgetWitness::unwitnessed(&admitted.cpu_micros),
            resident_bytes: BudgetWitness::unwitnessed(&admitted.resident_bytes),
            process_count: BudgetWitness::unwitnessed(&admitted.process_count),
            handle_count: BudgetWitness::unwitnessed(&admitted.handle_count),
            storage_bytes: BudgetWitness::unwitnessed(&admitted.storage_bytes),
            network_bytes: BudgetWitness::unwitnessed(&admitted.network_bytes),
        }
    }

    /// Witnesses with PROCESS COUNT measured (`process_peak_observed` = the cgroup v2
    /// `pids.peak` high-water mark the Linux backend read after the run), the other six
    /// genuinely unwitnessed. The pid count is the dimension a `pids.max` cgroup cap
    /// structurally enforces (`GuaranteeProfile::Hard`), so its witness carries a REAL
    /// measured peak instead of [`BudgetFinding::ObservationUnavailable`]. The
    /// within/at-limit finding is derived by [`BudgetWitness::classify`]; the kernel
    /// guarantees the peak never exceeds the cap, so a held cap reads `WithinLimit`.
    #[must_use]
    pub fn with_process_count(admitted: &AdmittedBudgets, process_peak_observed: u64) -> Self {
        Self {
            wall_micros: BudgetWitness::unwitnessed(&admitted.wall_micros),
            cpu_micros: BudgetWitness::unwitnessed(&admitted.cpu_micros),
            resident_bytes: BudgetWitness::unwitnessed(&admitted.resident_bytes),
            process_count: BudgetWitness::witnessed(&admitted.process_count, process_peak_observed),
            handle_count: BudgetWitness::unwitnessed(&admitted.handle_count),
            storage_bytes: BudgetWitness::unwitnessed(&admitted.storage_bytes),
            network_bytes: BudgetWitness::unwitnessed(&admitted.network_bytes),
        }
    }
}

#[cfg(test)]
mod budget_witness_tests {
    use super::{BudgetFinding, BudgetWitnesses, GuaranteeProfile};
    use crate::contract::budget::{
        budget_admit, BudgetAvailability, BudgetProfile, BudgetRequirements, DerivedMinimums,
        MinGuarantee,
    };
    use crate::contract::capability::{Enforcement, EvidenceSet};

    fn admitted() -> crate::contract::budget::AdmittedBudgets {
        let enforced = BudgetAvailability {
            available: 1_000_000,
            enforcement: Enforcement::Enforced,
            evidence: EvidenceSet::new(),
            mechanism: "sim".to_string(),
        };
        let profile = BudgetProfile {
            wall_micros: enforced.clone(),
            cpu_micros: enforced.clone(),
            resident_bytes: enforced.clone(),
            process_count: enforced.clone(),
            handle_count: enforced.clone(),
            storage_bytes: enforced.clone(),
            network_bytes: enforced,
        };
        budget_admit(
            &BudgetRequirements::uniform(64, MinGuarantee::Mediated),
            &profile,
            &DerivedMinimums::default(),
            [0u8; 32],
        )
        .expect("admits")
    }

    #[test]
    fn from_enforcement_maps_enforced_to_hard_and_mediated_to_sampling() {
        assert_eq!(
            GuaranteeProfile::from_enforcement(Enforcement::Enforced),
            GuaranteeProfile::Hard
        );
        assert!(matches!(
            GuaranteeProfile::from_enforcement(Enforcement::Mediated),
            GuaranteeProfile::Mediated { .. }
        ));
    }

    #[test]
    fn unwitnessed_echoes_the_contract_and_preserves_uncertainty() {
        let witnesses = BudgetWitnesses::unwitnessed(&admitted());
        // The contract (L, G, E, M) is echoed from the admitted plan.
        assert_eq!(witnesses.process_count.admitted_limit, 64);
        assert_eq!(
            witnesses.process_count.required_guarantee,
            MinGuarantee::Mediated
        );
        assert_eq!(
            witnesses.process_count.supplied_guarantee,
            GuaranteeProfile::Hard
        );
        // Usage is NOT fabricated — it is honestly unavailable.
        assert_eq!(witnesses.process_count.observed_usage, 0);
        assert_eq!(
            witnesses.network_bytes.finding,
            BudgetFinding::ObservationUnavailable
        );
    }

    #[test]
    fn with_wall_witnesses_time_via_a_real_measurement_not_unavailable() {
        // Wall is MEASURED (40 us against the admitted 64) — clock-sourced, within
        // limit. The other six are EARNED-unavailable (no counter yet).
        let witnesses = BudgetWitnesses::with_wall(&admitted(), 40);
        assert_eq!(witnesses.wall_micros.observed_usage, 40);
        assert_eq!(witnesses.wall_micros.finding, BudgetFinding::WithinLimit);
        assert_eq!(
            witnesses.cpu_micros.finding,
            BudgetFinding::ObservationUnavailable
        );
        assert_eq!(
            witnesses.network_bytes.finding,
            BudgetFinding::ObservationUnavailable
        );
    }

    #[test]
    fn with_process_count_witnesses_pids_via_a_real_peak_not_unavailable() {
        // Process count is MEASURED (peak 3 against the admitted 64) — cgroup
        // pids.peak-sourced, within limit, Hard guarantee. The other six are
        // EARNED-unavailable (no counter yet).
        let witnesses = BudgetWitnesses::with_process_count(&admitted(), 3);
        assert_eq!(witnesses.process_count.observed_usage, 3);
        assert_eq!(witnesses.process_count.finding, BudgetFinding::WithinLimit);
        assert_eq!(
            witnesses.process_count.supplied_guarantee,
            GuaranteeProfile::Hard
        );
        assert_eq!(
            witnesses.cpu_micros.finding,
            BudgetFinding::ObservationUnavailable
        );
    }

    #[test]
    fn a_measured_wall_over_the_limit_is_limit_reached_enforced() {
        // 100 us against the admitted 64 -> the wall budget tripped.
        let witnesses = BudgetWitnesses::with_wall(&admitted(), 100);
        assert_eq!(witnesses.wall_micros.observed_usage, 100);
        assert_eq!(
            witnesses.wall_micros.finding,
            BudgetFinding::LimitReachedEnforced
        );
    }
}
