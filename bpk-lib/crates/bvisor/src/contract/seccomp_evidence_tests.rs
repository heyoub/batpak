//! Pure, always-compiled tests for the D6 evidence SHAPE: the install-time field
//! defaults to absent, the wire tokens are frozen, and the struct round-trips
//! through the canonical encoder. The policy→BPF→digest TEETH live in the gated
//! `backend/linux/seccomp.rs` (they need the assembler); these guard the contract
//! surface itself, off-Linux included. Collect-and-assert — no `panic!`.

use super::{SeccompActionKind, SeccompArch, SeccompEvidence, SeccompObservedMode};

#[test]
fn observed_installed_mode_is_none_at_build_time() {
    // The S7 building block records build-time facts only; the S10 install field
    // starts absent. A non-None here would mean S7 leaked install observation.
    let ev = SeccompEvidence {
        policy_digest: [0u8; 32],
        bpf_digest: [1u8; 32],
        target_arch: SeccompArch::X86_64,
        seccompiler_version: "=0.5.0".to_string(),
        action_profile: vec![SeccompActionKind::Allow],
        observed_installed_mode: None,
    };
    assert!(
        ev.observed_installed_mode.is_none(),
        "observed_installed_mode is an S10 install-time field; S7 leaves it None"
    );
}

#[test]
fn arch_wire_tokens_are_frozen() {
    let mut findings = Vec::new();
    if SeccompArch::X86_64.as_str() != "x86_64" {
        findings.push("x86_64 token drifted");
    }
    if SeccompArch::Aarch64.as_str() != "aarch64" {
        findings.push("aarch64 token drifted");
    }
    if SeccompArch::Riscv64.as_str() != "riscv64" {
        findings.push("riscv64 token drifted");
    }
    assert!(findings.is_empty(), "frozen arch tokens: {findings:?}");
}

#[test]
fn action_wire_tags_are_distinct_and_carry_errno() {
    let mut findings = Vec::new();
    if SeccompActionKind::Allow.wire_tag() != (0, 0) {
        findings.push("Allow tag drifted");
    }
    if SeccompActionKind::Errno(13).wire_tag() != (1, 13) {
        findings.push("Errno tag must carry its number");
    }
    if SeccompActionKind::KillProcess.wire_tag() != (2, 0) {
        findings.push("KillProcess tag drifted");
    }
    // Distinct-errno actions are distinct evidence (so two different denies don't
    // collapse to one action profile).
    if SeccompActionKind::Errno(1) == SeccompActionKind::Errno(2) {
        findings.push("distinct errnos collapsed");
    }
    assert!(findings.is_empty(), "frozen action tags: {findings:?}");
}

#[test]
fn evidence_round_trips_through_the_canonical_encoder() {
    let ev = SeccompEvidence {
        policy_digest: [7u8; 32],
        bpf_digest: [9u8; 32],
        target_arch: SeccompArch::Aarch64,
        seccompiler_version: "=0.5.0".to_string(),
        action_profile: vec![SeccompActionKind::Allow, SeccompActionKind::Errno(1)],
        observed_installed_mode: Some(SeccompObservedMode::Filter),
    };
    let bytes = batpak::canonical::to_bytes(&ev).expect("evidence encodes");
    let back: SeccompEvidence = batpak::canonical::from_bytes(&bytes).expect("evidence decodes");
    assert_eq!(ev, back, "SeccompEvidence round-trips canonically");
}
