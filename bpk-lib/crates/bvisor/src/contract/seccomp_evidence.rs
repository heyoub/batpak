//! The D6 seccomp EVIDENCE surface (proof-spine §5 D6) — pure data, always
//! compiled.
//!
//! S7 produces the BUILDING BLOCK: a Rust seccomp policy model compiles to BPF and
//! binds its identity into [`SeccompEvidence`]. The struct lives here, in the
//! platform-agnostic contract, so the evidence shape + its digests are constructible
//! and unit-testable on ANY host (the policy model + the actual `compile()` that
//! mints these digests live in the gated `backend/linux/seccomp.rs`, which DOES need
//! seccompiler; this module never touches the OS or the assembler).
//!
//! THE FIELDS, and which are BUILD-TIME (S7) vs INSTALL-TIME (S10):
//! - `policy_digest`     — blake3 of the canonical policy repr.          [S7 build]
//! - `bpf_digest`        — blake3 of the compiled BPF bytes.             [S7 build]
//! - `target_arch`       — the LE arch the BPF was assembled for.        [S7 build]
//! - `seccompiler_version` — the pinned `=X.Y.Z` assembler version.      [S7 build]
//! - `action_profile`    — the seccomp actions the policy uses / the kernel would
//!   need to honor the filter.                                           [S7 build]
//! - `observed_installed_mode` — the kernel-confirmed install mode (e.g. the
//!   `/proc/<pid>/status` `Seccomp:` field). POPULATED AT INSTALL TIME (S10); S7
//!   leaves it `None`. S7 does NOT install, enforce, or read `/proc`.    [S10 install]
//!
//! This is NOT a `LoweringSchedule` entry: S7 mints no schedule and no Proven ledger
//! row (no enforcement, no oracle). The teeth of S7 are the determinism +
//! well-formedness tests on the policy→BPF→digest pipeline (see the gated module).

use crate::contract::ids::Digest32;
use serde::{Deserialize, Serialize};

/// The LITTLE-ENDIAN architectures a launcher seccomp filter targets (proof-spine
/// §5 D6: "seccompiler supports LE x86_64/aarch64/riscv64"). The BPF arch-audit
/// preamble is arch-specific, so the same policy compiles to DIFFERENT bytes per
/// arch — the digest binds the exact target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SeccompArch {
    /// x86-64 little-endian.
    X86_64,
    /// AArch64 little-endian.
    Aarch64,
    /// RISC-V 64 little-endian.
    Riscv64,
}

impl SeccompArch {
    /// The stable wire token for this arch (frozen — feeds the canonical digest).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Riscv64 => "riscv64",
        }
    }
}

/// One seccomp ACTION a policy may resolve to (proof-spine §5 D6: the
/// "kernel-supported action profile"). A stable, assembler-independent mirror of the
/// actions our policy model uses, so the evidence records WHAT the kernel must honor
/// without leaking seccompiler's type into the pure contract. `Errno` carries the
/// returned errno so two policies that deny via different errnos stay distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SeccompActionKind {
    /// Permit the syscall.
    Allow,
    /// Fail the syscall with the given errno (a soft, observable deny).
    Errno(u32),
    /// Kill the offending PROCESS (the hardest deny).
    KillProcess,
}

impl SeccompActionKind {
    /// The stable wire token for this action (frozen — feeds the canonical digest).
    /// `Errno` includes its number so distinct errnos are distinct evidence.
    #[must_use]
    pub fn wire_tag(self) -> (u8, u32) {
        match self {
            Self::Allow => (0, 0),
            Self::Errno(e) => (1, e),
            Self::KillProcess => (2, 0),
        }
    }
}

/// The D6 evidence binding for ONE compiled launcher seccomp filter.
///
/// Built at S7 BUILD time from the policy + its compiled BPF; the
/// `observed_installed_mode` is the ONLY install-time (S10) field and is `None`
/// until then. Pure data: serializable, hashable, no OS, no assembler.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SeccompEvidence {
    /// blake3 of the CANONICAL policy representation (default action + sorted
    /// per-syscall rules). Deterministic; distinct policies ⇒ distinct digests.
    pub policy_digest: Digest32,
    /// blake3 of the COMPILED BPF bytes (the canonical `sock_filter` stream). The
    /// same policy on the same arch compiles to the same bytes ⇒ the same digest.
    pub bpf_digest: Digest32,
    /// The LE arch the BPF was assembled for.
    pub target_arch: SeccompArch,
    /// The pinned seccompiler version string (`"=0.5.0"`), recorded so a future
    /// assembler bump that changes the BPF bytes is visible in the evidence.
    pub seccompiler_version: String,
    /// The DISTINCT seccomp actions this policy resolves to — what the kernel must
    /// support to honor the filter. Sorted + deduplicated for a canonical profile.
    pub action_profile: Vec<SeccompActionKind>,
    /// The kernel-confirmed install mode, POPULATED AT S10 INSTALL TIME (e.g. the
    /// `/proc/<pid>/status` `Seccomp:2` filter-mode read). S7 leaves it `None`: S7
    /// builds the filter but neither installs it nor observes a running child.
    pub observed_installed_mode: Option<SeccompObservedMode>,
}

/// The kernel-observed seccomp mode of a confined child, read at S10 install time
/// from `/proc/<pid>/status` (`Seccomp:` / `Seccomp_filters:`). Defined here so the
/// evidence type is complete now; S7 never constructs one (the field stays `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SeccompObservedMode {
    /// `Seccomp: 0` — no seccomp active (a FAIL-CLOSED red flag post-install).
    Disabled,
    /// `Seccomp: 1` — strict mode.
    Strict,
    /// `Seccomp: 2` — filter mode (the mode a BPF launcher filter installs).
    Filter,
}

#[cfg(test)]
#[path = "seccomp_evidence_tests.rs"]
mod tests;
