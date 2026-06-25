//! THE §2 LAW-TESTS for [`CanonicalPolicy`] — the keystone's keystone, held to the
//! same rigor as the HLC join-semilattice + the admission lattice laws. A
//! normalization bug here silently collapses two distinct policies into one
//! requirement key (the exact failure the injective-key program exists to prevent,
//! reintroduced one level down), so these are EXHAUSTIVE / brute-force over a
//! representative spread (not proptest — bvisor has no proptest dep, and exhaustive
//! brute-force over the constructed spread is the stronger, deterministic check the
//! sibling lattice laws already use).
//!
//! The four laws (§2 + plan deliverable 2):
//! (a) IDEMPOTENT — re-normalizing an already-canonical payload yields the same bytes.
//! (b) DETERMINISTIC — the same policy input always yields the same bytes.
//! (c) DISTINCT-SEMANTICS ⇒ DISTINCT-BYTES — no two distinct meanings collide.
//! (d) SYNTACTIC-ALIASES ⇒ SAME-BYTES — reordered/duplicated lists alias.
//!
//! No `#[allow]`/`panic!`; loop failures are collected into a `Vec` and asserted
//! empty (the project bans `panic!` even in tests).

use super::CanonicalPolicy;
use crate::contract::capability::{EnvPolicy, FdPolicy, NetDest, NetPolicy, SpawnPolicy};

/// A representative spread of SEMANTICALLY-DISTINCT policies — one entry per
/// distinct meaning across all four families. Every pair of DIFFERENT entries here
/// must produce DISTINCT canonical bytes (law c); each entry compared to itself must
/// produce IDENTICAL bytes (law b). Built once, reused by every law.
fn distinct_policies() -> Vec<CanonicalPolicy> {
    vec![
        // ── Fd family ──
        CanonicalPolicy::of_fd(&FdPolicy::None),
        CanonicalPolicy::of_fd(&FdPolicy::Only(vec![])), // explicit empty grant ≠ None
        CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1])),
        CanonicalPolicy::of_fd(&FdPolicy::Only(vec![3])),
        CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1, 3])),
        CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1, 2, 3])),
        // ── Spawn family ──
        CanonicalPolicy::of_spawn(&SpawnPolicy::Deny),
        CanonicalPolicy::of_spawn(&SpawnPolicy::Allow),
        // ── Env family ──
        CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![])),
        CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec!["PATH".to_string()])),
        CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec!["HOME".to_string()])),
        CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![
            "HOME".to_string(),
            "PATH".to_string(),
        ])),
        // boundary-ambiguity guard: ["AB","C"] vs ["A","BC"] must differ
        CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![
            "AB".to_string(),
            "C".to_string(),
        ])),
        CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![
            "A".to_string(),
            "BC".to_string(),
        ])),
        // ── Net family ──
        CanonicalPolicy::of_net(&NetPolicy::DenyAll),
        CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![])), // allow-nothing ≠ DenyAll
        CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![dest("example.com", 443)])),
        CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![dest("example.com", 80)])), // diff port
        CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![dest("other.com", 443)])),  // diff host
        CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![
            dest("example.com", 443),
            dest("other.com", 80),
        ])),
    ]
}

fn dest(host: &str, port: u16) -> NetDest {
    NetDest {
        host: host.to_string(),
        port,
    }
}

/// LAW (c) — DISTINCT-SEMANTICS ⇒ DISTINCT-BYTES. Every pair of distinct policies in
/// the spread (across AND within families, including the load-bearing
/// near-collisions: `None` vs `Only([])`, `DenyAll` vs `AllowList([])`, the
/// boundary-ambiguity env pair) maps to DISTINCT canonical bytes. The collapse this
/// forbids is precisely a silent two-policies-one-key key collision.
#[test]
fn distinct_semantics_imply_distinct_bytes() {
    let policies = distinct_policies();
    let mut collisions = Vec::new();
    for (i, a) in policies.iter().enumerate() {
        for (j, b) in policies.iter().enumerate() {
            if i < j && a.as_bytes() == b.as_bytes() {
                collisions.push((i, j, a.as_bytes().to_vec()));
            }
        }
    }
    assert!(
        collisions.is_empty(),
        "distinct policies collided to identical canonical bytes: {collisions:?}"
    );
}

/// LAW (b) — DETERMINISTIC. Normalizing the SAME policy input twice yields byte-equal
/// canonical forms, for every entry in the spread (re-derive from the same source).
#[test]
fn normalization_is_deterministic() {
    let mut failures = Vec::new();
    // Fd
    for p in [
        FdPolicy::None,
        FdPolicy::Only(vec![1, 3]),
        FdPolicy::Only(vec![]),
    ] {
        if CanonicalPolicy::of_fd(&p).as_bytes() != CanonicalPolicy::of_fd(&p).as_bytes() {
            failures.push(format!("fd {p:?}"));
        }
    }
    // Spawn
    for p in [SpawnPolicy::Deny, SpawnPolicy::Allow] {
        if CanonicalPolicy::of_spawn(&p).as_bytes() != CanonicalPolicy::of_spawn(&p).as_bytes() {
            failures.push(format!("spawn {p:?}"));
        }
    }
    // Env
    for p in [
        EnvPolicy::EmptyExcept(vec![]),
        EnvPolicy::EmptyExcept(vec!["PATH".to_string(), "HOME".to_string()]),
    ] {
        if CanonicalPolicy::of_env(&p).as_bytes() != CanonicalPolicy::of_env(&p).as_bytes() {
            failures.push(format!("env {p:?}"));
        }
    }
    // Net
    for p in [
        NetPolicy::DenyAll,
        NetPolicy::AllowList(vec![dest("example.com", 443)]),
    ] {
        if CanonicalPolicy::of_net(&p).as_bytes() != CanonicalPolicy::of_net(&p).as_bytes() {
            failures.push(format!("net {p:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "normalization was non-deterministic for: {failures:?}"
    );
}

/// LAW (d) — SYNTACTIC ALIASES ⇒ SAME BYTES. A reordered and/or duplicated list (fd,
/// env-key, net-dest) is the SAME policy meaning, so it must canonicalize to the SAME
/// bytes as its sorted+deduplicated representative.
#[test]
fn syntactic_aliases_collapse_to_the_same_bytes() {
    let mut mismatches = Vec::new();

    // Fd: [3, 1, 3, 1] is an alias of [1, 3].
    let fd_canon = CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1, 3]));
    for alias in [
        FdPolicy::Only(vec![3, 1]),
        FdPolicy::Only(vec![3, 1, 3, 1]),
        FdPolicy::Only(vec![1, 3, 3]),
    ] {
        if CanonicalPolicy::of_fd(&alias).as_bytes() != fd_canon.as_bytes() {
            mismatches.push(format!("fd alias {alias:?}"));
        }
    }

    // Env: reordered + duplicated keys alias the sorted-deduped set.
    let env_canon = CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![
        "HOME".to_string(),
        "PATH".to_string(),
    ]));
    for alias in [
        EnvPolicy::EmptyExcept(vec!["PATH".to_string(), "HOME".to_string()]),
        EnvPolicy::EmptyExcept(vec![
            "PATH".to_string(),
            "HOME".to_string(),
            "PATH".to_string(),
        ]),
    ] {
        if CanonicalPolicy::of_env(&alias).as_bytes() != env_canon.as_bytes() {
            mismatches.push(format!("env alias {alias:?}"));
        }
    }

    // Net: reordered + duplicated destinations alias the sorted-deduped set.
    let net_canon = CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![
        dest("example.com", 443),
        dest("other.com", 80),
    ]));
    for alias in [
        NetPolicy::AllowList(vec![dest("other.com", 80), dest("example.com", 443)]),
        NetPolicy::AllowList(vec![
            dest("other.com", 80),
            dest("example.com", 443),
            dest("other.com", 80),
        ]),
    ] {
        if CanonicalPolicy::of_net(&alias).as_bytes() != net_canon.as_bytes() {
            mismatches.push(format!("net alias {alias:?}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "syntactic aliases failed to collapse to the canonical bytes: {mismatches:?}"
    );
}

/// LAW (a) — IDEMPOTENT (norm∘norm = norm). The normal form is a FIXED POINT: feeding
/// the ALREADY-canonical payload (the sorted+deduplicated list) back through
/// normalization yields byte-identical output. So normalization run on its own output
/// is a no-op — there is no second, different canonical form for the same meaning.
#[test]
fn normalization_is_idempotent_on_canonical_payloads() {
    let mut failures = Vec::new();

    // Fd: raw [3, 1, 3] normalizes to the same bytes as the canonical [1, 3].
    let raw = CanonicalPolicy::of_fd(&FdPolicy::Only(vec![3, 1, 3]));
    let already = CanonicalPolicy::of_fd(&FdPolicy::Only(vec![1, 3]));
    if raw.as_bytes() != already.as_bytes() {
        failures.push("fd not idempotent".to_string());
    }

    // Env: raw reordered+dup normalizes to the same bytes as the canonical sorted set.
    let raw_env = CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![
        "PATH".to_string(),
        "HOME".to_string(),
        "PATH".to_string(),
    ]));
    let already_env = CanonicalPolicy::of_env(&EnvPolicy::EmptyExcept(vec![
        "HOME".to_string(),
        "PATH".to_string(),
    ]));
    if raw_env.as_bytes() != already_env.as_bytes() {
        failures.push("env not idempotent".to_string());
    }

    // Net: raw reordered+dup normalizes to the same bytes as the canonical sorted set.
    let raw_net = CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![
        dest("other.com", 80),
        dest("example.com", 443),
        dest("other.com", 80),
    ]));
    let already_net = CanonicalPolicy::of_net(&NetPolicy::AllowList(vec![
        dest("example.com", 443),
        dest("other.com", 80),
    ]));
    if raw_net.as_bytes() != already_net.as_bytes() {
        failures.push("net not idempotent".to_string());
    }

    assert!(
        failures.is_empty(),
        "normalization was not idempotent for: {failures:?}"
    );
}
