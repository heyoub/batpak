//! Frozen v1 goldens for bvisor's three 0xE event payloads.
//!
//! PROVES: INV-EVENT-PAYLOAD-DECODE-BACKCOMPAT for `BoundaryStartedEvent`
//! (0xE/0x001), `BoundaryReportEvent` (0xE/0x002), `BoundaryRecoveryEvent`
//! (0xE/0x003), and `BoundaryDispositionEvent` (0xE/0x004) — their v1 on-disk
//! msgpack bytes still decode into the current structs through batpak's canonical
//! decode seam.
//! CATCHES: a contract-struct edit that silently breaks decode of historical
//! 0xE bytes; a canonical-encoding drift in the bvisor payload surface.
//! SEEDED: append-only `.hex` fixtures under batpak core's
//! `tests/golden/payloads/` — the SINGLE directory the
//! `ART-EVENT-PAYLOAD-FROZEN-GOLDENS` structural lint scans, named by the
//! payload's `(category, type_id)`: `<cat:x>_<type_id:03x>__v<N>.hex`.
//!
//! These payloads live in `bvisor`, which depends on `batpak`; `batpak` cannot
//! depend back on `bvisor`, so the FIXTURE BYTES live in core's golden tree
//! (where the lint looks) while the FROZEN-DECODE TEST lives here (where the
//! types are constructible). The instances are hand-built and fully
//! deterministic (no subprocess / no machine probe) so the frozen bytes are
//! stable and host-independent.
//!
//! Each payload is PROVISIONAL or FROZEN (see `PAYLOAD_MANIFEST`). A golden is a
//! development snapshot while PROVISIONAL ("today's canonical bytes — the contract
//! has not frozen") and an eternal decode promise once FROZEN ("we understand these
//! bytes indefinitely"). The bvisor `0xE` family is provisional while the
//! transitive durable closure of `BoundaryStartedEvent` (`BoundaryPlan` /
//! `BudgetRequirements` / `AdmittedBudgets` / the support, admission-program, and
//! lowering-schedule digests) is still being finalized.
//!
//! - PROVISIONAL: a missing fixture is allowed (the round-trip still proves the
//!   current shape); a drifted fixture is RE-FROZEN under `GOLDEN_UPDATE` (the diff
//!   shows old vs new bytes). The payload version does NOT advance.
//! - FROZEN: the fixture must exist and decode; it is NEVER overwritten — a new
//!   shape needs a `__vN+1` fixture + an exact upcast or a typed canonical refusal.
//!
//!   GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test -p bvisor --test frozen_goldens

use batpak::canonical;
use batpak::EventPayload;
use bvisor::{
    AdmittedRequirement, ArtifactId, AttemptId, BackendId, BackendProfileSnapshot,
    BoundaryDispositionEvent, BoundaryFinding, BoundaryPlan, BoundaryPlanHash,
    BoundaryRecoveryEvent, BoundaryReport, BoundaryReportBody, BoundaryReportEvent,
    BoundaryRequirement, BoundaryStartedEvent, BudgetRequirements, CaptureRefs, DispositionAction,
    DispositionPhase, Enforcement, EvidenceRequirements, ExitStatus, HostControl, ObservedFact,
    Outcome, QuarantineRecord, RecoveryClassification, Workload, BOUNDARY_PLAN_SCHEMA_VERSION,
    BOUNDARY_REPORT_SCHEMA_VERSION,
};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

// ─── Frozen-decode fixture helper ───────────────────────────────────────────

/// batpak core's golden payload directory — the ONLY tree the structural lint
/// (`ART-EVENT-PAYLOAD-FROZEN-GOLDENS`) scans for `<cat>_<type_id>__v*.hex`.
fn payloads_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../core/tests/golden/payloads")
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Vec<u8> {
    let s = s.trim();
    assert!(s.len().is_multiple_of(2), "odd-length hex fixture");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

/// The compatibility state of a payload golden — the distinction between a
/// development snapshot and an eternal decode promise.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PayloadState {
    /// A development snapshot: re-freezable while the transitive durable type
    /// closure is still evolving. A missing fixture is allowed (it is regenerated
    /// once the shape settles); a drifted fixture is RE-FROZEN, not version-bumped.
    Provisional,
    /// An eternal decode promise: immutable. A new shape needs a `__vN+1` fixture +
    /// an exact upcast or a typed canonical refusal — never an edit of this one.
    Frozen,
}

/// Provisional-vs-frozen manifest. A golden in the compatibility directory means
/// "we promise to understand these bytes indefinitely" ONLY once FROZEN; while
/// PROVISIONAL it means "today's canonical bytes — the contract has not frozen."
/// Flip an entry to FROZEN only at the declared release freeze.
///
/// bvisor's `0xE` family stays provisional until the seven-dimensional budget
/// model, the final admission surface, and the final plan-identity material are all
/// integrated. The transitive closure of `BoundaryStartedEvent` (its `BoundaryPlan`,
/// `BudgetRequirements`, `AdmittedBudgets`, and the support, admission-program, and
/// lowering-schedule digests) is still moving, so the outer event is NOT frozen.
const PAYLOAD_MANIFEST: &[(&str, PayloadState)] = &[
    ("e_001__v1.hex", PayloadState::Provisional),
    ("e_002__v1.hex", PayloadState::Provisional),
    ("e_003__v1.hex", PayloadState::Provisional),
    ("e_004__v1.hex", PayloadState::Provisional),
];

/// The declared state of a fixture; an UNDECLARED fixture is treated as `Frozen`
/// (the safe default — a payload must be explicitly declared provisional).
fn payload_state(fixture: &str) -> PayloadState {
    PAYLOAD_MANIFEST
        .iter()
        .find(|(name, _)| *name == fixture)
        .map_or(PayloadState::Frozen, |(_, state)| *state)
}

/// Prove the CURRENT shape of `expected` encodes and decodes round-trip.
fn assert_round_trip<T>(expected: &T) -> Result<(), String>
where
    T: EventPayload + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = canonical::to_bytes(expected).map_err(|e| format!("encode payload: {e}"))?;
    let decoded: T =
        canonical::from_bytes(&bytes).map_err(|e| format!("decode current payload: {e}"))?;
    if &decoded != expected {
        return Err("payload does not round-trip through canonical encode/decode".to_string());
    }
    Ok(())
}

/// Re-freeze (write/overwrite) a provisional golden with the current canonical bytes.
fn refreeze<T: EventPayload>(path: &std::path::Path, expected: &T) -> Result<(), String> {
    let bytes =
        canonical::to_bytes(expected).map_err(|e| format!("encode fixture payload: {e}"))?;
    std::fs::create_dir_all(payloads_dir()).map_err(|e| format!("create payloads dir: {e}"))?;
    std::fs::write(path, hex_encode(&bytes)).map_err(|e| format!("write fixture: {e}"))
}

/// Freeze/decode the v1 payload bytes for `expected`, honoring the PROVISIONAL vs
/// FROZEN manifest:
///
/// - FROZEN: the fixture must exist and DECODE into the current type (proving
///   `INV-EVENT-PAYLOAD-DECODE-BACKCOMPAT`); it is never edited.
/// - PROVISIONAL: a missing fixture is allowed (a snapshot mid-reshape; the
///   round-trip still proves the current shape); a drifted fixture is RE-FROZEN
///   under `GOLDEN_UPDATE` (the diff shows old vs new bytes), not version-bumped.
fn assert_frozen_decode<T>(fixture: &str, expected: &T) -> Result<(), String>
where
    T: EventPayload + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let state = payload_state(fixture);
    let path = payloads_dir().join(fixture);
    let updating = std::env::var("GOLDEN_UPDATE").as_deref() == Ok("I_KNOW_WHAT_IM_DOING");

    if !path.exists() {
        if updating {
            return refreeze(&path, expected);
        }
        return match state {
            PayloadState::Provisional => assert_round_trip(expected),
            PayloadState::Frozen => Err(format!(
                "frozen payload fixture {} not found. Create it with \
                 GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test -p bvisor --test frozen_goldens",
                path.display()
            )),
        };
    }

    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read fixture: {e}"))?;
    let bytes = hex_decode(&raw);
    let decoded = canonical::from_bytes::<T>(&bytes);
    if let Ok(value) = &decoded {
        if value == expected {
            return Ok(());
        }
    }
    // Mismatch or decode failure.
    match state {
        PayloadState::Provisional => {
            if updating {
                return refreeze(&path, expected);
            }
            Err(format!(
                "PROVISIONAL payload {fixture} drifted from its development snapshot. Re-freeze \
                 with GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING (the diff shows old vs new canonical \
                 bytes); the payload version does NOT advance while provisional."
            ))
        }
        PayloadState::Frozen => match decoded {
            Ok(_) => Err(format!(
                "SCHEMA DRIFT: frozen fixture {fixture} decoded to a different value. Bump the \
                 payload version, add an Upcast, and freeze a __vN+1 fixture — do not edit this one."
            )),
            Err(e) => Err(format!("frozen fixture {fixture} failed current decode: {e}")),
        },
    }
}

// ─── Deterministic, host-independent representative instances ────────────────

fn sample_backend() -> BackendId {
    BackendId::new("inert")
}

/// A raw probe snapshot with stable, hardcoded facts (no machine probing), so
/// the frozen bytes never vary by host.
fn sample_profile() -> BackendProfileSnapshot {
    let mut probed = BTreeMap::new();
    probed.insert("confinement".to_string(), "none".to_string());
    probed.insert("reference".to_string(), "inert".to_string());
    BackendProfileSnapshot {
        backend: sample_backend(),
        probed,
    }
}

fn launch_requirement() -> BoundaryRequirement {
    BoundaryRequirement::HostControl(HostControl::LaunchWorkload)
}

fn sample_plan() -> BoundaryPlan {
    BoundaryPlan {
        schema_version: BOUNDARY_PLAN_SCHEMA_VERSION,
        plan_id: BoundaryPlanHash([7u8; 32]),
        backend: sample_backend(),
        profile: sample_profile(),
        admitted: vec![AdmittedRequirement {
            requirement: launch_requirement(),
            enforcement: Enforcement::Enforced,
            mechanism: "none/no-confinement".to_string(),
        }],
        workload: Workload::Process {
            exe: "true".to_string(),
            args: Vec::new(),
        },
        budgets: BudgetRequirements::deny_all(),
        evidence: EvidenceRequirements::default(),
    }
}

fn sample_report_body() -> BoundaryReportBody {
    BoundaryReportBody {
        schema_version: BOUNDARY_REPORT_SCHEMA_VERSION,
        plan_id: BoundaryPlanHash([7u8; 32]),
        backend: sample_backend(),
        profile: sample_profile(),
        outcome: Outcome::Completed,
        admitted: vec![AdmittedRequirement {
            requirement: launch_requirement(),
            enforcement: Enforcement::Enforced,
            mechanism: "none/no-confinement".to_string(),
        }],
        observed: vec![ObservedFact {
            kind: "workload_launched".to_string(),
            detail: "inert spawned true (no confinement)".to_string(),
        }],
        denied: Vec::new(),
        exit: Some(ExitStatus::Code(0)),
        captured: CaptureRefs::default(),
        artifacts: Vec::new(),
        findings: vec![
            BoundaryFinding::RequirementAdmitted {
                requirement: launch_requirement(),
                enforcement: Enforcement::Enforced,
            },
            BoundaryFinding::NoConfinement {
                requirement: launch_requirement(),
            },
        ],
    }
}

fn sample_report() -> BoundaryReport {
    let body = sample_report_body();
    let body_hash = body.body_hash().expect("seal sample report body");
    BoundaryReport { body, body_hash }
}

// ─── Frozen-decode proofs ───────────────────────────────────────────────────

#[test]
fn boundary_started_event_v1_still_decodes() -> Result<(), String> {
    assert_frozen_decode::<BoundaryStartedEvent>(
        "e_001__v1.hex",
        &BoundaryStartedEvent {
            plan: sample_plan(),
        },
    )
}

#[test]
fn boundary_report_event_v1_still_decodes() -> Result<(), String> {
    assert_frozen_decode::<BoundaryReportEvent>(
        "e_002__v1.hex",
        &BoundaryReportEvent {
            report: sample_report(),
        },
    )
}

#[test]
fn boundary_recovery_event_v1_still_decodes() -> Result<(), String> {
    assert_frozen_decode::<BoundaryRecoveryEvent>(
        "e_003__v1.hex",
        &BoundaryRecoveryEvent {
            plan_id: BoundaryPlanHash([7u8; 32]),
            classification: RecoveryClassification::RolledBack,
            quarantined: vec![
                QuarantineRecord {
                    kind: "process".to_string(),
                    reference: "pidfd:inert-orphan".to_string(),
                },
                QuarantineRecord {
                    kind: "dir".to_string(),
                    reference: "quarantine/boundary-7".to_string(),
                },
            ],
        },
    )
}

#[test]
fn boundary_disposition_event_v1_still_decodes() -> Result<(), String> {
    assert_frozen_decode::<BoundaryDispositionEvent>(
        "e_004__v1.hex",
        &BoundaryDispositionEvent {
            plan_id: BoundaryPlanHash([7u8; 32]),
            attempt: AttemptId([3u8; 32]),
            artifact: ArtifactId([4u8; 32]),
            phase: DispositionPhase::Decided {
                action: DispositionAction::Promote,
            },
        },
    )
}

#[test]
fn frozen_payloads_are_v1() {
    assert_eq!(BoundaryStartedEvent::PAYLOAD_VERSION, 1);
    assert_eq!(BoundaryReportEvent::PAYLOAD_VERSION, 1);
    assert_eq!(BoundaryRecoveryEvent::PAYLOAD_VERSION, 1);
    assert_eq!(BoundaryDispositionEvent::PAYLOAD_VERSION, 1);
}
