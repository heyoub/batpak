// FROZEN host↔launcher Linux wire-protocol tests (kernel plan §10.8). The protocol
// is PURE library types (no OS code), so this compiles on ANY host with the feature
// — no `target_os` gate. It exercises ONLY the public protocol surface.
#![cfg(feature = "backend-linux")]
//! Wire-protocol conformance: canonical round-trip, a frozen golden vector that
//! catches silent wire drift, the envelope red fixtures (each typed reject), the
//! descriptor-table structural checks, and the launcher status state machine
//! (monotone-forward, fail-closed terminals, exec gate, outcome mapping).

use bvisor::linux::protocol::{
    can_exec, confinement_installed, frame, is_valid_transition, outcome_class, parse_and_verify,
    phase_resolution_consistent, ready_to_exec, validate_table, DescriptorKind, DescriptorRole,
    DescriptorShape, DescriptorSlotV1, EnvelopeReject, LauncherState, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, PhaseResult, SetupPhase, TableReject,
    TargetSpecV1, HEADER_LEN,
};
use bvisor::{
    compile_schedule, AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash,
    Outcome,
};

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn sample_table() -> Vec<DescriptorSlotV1> {
    vec![
        DescriptorSlotV1 {
            slot_index: 0,
            role: DescriptorRole::TargetExe,
            expected: DescriptorShape {
                kind: DescriptorKind::Regular,
                writable: false,
            },
        },
        DescriptorSlotV1 {
            slot_index: 1,
            role: DescriptorRole::ReadRoot,
            expected: DescriptorShape {
                kind: DescriptorKind::Directory,
                writable: false,
            },
        },
        DescriptorSlotV1 {
            slot_index: 2,
            role: DescriptorRole::Stdout,
            expected: DescriptorShape {
                kind: DescriptorKind::Pipe,
                writable: true,
            },
        },
    ]
}

/// A fixed, representative body. Empty lowering (no compiled primitives) keeps the
/// golden vector stable and dependency-free.
fn sample_body() -> LinuxLaunchBodyV1 {
    let schedule = compile_schedule(&[]).expect("empty schedule is valid");
    LinuxLaunchBodyV1 {
        attempt_id: AttemptId([7u8; 32]),
        plan_id: BoundaryPlanHash([1u8; 32]),
        h_a: AdmissionProgramHash([2u8; 32]),
        h_p: BackendProfileHash([3u8; 32]),
        h_l: schedule.digest().expect("H_L"),
        lowering: LoweringWireV1::from_schedule(&schedule),
        descriptor_table: sample_table(),
        target: TargetSpecV1 {
            argv: vec!["prog".to_owned()],
            envp: vec![("K".to_owned(), "V".to_owned())],
            exe_slot: 0,
            user_namespace: None,
            network_namespace: None,
        },
    }
}

/// The eleven launcher states, terminals last. Mirrors the canonical order; the
/// public progression accessor supplies the ten non-terminals.
fn all_states() -> Vec<LauncherState> {
    let mut v = LauncherState::non_terminal_progression().to_vec();
    v.push(LauncherState::ExecSucceeded);
    v.push(LauncherState::SetupRefused);
    v.push(LauncherState::SetupFaulted);
    v
}

fn hex_of(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ── Canonical round-trip ──────────────────────────────────────────────────────

#[test]
fn plan_round_trips_through_encode_decode() {
    let plan = LinuxLaunchPlanV1 {
        body: sample_body(),
    };
    let bytes = plan.encode().expect("encode");
    let back = LinuxLaunchPlanV1::decode(&bytes).expect("decode");
    assert_eq!(plan, back, "encode∘decode is identity");
}

// ── Frozen golden vector ──────────────────────────────────────────────────────

/// The frozen canonical bytes of [`sample_body`] (the BODY, not the framed plan —
/// independent of the envelope digest field). If the wire format drifts, this
/// fails. To regenerate INTENTIONALLY: print the hex and replace this literal with
/// a justification.
const GOLDEN_BODY_HEX: &str = "88aa617474656d70745f6964dc00200707070707070707070707070707070707070707070707070707070707070707a7706c616e5f6964dc00200101010101010101010101010101010101010101010101010101010101010101a3685f61dc00200202020202020202020202020202020202020202020202020202020202020202a3685f70dc00200303030303030303030303030303030303030303030303030303030303030303a3685f6cdc0020ccb51dccd6cce8ccee23ccc64344ccf4cc9bcce5193ecce5ccc40669ccbb54ccfb1f6f75ccb2ccb92f137b542a19a86c6f776572696e6781a7656e747269657390b064657363726970746f725f7461626c659383aa736c6f745f696e64657800a4726f6c65a9546172676574457865a8657870656374656482a46b696e64a7526567756c6172a87772697461626c65c283aa736c6f745f696e64657801a4726f6c65a852656164526f6f74a8657870656374656482a46b696e64a94469726563746f7279a87772697461626c65c283aa736c6f745f696e64657802a4726f6c65a65374646f7574a8657870656374656482a46b696e64a450697065a87772697461626c65c3a674617267657483a46172677691a470726f67a4656e76709192a14ba156a86578655f736c6f7400";

#[test]
fn golden_body_vector_is_frozen() {
    let body = sample_body();
    let bytes = batpak::canonical::to_bytes(&body).expect("encode body");
    assert_eq!(
        hex_of(&bytes),
        GOLDEN_BODY_HEX,
        "canonical wire format drifted; regenerate ONLY intentionally"
    );
}

// ── Envelope red fixtures ──────────────────────────────────────────────────────

#[test]
fn envelope_truncated_below_header_rejects() {
    let short = [0u8; HEADER_LEN - 1];
    assert_eq!(parse_and_verify(&short), Err(EnvelopeReject::Truncated));
}

#[test]
fn envelope_bad_magic_rejects() {
    let mut framed = frame(b"hello");
    framed[0] ^= 0xff;
    assert_eq!(parse_and_verify(&framed), Err(EnvelopeReject::BadMagic));
}

#[test]
fn envelope_unsupported_version_rejects() {
    let mut framed = frame(b"hello");
    // proto_version is at bytes 8..10 (LE). Bump it past PROTO_VERSION.
    framed[8] = 0xff;
    framed[9] = 0xff;
    assert_eq!(
        parse_and_verify(&framed),
        Err(EnvelopeReject::UnsupportedVersion { found: 0xffff })
    );
}

#[test]
fn envelope_length_mismatch_rejects() {
    let mut framed = frame(b"hello");
    framed.push(0x00); // trailing byte → total ≠ header + body_len
    assert_eq!(
        parse_and_verify(&framed),
        Err(EnvelopeReject::LengthMismatch)
    );
}

#[test]
fn envelope_digest_mismatch_rejects() {
    let mut framed = frame(b"hello");
    let last = framed.len() - 1;
    framed[last] ^= 0x01; // flip a body byte WITHOUT updating the header digest
    assert_eq!(
        parse_and_verify(&framed),
        Err(EnvelopeReject::DigestMismatch)
    );
}

#[test]
fn envelope_well_formed_returns_body() {
    let framed = frame(b"payload");
    assert_eq!(parse_and_verify(&framed), Ok(&b"payload"[..]));
}

// ── Descriptor table validation ────────────────────────────────────────────────

#[test]
fn validate_table_accepts_well_formed() {
    assert_eq!(validate_table(&sample_table()), Ok(()));
}

#[test]
fn validate_table_rejects_duplicate_slot_index() {
    let mut table = sample_table();
    table[1].slot_index = 0; // collide with the exe slot
    assert_eq!(
        validate_table(&table),
        Err(TableReject::DuplicateSlotIndex { slot_index: 0 })
    );
}

#[test]
fn validate_table_rejects_two_stdout() {
    let mut table = sample_table();
    table.push(DescriptorSlotV1 {
        slot_index: 3,
        role: DescriptorRole::Stdout,
        expected: DescriptorShape {
            kind: DescriptorKind::Pipe,
            writable: true,
        },
    });
    assert_eq!(
        validate_table(&table),
        Err(TableReject::DuplicateSingletonRole {
            role: DescriptorRole::Stdout
        })
    );
}

#[test]
fn validate_table_allows_multiple_read_roots() {
    let mut table = sample_table();
    table.push(DescriptorSlotV1 {
        slot_index: 9,
        role: DescriptorRole::ReadRoot,
        expected: DescriptorShape {
            kind: DescriptorKind::Directory,
            writable: false,
        },
    });
    assert_eq!(validate_table(&table), Ok(()), "roots are not singletons");
}

// ── State machine ──────────────────────────────────────────────────────────────

#[test]
fn every_legal_forward_step_is_valid() {
    for pair in LauncherState::non_terminal_progression().windows(2) {
        assert!(
            is_valid_transition(pair[0], pair[1]),
            "{:?} -> {:?} must be legal",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn ready_to_exec_reaches_exec_succeeded() {
    assert!(is_valid_transition(
        LauncherState::ReadyToExec,
        LauncherState::ExecSucceeded
    ));
}

#[test]
fn refusal_and_fault_reachable_from_any_nonterminal() {
    for s in LauncherState::non_terminal_progression() {
        assert!(is_valid_transition(*s, LauncherState::SetupRefused));
        assert!(is_valid_transition(*s, LauncherState::SetupFaulted));
    }
}

#[test]
fn skipping_a_step_is_rejected() {
    assert!(
        !is_valid_transition(LauncherState::LauncherStarted, LauncherState::ReadyToExec),
        "no skipping forward"
    );
}

#[test]
fn going_backwards_is_rejected() {
    assert!(
        !is_valid_transition(
            LauncherState::ConfinementPhaseResolved,
            LauncherState::ChildCreated
        ),
        "no going back"
    );
}

#[test]
fn exec_succeeded_only_from_ready_to_exec() {
    assert!(
        !is_valid_transition(
            LauncherState::ConfinementPhaseResolved,
            LauncherState::ExecSucceeded
        ),
        "exec only from ReadyToExec"
    );
}

#[test]
fn terminals_are_absorbing() {
    for term in [
        LauncherState::ExecSucceeded,
        LauncherState::SetupRefused,
        LauncherState::SetupFaulted,
    ] {
        for to in all_states() {
            assert!(!is_valid_transition(term, to), "{term:?} is absorbing");
        }
        assert!(!is_valid_transition(term, term), "no terminal self-loop");
    }
}

#[test]
fn can_exec_only_in_ready_to_exec() {
    for s in all_states() {
        assert_eq!(can_exec(s), s == LauncherState::ReadyToExec);
    }
}

#[test]
fn terminal_outcome_class_mapping_is_correct() {
    assert_eq!(
        outcome_class(LauncherState::ExecSucceeded),
        Some(Outcome::Completed)
    );
    assert_eq!(
        outcome_class(LauncherState::SetupRefused),
        Some(Outcome::Unsupported)
    );
    assert_eq!(
        outcome_class(LauncherState::SetupFaulted),
        Some(Outcome::SupervisorFault)
    );
    assert_eq!(outcome_class(LauncherState::ReadyToExec), None);
}

// ── Phase-honesty: fixtures ─────────────────────────────────────────────────────

/// One representative scheduled/observed lowering entry.
fn wire_entry(id: &str, version: u32) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version,
        phase_code: 3,
        param_digest: [0x11; 32],
        decl_digest: [0x22; 32],
    }
}

/// A nonempty scheduled phase (two entries, in order).
fn nonempty_phase() -> Vec<LoweringWireEntryV1> {
    vec![wire_entry("linux.a.v1", 1), wire_entry("linux.b.v1", 1)]
}

/// `resolved_state` maps each phase to its own `*PhaseResolved` checkpoint.
#[test]
fn setup_phase_maps_to_its_resolved_state() {
    assert_eq!(
        SetupPhase::Identity.resolved_state(),
        LauncherState::IdentityPhaseResolved
    );
    assert_eq!(
        SetupPhase::Visibility.resolved_state(),
        LauncherState::VisibilityPhaseResolved
    );
    assert_eq!(
        SetupPhase::AmbientAuthority.resolved_state(),
        LauncherState::AmbientAuthorityPhaseResolved
    );
    assert_eq!(
        SetupPhase::Confinement.resolved_state(),
        LauncherState::ConfinementPhaseResolved
    );
}

// ── Phase-honesty: phase_resolution_consistent (RED fixtures) ───────────────────

/// NotRequired is honest ONLY when nothing was scheduled and nothing observed.
#[test]
fn not_required_on_empty_phase_passes() {
    assert!(phase_resolution_consistent(
        &[],
        &[],
        PhaseResult::NotRequired
    ));
}

/// RED: NotRequired on a NONEMPTY scheduled phase is an under-claim and FAILS.
#[test]
fn not_required_on_nonempty_phase_fails() {
    assert!(
        !phase_resolution_consistent(&nonempty_phase(), &[], PhaseResult::NotRequired),
        "a scheduled phase may never resolve NotRequired"
    );
}

/// RED: NotRequired while claiming observed actions FAILS.
#[test]
fn not_required_with_observed_actions_fails() {
    assert!(
        !phase_resolution_consistent(&[], &nonempty_phase(), PhaseResult::NotRequired),
        "NotRequired may not have observed actions"
    );
}

/// Applied is honest when scheduled is nonempty and observed equals it exactly.
#[test]
fn applied_with_exact_observed_passes() {
    let sched = nonempty_phase();
    let obs = nonempty_phase();
    assert!(phase_resolution_consistent(
        &sched,
        &obs,
        PhaseResult::Applied
    ));
}

/// RED: Applied on an EMPTY phase is an over-claim and FAILS.
#[test]
fn applied_on_empty_phase_fails() {
    assert!(
        !phase_resolution_consistent(&[], &[], PhaseResult::Applied),
        "an empty phase may never resolve Applied (over-claim)"
    );
}

/// RED: Applied with FEWER observed than scheduled (count mismatch) FAILS.
#[test]
fn applied_with_count_mismatch_fails() {
    let sched = nonempty_phase();
    let obs = vec![wire_entry("linux.a.v1", 1)]; // dropped one
    assert!(
        !phase_resolution_consistent(&sched, &obs, PhaseResult::Applied),
        "count divergence is an over-claim"
    );
}

/// RED: Applied with a differing id FAILS.
#[test]
fn applied_with_id_mismatch_fails() {
    let sched = nonempty_phase();
    let mut obs = nonempty_phase();
    obs[1].id = "linux.z.v1".to_owned();
    assert!(
        !phase_resolution_consistent(&sched, &obs, PhaseResult::Applied),
        "id divergence is an over-claim"
    );
}

/// RED: Applied with a differing version FAILS.
#[test]
fn applied_with_version_mismatch_fails() {
    let sched = nonempty_phase();
    let mut obs = nonempty_phase();
    obs[0].version = 99;
    assert!(
        !phase_resolution_consistent(&sched, &obs, PhaseResult::Applied),
        "version divergence is an over-claim"
    );
}

/// RED: Applied with a differing param_digest FAILS.
#[test]
fn applied_with_param_digest_mismatch_fails() {
    let sched = nonempty_phase();
    let mut obs = nonempty_phase();
    obs[0].param_digest = [0xff; 32];
    assert!(
        !phase_resolution_consistent(&sched, &obs, PhaseResult::Applied),
        "param divergence is an over-claim"
    );
}

/// RED: Applied with the same entries REORDERED FAILS (order is load-bearing).
#[test]
fn applied_with_reordered_observed_fails() {
    let sched = nonempty_phase();
    let mut obs = nonempty_phase();
    obs.reverse();
    assert!(
        !phase_resolution_consistent(&sched, &obs, PhaseResult::Applied),
        "reordering is divergence (the schedule order is canonical)"
    );
}

/// Refused / Faulted assert no action parity — structurally consistent regardless.
#[test]
fn refused_and_faulted_are_structurally_consistent() {
    assert!(phase_resolution_consistent(
        &nonempty_phase(),
        &[],
        PhaseResult::Refused
    ));
    assert!(phase_resolution_consistent(&[], &[], PhaseResult::Faulted));
}

// ── Phase-honesty: confinement_installed (DERIVED evidence) ─────────────────────

/// Empty confinement schedule can NEVER be an install — for every result, incl.
/// NotRequired (the exec-only-plan over-claim the rename exists to kill).
#[test]
fn confinement_installed_false_on_empty_schedule() {
    for result in [
        PhaseResult::Applied,
        PhaseResult::NotRequired,
        PhaseResult::Refused,
        PhaseResult::Faulted,
    ] {
        assert!(
            !confinement_installed(0, result),
            "empty confinement schedule is never an install ({result:?})"
        );
    }
}

/// Nonempty schedule + Applied ⇒ confinement actually installed.
#[test]
fn confinement_installed_true_on_nonempty_applied() {
    assert!(confinement_installed(2, PhaseResult::Applied));
}

/// Nonempty schedule but a non-Applied result is NOT an install.
#[test]
fn confinement_installed_false_on_nonempty_non_applied() {
    for result in [
        PhaseResult::NotRequired,
        PhaseResult::Refused,
        PhaseResult::Faulted,
    ] {
        assert!(
            !confinement_installed(2, result),
            "non-Applied is never an install ({result:?})"
        );
    }
}

// ── Phase-honesty: ready_to_exec gate ───────────────────────────────────────────

const H_L_FIX: [u8; 32] = [0x44; 32];

/// All phases resolved Applied/NotRequired, Ambient Applied, child created, digest
/// bound. Helper builds the happy four-phase array.
fn happy_phases() -> [(SetupPhase, PhaseResult); 4] {
    [
        (SetupPhase::Identity, PhaseResult::NotRequired),
        (SetupPhase::Visibility, PhaseResult::NotRequired),
        (SetupPhase::AmbientAuthority, PhaseResult::Applied),
        (SetupPhase::Confinement, PhaseResult::NotRequired),
    ]
}

#[test]
fn ready_to_exec_happy_case_passes() {
    assert!(ready_to_exec(true, happy_phases(), H_L_FIX, H_L_FIX));
}

/// RED: no child created ⇒ never ready.
#[test]
fn ready_to_exec_false_without_child() {
    assert!(!ready_to_exec(false, happy_phases(), H_L_FIX, H_L_FIX));
}

/// RED: any phase Refused ⇒ never ready (fail-closed deny short-circuits exec).
#[test]
fn ready_to_exec_false_when_a_phase_refused() {
    let mut phases = happy_phases();
    phases[1].1 = PhaseResult::Refused;
    assert!(!ready_to_exec(true, phases, H_L_FIX, H_L_FIX));
}

/// RED: any phase Faulted ⇒ never ready.
#[test]
fn ready_to_exec_false_when_a_phase_faulted() {
    let mut phases = happy_phases();
    phases[0].1 = PhaseResult::Faulted;
    assert!(!ready_to_exec(true, phases, H_L_FIX, H_L_FIX));
}

/// RED: Ambient resolving NotRequired violates the MANDATORY scrub ⇒ not ready.
#[test]
fn ready_to_exec_false_when_ambient_not_required() {
    let mut phases = happy_phases();
    phases[2].1 = PhaseResult::NotRequired;
    assert!(
        !ready_to_exec(true, phases, H_L_FIX, H_L_FIX),
        "the ambient-authority scrub is mandatory"
    );
}

/// RED: observed schedule digest ≠ bound h_l ⇒ not ready (schedule drift).
#[test]
fn ready_to_exec_false_on_digest_drift() {
    assert!(!ready_to_exec(true, happy_phases(), [0x55; 32], H_L_FIX));
}
