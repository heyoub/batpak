//! [`Capability`] — the guest-invokable admitted authority POLICY, plus
//! [`Enforcement`] (the matrix verdict) and the guarantee-shaped grades.
//!
//! A [`Capability`] is the admitted rule the boundary ENFORCES on what the
//! WORKLOAD may attempt. It carries GRANTS and RESTRICTIONS — a deny-all
//! network policy is a restriction, still a Capability because it is the
//! admitted authority policy the backend must honor. Host lifecycle lives in
//! [`crate::HostControl`], NOT here: the confined workload cannot self-grant a
//! commit, a temp root, or its own launch.
//!
//! GRADES ARE GUARANTEE-SHAPED, NOT MECHANISM-SHAPED. The spec says WHAT
//! guarantee is required; the backend says HOW (pivot_root / Landlock / preopen
//! / Job Object / …) and records it in
//! [`crate::AdmittedRequirement::mechanism`] as evidence. [`Enforcement::Unsupported`]
//! is NEVER a requested value — it is only ever the backend's answer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The enforcement-strength axis of a support verdict.
///
/// One of two ORTHOGONAL axes (the other is [`EvidenceSet`]): this grades how
/// strongly a requirement is held; the evidence set grades what can be
/// witnessed about it. The two never collapse — a backend may enforce strongly
/// yet witness little (a structural guarantee with nothing per-attempt to see).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Enforcement {
    /// The backend can guarantee the requirement (strong primitive present).
    Enforced,
    /// The backend can honor the requirement only by mediating each attempt
    /// (e.g. a broker / notifier), not by a structural guarantee.
    Mediated,
    /// The backend cannot honor the requirement at all on this machine. Only
    /// ever a backend ANSWER; never a requested value. Forces `plan()` closed.
    Unsupported,
}

impl Enforcement {
    /// The MEET of the enforcement lattice in the SECURITY order (`Enforced`
    /// strongest, `Unsupported` the fail-closed bottom). `Unsupported` on either
    /// side wins (absorbing); this is the algebra the admission matrix floors a
    /// family best-case by a machine ceiling with. (Note: the derived `Ord` runs
    /// the other way — declaration order — so this meet equals the `Ord` MAX.)
    #[must_use]
    pub fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unsupported, _) | (_, Self::Unsupported) => Self::Unsupported,
            (Self::Mediated, _) | (_, Self::Mediated) => Self::Mediated,
            (Self::Enforced, Self::Enforced) => Self::Enforced,
        }
    }
}

/// One kind of evidence a backend can produce for a requirement.
///
/// The members of the EVIDENCE axis — orthogonal to [`Enforcement`]. A scalar
/// "coverage" level would be dishonest: a backend that witnesses denied attempts
/// but not allowed actions is incomparable to one that does the reverse. So
/// evidence is a SET of explicit claims, and composition is set intersection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EvidenceClaim {
    /// The run's terminal outcome + exit status are observable.
    TerminalOutcome,
    /// Captured stdout/stderr are observable.
    CapturedStreams,
    /// CPU/memory/IO resource usage is observable.
    ResourceUsage,
    /// The operations the workload performed are observable.
    AllowedActions,
    /// Each attempt the boundary blocked is observable.
    DeniedAttempts,
    /// Filesystem creations/modifications are observable.
    FilesystemDelta,
    /// The child process tree is observable.
    ProcessTree,
    /// Network connections/traffic are observable.
    NetworkActivity,
    /// Produced-artifact provenance is observable.
    ArtifactLineage,
    /// The confinement mechanism actually applied is attestable.
    MechanismAttestation,
}

/// A set of [`EvidenceClaim`]s — the evidence a backend can produce (the
/// "available" set) or a caller requires (the "required" set).
///
/// Forms a lattice under `⊆`: the MEET is INTERSECTION (composing two backends/
/// ceilings yields only the evidence BOTH can produce); the JOIN is UNION (the
/// total evidence a plan can produce across its admitted requirements). The
/// empty set is the absorbing bottom of the meet; planning admits only when the
/// required set is a subset of the available set.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EvidenceSet(BTreeSet<EvidenceClaim>);

impl EvidenceSet {
    /// The empty evidence set.
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeSet::new())
    }

    /// Insert a claim; returns true if newly added.
    pub fn insert(&mut self, claim: EvidenceClaim) -> bool {
        self.0.insert(claim)
    }

    /// Whether the set contains a claim.
    #[must_use]
    pub fn contains(&self, claim: EvidenceClaim) -> bool {
        self.0.contains(&claim)
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of claims in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether every claim in `self` is also in `other` (the lattice `⊆`).
    #[must_use]
    pub fn is_subset(&self, other: &Self) -> bool {
        self.0.is_subset(&other.0)
    }

    /// The MEET: claims present in BOTH sets.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0).copied().collect())
    }

    /// The JOIN: claims present in EITHER set.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self(self.0.union(&other.0).copied().collect())
    }

    /// Fold another set's claims into this one (in-place union).
    pub fn extend_from(&mut self, other: &Self) {
        self.0.extend(other.0.iter().copied());
    }

    /// Iterate the claims in canonical (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = EvidenceClaim> + '_ {
        self.0.iter().copied()
    }
}

impl FromIterator<EvidenceClaim> for EvidenceSet {
    fn from_iter<I: IntoIterator<Item = EvidenceClaim>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// The full support answer for one requirement: a PRODUCT of the two orthogonal
/// axes — [`Enforcement`] (how strongly held) and [`EvidenceSet`] (what can be
/// witnessed). The matrix grades both; planning floors both via [`Self::meet`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportVerdict {
    /// How strongly the requirement is enforced.
    pub enforcement: Enforcement,
    /// What evidence the backend can produce for it.
    pub evidence: EvidenceSet,
}

impl SupportVerdict {
    /// Construct a verdict from both axes.
    #[must_use]
    pub fn new(enforcement: Enforcement, evidence: EvidenceSet) -> Self {
        Self {
            enforcement,
            evidence,
        }
    }

    /// The fail-closed bottom: unsupported, witnessing nothing.
    #[must_use]
    pub fn unsupported() -> Self {
        Self {
            enforcement: Enforcement::Unsupported,
            evidence: EvidenceSet::new(),
        }
    }

    /// The MEET of two verdicts — floor the enforcement, intersect the evidence.
    /// A product of two meet-semilattices is a meet-semilattice, so flooring a
    /// family best-case by a machine ceiling (or composing N ceilings) is
    /// commutative, associative, and order-independent.
    #[must_use]
    pub fn meet(&self, other: &Self) -> Self {
        Self {
            enforcement: self.enforcement.meet(other.enforcement),
            evidence: self.evidence.intersection(&other.evidence),
        }
    }
}

/// Guest-invokable admitted authority policy (grants AND restrictions).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Capability {
    /// Filesystem authority confined to a declared scope.
    Filesystem {
        /// Read / write / read-write grant.
        access: FsAccess,
        /// The declared roots the access is scoped to.
        scope: PathSet,
        /// Whether the scope applies recursively under each root.
        recursive: bool,
        /// The confinement GUARANTEE required (not a mechanism).
        confinement: FsConfinement,
    },
    /// Network authority: deny-all (restriction) or a scoped allow-list (grant).
    Network {
        /// The admitted network policy.
        policy: NetPolicy,
    },
    /// Authority for the workload to spawn its OWN children. The workload's
    /// initial launch is a [`crate::HostControl::LaunchWorkload`], not this.
    ChildSpawn {
        /// The admitted child-task policy: deny all new tasks, allow boundary-confined
        /// threads, or allow boundary-confined descendant processes ([`SpawnPolicy`]).
        policy: SpawnPolicy,
    },
    /// Environment authority: empty-by-default; explicit grants only.
    Environment {
        /// The admitted environment policy.
        policy: EnvPolicy,
    },
    /// Which host file descriptors survive into the workload; default is none.
    InheritedFds {
        /// The admitted fd-inheritance policy.
        policy: FdPolicy,
    },
}

/// Filesystem access grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FsAccess {
    /// Read only.
    Read,
    /// Write only.
    Write,
    /// Read and write.
    ReadWrite,
}

/// GUARANTEE: "reads/writes confined to the declared scope" — not a mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FsConfinement {
    /// Access is confined to the declared roots and nothing outside them.
    DeclaredRootsOnly,
}

/// GUARANTEE: deny vs scoped-allow (a policy, not a mechanism).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NetPolicy {
    /// All network access is denied (a restriction).
    DenyAll,
    /// Only the listed destinations are reachable (a scoped grant).
    AllowList(Vec<NetDest>),
}

/// The admitted CHILD-TASK policy — what new tasks (threads / processes) the
/// confined workload may itself create. The three variants are FROZEN SEMANTICS
/// (proof-spine §2/§6/S6): they are an object-capability ATTENUATION ladder
/// (§8 seL4/Capsicum), strictest first. The VARIANT *is* the policy (no payload).
///
/// Each variant names a distinct GUARANTEE, not a mechanism. The mechanism that
/// will REALIZE each (seccomp / cgroup) is decided in S10, NOT here — S6 only
/// freezes the semantics + records the enforcement constraint below. Until S10
/// every variant stays FAIL-CLOSED in the production ceiling (no proof oracle).
///
/// # The clone3-pointer / classic-BPF problem (the load-bearing enforcement note)
///
/// A seccomp classic-BPF filter can only inspect syscall arguments that are SCALAR
/// REGISTERS. `clone3(2)` passes its flags inside a `struct clone_args` BEHIND A
/// POINTER (`rdi` → struct), so a seccomp filter CANNOT read the clone3 flags to
/// distinguish a THREAD (`CLONE_THREAD`) from a new PROCESS. This single fact sets
/// the per-variant enforcement strategy S10 will realize:
///
/// - [`Self::DenyNewTasks`] is ENFORCEABLE by seccomp at the SYSCALL-NUMBER level —
///   deny `clone`/`clone3`/`fork`/`vfork` outright, no argument dereference needed.
/// - [`Self::AllowDescendantsWithinBoundary`] is ENFORCEABLE by CGROUP confinement —
///   descendants inherit the cgroup, so `cgroup.kill` reaps them and `pids.max`
///   counts them (the S1 Kill / process_count mechanisms already exist).
/// - [`Self::AllowThreadsWithinBoundary`] is the HARD one: seccomp cannot deref the
///   clone3 flags to permit-threads-but-deny-processes, and denying `clone3` outright
///   breaks modern glibc thread creation. This is the OPEN enforcement problem. S6
///   does NOT pick a winner; S10 resolves it (deny-clone3-allow-legacy-clone-with-
///   `CLONE_THREAD` / cgroup-mediate / FAIL_CLOSED). The SEMANTICS are frozen here;
///   only the enforcement is deferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SpawnPolicy {
    /// The workload may create NO new task at all — no new thread, no new process.
    /// The STRICTEST variant. Enforceable by a seccomp syscall-number deny of the
    /// whole `clone`/`clone3`/`fork`/`vfork` family (no clone3-flag dereference).
    DenyNewTasks,
    /// The workload may create THREADS (shared address space, `CLONE_THREAD`) that
    /// stay WITHIN the confinement boundary (same cgroup + namespaces); it may NOT
    /// create new processes. The hard variant for seccomp — see the type-level note
    /// on the clone3-pointer / classic-BPF problem; S10 decides the mechanism.
    AllowThreadsWithinBoundary,
    /// The workload may create DESCENDANT PROCESSES, but they are CONFINED to the
    /// boundary: same cgroup ⇒ killable via `cgroup.kill`, counted by `pids.max`,
    /// unable to escape the namespaces. Enforceable by cgroup confinement (the S1
    /// Kill / process_count mechanisms), not by per-syscall filtering.
    AllowDescendantsWithinBoundary,
}

/// Environment-variable policy (proof-spine §5 D2 — `Environment::Exact`).
///
/// NO ambient passthrough. The child's environment is built EXACTLY from the
/// admitted table: every variable is an explicit [`EnvSource::Literal`] value or a
/// [`EnvSource::SecretLease`] reference resolved JIT in the parent. There is no
/// "inherit these named host keys" variant — that was the old `EmptyExcept` fossil,
/// removed outright (no compat). A platform-generated entry (e.g. a `PATH` default)
/// must be DECLARED as an explicit `Literal` here, never invisible inheritance.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EnvPolicy {
    /// The child environment is EXACTLY these entries — nothing inherited. Each
    /// entry is a `name → source` binding; [`Self::validate`] is the fail-closed
    /// contract gate over the whole table (encoding, duplicates, bounds).
    Exact(Vec<EnvEntry>),
}

/// One admitted environment binding: a variable `name` and where its value comes
/// from ([`EnvSource`]). The value is NEVER ambient — it is a literal or a lease ref.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnvEntry {
    /// The variable name (the part before `=` in the child env). Validated by
    /// [`EnvPolicy::validate`]: non-empty UTF-8, no `=`/NUL, case-sensitive-unique.
    pub name: String,
    /// Where the variable's value comes from.
    pub source: EnvSource,
}

impl EnvEntry {
    /// A literal-valued entry (`name=value`).
    #[must_use]
    pub fn literal(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: EnvSource::Literal(value.into()),
        }
    }

    /// A secret-lease entry: the value is resolved JIT in the parent from `lease`,
    /// never carried in the durable plan.
    #[must_use]
    pub fn lease(name: impl Into<String>, lease: SecretRef) -> Self {
        Self {
            name: name.into(),
            source: EnvSource::SecretLease(lease),
        }
    }
}

/// Where an [`EnvEntry`]'s value comes from. A `Literal` carries the value inline
/// (durable); a `SecretLease` carries only an opaque REF — the value is resolved in
/// the parent immediately before launch and NEVER persisted (proof-spine §5 D2 +
/// §8 Vault-style JIT secrets).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EnvSource {
    /// An explicit literal value, carried inline in the durable plan/report.
    Literal(String),
    /// A reference to a leased secret. Resolved JIT in the parent
    /// ([`crate::SecretResolver`]); the durable plan/report carries ONLY the ref.
    SecretLease(SecretRef),
}

/// An OPAQUE, typed reference to a leased secret. NEVER carries a value — only an
/// identifier a [`crate::SecretResolver`] dereferences in the parent at launch time.
/// Serializing this (in a plan or report) leaks only the ref string, never a secret.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SecretRef(pub String);

impl SecretRef {
    /// Construct a secret reference from its opaque identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The opaque identifier (the lease key a resolver dereferences).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.0
    }
}

/// The maximum number of entries an [`EnvPolicy::Exact`] table may declare. A
/// defensible cap (kernel `ARG_MAX` admits far more, but a confined workload that
/// genuinely needs hundreds of explicit env vars is pathological — the cap bounds
/// the admitted table and the canonical-key payload). 256 covers every realistic
/// declared environment with headroom.
pub const MAX_ENV_ENTRIES: usize = 256;

/// The maximum total byte budget of an [`EnvPolicy::Exact`] table, summed over every
/// `name` and every inline `Literal` value (a `SecretLease` ref counts its ref bytes,
/// not a resolved value — the value is unknown at admission). 128 KiB is a defensible
/// fraction of a typical 2 MiB `ARG_MAX`/env budget, leaving ample room for argv.
pub const MAX_ENV_TOTAL_BYTES: usize = 128 * 1024;

/// Why an [`EnvPolicy::Exact`] table is contract-invalid. The contract fails CLOSED
/// on the FIRST violation (admission refuses before any execution). Every variant
/// names a malformed class the §4 oracle's fail-closed branch plants.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvPolicyError {
    /// A variable name is empty (the part before `=` cannot be empty).
    EmptyName,
    /// A variable name contains `=` or a NUL byte (it could not become a `name=value`
    /// C string unambiguously). Names the offending name + the bad byte.
    NameHasReservedByte {
        /// The offending name.
        name: String,
        /// The reserved byte found (`b'='` or `0`).
        byte: u8,
    },
    /// A literal value contains a NUL byte (rejected — the §5 portable UTF-8 subset
    /// forbids NUL so the canonical bytes are unambiguous across platforms).
    ValueHasNul {
        /// The name whose literal value contained a NUL.
        name: String,
    },
    /// Two entries declare the SAME name (case-sensitive). The admitted environment
    /// must be a function name → value; a duplicate is ambiguous, so it is refused.
    DuplicateName {
        /// The duplicated name.
        name: String,
    },
    /// The table declares more than [`MAX_ENV_ENTRIES`] entries.
    TooManyEntries {
        /// The entry count found.
        found: usize,
    },
    /// The table's total `name`+inline-value byte budget exceeds
    /// [`MAX_ENV_TOTAL_BYTES`].
    TooManyBytes {
        /// The total byte count found.
        found: usize,
    },
}

impl std::fmt::Display for EnvPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("an environment variable name is empty"),
            Self::NameHasReservedByte { name, byte } => write!(
                f,
                "environment name {name:?} contains the reserved byte {byte:#04x} (= or NUL)"
            ),
            Self::ValueHasNul { name } => {
                write!(f, "environment value for {name:?} contains a NUL byte")
            }
            Self::DuplicateName { name } => {
                write!(f, "environment name {name:?} is declared more than once")
            }
            Self::TooManyEntries { found } => write!(
                f,
                "environment declares {found} entries, exceeding the cap {MAX_ENV_ENTRIES}"
            ),
            Self::TooManyBytes { found } => write!(
                f,
                "environment totals {found} bytes, exceeding the cap {MAX_ENV_TOTAL_BYTES}"
            ),
        }
    }
}

impl std::error::Error for EnvPolicyError {}

impl EnvPolicy {
    /// THE CONTRACT GATE (fail-closed): validate the table's encoding, uniqueness,
    /// and bounds. Returns `Ok(())` only for a well-formed admitted environment; the
    /// FIRST violation is returned. Called at admission BEFORE any execution, so a
    /// contract-invalid policy (a duplicate name, a NUL-bearing value) NEVER reaches
    /// lowering — the workload never runs.
    ///
    /// Rules (proof-spine §5 D2 + the §5 portable UTF-8 subset):
    /// - names: non-empty, UTF-8 (the `String` type guarantees it), no `=`/NUL,
    ///   case-sensitive, NO duplicates;
    /// - literal values: UTF-8, no NUL (a `SecretLease` ref is opaque, not validated
    ///   as a value — its resolved value is checked at lowering);
    /// - at most [`MAX_ENV_ENTRIES`] entries and [`MAX_ENV_TOTAL_BYTES`] total bytes.
    ///
    /// # Errors
    /// The first [`EnvPolicyError`] found.
    pub fn validate(&self) -> Result<(), EnvPolicyError> {
        let Self::Exact(entries) = self;
        if entries.len() > MAX_ENV_ENTRIES {
            return Err(EnvPolicyError::TooManyEntries {
                found: entries.len(),
            });
        }
        let mut total: usize = 0;
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for entry in entries {
            if entry.name.is_empty() {
                return Err(EnvPolicyError::EmptyName);
            }
            for byte in entry.name.bytes() {
                if byte == b'=' || byte == 0 {
                    return Err(EnvPolicyError::NameHasReservedByte {
                        name: entry.name.clone(),
                        byte,
                    });
                }
            }
            if !seen.insert(entry.name.as_str()) {
                return Err(EnvPolicyError::DuplicateName {
                    name: entry.name.clone(),
                });
            }
            total = total.saturating_add(entry.name.len());
            match &entry.source {
                EnvSource::Literal(value) => {
                    if value.bytes().any(|b| b == 0) {
                        return Err(EnvPolicyError::ValueHasNul {
                            name: entry.name.clone(),
                        });
                    }
                    total = total.saturating_add(value.len());
                }
                EnvSource::SecretLease(lease) => {
                    total = total.saturating_add(lease.id().len());
                }
            }
        }
        if total > MAX_ENV_TOTAL_BYTES {
            return Err(EnvPolicyError::TooManyBytes { found: total });
        }
        Ok(())
    }
}

/// Host-fd inheritance policy: none by default, explicit fds only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FdPolicy {
    /// No host file descriptors survive into the workload.
    None,
    /// Only the listed raw fds survive into the workload.
    Only(Vec<u32>),
}

/// A declared set of filesystem roots a [`Capability::Filesystem`] is scoped to.
///
/// Portable, inert string paths — the contract never touches the filesystem, so
/// these are evidence/scope data, not opened handles.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PathSet {
    /// The declared roots, as portable path strings.
    pub roots: Vec<String>,
}

impl PathSet {
    /// An empty path set (no roots declared).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// A single allow-listed network destination (host + port), inert evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NetDest {
    /// Destination host (name or address), as a portable string.
    pub host: String,
    /// Destination port.
    pub port: u16,
}

#[cfg(test)]
mod lattice_laws {
    //! The admission algebra is a bounded meet-semilattice on each axis and on
    //! their product. These exhaustive laws pin it: `Enforcement::meet` (the
    //! enforcement floor), `EvidenceSet::intersection` (the evidence meet), and
    //! `SupportVerdict::meet` (the product). Associativity is what makes
    //! composing N machine ceilings / primitives deterministic regardless of
    //! grouping; the absorbing bottoms (`Unsupported`, the empty set) are the
    //! fail-closed properties.
    use super::{Enforcement, EvidenceClaim, EvidenceSet, SupportVerdict};

    const ENFORCEMENTS: [Enforcement; 3] = [
        Enforcement::Enforced,
        Enforcement::Mediated,
        Enforcement::Unsupported,
    ];

    fn enforcement_strength(e: Enforcement) -> u8 {
        match e {
            Enforcement::Enforced => 2,
            Enforcement::Mediated => 1,
            Enforcement::Unsupported => 0,
        }
    }

    /// All evidence claims — the lattice top, listed in-crate (we own the enum).
    fn full_evidence() -> EvidenceSet {
        [
            EvidenceClaim::TerminalOutcome,
            EvidenceClaim::CapturedStreams,
            EvidenceClaim::ResourceUsage,
            EvidenceClaim::AllowedActions,
            EvidenceClaim::DeniedAttempts,
            EvidenceClaim::FilesystemDelta,
            EvidenceClaim::ProcessTree,
            EvidenceClaim::NetworkActivity,
            EvidenceClaim::ArtifactLineage,
            EvidenceClaim::MechanismAttestation,
        ]
        .into_iter()
        .collect()
    }

    /// A small, representative spread of evidence sets for brute-forcing laws.
    fn evidence_samples() -> Vec<EvidenceSet> {
        vec![
            EvidenceSet::new(),
            [EvidenceClaim::TerminalOutcome].into_iter().collect(),
            [
                EvidenceClaim::TerminalOutcome,
                EvidenceClaim::CapturedStreams,
            ]
            .into_iter()
            .collect(),
            [
                EvidenceClaim::CapturedStreams,
                EvidenceClaim::NetworkActivity,
            ]
            .into_iter()
            .collect(),
            full_evidence(),
        ]
    }

    // ── Enforcement meet (moved from support::floor; same algebra) ──

    #[test]
    fn enforcement_meet_is_commutative_associative_idempotent() {
        for a in ENFORCEMENTS {
            assert_eq!(a.meet(a), a, "idempotent at {a:?}");
            for b in ENFORCEMENTS {
                assert_eq!(a.meet(b), b.meet(a), "commutative at ({a:?},{b:?})");
                for c in ENFORCEMENTS {
                    assert_eq!(
                        a.meet(b).meet(c),
                        a.meet(b.meet(c)),
                        "associative at ({a:?},{b:?},{c:?})",
                    );
                }
            }
        }
    }

    #[test]
    fn enforcement_unsupported_absorbs_and_enforced_is_identity() {
        for a in ENFORCEMENTS {
            assert_eq!(Enforcement::Unsupported.meet(a), Enforcement::Unsupported);
            assert_eq!(a.meet(Enforcement::Unsupported), Enforcement::Unsupported);
            assert_eq!(Enforcement::Enforced.meet(a), a);
            assert_eq!(a.meet(Enforcement::Enforced), a);
        }
    }

    #[test]
    fn enforcement_meet_is_the_glb_in_security_order() {
        for a in ENFORCEMENTS {
            for b in ENFORCEMENTS {
                let m = a.meet(b);
                assert_eq!(
                    enforcement_strength(m),
                    enforcement_strength(a).min(enforcement_strength(b)),
                    "meet is the GLB at ({a:?},{b:?})",
                );
            }
        }
    }

    // ── Evidence intersection (the evidence meet) ──

    #[test]
    fn evidence_intersection_is_commutative_associative_idempotent() {
        for a in &evidence_samples() {
            assert_eq!(&a.intersection(a), a, "idempotent");
            for b in &evidence_samples() {
                assert_eq!(a.intersection(b), b.intersection(a), "commutative");
                for c in &evidence_samples() {
                    assert_eq!(
                        a.intersection(b).intersection(c),
                        a.intersection(&b.intersection(c)),
                        "associative",
                    );
                }
            }
        }
    }

    #[test]
    fn evidence_empty_absorbs_and_full_is_identity() {
        let empty = EvidenceSet::new();
        let full = full_evidence();
        for a in &evidence_samples() {
            assert_eq!(
                a.intersection(&empty),
                empty,
                "empty is the absorbing bottom"
            );
            assert_eq!(&a.intersection(&full), a, "full is the identity");
        }
    }

    #[test]
    fn evidence_intersection_is_a_lower_bound() {
        for a in &evidence_samples() {
            for b in &evidence_samples() {
                let m = a.intersection(b);
                assert!(m.is_subset(a) && m.is_subset(b), "meet ⊆ both inputs");
            }
        }
    }

    // ── Product verdict meet ──

    #[test]
    fn verdict_meet_is_commutative_associative_idempotent() {
        let verdicts: Vec<SupportVerdict> = ENFORCEMENTS
            .iter()
            .zip(evidence_samples())
            .map(|(&e, ev)| SupportVerdict::new(e, ev))
            .collect();
        for a in &verdicts {
            assert_eq!(&a.meet(a), a, "idempotent");
            for b in &verdicts {
                assert_eq!(a.meet(b), b.meet(a), "commutative");
                for c in &verdicts {
                    assert_eq!(a.meet(b).meet(c), a.meet(&b.meet(c)), "associative",);
                }
            }
        }
    }

    #[test]
    fn verdict_meet_floors_both_axes() {
        let a = SupportVerdict::new(
            Enforcement::Enforced,
            [
                EvidenceClaim::TerminalOutcome,
                EvidenceClaim::CapturedStreams,
            ]
            .into_iter()
            .collect(),
        );
        let b = SupportVerdict::new(
            Enforcement::Mediated,
            [
                EvidenceClaim::CapturedStreams,
                EvidenceClaim::NetworkActivity,
            ]
            .into_iter()
            .collect(),
        );
        let m = a.meet(&b);
        assert_eq!(m.enforcement, Enforcement::Mediated);
        assert_eq!(
            m.evidence,
            [EvidenceClaim::CapturedStreams].into_iter().collect()
        );
    }
}

#[cfg(test)]
#[path = "capability_env_tests.rs"]
mod capability_env_tests;
