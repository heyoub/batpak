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
//! Regeneration is APPEND-ONLY, mirroring core's `schema_evolution.rs`: under
//! `GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING` a MISSING fixture is written; an EXISTING
//! fixture is NEVER overwritten (bump the version and freeze `__vN+1` instead):
//!
//!   GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test -p bvisor --test frozen_goldens

use batpak::canonical;
use batpak::EventPayload;
use bvisor::{
    AdmittedRequirement, ArtifactId, AttemptId, BackendId, BackendProfileSnapshot,
    BoundaryDispositionEvent, BoundaryFinding, BoundaryPlan, BoundaryPlanHash,
    BoundaryRecoveryEvent, BoundaryReport, BoundaryReportBody, BoundaryReportEvent,
    BoundaryRequirement, BoundaryStartedEvent, Budgets, CaptureRefs, DispositionAction,
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

/// Freeze (append-only) and frozen-decode the v1 payload bytes for `expected`.
///
/// If the fixture is ABSENT it is written from the current canonical encoding of
/// `expected` (only under the `GOLDEN_UPDATE` sentinel); if it is PRESENT it is
/// read, decoded with the current decoder, and asserted equal to `expected` —
/// the real proof that the v1-on-disk bytes still decode into the current type.
fn assert_frozen_decode<T>(fixture: &str, expected: &T) -> Result<(), String>
where
    T: EventPayload + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let path = payloads_dir().join(fixture);
    let updating = std::env::var("GOLDEN_UPDATE").as_deref() == Ok("I_KNOW_WHAT_IM_DOING");

    if !path.exists() {
        if !updating {
            return Err(format!(
                "frozen payload fixture {} not found. To create it (append-only), run \
                 GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test -p bvisor --test frozen_goldens",
                path.display()
            ));
        }
        let bytes =
            canonical::to_bytes(expected).map_err(|e| format!("encode fixture payload: {e}"))?;
        std::fs::create_dir_all(payloads_dir()).map_err(|e| format!("create payloads dir: {e}"))?;
        std::fs::write(&path, hex_encode(&bytes))
            .map_err(|e| format!("write frozen fixture: {e}"))?;
        // GOLDEN_UPDATE append path: the new fixture file is the artifact; inspect
        // the diff before committing. (No stderr print — print_stderr is denied.)
        return Ok(());
    }

    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read frozen fixture: {e}"))?;
    let bytes = hex_decode(&raw);
    let decoded: T = canonical::from_bytes(&bytes)
        .map_err(|e| format!("frozen fixture {fixture} failed current decode: {e}"))?;
    if &decoded != expected {
        return Err(format!(
            "SCHEMA DRIFT: frozen fixture {fixture} decoded to a different value than expected. \
             If the change is intentional and non-additive, bump the payload version, add an \
             Upcast, and freeze a __vN+1 fixture — do not edit this one."
        ));
    }
    Ok(())
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
        budgets: Budgets::default(),
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
