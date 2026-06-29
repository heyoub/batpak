use super::*;

#[test]
fn only_proven_permits_enforced() {
    assert!(QualificationStatus::Proven.permits_enforced());
    for s in [
        QualificationStatus::FailClosed,
        QualificationStatus::Incomplete,
        QualificationStatus::Waived,
        QualificationStatus::FaultInjected,
    ] {
        assert!(!s.permits_enforced(), "{s:?} must NOT permit Enforced");
    }
}

#[test]
fn coupling_law_blocks_unproven_enforced_only() {
    // Enforced requires Proven.
    assert!(enforced_claim_is_qualified(
        Enforcement::Enforced,
        QualificationStatus::Proven
    ));
    assert!(!enforced_claim_is_qualified(
        Enforcement::Enforced,
        QualificationStatus::Incomplete
    ));
    // Weaker claims are always admissible regardless of qualification.
    for q in [
        QualificationStatus::FailClosed,
        QualificationStatus::Incomplete,
        QualificationStatus::Waived,
    ] {
        assert!(enforced_claim_is_qualified(Enforcement::Mediated, q));
        assert!(enforced_claim_is_qualified(Enforcement::Unsupported, q));
    }
}

// ── MechanismDigest ──

#[test]
fn mechanism_digest_is_deterministic_and_distinguishes_mechanisms() {
    let a = MechanismDigest::of_mechanism("linux:landlock:Enforced");
    let a2 = MechanismDigest::of_mechanism("linux:landlock:Enforced");
    let b = MechanismDigest::of_mechanism("linux:cgroup_kill:Enforced");
    assert_eq!(a, a2, "same mechanism string ⇒ same digest");
    assert_ne!(a, b, "distinct mechanisms ⇒ distinct digests");
    // The hex form is the 64-char lowercase spelling of the 32 bytes.
    assert_eq!(a.to_hex().len(), 64);
    assert!(a.to_hex().chars().all(|c| c.is_ascii_hexdigit()));
}

// ── ProfileFloor: the §3 domination check ──

/// A small spread of representative profiles ordered by strength.
fn profile_samples() -> Vec<ProfileFacts> {
    vec![
        ProfileFacts {
            landlock_abi: 0,
            has_cgroup_kill: false,
            has_pids_peak: false,
            has_unprivileged_userns: false,
            has_seccomp_filter: false,
        },
        ProfileFacts {
            landlock_abi: 1,
            has_cgroup_kill: false,
            has_pids_peak: false,
            has_unprivileged_userns: false,
            has_seccomp_filter: false,
        },
        ProfileFacts {
            landlock_abi: 4,
            has_cgroup_kill: true,
            has_pids_peak: false,
            has_unprivileged_userns: true,
            has_seccomp_filter: true,
        },
        ProfileFacts {
            landlock_abi: 6,
            has_cgroup_kill: true,
            has_pids_peak: true,
            has_unprivileged_userns: true,
            has_seccomp_filter: true,
        },
    ]
}

fn dominates(strong: &ProfileFacts, weak: &ProfileFacts) -> bool {
    strong.landlock_abi >= weak.landlock_abi
        && (strong.has_cgroup_kill || !weak.has_cgroup_kill)
        && (strong.has_pids_peak || !weak.has_pids_peak)
        && (strong.has_unprivileged_userns || !weak.has_unprivileged_userns)
        && (strong.has_seccomp_filter || !weak.has_seccomp_filter)
}

/// THE §3 LAW: a floor EARNED at some profile is satisfied by every profile
/// that dominates it. We use each sample as both a hypothetical "earned-at"
/// profile (lowered into a floor) and as a candidate production profile.
#[test]
fn floor_earned_at_a_profile_is_satisfied_by_any_stronger_profile() {
    for earned_at in &profile_samples() {
        // The floor a qualification earned at `earned_at` covers: exactly that
        // profile's facts as minimums.
        let floor = ProfileFloor {
            landlock_abi_min: u8::try_from(earned_at.landlock_abi).ok(),
            requires_cgroup_kill: earned_at.has_cgroup_kill,
            requires_pids_peak: earned_at.has_pids_peak,
            requires_unprivileged_userns: earned_at.has_unprivileged_userns,
            requires_seccomp_filter: earned_at.has_seccomp_filter,
        };
        for prod in &profile_samples() {
            if dominates(prod, earned_at) {
                assert!(
                    floor.satisfied_by(prod),
                    "a stronger profile {prod:?} must satisfy the floor earned at \
                         {earned_at:?}",
                );
            }
        }
        // And the floor is always satisfied by the very profile it was earned at.
        assert!(
            floor.satisfied_by(earned_at),
            "the earned-at profile {earned_at:?} satisfies its own floor",
        );
    }
}

#[test]
fn floor_is_not_satisfied_by_a_weaker_profile() {
    // The Kill floor (cgroup.kill required) is NOT satisfied without cgroup.kill.
    let kill_floor = ProfileFloor {
        landlock_abi_min: None,
        requires_cgroup_kill: true,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
    };
    let no_cgroup = ProfileFacts {
        landlock_abi: 6,
        has_cgroup_kill: false,
        has_pids_peak: false,
        has_unprivileged_userns: true,
        has_seccomp_filter: true,
    };
    assert!(!kill_floor.satisfied_by(&no_cgroup));
    // The Filesystem floor (ABI ≥ 1) is NOT satisfied below the floor.
    let fs_floor = ProfileFloor {
        landlock_abi_min: Some(1),
        requires_cgroup_kill: false,
        requires_pids_peak: false,
        requires_unprivileged_userns: false,
        requires_seccomp_filter: false,
    };
    let no_landlock = ProfileFacts {
        landlock_abi: 0,
        has_cgroup_kill: true,
        has_pids_peak: true,
        has_unprivileged_userns: true,
        has_seccomp_filter: true,
    };
    assert!(!fs_floor.satisfied_by(&no_landlock));
    // The NetworkDenyAll floor (unprivileged userns+netns) is NOT satisfied without it.
    let netns_floor = ProfileFloor::unprivileged_userns_netns();
    let no_userns = ProfileFacts {
        landlock_abi: 6,
        has_cgroup_kill: true,
        has_pids_peak: true,
        has_unprivileged_userns: false,
        has_seccomp_filter: false,
    };
    assert!(
        !netns_floor.satisfied_by(&no_userns),
        "the empty-netns floor must NOT be satisfied without unprivileged userns+netns"
    );
}

// ── The committed ledger ──

#[test]
fn ledger_keys_are_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for row in LINUX_QUALIFICATION_LEDGER {
        assert!(seen.insert(row.key), "duplicate ledger key {:?}", row.key);
    }
}

#[test]
fn every_proven_row_cites_a_receipt_and_a_real_enforced_mechanism() {
    // NO FABRICATION: a Proven row MUST cite a receipt (path::fn). The digest
    // re-derives deterministically from the mechanism string the row commits.
    for row in LINUX_QUALIFICATION_LEDGER {
        if row.status == QualificationStatus::Proven {
            assert!(
                !row.proof_receipts.is_empty(),
                "Proven row {:?} must cite at least one receipt",
                row.key
            );
            for receipt in row.proof_receipts {
                assert!(
                    receipt.contains("::"),
                    "Proven row {:?} receipt must be `path::fn`, got {receipt:?}",
                    row.key,
                );
            }
            assert!(
                row.mechanism.ends_with(":Enforced"),
                "a Proven cell's mechanism must be the Enforced spelling: {:?}",
                row.mechanism
            );
            // The digest is derivable (no panic / placeholder).
            let _ = row.mechanism_digest();
        } else {
            // Non-proven rows must NOT carry a receipt masquerading as proof.
            assert!(
                row.proof_receipts.is_empty(),
                "non-Proven row {:?} must not cite a proof receipt",
                row.key
            );
        }
    }
}

// ── Anti-fabrication: every Proven receipt must resolve to a REAL `#[test]` ──
// Closes `Proven ⟹ a real oracle exists`. Runs on the DEFAULT build (no kernel,
// no feature gate) — it reads the cited test files as TEXT, so a Proven row that
// cites a ghost file/fn fails CI. Mirrors `docs_catalog::check_witness_tests`.

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve repo root from crates/bvisor")
}

/// `true` when `src` declares a fn named `fn_name` carrying a `#[test]`-family
/// attribute (`#[test]` / `#[tokio::test]` / `#[test_case…]`). A plain non-test
/// fn is rejected, so a Proven row cannot cite ordinary code.
fn source_declares_test_fn(src: &str, fn_name: &str) -> bool {
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        // Word-boundary match on `fn <name>` (tolerate `pub`/`async` prefixes).
        let needle = format!("fn {fn_name}");
        let Some(pos) = line.find(&needle) else {
            continue;
        };
        let after = &line[pos + needle.len()..];
        if !matches!(after.chars().next(), Some('(' | '<' | ' ' | '\t') | None) {
            continue; // `fn fooBar` when looking for `fn foo`
        }
        // Scan upward over the fn's attribute/comment block for a test attr.
        let mut j = i;
        while j > 0 {
            j -= 1;
            let t = lines[j].trim();
            if t.is_empty() || t.starts_with("//") {
                continue;
            }
            if t.starts_with("#[") {
                // Precise: `#[test]` or `…::test]` (tokio) or `test_case` — NOT
                // `#[cfg(test)]` (which would false-positive on "test").
                if t.starts_with("#[test]")
                    || t.starts_with("#[test ")
                    || t.contains("::test]")
                    || t.contains("test_case")
                {
                    return true;
                }
                continue; // other attr (e.g. #[cfg(...)]) — keep scanning up
            }
            break; // a non-attr, non-comment line — no test attr binds this fn
        }
    }
    false
}

/// Resolve one `path::fn` receipt to a real `#[test]` fn under `repo_root`.
fn resolve_receipt(repo_root: &std::path::Path, receipt: &str) -> Result<(), String> {
    let (rel, fn_name) = receipt
        .rsplit_once("::")
        .ok_or_else(|| format!("receipt is not `path::fn`: {receipt}"))?;
    let path = repo_root.join(rel);
    let src = std::fs::read_to_string(&path)
        .map_err(|e| format!("{rel}: cannot read cited file ({e})"))?;
    if source_declares_test_fn(&src, fn_name) {
        Ok(())
    } else {
        Err(format!("{rel}: declares no `#[test] fn {fn_name}`"))
    }
}

#[test]
fn every_proven_receipt_resolves_to_a_real_test() {
    let root = repo_root();
    let mut unresolved: Vec<String> = Vec::new();
    for row in LINUX_QUALIFICATION_LEDGER {
        if row.status != QualificationStatus::Proven {
            continue;
        }
        for receipt in row.proof_receipts {
            if let Err(e) = resolve_receipt(&root, receipt) {
                unresolved.push(format!("{:?}: {e}", row.key));
            }
        }
    }
    assert!(
        unresolved.is_empty(),
        "Proven rows cite receipts that do NOT resolve to a real #[test]: {unresolved:?}"
    );
}

#[test]
fn ghost_receipt_is_rejected() {
    // The anti-fabrication gate's red fixture: a Proven row citing a ghost file
    // OR a real file with no such #[test] fn MUST be rejected.
    let root = repo_root();
    let ghost_file = resolve_receipt(&root, "crates/bvisor/tests/GHOST_DOES_NOT_EXIST.rs::nope");
    assert!(ghost_file.is_err(), "a missing cited file must be rejected");
    let ghost_fn = resolve_receipt(
        &root,
        "crates/bvisor/tests/coupling_proof.rs::this_fn_is_not_declared_anywhere",
    );
    assert!(ghost_fn.is_err(), "a missing cited fn must be rejected");
    let malformed = resolve_receipt(&root, "no-colons-here");
    assert!(
        malformed.is_err(),
        "a non-`path::fn` receipt must be rejected"
    );
}

#[test]
fn ledger_lookup_finds_proven_and_nonproven_keys() {
    assert_eq!(
        linux_ledger_row(RequirementKind::Filesystem)
            .expect("Filesystem row present")
            .status,
        QualificationStatus::Proven
    );
    // NetworkDenyAll is Proven (S9: empty netns + the §4 dual-channel oracle).
    assert_eq!(
        linux_ledger_row(RequirementKind::NetworkDenyAll)
            .expect("NetworkDenyAll row present")
            .status,
        QualificationStatus::Proven
    );
    // NetworkAllowList STAYS FailClosed (no broker in v1).
    assert_eq!(
        linux_ledger_row(RequirementKind::NetworkAllowList)
            .expect("NetworkAllowList row present")
            .status,
        QualificationStatus::FailClosed
    );
    // A key not in the ledger resolves to None (e.g. TempRoot — never claimed).
    assert!(linux_ledger_row(RequirementKind::TempRoot).is_none());
}

#[test]
fn linux_mechanism_helper_matches_the_committed_proven_spellings() {
    // The helper builds the same `"linux:{primitive}:{enforcement:?}"` shape the
    // backend's `mechanism(..)` does, so the ledger's committed string and a
    // freshly-built one agree (the coupling test relies on this equality).
    assert_eq!(
        linux_mechanism("landlock", Enforcement::Enforced),
        "linux:landlock:Enforced"
    );
    assert_eq!(
        linux_mechanism("cgroup_kill", Enforcement::Enforced),
        "linux:cgroup_kill:Enforced"
    );
}
