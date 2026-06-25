//! The D6 seccomp POLICY MODEL → BPF (proof-spine §5 D6, task S7).
//!
//! S7 SCAFFOLDING ONLY: this module defines a Rust [`SeccompPolicy`] (NOT JSON), the
//! mandatory launcher allowlist, and the deterministic [`SeccompPolicy::compile`]
//! that assembles the policy into a BPF program via seccompiler's Rust API
//! ([`seccompiler::SeccompFilter`] → [`seccompiler::BpfProgram`]) and binds its
//! identity into a [`SeccompEvidence`]. There is NO install here — `seccomp(2)` /
//! `prctl` / `NO_NEW_PRIVS` / the install ordering / the `/proc/<pid>/status`
//! observed-mode read are ALL S10 (the unsafe `sys.rs` basement). S7 mints no Proven
//! ledger row: its teeth are the determinism + well-formedness tests at the foot of
//! this module.
//!
//! ## Why a default-DENY ALLOWLIST (and not per-syscall mixed terminals)
//! seccompiler assembles ONE terminal `match_action` for every syscall present in a
//! filter plus ONE `mismatch_action` default. A launcher seccomp filter is therefore
//! a default-DENY allowlist: deny (errno / kill-process) everything, ALLOW exactly
//! the declared syscalls. The model exposes the deny floor as [`DefaultAction`] and
//! the allowlist as a sorted set of [`Syscall`]s; the policy can never advertise a
//! default of `Allow` (a permit-all filter is not a confinement) — that is rejected
//! at build.
//!
//! ## The mandatory base (D6, non-negotiable)
//! A launcher filter MUST always permit `execve` / `execveat` / `write` /
//! `exit_group`, so the post-filter exec survives and an error can still be reported.
//! [`SeccompPolicy::launcher_base`] encodes this base, and [`SeccompPolicy::compile`]
//! FAILS CLOSED if the resulting allowlist omits any base syscall — you cannot build
//! a filter that traps its own exec.
//!
//! ## No `unsafe`
//! Everything here is safe Rust: seccompiler's `compile()` (`TryFrom<SeccompFilter>
//! for BpfProgram`) is a pure assembler. Only S10's install touches a syscall.

use crate::contract::seccomp_evidence::{SeccompActionKind, SeccompArch, SeccompEvidence};
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};
use std::collections::{BTreeMap, BTreeSet};

/// The PINNED seccompiler version (must mirror `Cargo.toml`'s `=0.5.0`). Recorded in
/// every [`SeccompEvidence`] so a future assembler bump that changes the BPF bytes is
/// visible in the evidence rather than silent.
pub const SECCOMPILER_VERSION: &str = "=0.5.0";

/// Domain separator for the canonical seccomp-policy digest. Frozen.
const POLICY_DIGEST_DOMAIN: &str = "bvisor.seccomp-policy.v1";

/// A single syscall identified by its STABLE name + its arch-resolved number.
///
/// The number is resolved from `libc::SYS_*` for the build target, so the same
/// policy keyed by name compiles to the arch-correct BPF. Two syscalls with the same
/// name+number are equal; the name feeds the canonical policy digest (stable across
/// arches), the number feeds the assembler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Syscall {
    name: &'static str,
    nr: i64,
}

impl Syscall {
    /// Build a syscall handle from a frozen name + its arch-resolved number.
    #[must_use]
    const fn new(name: &'static str, nr: i64) -> Self {
        Self { name, nr }
    }

    /// The stable syscall name (digest input, arch-independent).
    #[must_use]
    pub fn name(self) -> &'static str {
        self.name
    }

    /// The arch-resolved syscall number (assembler input).
    #[must_use]
    pub fn number(self) -> i64 {
        self.nr
    }

    /// Test-only handle constructor (the production ctor is crate-private and only
    /// resolves the frozen base set). Lets the teeth tests build extra/forged
    /// allowlist entries without exposing a public ctor.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_test(name: &'static str, nr: i64) -> Self {
        Self::new(name, nr)
    }
}

/// The DENY FLOOR a default-deny launcher filter falls through to. Never `Allow` —
/// a permit-all default is not a confinement and is rejected at build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DefaultAction {
    /// Fail every non-allowed syscall with this errno (a soft, observable deny —
    /// e.g. `EPERM`/`ENOSYS`). The errno is part of the policy identity.
    Errno(u32),
    /// KILL the calling process on any non-allowed syscall (the hardest deny).
    KillProcess,
}

impl DefaultAction {
    fn to_seccomp(self) -> SeccompAction {
        match self {
            Self::Errno(e) => SeccompAction::Errno(e),
            Self::KillProcess => SeccompAction::KillProcess,
        }
    }

    fn to_kind(self) -> SeccompActionKind {
        match self {
            Self::Errno(e) => SeccompActionKind::Errno(e),
            Self::KillProcess => SeccompActionKind::KillProcess,
        }
    }
}

/// Why a [`SeccompPolicy`] could not be compiled to a BPF filter. FAIL-CLOSED: any
/// inconsistency aborts rather than emitting a filter that mis-confines.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SeccompCompileError {
    /// The allowlist omits a mandatory base syscall (`execve`/`execveat`/`write`/
    /// `exit_group`) — building this filter would trap the launcher's own exec or
    /// its error reporting. The missing base syscall name is named.
    MissingMandatoryBase {
        /// The base syscall the allowlist failed to include.
        syscall: &'static str,
    },
    /// The allowlist is empty — a filter with no allowed syscall denies even the
    /// mandatory base and could never exec the workload.
    EmptyAllowlist,
    /// seccompiler rejected the filter or the assembly (rendered to a `String` so
    /// this error stays `Clone + PartialEq`).
    Assembler(String),
    /// The canonical policy bytes could not be encoded for the policy digest
    /// (effectively unreachable for the frozen wire shape).
    CanonicalEncoding(String),
}

impl std::fmt::Display for SeccompCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMandatoryBase { syscall } => write!(
                f,
                "seccomp policy omits mandatory base syscall {syscall}: a launcher filter must \
                 always allow execve/execveat/write/exit_group"
            ),
            Self::EmptyAllowlist => {
                write!(
                    f,
                    "seccomp policy has an empty allowlist (denies even the mandatory base)"
                )
            }
            Self::Assembler(e) => write!(f, "seccompiler rejected the filter: {e}"),
            Self::CanonicalEncoding(e) => {
                write!(f, "could not canonically encode the seccomp policy: {e}")
            }
        }
    }
}

impl std::error::Error for SeccompCompileError {}

/// A launcher seccomp POLICY: a deny floor + a sorted allowlist of permitted
/// syscalls (proof-spine §5 D6). The Rust model, not JSON. Constructing one is cheap
/// and pure; [`Self::compile`] assembles it to BPF and binds its evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeccompPolicy {
    default_action: DefaultAction,
    allow: BTreeSet<Syscall>,
}

impl SeccompPolicy {
    /// The MANDATORY base allowlist (D6, non-negotiable): the syscalls a launcher
    /// filter must ALWAYS permit so the post-filter `execve`/`execveat` survives and
    /// `write` + `exit_group` can still report an error and exit.
    #[must_use]
    pub fn mandatory_base() -> [Syscall; 4] {
        [
            Syscall::new("execve", libc::SYS_execve),
            Syscall::new("execveat", libc::SYS_execveat),
            Syscall::new("write", libc::SYS_write),
            Syscall::new("exit_group", libc::SYS_exit_group),
        ]
    }

    /// The minimal launcher policy: the mandatory base allowlist over a chosen deny
    /// floor. This is the non-negotiable starting point every launcher filter
    /// extends; it always compiles (the base is present by construction).
    #[must_use]
    pub fn launcher_base(default_action: DefaultAction) -> Self {
        let allow = Self::mandatory_base().into_iter().collect();
        Self {
            default_action,
            allow,
        }
    }

    /// Extend the allowlist with one more permitted syscall (chainable builder).
    #[must_use]
    pub fn allow(mut self, syscall: Syscall) -> Self {
        self.allow.insert(syscall);
        self
    }

    /// The deny floor.
    #[must_use]
    pub fn default_action(&self) -> DefaultAction {
        self.default_action
    }

    /// Test-only: a policy with an EMPTY allowlist (rejected by `compile`).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn empty_for_test(default_action: DefaultAction) -> Self {
        Self {
            default_action,
            allow: BTreeSet::new(),
        }
    }

    /// Test-only: drop a mandatory-base syscall by NAME to drive the fail-closed
    /// path (e.g. an `execve`-denying policy that `compile` must reject).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn without_for_test(mut self, name: &str) -> Self {
        self.allow.retain(|s| s.name != name);
        self
    }

    /// The sorted allowlist.
    pub fn allowlist(&self) -> impl Iterator<Item = Syscall> + '_ {
        self.allow.iter().copied()
    }

    /// The canonical seccomp-policy digest — blake3 over the domain-separated,
    /// deterministically-ordered policy repr (deny floor + sorted allowlist by name).
    /// Distinct policies ⇒ distinct digests; the same policy ⇒ the same digest.
    ///
    /// # Errors
    /// [`SeccompCompileError::CanonicalEncoding`] if encoding fails (unreachable for
    /// the frozen wire shape).
    pub fn policy_digest(&self) -> Result<[u8; 32], SeccompCompileError> {
        #[derive(serde::Serialize)]
        struct PolicyDigestInput<'a> {
            domain: &'a str,
            default_action: (u8, u32),
            allow: Vec<&'a str>,
        }
        let default_action = self.default_action.to_kind().wire_tag();
        // Sort the allowlist by stable NAME so the digest is arch-independent and
        // input-order-independent (the BTreeSet already orders by name first).
        let mut allow: Vec<&str> = self.allow.iter().map(|s| s.name).collect();
        allow.sort_unstable();
        allow.dedup();
        let input = PolicyDigestInput {
            domain: POLICY_DIGEST_DOMAIN,
            default_action,
            allow,
        };
        let bytes = batpak::canonical::to_bytes(&input)
            .map_err(|e| SeccompCompileError::CanonicalEncoding(e.to_string()))?;
        Ok(batpak::event::hash::compute_hash(&bytes))
    }

    /// Compile the policy to a BPF program for `arch` and bind its D6 evidence.
    ///
    /// FAIL-CLOSED, in order: the allowlist is non-empty · every mandatory-base
    /// syscall is present (you cannot build a filter that traps its own exec) ·
    /// seccompiler assembles the default-deny allowlist into one [`BpfProgram`]. The
    /// returned [`SeccompEvidence`] carries the policy digest, the compiled-BPF
    /// digest (over the canonical `sock_filter` byte stream), the target arch, the
    /// pinned seccompiler version, and the action profile; `observed_installed_mode`
    /// is `None` (S10 install populates it).
    ///
    /// SAME-ARCH ONLY for a usable filter: the allowlist syscall NUMBERS are resolved
    /// from `libc::SYS_*` of the BUILD target (see [`Syscall::new`]). Passing an `arch`
    /// other than the build target embeds the build-target's numbers into a foreign-arch
    /// BPF preamble — bytes that are deterministic (used by the digest-distinctness
    /// tests) but NOT a correct filter for that foreign arch. The real install (S10) is
    /// always same-arch; cross-arch `compile()` is for digest comparison only.
    ///
    /// # Errors
    /// Any [`SeccompCompileError`].
    pub fn compile(&self, arch: SeccompArch) -> Result<CompiledFilter, SeccompCompileError> {
        if self.allow.is_empty() {
            return Err(SeccompCompileError::EmptyAllowlist);
        }
        // Fail-closed: the mandatory base must be in the allowlist by NAME (the
        // arch-resolved number can differ per arch; identity is the name).
        let allowed_names: BTreeSet<&str> = self.allow.iter().map(|s| s.name).collect();
        for base in Self::mandatory_base() {
            if !allowed_names.contains(base.name) {
                return Err(SeccompCompileError::MissingMandatoryBase { syscall: base.name });
            }
        }

        // Assemble a default-deny allowlist: every allowed syscall → `Allow`
        // (empty rule chain = unconditional match), the default floor = mismatch.
        let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
            self.allow.iter().map(|s| (s.nr, Vec::new())).collect();
        let filter = SeccompFilter::new(
            rules,
            self.default_action.to_seccomp(),
            SeccompAction::Allow,
            to_target_arch(arch),
        )
        .map_err(|e| SeccompCompileError::Assembler(e.to_string()))?;
        let program: BpfProgram = filter.try_into().map_err(|e: seccompiler::BackendError| {
            SeccompCompileError::Assembler(e.to_string())
        })?;

        let policy_digest = self.policy_digest()?;
        let bpf_bytes = canonical_bpf_bytes(&program);
        let bpf_digest = batpak::event::hash::compute_hash(&bpf_bytes);

        // Action profile: Allow (every allowlisted syscall) + the deny floor,
        // deduplicated + sorted for a canonical profile.
        let mut action_profile = vec![SeccompActionKind::Allow, self.default_action.to_kind()];
        action_profile.sort_unstable_by_key(|a| a.wire_tag());
        action_profile.dedup();

        let evidence = SeccompEvidence {
            policy_digest,
            bpf_digest,
            target_arch: arch,
            seccompiler_version: SECCOMPILER_VERSION.to_string(),
            action_profile,
            observed_installed_mode: None,
        };
        Ok(CompiledFilter { program, evidence })
    }
}

/// A compiled launcher seccomp filter + its D6 evidence. The `program` is the BPF
/// S10 will install (S7 does NOT install it); `evidence` is the build-time binding.
#[derive(Clone, Debug)]
pub struct CompiledFilter {
    program: BpfProgram,
    evidence: SeccompEvidence,
}

impl CompiledFilter {
    /// The assembled BPF program (the `sock_filter` stream S10 installs).
    #[must_use]
    pub fn program(&self) -> &BpfProgram {
        &self.program
    }

    /// The D6 evidence binding (build-time; `observed_installed_mode` is `None`).
    #[must_use]
    pub fn evidence(&self) -> &SeccompEvidence {
        &self.evidence
    }

    /// The canonical compiled-BPF bytes whose blake3 is [`SeccompEvidence::bpf_digest`].
    #[must_use]
    pub fn bpf_bytes(&self) -> Vec<u8> {
        canonical_bpf_bytes(&self.program)
    }
}

/// Map the contract's arch enum to seccompiler's `TargetArch`.
fn to_target_arch(arch: SeccompArch) -> TargetArch {
    match arch {
        SeccompArch::X86_64 => TargetArch::x86_64,
        SeccompArch::Aarch64 => TargetArch::aarch64,
        SeccompArch::Riscv64 => TargetArch::riscv64,
    }
}

/// The CANONICAL byte serialization of a compiled BPF program: each `sock_filter`'s
/// fields in the kernel `struct sock_filter` order (`code` u16 LE, `jt` u8, `jf` u8,
/// `k` u32 LE). Deterministic + assembler-stable, so the same program ⇒ the same
/// bytes ⇒ the same `bpf_digest`. (`sock_filter` is not serde-serializable, so we
/// emit the wire bytes directly.)
fn canonical_bpf_bytes(program: &BpfProgram) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for insn in program {
        bytes.extend_from_slice(&insn.code.to_le_bytes());
        bytes.push(insn.jt);
        bytes.push(insn.jf);
        bytes.extend_from_slice(&insn.k.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
#[path = "seccomp_tests.rs"]
mod tests;
