//! The TEETH of S7 (proof-spine §5 D6): determinism + well-formedness of the
//! policy → BPF → digest pipeline. No install, no oracle, no Proven row — these
//! unit tests ARE the S7 guarantee. Collect-and-assert: `panic!` is DENIED even in
//! tests, so every check accumulates a finding and one `assert!` reports them.

use super::{DefaultAction, SeccompCompileError, SeccompPolicy, Syscall, SECCOMPILER_VERSION};
use crate::contract::seccomp_evidence::{SeccompActionKind, SeccompArch};

const ARCHES: [SeccompArch; 3] = [
    SeccompArch::X86_64,
    SeccompArch::Aarch64,
    SeccompArch::Riscv64,
];

/// A non-base extra syscall (`read`) for exercising allowlist extension.
fn extra_read() -> Syscall {
    Syscall::for_test("read", libc::SYS_read)
}

#[test]
fn compiled_bpf_is_deterministic_same_policy_same_bytes_same_digest() {
    let mut findings: Vec<String> = Vec::new();
    for arch in ARCHES {
        let p1 = SeccompPolicy::launcher_base(DefaultAction::KillProcess);
        let p2 = SeccompPolicy::launcher_base(DefaultAction::KillProcess);
        let (Ok(c1), Ok(c2)) = (p1.compile(arch), p2.compile(arch)) else {
            findings.push(format!("base policy failed to compile for {arch:?}"));
            continue;
        };
        if c1.bpf_bytes() != c2.bpf_bytes() {
            findings.push(format!("BPF bytes non-deterministic for {arch:?}"));
        }
        if c1.evidence().bpf_digest != c2.evidence().bpf_digest {
            findings.push(format!("bpf_digest non-deterministic for {arch:?}"));
        }
        if c1.evidence().policy_digest != c2.evidence().policy_digest {
            findings.push(format!("policy_digest non-deterministic for {arch:?}"));
        }
    }
    assert!(findings.is_empty(), "determinism: {findings:?}");
}

#[test]
fn distinct_policies_yield_distinct_policy_digests() {
    let mut findings: Vec<String> = Vec::new();
    let base_kill = SeccompPolicy::launcher_base(DefaultAction::KillProcess);
    let base_errno = SeccompPolicy::launcher_base(DefaultAction::Errno(1));
    let base_plus = base_kill.clone().allow(extra_read());
    let base_errno2 = SeccompPolicy::launcher_base(DefaultAction::Errno(2));

    match (
        base_kill.policy_digest(),
        base_errno.policy_digest(),
        base_plus.policy_digest(),
        base_errno2.policy_digest(),
    ) {
        (Ok(d_kill), Ok(d_errno), Ok(d_plus), Ok(d_errno2)) => {
            // Different deny floor ⇒ different policy.
            if d_kill == d_errno {
                findings.push("kill vs errno default collapsed to one policy digest".into());
            }
            // Different allowlist ⇒ different policy.
            if d_kill == d_plus {
                findings.push("base vs base+read collapsed to one policy digest".into());
            }
            // Different errno values ⇒ different policy.
            if d_errno == d_errno2 {
                findings.push("distinct deny errnos collapsed to one policy digest".into());
            }
        }
        _ => findings.push("a policy_digest failed to compute".into()),
    }
    assert!(findings.is_empty(), "distinct policies: {findings:?}");
}

#[test]
fn policy_digest_is_input_order_and_dup_independent() {
    let read = extra_read();
    let p1 = SeccompPolicy::launcher_base(DefaultAction::KillProcess).allow(read);
    // Re-allowing the same syscall (and a base syscall) must be idempotent.
    let p2 = SeccompPolicy::launcher_base(DefaultAction::KillProcess)
        .allow(read)
        .allow(read);
    let mut findings: Vec<String> = Vec::new();
    match (p1.policy_digest(), p2.policy_digest()) {
        (Ok(a), Ok(b)) if a == b => {}
        (Ok(_), Ok(_)) => findings.push("policy digest is NOT order/dup independent".into()),
        _ => findings.push("order-independence digests failed to compute".into()),
    }
    assert!(findings.is_empty(), "order/dup independence: {findings:?}");
}

#[test]
fn mandatory_base_is_always_present_in_a_compiled_filter() {
    let mut findings: Vec<String> = Vec::new();
    for arch in ARCHES {
        let policy = SeccompPolicy::launcher_base(DefaultAction::KillProcess);
        let Ok(compiled) = policy.compile(arch) else {
            findings.push(format!("base did not compile for {arch:?}"));
            continue;
        };
        let nrs: Vec<i64> = SeccompPolicy::mandatory_base()
            .iter()
            .map(|s| s.number())
            .collect();
        let allowed: Vec<i64> = policy.allowlist().map(|s| s.number()).collect();
        for base_nr in &nrs {
            if !allowed.contains(base_nr) {
                findings.push(format!(
                    "base nr {base_nr} missing from allowlist ({arch:?})"
                ));
            }
            // The compiled program must reference the base number in a comparison.
            let k = u32::try_from(*base_nr).unwrap_or(u32::MAX);
            if !compiled.program().iter().any(|insn| insn.k == k) {
                findings.push(format!(
                    "base nr {base_nr} not compiled into BPF ({arch:?})"
                ));
            }
        }
    }
    assert!(findings.is_empty(), "base allowlist: {findings:?}");
}

#[test]
fn a_policy_that_would_deny_execve_is_rejected_at_build() {
    // Fail-closed: a launcher filter must allow execve. A policy whose allowlist
    // omits execve must be REJECTED by compile() — you cannot build a filter that
    // traps its own exec.
    let mut findings: Vec<String> = Vec::new();
    let no_execve =
        SeccompPolicy::launcher_base(DefaultAction::KillProcess).without_for_test("execve");

    for arch in ARCHES {
        match no_execve.compile(arch) {
            Ok(_) => findings.push(format!(
                "compile() accepted an execve-denying policy for {arch:?} (must fail closed)"
            )),
            Err(SeccompCompileError::MissingMandatoryBase { syscall }) => {
                if syscall != "execve" {
                    findings.push(format!("rejected for wrong syscall {syscall} ({arch:?})"));
                }
            }
            Err(other) => {
                findings.push(format!(
                    "rejected with unexpected error {other:?} ({arch:?})"
                ));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "fail-closed on deny-execve: {findings:?}"
    );
}

#[test]
fn empty_allowlist_is_rejected() {
    let empty = SeccompPolicy::empty_for_test(DefaultAction::KillProcess);
    let mut findings: Vec<String> = Vec::new();
    for arch in ARCHES {
        if !matches!(
            empty.compile(arch),
            Err(SeccompCompileError::EmptyAllowlist)
        ) {
            findings.push(format!("empty allowlist not rejected for {arch:?}"));
        }
    }
    assert!(findings.is_empty(), "empty allowlist: {findings:?}");
}

#[test]
fn evidence_records_build_time_facts_and_no_install_mode() {
    let mut findings: Vec<String> = Vec::new();
    for arch in ARCHES {
        let policy = SeccompPolicy::launcher_base(DefaultAction::Errno(1));
        let Ok(compiled) = policy.compile(arch) else {
            findings.push(format!("compile failed {arch:?}"));
            continue;
        };
        let ev = compiled.evidence();
        if ev.target_arch != arch {
            findings.push(format!("evidence arch mismatch {arch:?}"));
        }
        if ev.seccompiler_version != SECCOMPILER_VERSION {
            findings.push("evidence seccompiler version drift".into());
        }
        if ev.observed_installed_mode.is_some() {
            findings.push(format!("S7 leaked an install mode for {arch:?}"));
        }
        if !ev.action_profile.contains(&SeccompActionKind::Allow) {
            findings.push(format!("action profile missing Allow {arch:?}"));
        }
        if !ev.action_profile.contains(&SeccompActionKind::Errno(1)) {
            findings.push(format!("action profile missing deny floor {arch:?}"));
        }
    }
    assert!(findings.is_empty(), "evidence facts: {findings:?}");
}

#[test]
fn different_arches_produce_different_bpf_but_same_policy_digest() {
    // The arch-audit preamble differs per arch ⇒ different BPF bytes; the policy
    // digest is arch-independent (keyed by syscall NAME) ⇒ identical.
    let policy = SeccompPolicy::launcher_base(DefaultAction::KillProcess);
    let mut findings: Vec<String> = Vec::new();
    match (
        policy.compile(SeccompArch::X86_64),
        policy.compile(SeccompArch::Aarch64),
    ) {
        (Ok(x), Ok(a)) => {
            if x.evidence().bpf_digest == a.evidence().bpf_digest {
                findings.push("arch-distinct BPF collapsed to one bpf_digest".into());
            }
            if x.evidence().policy_digest != a.evidence().policy_digest {
                findings.push("policy_digest should be arch-independent".into());
            }
        }
        _ => findings.push("an arch compile failed".into()),
    }
    assert!(findings.is_empty(), "arch distinctness: {findings:?}");
}
