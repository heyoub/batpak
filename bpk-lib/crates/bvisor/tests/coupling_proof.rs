// The S1 COUPLING GATE: the §1 law that a production profile may advertise
// `Enforced(k)` ONLY when the committed qualification ledger holds `Proven(k)` for
// a matching mechanism digest at a profile floor the machine satisfies. The proof
// hooks (`with_cgroup_for_proof`, `proof_ceiling`, `proof_facts`,
// `proof_mechanism`) live behind `dangerous-test-hooks`, so the whole file is
// gated to Linux + that feature.
#![cfg(all(
    feature = "backend-linux",
    feature = "dangerous-test-hooks",
    target_os = "linux"
))]
//! GAUNTLET bvisor S1 — the QUALIFICATION COUPLING gate.
//!
//! For representative machine profiles, this asserts the §1 COUPLING LAW directly:
//! for EVERY [`RequirementKind`] the production [`BackendProfile`] ceiling advertises
//! at [`Enforcement::Enforced`], the committed [`LINUX_QUALIFICATION_LEDGER`] MUST
//! hold a [`QualificationStatus::Proven`] row whose:
//!   - [`ProfileFloor`] is SATISFIED by the running profile's [`ProfileFacts`]
//!     (`p_prod ⊒ floor`, the §3 profile-class domination), AND
//!   - [`MechanismDigest`] MATCHES the backend's live `mechanism(req, Enforced)`
//!     digest (so a backend cannot satisfy the gate under a swapped, unproven
//!     mechanism).
//!
//! A backend can NEVER self-stamp `Proven`: the ledger is the repository's
//! independent record, each Proven row citing a real passing oracle.
//!
//! REPRESENTATIVE PROFILES (built WITHOUT touching the live kernel):
//!   - `with_cgroup_for_proof(true)` — the production-shaped profile that backs
//!     Filesystem + LaunchWorkload + CaptureStreams + Kill (cgroup) Enforced.
//!   - `with_abi_for_proof(FLOOR)` — at the landlock floor, no cgroup: backs
//!     Filesystem + Launch + Capture, but NOT Kill (so the gate never demands a
//!     Kill row it cannot satisfy).
//!   - `with_abi_for_proof(FLOOR - 1)` — below the floor: Filesystem drops out of
//!     the ceiling entirely (fail-closed), so the gate demands NO Filesystem row.
//!
//! RED FIXTURE (`--cfg gauntlet_red_fixture`, ProductionFlip): plants an `Enforced`
//! ceiling cell (`NetworkDenyAll`) that has NO `Proven` ledger row and asserts the
//! coupling check REDs. The real ceiling never advertises it, so this can only
//! happen by flipping production — and a biting gate catches it, so the red half
//! FAILS, proving the gate is anti-vacuous. Registered as the blocking
//! ProductionFlip gate `bvisor-qualification-coupling` in `gate_registry.rs`.

use bvisor::{
    BackendProfile, Enforcement, LinuxBackend, MechanismDigest, ProfileFacts, QualificationStatus,
    RequirementKind, LINUX_QUALIFICATION_LEDGER,
};

/// A single coupling violation — why an `Enforced` cell is not properly qualified.
#[derive(Debug, PartialEq, Eq)]
enum CouplingViolation {
    /// The ledger holds no row at all for an Enforced cell.
    NoLedgerRow(RequirementKind),
    /// The ledger row exists but is not `Proven`.
    NotProven(RequirementKind, QualificationStatus),
    /// The row is Proven but the running profile does not satisfy its floor.
    FloorNotSatisfied(RequirementKind),
    /// The row is Proven but its mechanism digest does not match the backend's.
    MechanismDigestMismatch(RequirementKind),
}

impl CouplingViolation {
    /// A human message that READS every field — so the diagnostic is informative
    /// AND the payloads are genuinely consumed (no dead-field warning, no `#[allow]`).
    fn describe(&self) -> String {
        match self {
            Self::NoLedgerRow(kind) => {
                format!("Enforced cell {kind:?} has NO ledger row (uncoupled over-claim)")
            }
            Self::NotProven(kind, status) => {
                format!("Enforced cell {kind:?} ledger row is {status:?}, not Proven")
            }
            Self::FloorNotSatisfied(kind) => {
                format!("Enforced cell {kind:?} Proven row's ProfileFloor is not satisfied")
            }
            Self::MechanismDigestMismatch(kind) => {
                format!("Enforced cell {kind:?} mechanism digest does not match the ledger")
            }
        }
    }
}

/// THE PURE COUPLING CHECK — split out so the red fixture can drive it with a
/// planted (Enforced-without-Proven) cell. For each Enforced kind, find the ledger
/// row, require `Proven` + floor-satisfied + matching mechanism digest.
///
/// `mechanism_digest_of` yields the backend's live digest for a kind (so the test
/// proves the LEDGER's committed mechanism equals the BACKEND's actual mechanism).
fn check_coupling(
    enforced: &[RequirementKind],
    facts: &ProfileFacts,
    mechanism_digest_of: &dyn Fn(RequirementKind) -> MechanismDigest,
) -> Result<(), CouplingViolation> {
    for &kind in enforced {
        let row = LINUX_QUALIFICATION_LEDGER
            .iter()
            .find(|r| r.key == kind)
            .ok_or(CouplingViolation::NoLedgerRow(kind))?;
        if row.status != QualificationStatus::Proven {
            return Err(CouplingViolation::NotProven(kind, row.status));
        }
        if !row.profile_floor.satisfied_by(facts) {
            return Err(CouplingViolation::FloorNotSatisfied(kind));
        }
        if row.mechanism_digest() != mechanism_digest_of(kind) {
            return Err(CouplingViolation::MechanismDigestMismatch(kind));
        }
    }
    Ok(())
}

/// The backend's live digest for an Enforced cell of `kind` — the digest the ledger
/// must match. Closures over the backend so the check stays pure.
fn live_digest_fn(backend: &LinuxBackend) -> impl Fn(RequirementKind) -> MechanismDigest + '_ {
    move |kind| MechanismDigest::of_mechanism(&backend.proof_mechanism(kind, Enforcement::Enforced))
}

/// Run the coupling check for a backend against its OWN production ceiling.
fn coupling_for(backend: &LinuxBackend) -> Result<(), CouplingViolation> {
    let ceiling: BackendProfile = backend.proof_ceiling();
    let enforced = ceiling.enforced_kinds();
    check_coupling(&enforced, &backend.proof_facts(), &live_digest_fn(backend))
}

#[test]
fn cgroup_profile_couples_every_enforced_cell_to_a_proven_row() {
    let backend = LinuxBackend::with_cgroup_for_proof(true);
    // Non-vacuous: this profile DOES advertise Enforced cells (Filesystem, Launch,
    // Capture, Kill) — the gate is checking real cells, not an empty set.
    let enforced = backend.proof_ceiling().enforced_kinds();
    assert!(
        enforced.contains(&RequirementKind::Filesystem)
            && enforced.contains(&RequirementKind::Kill)
            && enforced.contains(&RequirementKind::LaunchWorkload)
            && enforced.contains(&RequirementKind::CaptureStreams),
        "the cgroup profile must advertise the Filesystem/Kill/Launch/Capture cells \
         the gate then qualifies; got {enforced:?}"
    );
    let result = coupling_for(&backend);
    assert!(
        result.is_ok(),
        "cgroup profile must couple every Enforced cell to a Proven row: {}",
        result.err().map_or_else(String::new, |v| v.describe())
    );
}

#[test]
fn at_floor_no_cgroup_profile_couples_and_omits_kill() {
    let backend = LinuxBackend::with_abi_for_proof(LinuxBackend::LANDLOCK_ABI_FLOOR);
    let enforced = backend.proof_ceiling().enforced_kinds();
    // Filesystem is enforced at the floor; Kill is NOT (no cgroup base), so the gate
    // must NOT demand a Kill row for this profile.
    assert!(
        enforced.contains(&RequirementKind::Filesystem),
        "Filesystem is Enforced at the ABI floor: {enforced:?}"
    );
    assert!(
        !enforced.contains(&RequirementKind::Kill),
        "Kill must be absent (no cgroup base) so the gate never demands a Kill row: {enforced:?}"
    );
    let result = coupling_for(&backend);
    assert!(
        result.is_ok(),
        "at-floor profile must couple cleanly: {}",
        result.err().map_or_else(String::new, |v| v.describe())
    );
}

#[test]
fn below_floor_profile_drops_filesystem_from_the_ceiling() {
    let backend = LinuxBackend::with_abi_for_proof(LinuxBackend::LANDLOCK_ABI_FLOOR - 1);
    let enforced = backend.proof_ceiling().enforced_kinds();
    // Below the floor Filesystem fails closed (absent from the ceiling), so the gate
    // demands NO Filesystem row — the §3 floor would not be satisfiable anyway.
    assert!(
        !enforced.contains(&RequirementKind::Filesystem),
        "Filesystem must drop out of the ceiling below the ABI floor: {enforced:?}"
    );
    let result = coupling_for(&backend);
    assert!(
        result.is_ok(),
        "below-floor profile must couple cleanly: {}",
        result.err().map_or_else(String::new, |v| v.describe())
    );
}

/// The mechanism-digest binding is REAL: the ledger's committed Filesystem mechanism
/// string digests to exactly the backend's live `mechanism(Filesystem, Enforced)`
/// digest. (If they ever diverged, `cgroup_profile_couples_*` would red with a
/// `MechanismDigestMismatch` — this asserts the match directly, non-vacuously.)
#[test]
fn ledger_mechanism_digest_equals_the_backend_live_digest() {
    let backend = LinuxBackend::with_cgroup_for_proof(true);
    let row = LINUX_QUALIFICATION_LEDGER
        .iter()
        .find(|r| r.key == RequirementKind::Filesystem)
        .expect("Filesystem ledger row");
    let live = MechanismDigest::of_mechanism(
        &backend.proof_mechanism(RequirementKind::Filesystem, Enforcement::Enforced),
    );
    assert_eq!(
        row.mechanism_digest(),
        live,
        "the committed Filesystem mechanism digest must equal the backend's live one"
    );
}

/// A swapped/unproven mechanism is caught: if the backend reported a DIFFERENT
/// mechanism for an Enforced cell than the ledger committed, the digest match fails.
/// This proves the digest binding is load-bearing (not a tautology).
#[test]
fn a_swapped_mechanism_is_rejected_by_the_digest_match() {
    let backend = LinuxBackend::with_cgroup_for_proof(true);
    let enforced = backend.proof_ceiling().enforced_kinds();
    // A liar digest fn that returns a digest of a DIFFERENT mechanism for Filesystem.
    let liar = |kind: RequirementKind| {
        if kind == RequirementKind::Filesystem {
            MechanismDigest::of_mechanism("linux:SWAPPED-UNPROVEN-MECHANISM:Enforced")
        } else {
            MechanismDigest::of_mechanism(&backend.proof_mechanism(kind, Enforcement::Enforced))
        }
    };
    let result = check_coupling(&enforced, &backend.proof_facts(), &liar);
    assert!(
        matches!(
            result,
            Err(CouplingViolation::MechanismDigestMismatch(
                RequirementKind::Filesystem
            ))
        ),
        "a swapped Filesystem mechanism must be rejected as a digest mismatch, got {result:?}"
    );
}

/// RED FIXTURE (ProductionFlip): plant an `Enforced` cell (`NetworkDenyAll`) that
/// has NO `Proven` ledger row (it is `FailClosed`), and assert the coupling check
/// PASSES — which is FALSE, because a biting gate returns `NotProven`. So the red
/// half FAILS, proving the gate is anti-vacuous. Under correct production the
/// ceiling never advertises NetworkDenyAll, so this scenario only arises by a
/// production flip.
#[cfg(gauntlet_red_fixture)]
#[test]
fn coupling_red_fixture_enforced_without_proven_row_must_fail() {
    let backend = LinuxBackend::with_cgroup_for_proof(true);
    // The PLANTED over-claim: NetworkDenyAll advertised Enforced, but its ledger row
    // is FailClosed (no Proven, no oracle) — exactly the over-claim the gate catches.
    let mut enforced = backend.proof_ceiling().enforced_kinds();
    enforced.push(RequirementKind::NetworkDenyAll);
    let result = check_coupling(&enforced, &backend.proof_facts(), &live_digest_fn(&backend));
    // A biting gate returns Err(NotProven(NetworkDenyAll, FailClosed)); asserting the
    // check PASSED is therefore FALSE — the red half FAILS, proving anti-vacuity.
    assert!(
        result.is_ok(),
        "RED FIXTURE: a biting coupling gate catches the planted Enforced-without-Proven \
         cell, so this assertion must FAIL"
    );
}
