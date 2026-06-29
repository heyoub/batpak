//! The Linux backend's honest budget profile, split out of `backend_impl.rs` to hold
//! that file under the non-overridable structural-check size cap. Pure derivation over
//! the probed cgroup facts; no OS work here.

use crate::contract::budget::{BudgetAvailability, BudgetProfile};
use crate::contract::capability::{Enforcement, EvidenceClaim, EvidenceSet};

/// The honest budget profile. PROCESS COUNT is STRUCTURALLY enforced via the cgroup
/// v2 `pids` controller (`pids.max`) when a cgroup base was probed (`cgroup_pids_enforced`)
/// — then it is `Enforced`/Hard. The `ResourceUsage` evidence claim (the `pids.peak`
/// usage witness) is advertised SEPARATELY, ONLY when `pids_peak_witness` was probed: a
/// kernel can cap pids (≥ 4.3) WITHOUT exposing `pids.peak` (≥ 6.1), so a Hard cap does
/// NOT imply a witness — advertising the witness off the cap would be the over-claim
/// codex caught. Every OTHER dimension is `Mediated` (supervised, not structurally capped)
/// with no resource evidence — no cap is installed, so claiming `Enforced` there would
/// over-claim. With no cgroup base, process count is `Mediated` too (no unbacked cap).
pub(super) fn observed_budget_profile(
    cgroup_pids_enforced: bool,
    pids_peak_witness: bool,
) -> BudgetProfile {
    let observed = |mechanism: &str| BudgetAvailability {
        // Headroom only — we do NOT cap, so we never refuse on capacity here; the
        // honest signal is the `Mediated` (not `Enforced`) guarantee + empty
        // evidence, which forbids a spec from demanding a witnessed/enforced cap.
        available: u64::MAX,
        enforcement: Enforcement::Mediated,
        evidence: EvidenceSet::new(),
        mechanism: mechanism.to_string(),
    };
    // ProcessCount: a real structural cap (cgroup pids.max) when cgroup is available. The
    // ResourceUsage evidence is advertised ONLY when the pids.peak witness is also present.
    let process_count = if cgroup_pids_enforced {
        let evidence = if pids_peak_witness {
            [EvidenceClaim::ResourceUsage].into_iter().collect()
        } else {
            EvidenceSet::new()
        };
        BudgetAvailability {
            // The pids controller caps any count up to the kernel's pid ceiling.
            available: u64::MAX,
            enforcement: Enforcement::Enforced,
            evidence,
            mechanism: "cgroup_v2_pids:enforced".to_string(),
        }
    } else {
        observed("os_process:observed-not-capped")
    };
    BudgetProfile {
        wall_micros: observed("os_process_wait:observed-not-capped"),
        cpu_micros: observed("os_rusage:observed-not-capped"),
        resident_bytes: observed("os_rusage:observed-not-capped"),
        process_count,
        handle_count: observed("os_fd:observed-not-capped"),
        storage_bytes: observed("os_fs:observed-not-capped"),
        network_bytes: observed("os_net:observed-not-capped"),
    }
}
