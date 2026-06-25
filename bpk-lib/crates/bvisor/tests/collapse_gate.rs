//! GAUNTLET bvisor S3 — the P0-A COLLAPSE GATE (proof-spine §9 S3).
//!
//! Two blocking properties, each registered with an anti-vacuous RED fixture, plus
//! a documenting confirmation of the S1 snapshot-vs-ceiling coupling (deliverable
//! #3). All three operate on the PURE public surface (`RequirementKind::of`, the
//! per-backend `support_matrix()`, `CanonicalPolicy`), so the file builds and runs
//! on the DEFAULT target with no proof hooks — like `bvisor-proof-receipt-
//! resolution`, not the Linux-gated `coupling_proof`.
//!
//! 1. INJECTIVE-COLLAPSE (`bvisor-injective-collapse`, ProductionFlip). Promotes
//!    S2's plain injectivity to a registered gate. For EVERY constructible
//!    [`CanonicalPolicy`] variant across ALL FOUR families (Fd / Spawn / Env / Net),
//!    the production `RequirementKind::of` map must (i) be WELL-DEFINED — equal
//!    canonical bytes ⇒ equal key — and (ii) be INJECTIVE-ON-VARIANTS — equal key ⇒
//!    equal canonical VARIANT (no key fuses two SEMANTICALLY-DISTINCT variants). The
//!    `gauntlet_red_fixture` branch drives the SAME injectivity check with a
//!    POLICY-BLIND key map that collapses two distinct variants
//!    (`InheritedFds::None`/`::Only`) onto one key, and asserts the check PASSES —
//!    which a biting check refuses, so the red half FAILS.
//!
//! 2. PER-PROFILE COMPLETENESS (`bvisor-support-completeness`, ProductionFlip).
//!    Every `RequirementKind::ALL` key must carry an EXPLICIT support claim in EVERY
//!    backend's `support_matrix()` — even `Unsupported` must be STATED, never
//!    silently absent (a silent gap is a finding). The `gauntlet_red_fixture` branch
//!    runs the SAME completeness check against a matrix with ONE key dropped and
//!    asserts it PASSES — which a biting check refuses, so the red half FAILS.
//!
//! 3. SNAPSHOT-IS-ASPIRATION (deliverable #3, documenting). The §1 two-axis model:
//!    `support_matrix()`/`capability_snapshot.yaml` is the ASPIRATION claim (may say
//!    `Enforced` for a not-yet-`Proven` cell); only the PRODUCTION CEILING is coupled
//!    to the ledger (the S1 `coupling_proof` gate). This asserts that explicitly so
//!    the design intent is witnessed and cannot be misread as a coupling bug.
//!
//! No `#![allow(..)]` of any kind; the project bans `panic!` even in tests, so every
//! failure collects into a `Vec` asserted empty (or uses `assert!`/`assert_eq!`).

use bvisor::{linux, macos, wasm, windows};
use bvisor::{
    BoundaryRequirement, CanonicalPolicy, Capability, Enforcement, EnvPolicy, FdPolicy, NetDest,
    NetPolicy, RequirementKind, SpawnPolicy, SupportMatrix,
};

// ════════════════════════════════════════════════════════════════════════════
// Shared: the constructible canonical-policy spread across ALL FOUR families.
// ════════════════════════════════════════════════════════════════════════════

/// One sample: a name, the production key `RequirementKind::of` derives for a
/// capability, and the canonical bytes of that capability's policy. The spread
/// covers EVERY canonical-policy variant the §2 keys distinguish — both
/// `InheritedFds` variants, both `ChildSpawn` variants, both `Network` variants,
/// the single `Environment` variant, plus the `Filesystem` capability — AND
/// syntactic aliases (reordered fd/dest lists) that must share BOTH key and bytes.
struct Sample {
    name: &'static str,
    key: RequirementKind,
    canonical: CanonicalPolicy,
}

/// Build a capability's production key through the REAL `RequirementKind::of`
/// path (via `BoundaryRequirement::Capability`) — the exact map production uses,
/// not a test-only shortcut.
fn key_of(cap: Capability) -> RequirementKind {
    RequirementKind::of(&BoundaryRequirement::Capability(cap))
}

fn dest(host: &str, port: u16) -> NetDest {
    NetDest {
        host: host.to_string(),
        port,
    }
}

/// The full spread. Each policy-bearing sample pairs its production key with the
/// canonical bytes of the SAME policy, so the two injectivity directions can be
/// checked by comparing keys against canonical-variant equality.
fn samples() -> Vec<Sample> {
    vec![
        // ── InheritedFds: None vs Only (distinct variants ⇒ distinct keys) ──
        Sample {
            name: "fd-none",
            key: key_of(Capability::InheritedFds {
                policy: FdPolicy::None,
            }),
            canonical: CanonicalPolicy::of_fd(&FdPolicy::None),
        },
        Sample {
            name: "fd-only-empty",
            key: key_of(Capability::InheritedFds {
                policy: FdPolicy::Only(vec![]),
            }),
            canonical: CanonicalPolicy::of_fd(&FdPolicy::Only(vec![])),
        },
        Sample {
            name: "fd-only-13",
            key: key_of(Capability::InheritedFds {
                policy: FdPolicy::Only(vec![1, 3]),
            }),
            canonical: CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1, 3])),
        },
        Sample {
            name: "fd-only-13-alias",
            key: key_of(Capability::InheritedFds {
                policy: FdPolicy::Only(vec![3, 1, 3]),
            }),
            canonical: CanonicalPolicy::of_fd(&FdPolicy::Only(vec![3, 1, 3])),
        },
        // ── ChildSpawn: Deny vs Allow (distinct variants ⇒ distinct keys) ──
        Sample {
            name: "spawn-deny",
            key: key_of(Capability::ChildSpawn {
                policy: SpawnPolicy::Deny,
            }),
            canonical: CanonicalPolicy::of_spawn(&SpawnPolicy::Deny),
        },
        Sample {
            name: "spawn-allow",
            key: key_of(Capability::ChildSpawn {
                policy: SpawnPolicy::Allow,
            }),
            canonical: CanonicalPolicy::of_spawn(&SpawnPolicy::Allow),
        },
        // ── Environment: one variant today, one key ──
        Sample {
            name: "env-empty",
            key: key_of(Capability::Environment {
                policy: EnvPolicy::EmptyExcept(vec![]),
            }),
            canonical: CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![])),
        },
        Sample {
            name: "env-keys",
            key: key_of(Capability::Environment {
                policy: EnvPolicy::EmptyExcept(vec!["PATH".to_string()]),
            }),
            canonical: CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec!["PATH".to_string()])),
        },
        // ── Network: DenyAll vs AllowList (distinct variants ⇒ distinct keys) ──
        Sample {
            name: "net-deny",
            key: key_of(Capability::Network {
                policy: NetPolicy::DenyAll,
            }),
            canonical: CanonicalPolicy::of_net(&NetPolicy::DenyAll),
        },
        Sample {
            name: "net-allow",
            key: key_of(Capability::Network {
                policy: NetPolicy::AllowList(vec![dest("example.com", 443)]),
            }),
            canonical: CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![dest(
                "example.com",
                443,
            )])),
        },
        // ── Network allow-list alias: reordered dests share key AND canonical bytes ──
        Sample {
            name: "net-allow-alias",
            key: key_of(Capability::Network {
                policy: NetPolicy::AllowList(vec![dest("b.example", 80), dest("a.example", 80)]),
            }),
            canonical: CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![
                dest("b.example", 80),
                dest("a.example", 80),
            ])),
        },
    ]
}

/// The canonical VARIANT a sample projects from: the family tag + variant
/// discriminant (the canonical bytes' two-byte prefix). Two policies share a key
/// iff they share this prefix; the bytes BEYOND it are the payload the key
/// abstracts over (and which `CanonicalPolicy`'s own §2 law keeps injective).
fn variant(canonical: &CanonicalPolicy) -> &[u8] {
    let bytes = canonical.as_bytes();
    &bytes[..bytes.len().min(2)]
}

// ════════════════════════════════════════════════════════════════════════════
// Deliverable 1 — INJECTIVE-COLLAPSE (`bvisor-injective-collapse`).
// ════════════════════════════════════════════════════════════════════════════

/// A pure injectivity violation — split out so the green test and the red fixture
/// drive the SAME check, the latter through a policy-blind key map.
#[derive(Debug, PartialEq, Eq)]
enum InjectivityViolation {
    /// Two samples share a canonical variant but map to DIFFERENT keys — the
    /// policy→key map is not a well-defined function.
    NotAFunction { a: &'static str, b: &'static str },
    /// Two samples map to the SAME key but have DISTINCT canonical variants — a
    /// key fuses two semantically-distinct variants (a policy-blind collapse).
    Collapse { a: &'static str, b: &'static str },
}

impl InjectivityViolation {
    fn describe(&self) -> String {
        match self {
            Self::NotAFunction { a, b } => {
                format!("{a} and {b} share a canonical variant but map to DIFFERENT keys")
            }
            Self::Collapse { a, b } => {
                format!("{a} and {b} share a key but have DISTINCT canonical variants (collapse)")
            }
        }
    }
}

/// THE PURE INJECTIVITY CHECK. `key_of_sample` yields the key the map under test
/// assigns a sample (the real `RequirementKind::of` in production; a planted
/// policy-blind map in the red fixture). Both directions of the §2 biconditional
/// are checked against canonical-variant equality.
fn check_injectivity(
    samples: &[Sample],
    key_of_sample: &dyn Fn(&Sample) -> RequirementKind,
) -> Vec<InjectivityViolation> {
    let mut violations = Vec::new();
    for a in samples {
        for b in samples {
            let same_variant = variant(&a.canonical) == variant(&b.canonical);
            let same_key = key_of_sample(a) == key_of_sample(b);
            // (i) WELL-DEFINED: same canonical variant ⇒ same key.
            if same_variant && !same_key {
                violations.push(InjectivityViolation::NotAFunction {
                    a: a.name,
                    b: b.name,
                });
            }
            // (ii) INJECTIVE-ON-VARIANTS: same key ⇒ same canonical variant.
            if same_key && !same_variant {
                violations.push(InjectivityViolation::Collapse {
                    a: a.name,
                    b: b.name,
                });
            }
        }
    }
    violations
}

/// The production key map: each sample's already-resolved `RequirementKind::of`
/// key. (The samples cache it at construction, so this just reads it back.)
fn production_key(sample: &Sample) -> RequirementKind {
    sample.key
}

/// GREEN: across all four families the production policy→key map is injective —
/// well-defined AND no key fuses two distinct canonical variants.
#[test]
fn production_policy_to_key_map_is_injective_across_all_families() {
    let samples = samples();
    let violations = check_injectivity(&samples, &production_key);
    let messages: Vec<String> = violations
        .iter()
        .map(InjectivityViolation::describe)
        .collect();
    assert!(
        messages.is_empty(),
        "production policy→key map is not injective: {messages:?}"
    );
}

/// NON-VACUITY: the spread exercises EVERY family AND the formerly policy-blind
/// kinds really do SPLIT into distinct keys (so the injectivity check above is not
/// passing over a degenerate all-one-key spread).
#[test]
fn the_split_keys_are_genuinely_distinct() {
    let distinct = [
        (
            "InheritedFds None vs Only",
            key_of(Capability::InheritedFds {
                policy: FdPolicy::None,
            }) != key_of(Capability::InheritedFds {
                policy: FdPolicy::Only(vec![]),
            }),
        ),
        (
            "ChildSpawn Deny vs Allow",
            key_of(Capability::ChildSpawn {
                policy: SpawnPolicy::Deny,
            }) != key_of(Capability::ChildSpawn {
                policy: SpawnPolicy::Allow,
            }),
        ),
        (
            "Network DenyAll vs AllowList",
            key_of(Capability::Network {
                policy: NetPolicy::DenyAll,
            }) != key_of(Capability::Network {
                policy: NetPolicy::AllowList(vec![dest("h", 1)]),
            }),
        ),
    ];
    let unsplit: Vec<&str> = distinct
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(name, _)| *name)
        .collect();
    assert!(
        unsplit.is_empty(),
        "these distinct semantics must map to DISTINCT keys but did not: {unsplit:?}"
    );
}

/// ANTI-VACUITY of the CHECK ITSELF: a POLICY-BLIND key map that collapses two
/// distinct variants onto one key MUST be flagged as a `Collapse`. Without this, a
/// `check_injectivity` that always returned an empty `Vec` would pass the green
/// test vacuously.
#[test]
fn the_injectivity_check_flags_a_policy_blind_collapse() {
    let samples = samples();
    let violations = check_injectivity(&samples, &policy_blind_key);
    assert!(
        violations
            .iter()
            .any(|v| matches!(v, InjectivityViolation::Collapse { .. })),
        "a policy-blind key map must be flagged as a collapse, got {violations:?}"
    );
}

/// A PLANTED policy-blind key map: it fuses `InheritedFds::None` and
/// `InheritedFds::Only` (distinct canonical variants) onto the SAME key, exactly
/// the regression the §2 law forbids. Every other sample keeps its real key.
fn policy_blind_key(sample: &Sample) -> RequirementKind {
    match sample.name {
        // Collapse both fd variants onto ONE key (the policy-blind bug).
        "fd-none" | "fd-only-empty" | "fd-only-13" | "fd-only-13-alias" => {
            RequirementKind::InheritedFdsNone
        }
        _ => sample.key,
    }
}

/// RED FIXTURE (`--cfg gauntlet_red_fixture`, ProductionFlip): assert the ILLEGAL
/// outcome — that the injectivity check finds NO collapse on the policy-blind map.
/// A biting check ALWAYS reports the planted `InheritedFds::None`/`::Only` fusion,
/// so this assertion is FALSE and the red half FAILS, proving the gate is
/// anti-vacuous.
#[cfg(gauntlet_red_fixture)]
#[test]
fn injective_collapse_red_fixture_policy_blind_map_must_escape() {
    let samples = samples();
    let violations = check_injectivity(&samples, &policy_blind_key);
    assert!(
        violations.is_empty(),
        "RED FIXTURE: asserts the (illegal) no-collapse-found outcome on a policy-blind map; \
         MUST fail because a biting injectivity check always catches the planted \
         InheritedFds::None/::Only fusion"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Deliverable 2 — PER-PROFILE COMPLETENESS (`bvisor-support-completeness`).
// ════════════════════════════════════════════════════════════════════════════

/// One silent-gap finding: a backend's matrix declares NO explicit claim for a key.
#[derive(Debug, PartialEq, Eq)]
struct SilentGap {
    backend: &'static str,
    kind: RequirementKind,
}

/// THE PURE COMPLETENESS CHECK: every `RequirementKind::ALL` key must be EXPLICITLY
/// declared in `matrix` (via `declares`, which sees the literal key set — an
/// explicit `Unsupported` counts, a silent absence does NOT). Returns one
/// [`SilentGap`] per missing key.
fn check_completeness(backend: &'static str, matrix: &SupportMatrix) -> Vec<SilentGap> {
    RequirementKind::ALL
        .into_iter()
        .filter(|&kind| !matrix.declares(kind))
        .map(|kind| SilentGap { backend, kind })
        .collect()
}

/// The four platform backends + their always-compiled aspiration matrices.
fn backend_matrices() -> Vec<(&'static str, SupportMatrix)> {
    vec![
        ("linux", linux::support_matrix()),
        ("macos", macos::support_matrix()),
        ("wasm", wasm::support_matrix()),
        ("windows", windows::support_matrix()),
    ]
}

/// GREEN: every backend declares an EXPLICIT claim for EVERY `RequirementKind::ALL`
/// key — no silent gaps. (A previously-silent gap, e.g. macOS ChildSpawn or wasm
/// InheritedFds, is now a stated `Unsupported`/`Enforced` answer.)
#[test]
fn every_backend_declares_every_requirement_kind() {
    let mut gaps = Vec::new();
    for (backend, matrix) in backend_matrices() {
        gaps.extend(check_completeness(backend, &matrix));
    }
    let messages: Vec<String> = gaps
        .iter()
        .map(|g| {
            format!(
                "{} has a SILENT GAP for {:?} (no explicit claim)",
                g.backend, g.kind
            )
        })
        .collect();
    assert!(
        messages.is_empty(),
        "per-profile completeness violated — every key must carry an explicit claim: {messages:?}"
    );
}

/// NON-VACUITY: the declared key set is the FULL `ALL` set per backend (so the
/// green completeness check is not passing over a too-small key universe), and an
/// explicit `Unsupported` cell really IS counted as declared.
#[test]
fn completeness_is_over_the_full_key_set_and_counts_unsupported() {
    for (backend, matrix) in backend_matrices() {
        assert_eq!(
            matrix.declared_kinds().len(),
            RequirementKind::ALL.len(),
            "{backend} must declare exactly the full ALL key set"
        );
    }
    // An explicit Unsupported cell is declared (macOS ChildSpawnDeny is the
    // formerly-silent gap we closed with a stated Unsupported answer).
    let macos = macos::support_matrix();
    assert!(
        macos.declares(RequirementKind::ChildSpawnDeny),
        "macOS must EXPLICITLY declare ChildSpawnDeny (an Unsupported answer still counts)"
    );
    assert_eq!(
        macos
            .best_case_for(RequirementKind::ChildSpawnDeny)
            .enforcement,
        Enforcement::Unsupported,
        "macOS ChildSpawnDeny is the explicit Unsupported answer"
    );
}

/// ANTI-VACUITY of the CHECK ITSELF: a matrix with ONE key dropped MUST be flagged
/// as a silent gap. Without this, a `check_completeness` that always returned an
/// empty `Vec` would pass the green test vacuously.
#[test]
fn the_completeness_check_flags_a_dropped_key() {
    let gaps = check_completeness("linux-with-dropped-key", &matrix_missing_one_key());
    assert!(
        gaps.iter().any(|g| g.kind == RequirementKind::Kill),
        "a matrix missing Kill must be flagged as a silent gap, got {gaps:?}"
    );
}

/// A PLANTED incomplete matrix: linux's real matrix with the `Kill` key DROPPED —
/// the exact silent-gap regression the completeness gate forbids (a key with no
/// stated answer, defaulting invisibly to the fail-closed bottom).
fn matrix_missing_one_key() -> SupportMatrix {
    use std::collections::BTreeMap;
    let full = linux::support_matrix();
    let mut best = BTreeMap::new();
    for kind in full.declared_kinds() {
        if kind == RequirementKind::Kill {
            continue; // drop exactly one key — the planted silent gap.
        }
        best.insert(kind, full.best_case_for(kind));
    }
    SupportMatrix::from_best_case(best)
}

/// RED FIXTURE (`--cfg gauntlet_red_fixture`, ProductionFlip): assert the ILLEGAL
/// outcome — that the completeness check finds NO gap in a matrix that is missing a
/// key. A biting check ALWAYS reports the dropped `Kill`, so this assertion is
/// FALSE and the red half FAILS, proving the gate is anti-vacuous.
#[cfg(gauntlet_red_fixture)]
#[test]
fn support_completeness_red_fixture_dropped_key_must_escape() {
    let gaps = check_completeness("linux-with-dropped-key", &matrix_missing_one_key());
    assert!(
        gaps.is_empty(),
        "RED FIXTURE: asserts the (illegal) no-gap outcome on a matrix missing Kill; MUST fail \
         because a biting completeness check always catches the dropped key"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Deliverable 3 — SNAPSHOT IS ASPIRATION, only the ceiling is coupled (documenting).
// ════════════════════════════════════════════════════════════════════════════

/// DOCUMENTING (deliverable #3): the §1 two-axis design, made EXPLICIT so it cannot
/// be misread as a coupling bug. The S1 `coupling_proof` gate already couples the
/// PRODUCTION CEILING to the ledger (`production Enforced(k) ⟺ ledger Proven(k)`).
/// This asserts the COMPLEMENT the snapshot relies on: the aspiration
/// `support_matrix()` MAY advertise `Enforced` for a cell the production ceiling
/// does NOT yet back as Proven — that is the §1 two-axis design, NOT a lie.
///
/// Concretely: linux's aspiration matrix advertises `NetworkDenyAll` at `Enforced`,
/// yet §10 records it FAIL_CLOSED in the production ceiling (no Proven ledger row).
/// The coupling gate lives on the CEILING (see `coupling_proof.rs`), not on this
/// aspiration table — so this gap is expected and witnessed here, with NO new
/// coupling code (the S1 gate already fully covers the ceiling↔ledger coupling).
#[test]
fn aspiration_matrix_may_outrun_the_proven_ceiling() {
    let linux = linux::support_matrix();
    // The aspiration table CLAIMS NetworkDenyAll Enforced ...
    assert_eq!(
        linux
            .best_case_for(RequirementKind::NetworkDenyAll)
            .enforcement,
        Enforcement::Enforced,
        "the aspiration matrix advertises NetworkDenyAll Enforced (the §1 aspiration claim)"
    );
    // ... and that is permitted precisely because the coupling law binds the
    // PRODUCTION CEILING (coupling_proof.rs), not this aspiration surface. The
    // snapshot is the aspiration FLOOR (capability_snapshot.yaml), independently
    // guarded for drift by the `capability-snapshot` gate. No duplicate coupling
    // assertion is added here: S1's `coupling_proof` already proves the ceiling
    // side end-to-end.
    assert!(
        linux.declares(RequirementKind::NetworkDenyAll),
        "the aspiration claim is an EXPLICIT, declared cell (auditable), not a silent default"
    );
}
