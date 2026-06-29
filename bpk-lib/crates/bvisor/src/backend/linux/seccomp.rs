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
//! at build for the ALLOWLIST mode.
//!
//! ## The S10 extension: a TARGETED DENYLIST mode (default-ALLOW, deny specific)
//! S7's allowlist is the right shape for "this launcher may call exactly these
//! syscalls". But the S10 child-task taxonomy needs the OPPOSITE shape for a GENERAL
//! workload: `ChildSpawn::DenyNewTasks` must DENY `clone`/`clone3`/`fork`/`vfork`
//! while letting the rest of an arbitrary workload run, and `NetworkDenyAll` wants a
//! defense-in-depth DENY of externally-routable `socket(2)` families. A default-deny
//! allowlist cannot express that (it would have to enumerate every syscall a general
//! workload might make — impossible + brittle). So [`SeccompPolicy`] gains a SECOND,
//! clearly-named mode: [`SeccompPolicy::denylist`] — default-ALLOW, deny exactly the
//! named syscalls.
//!
//! A DENYLIST FILTER IS **NOT** A STANDALONE SANDBOX (the load-bearing caveat): a
//! default-allow filter permits everything it does not explicitly deny, so it can
//! never be the whole confinement. It is ONE LAYER of a COMPOSED mechanism — the
//! broad confinement is the landlock ruleset (FS), the empty netns (network), the
//! cgroup boundary (descendants), and the fd-scrub (ambient authority); the denylist
//! filter's job is the SPECIFIC, syscall-number-level deny those structural layers
//! cannot express (deny task-creation syscalls; deny routable-socket creation as
//! DiD). This mirrors S7's deliberate ban — "permit-all is not a confinement" — by
//! NEVER claiming the denylist alone confines: it is only ever admitted ALONGSIDE the
//! structural layers (the §8 Swiss-cheese model).
//!
//! BOTH modes preserve S7's invariants: deterministic compile, the bound
//! [`SeccompEvidence`] (policy + BPF digests, arch, version, action profile), and the
//! mandatory base — a denylist must NEVER deny `execve`/`execveat`/`write`/`exit_group`
//! (the immediately-following `fexecve` + error reporting must survive), which
//! [`SeccompPolicy::denylist`] enforces by REJECTING any deny set that names a base
//! syscall (the fail-closed-on-deny-execve law, restated for the denylist).
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
    /// A DENYLIST policy named a mandatory base syscall in its deny set — building it
    /// would trap the launcher's own `fexecve`/error-reporting (the fail-closed-on-
    /// deny-execve law, restated for the denylist). The offending base name is named.
    DenylistDeniesMandatoryBase {
        /// The base syscall the deny set illegally named.
        syscall: &'static str,
    },
    /// A DENYLIST policy has an EMPTY deny set — a default-allow filter that denies
    /// nothing is a permit-all no-op, not a confinement layer. Rejected at build.
    EmptyDenylist,
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
            Self::DenylistDeniesMandatoryBase { syscall } => write!(
                f,
                "seccomp denylist names mandatory base syscall {syscall} in its deny set: a \
                 launcher filter must NEVER deny execve/execveat/write/exit_group"
            ),
            Self::EmptyDenylist => write!(
                f,
                "seccomp denylist has an empty deny set (a default-allow filter that denies \
                 nothing is a permit-all no-op, not a confinement layer)"
            ),
        }
    }
}

impl std::error::Error for SeccompCompileError {}

/// The MODE of a launcher seccomp policy (proof-spine §5 D6 + the S10 extension). The
/// two are duals; the policy is exactly one of them and [`SeccompPolicy::compile`]
/// assembles each via seccompiler with the matching default + per-syscall terminals.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    /// DEFAULT-DENY ALLOWLIST (S7): the deny floor + the sorted set of PERMITTED
    /// syscalls. The launcher's own filter shape — deny everything, allow exactly the
    /// declared. Rejected at build if the allowlist omits a mandatory base.
    Allowlist {
        /// The deny floor every non-allowed syscall falls through to.
        default_action: DefaultAction,
        /// The sorted set of permitted syscalls (always includes the mandatory base).
        allow: BTreeSet<Syscall>,
    },
    /// DEFAULT-ALLOW DENYLIST (S10): a deny floor + the sorted set of DENIED syscalls;
    /// everything else is permitted. The child-task / network-DiD shape — deny exactly
    /// the named task-creation / routable-socket syscalls, let a general workload run.
    /// NOT a standalone sandbox (one composed layer); rejected at build if the deny set
    /// names a mandatory base (it must never trap the launcher's own `fexecve`).
    Denylist {
        /// The terminal action each DENIED syscall resolves to (errno / kill-process).
        deny_action: DefaultAction,
        /// The sorted set of denied syscalls (must NOT include any mandatory base).
        deny: BTreeSet<Syscall>,
    },
}

/// A launcher seccomp POLICY: either a default-deny ALLOWLIST (S7) or a default-allow
/// DENYLIST (S10), distinguished by [`Mode`] (proof-spine §5 D6). The Rust model, not
/// JSON. Constructing one is cheap and pure; [`Self::compile`] assembles it to BPF and
/// binds its evidence. A denylist is ONE composed layer, never a standalone sandbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeccompPolicy {
    mode: Mode,
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

    /// The task-creation syscalls a [`Self::deny_new_tasks`] denylist refuses at the
    /// SYSCALL-NUMBER level (proof-spine S6: no `clone3` arg-deref needed — denying the
    /// whole `clone`/`clone3`/`fork`/`vfork` family by number is exact). `fork`/`vfork`
    /// are arch-optional (aarch64/riscv64 have no `SYS_fork`); the per-arch resolver
    /// includes only those the build target defines.
    #[must_use]
    pub fn task_creation_syscalls() -> Vec<Syscall> {
        let mut v = vec![
            Syscall::new("clone", libc::SYS_clone),
            Syscall::new("clone3", libc::SYS_clone3),
        ];
        #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
        {
            v.push(Syscall::new("fork", libc::SYS_fork));
            v.push(Syscall::new("vfork", libc::SYS_vfork));
        }
        v
    }

    /// The `socket(2)` syscall a [`Self::network_deny_did`] denylist refuses, as the
    /// NetworkDenyAll defense-in-depth (proof-spine §5 D3): denying `socket` blocks
    /// creation of any externally-routable socket (AF_INET/AF_INET6/…) on TOP of the
    /// structural empty-netns. `socket` exists on every supported arch.
    #[must_use]
    pub fn socket_syscall() -> Syscall {
        Syscall::new("socket", libc::SYS_socket)
    }

    /// The minimal launcher policy: the mandatory base ALLOWLIST over a chosen deny
    /// floor (default-deny). This is the non-negotiable starting point every launcher
    /// allowlist filter extends; it always compiles (the base is present by construction).
    #[must_use]
    pub fn launcher_base(default_action: DefaultAction) -> Self {
        let allow = Self::mandatory_base().into_iter().collect();
        Self {
            mode: Mode::Allowlist {
                default_action,
                allow,
            },
        }
    }

    /// A DENYLIST policy (default-allow, S10): deny exactly `deny_syscalls`, permit
    /// everything else. ONE composed confinement layer — never a standalone sandbox.
    /// `deny_action` is the terminal each denied syscall resolves to. Building one is
    /// pure; [`Self::compile`] rejects a deny set that names a mandatory base (the
    /// fail-closed-on-deny-execve law) or an empty deny set (a permit-all no-op).
    #[must_use]
    pub fn denylist(
        deny_action: DefaultAction,
        deny_syscalls: impl IntoIterator<Item = Syscall>,
    ) -> Self {
        Self {
            mode: Mode::Denylist {
                deny_action,
                deny: deny_syscalls.into_iter().collect(),
            },
        }
    }

    /// The `ChildSpawn::DenyNewTasks` denylist (proof-spine S10): deny the whole
    /// `clone`/`clone3`/`fork`/`vfork` family by syscall number with the given terminal
    /// (typically `Errno(EPERM)` so the workload's `fork()` fails observably, or
    /// `KillProcess` for the hardest deny). Composed with the cgroup/ns/fd-scrub layers.
    #[must_use]
    pub fn deny_new_tasks(deny_action: DefaultAction) -> Self {
        Self::denylist(deny_action, Self::task_creation_syscalls())
    }

    /// The `NetworkDenyAll` defense-in-depth denylist (proof-spine §5 D3): deny
    /// `socket(2)` so no externally-routable socket can be created, ON TOP OF the
    /// structural empty netns (which stays the primary guarantee). DiD, not a substitute.
    #[must_use]
    pub fn network_deny_did(deny_action: DefaultAction) -> Self {
        Self::denylist(deny_action, [Self::socket_syscall()])
    }

    /// Extend an ALLOWLIST policy with one more permitted syscall (chainable builder).
    /// A no-op on a denylist policy (a denylist has no allowlist to extend).
    #[must_use]
    pub fn allow(mut self, syscall: Syscall) -> Self {
        if let Mode::Allowlist { allow, .. } = &mut self.mode {
            allow.insert(syscall);
        }
        self
    }

    /// The deny floor / deny terminal — the action the filter's default (allowlist) or
    /// matched-deny (denylist) syscalls resolve to.
    #[must_use]
    pub fn default_action(&self) -> DefaultAction {
        match &self.mode {
            Mode::Allowlist { default_action, .. } => *default_action,
            Mode::Denylist { deny_action, .. } => *deny_action,
        }
    }

    /// Whether this policy is a default-allow DENYLIST (S10) rather than a default-deny
    /// allowlist (S7). Exposed so the launcher install + the evidence can record which
    /// shape was assembled (the two have different action profiles).
    #[must_use]
    pub fn is_denylist(&self) -> bool {
        matches!(self.mode, Mode::Denylist { .. })
    }

    /// Test-only: a policy with an EMPTY allowlist (rejected by `compile`).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn empty_for_test(default_action: DefaultAction) -> Self {
        Self {
            mode: Mode::Allowlist {
                default_action,
                allow: BTreeSet::new(),
            },
        }
    }

    /// Test-only: drop a mandatory-base syscall by NAME to drive the fail-closed
    /// path (e.g. an `execve`-denying allowlist that `compile` must reject).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn without_for_test(mut self, name: &str) -> Self {
        if let Mode::Allowlist { allow, .. } = &mut self.mode {
            allow.retain(|s| s.name != name);
        }
        self
    }

    /// Test-only: a DENYLIST that ILLEGALLY denies a mandatory base syscall by NAME, to
    /// drive the fail-closed-on-deny-execve denylist rejection.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn denylist_with_base_for_test(
        deny_action: DefaultAction,
        base: &'static str,
    ) -> Self {
        let nr = Self::mandatory_base()
            .into_iter()
            .find(|s| s.name == base)
            .map_or(libc::SYS_execve, Syscall::number);
        Self::denylist(deny_action, [Syscall::for_test(base, nr)])
    }

    /// The sorted set of syscalls this policy names — the allowlist (permitted) for an
    /// allowlist policy, or the deny set (refused) for a denylist policy.
    pub fn allowlist(&self) -> impl Iterator<Item = Syscall> + '_ {
        let set: &BTreeSet<Syscall> = match &self.mode {
            Mode::Allowlist { allow, .. } => allow,
            Mode::Denylist { deny, .. } => deny,
        };
        set.iter().copied()
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
            // 0 = allowlist (default-deny), 1 = denylist (default-allow). A mode tag in
            // the digest input keeps an allowlist and a denylist over the SAME syscalls +
            // the same floor distinct (distinct policies ⇒ distinct digests).
            mode: u8,
            default_action: (u8, u32),
            syscalls: Vec<&'a str>,
        }
        let (mode_tag, action, set) = match &self.mode {
            Mode::Allowlist {
                default_action,
                allow,
            } => (0u8, *default_action, allow),
            Mode::Denylist { deny_action, deny } => (1u8, *deny_action, deny),
        };
        // Sort the named syscalls by stable NAME so the digest is arch-independent and
        // input-order-independent (the BTreeSet already orders by name first).
        let mut syscalls: Vec<&str> = set.iter().map(|s| s.name).collect();
        syscalls.sort_unstable();
        syscalls.dedup();
        let input = PolicyDigestInput {
            domain: POLICY_DIGEST_DOMAIN,
            mode: mode_tag,
            default_action: action.to_kind().wire_tag(),
            syscalls,
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
        // Assemble the seccompiler filter for the mode: an allowlist matches each allowed
        // syscall → Allow with the deny floor as mismatch; a denylist matches each denied
        // syscall → the deny terminal with Allow as mismatch (default-allow). Both fail
        // CLOSED on a mandatory-base violation (allowlist must include them; denylist must
        // not deny them) so a filter can never trap the launcher's own fexecve.
        // `named_action` = what each NAMED syscall (rule key) resolves to; `default_action`
        // = the filter's mismatch terminal. `deny_terminal` is the deny action for the
        // evidence action profile (the deny floor for an allowlist, the matched-deny for a
        // denylist).
        let (rules, named_action, default_action, deny_terminal): (
            BTreeMap<i64, Vec<seccompiler::SeccompRule>>,
            SeccompAction,
            SeccompAction,
            DefaultAction,
        ) = match &self.mode {
            Mode::Allowlist {
                default_action,
                allow,
            } => {
                if allow.is_empty() {
                    return Err(SeccompCompileError::EmptyAllowlist);
                }
                // Fail-closed: the mandatory base must be in the allowlist by NAME.
                let allowed_names: BTreeSet<&str> = allow.iter().map(|s| s.name).collect();
                for base in Self::mandatory_base() {
                    if !allowed_names.contains(base.name) {
                        return Err(SeccompCompileError::MissingMandatoryBase {
                            syscall: base.name,
                        });
                    }
                }
                let rules = allow.iter().map(|s| (s.nr, Vec::new())).collect();
                // Each allowed syscall → Allow; mismatch → the deny floor.
                (
                    rules,
                    SeccompAction::Allow,
                    default_action.to_seccomp(),
                    *default_action,
                )
            }
            Mode::Denylist { deny_action, deny } => {
                if deny.is_empty() {
                    return Err(SeccompCompileError::EmptyDenylist);
                }
                // Fail-closed-on-deny-execve (restated for the denylist): the deny set must
                // NOT name a mandatory base by NAME — denying execve/write/exit_group would
                // trap the launcher's own fexecve / error reporting.
                let denied_names: BTreeSet<&str> = deny.iter().map(|s| s.name).collect();
                for base in Self::mandatory_base() {
                    if denied_names.contains(base.name) {
                        return Err(SeccompCompileError::DenylistDeniesMandatoryBase {
                            syscall: base.name,
                        });
                    }
                }
                let rules = deny.iter().map(|s| (s.nr, Vec::new())).collect();
                // Each denied syscall → the deny terminal; mismatch → Allow (default-allow).
                (
                    rules,
                    deny_action.to_seccomp(),
                    SeccompAction::Allow,
                    *deny_action,
                )
            }
        };
        let filter = SeccompFilter::new(rules, default_action, named_action, to_target_arch(arch))
            .map_err(|e| SeccompCompileError::Assembler(e.to_string()))?;
        let program: BpfProgram = filter.try_into().map_err(|e: seccompiler::BackendError| {
            SeccompCompileError::Assembler(e.to_string())
        })?;

        let policy_digest = self.policy_digest()?;
        let bpf_bytes = canonical_bpf_bytes(&program);
        let bpf_digest = batpak::event::hash::compute_hash(&bpf_bytes);

        // Action profile: the two terminals the kernel must honor — Allow + the deny
        // terminal (the deny floor for an allowlist, the matched-deny for a denylist),
        // deduplicated + sorted for a canonical profile.
        let mut action_profile = vec![SeccompActionKind::Allow, deny_terminal.to_kind()];
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

/// Whether this host supports installing a seccomp BPF FILTER (the
/// `ChildSpawn::DenyNewTasks` floor, S10). SAFE host-side probe: `seccomp(2)` filter mode
/// advertises its supported actions in `/proc/sys/kernel/seccomp/actions_avail` (present
/// IFF `CONFIG_SECCOMP_FILTER` is built in and the syscall is available). A readable,
/// non-empty `actions_avail` ⇒ filter mode is supported; absence ⇒ the cell is FAIL_CLOSED
/// (the workload's ChildSpawn::DenyNewTasks would have no mechanism, so the cell drops from
/// the ceiling — never a silent unfiltered run). Reads only `/proc/sys`; no `unsafe`.
#[must_use]
pub fn seccomp_filter_available() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/seccomp/actions_avail")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
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
