//! THE INJECTIVE GATE (proof-spine §2 deliverable 4 — the seed of S3's P0-A collapse
//! gate).
//!
//! GRANULARITY (load-bearing): a [`RequirementKind`] is the VARIANT-LEVEL projection
//! of a [`CanonicalPolicy`] — the family tag + variant discriminant (the canonical
//! bytes' two-byte prefix), NOT the full payload. `InheritedFdsOnly` is one key for
//! every `Only(..)` fd list; `Environment` is one key for every `EmptyExcept(..)`
//! key set. The FULL payload-level injectivity (distinct fd lists ⇒ distinct
//! canonical bytes) is [`CanonicalPolicy`]'s OWN §2 law, proven exhaustively in
//! `canonical_policy_tests.rs`. This gate proves the COMPLEMENTARY half — that the
//! policy→key map respects the canonical VARIANT, with no policy-blind collapse
//! across variants:
//!
//! (i)  WELL-DEFINED: two policies with the SAME canonical VARIANT (same two-byte
//!      prefix) map to the SAME key — the projection is a function, AND
//! (ii) INJECTIVE-ON-VARIANTS: two policies that map to the SAME key have the SAME
//!      canonical variant — no key fuses two distinct variants (the regression this
//!      catches is re-merging `InheritedFds::None`/`::Only` or
//!      `ChildSpawn::Deny`/`::Allow` into one policy-blind key).
//!
//! Kept a focused brute-force over a representative spread (the project bans `panic!`
//! even in tests, so failures collect into a `Vec` asserted empty).

use super::RequirementKind;
use crate::contract::canonical_policy::CanonicalPolicy;
use crate::contract::capability::{
    Capability, EnvPolicy, FdPolicy, NetDest, NetPolicy, SpawnPolicy,
};

/// One sample: a capability, its policy-aware key, and the canonical bytes of its
/// policy. The spread covers EVERY canonical-policy variant the S2 keys distinguish
/// (both `ChildSpawn` variants, both `InheritedFds` variants, both `Network`
/// variants, the single `Environment` variant) PLUS syntactic aliases (reordered
/// fd/env/dest lists) that must share BOTH the key and the canonical bytes.
fn samples() -> Vec<(&'static str, RequirementKind, CanonicalPolicy)> {
    let dest = |host: &str, port| NetDest {
        host: host.to_string(),
        port,
    };
    vec![
        // ── InheritedFds: None vs Only (distinct keys) ──
        (
            "fd-none",
            RequirementKind::of_capability_for_test(&Capability::InheritedFds {
                policy: FdPolicy::None,
            }),
            CanonicalPolicy::of_fd(&FdPolicy::None),
        ),
        (
            "fd-only-empty",
            RequirementKind::of_capability_for_test(&Capability::InheritedFds {
                policy: FdPolicy::Only(vec![]),
            }),
            CanonicalPolicy::of_fd(&FdPolicy::Only(vec![])),
        ),
        (
            "fd-only-13",
            RequirementKind::of_capability_for_test(&Capability::InheritedFds {
                policy: FdPolicy::Only(vec![1, 3]),
            }),
            CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1, 3])),
        ),
        (
            "fd-only-13-alias",
            RequirementKind::of_capability_for_test(&Capability::InheritedFds {
                policy: FdPolicy::Only(vec![3, 1, 3]),
            }),
            CanonicalPolicy::of_fd(&FdPolicy::Only(vec![3, 1, 3])),
        ),
        // ── ChildSpawn: Deny vs Allow (distinct keys) ──
        (
            "spawn-deny",
            RequirementKind::of_capability_for_test(&Capability::ChildSpawn {
                policy: SpawnPolicy::Deny,
            }),
            CanonicalPolicy::of_spawn(&SpawnPolicy::Deny),
        ),
        (
            "spawn-allow",
            RequirementKind::of_capability_for_test(&Capability::ChildSpawn {
                policy: SpawnPolicy::Allow,
            }),
            CanonicalPolicy::of_spawn(&SpawnPolicy::Allow),
        ),
        // ── Environment: one variant, one key ──
        (
            "env-empty",
            RequirementKind::of_capability_for_test(&Capability::Environment {
                policy: EnvPolicy::EmptyExcept(vec![]),
            }),
            CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![])),
        ),
        (
            "env-keys",
            RequirementKind::of_capability_for_test(&Capability::Environment {
                policy: EnvPolicy::EmptyExcept(vec!["PATH".to_string()]),
            }),
            CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec!["PATH".to_string()])),
        ),
        // ── Network: DenyAll vs AllowList (distinct keys) ──
        (
            "net-deny",
            RequirementKind::of_capability_for_test(&Capability::Network {
                policy: NetPolicy::DenyAll,
            }),
            CanonicalPolicy::of_net(&NetPolicy::DenyAll),
        ),
        (
            "net-allow",
            RequirementKind::of_capability_for_test(&Capability::Network {
                policy: NetPolicy::AllowList(vec![dest("example.com", 443)]),
            }),
            CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![dest("example.com", 443)])),
        ),
    ]
}

/// The canonical VARIANT a [`RequirementKind`] projects from: the family tag +
/// variant discriminant (the canonical bytes' two-byte prefix). Two policies share a
/// key iff they share this prefix; the bytes BEYOND it are the payload the key
/// deliberately abstracts over (and which `CanonicalPolicy`'s own law keeps
/// injective). Every encoding writes a family byte then a variant byte, so the slice
/// is always well-formed.
fn variant(policy: &CanonicalPolicy) -> &[u8] {
    let bytes = policy.as_bytes();
    &bytes[..bytes.len().min(2)]
}

/// (i) WELL-DEFINED FUNCTION: any two samples with the SAME canonical VARIANT must
/// map to the SAME key. A violation would mean one policy variant resolves to two
/// keys — a non-deterministic classification.
#[test]
fn equal_canonical_variants_map_to_one_key() {
    let samples = samples();
    let mut violations = Vec::new();
    for (na, ka, ca) in &samples {
        for (nb, kb, cb) in &samples {
            if variant(ca) == variant(cb) && ka != kb {
                violations.push(format!(
                    "{na} and {nb} share a canonical variant but map to {ka:?} vs {kb:?}"
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "policy→key is not a function (one canonical variant, two keys): {violations:?}"
    );
}

/// (ii) INJECTIVE ON VARIANTS: any two samples that map to the SAME key must share
/// the SAME canonical VARIANT. The contrapositive is the §2 law's forward direction
/// at variant granularity — distinct canonical variants never share a key. THIS is
/// the gate that catches a regressed policy-blind collapse (e.g. re-merging
/// `InheritedFds::None`/`::Only` or `ChildSpawn::Deny`/`::Allow` into one key).
#[test]
fn one_key_implies_one_canonical_variant() {
    let samples = samples();
    let mut collisions = Vec::new();
    for (na, ka, ca) in &samples {
        for (nb, kb, cb) in &samples {
            if ka == kb && variant(ca) != variant(cb) {
                collisions.push(format!(
                    "{na} and {nb} share key {ka:?} but have DISTINCT canonical variants"
                ));
            }
        }
    }
    assert!(
        collisions.is_empty(),
        "a key is shared by two distinct canonical variants: {collisions:?}"
    );
}

/// NON-VACUITY + the load-bearing SPLITS: the formerly policy-blind kinds really do
/// split into distinct keys now, and the alias really does collapse. Without this a
/// degenerate spread (e.g. all-one-key) could pass the two laws above vacuously.
#[test]
fn the_policy_aware_splits_are_real_and_aliases_collapse() {
    // The split keys are genuinely distinct.
    assert_ne!(
        RequirementKind::of_capability_for_test(&Capability::InheritedFds {
            policy: FdPolicy::None
        }),
        RequirementKind::of_capability_for_test(&Capability::InheritedFds {
            policy: FdPolicy::Only(vec![])
        }),
        "InheritedFds None vs Only must be distinct keys"
    );
    assert_ne!(
        RequirementKind::of_capability_for_test(&Capability::ChildSpawn {
            policy: SpawnPolicy::Deny
        }),
        RequirementKind::of_capability_for_test(&Capability::ChildSpawn {
            policy: SpawnPolicy::Allow
        }),
        "ChildSpawn Deny vs Allow must be distinct keys"
    );
    // A syntactic alias collapses to the same key AND the same canonical bytes.
    let canon = RequirementKind::of_capability_for_test(&Capability::InheritedFds {
        policy: FdPolicy::Only(vec![1, 3]),
    });
    let alias = RequirementKind::of_capability_for_test(&Capability::InheritedFds {
        policy: FdPolicy::Only(vec![3, 1, 3]),
    });
    assert_eq!(canon, alias, "fd-list aliases must share one key");
    assert_eq!(
        CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1, 3])).as_bytes(),
        CanonicalPolicy::of_fd(&FdPolicy::Only(vec![3, 1, 3])).as_bytes(),
        "fd-list aliases must share canonical bytes"
    );
}
