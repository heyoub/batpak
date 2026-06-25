//! Contract-gate tests for [`EnvPolicy::Exact`] validation (proof-spine §5 D2).
//!
//! These pin the FAIL-CLOSED contract: a well-formed table validates; every
//! malformed class (empty/reserved-byte name, NUL value, duplicate, over-cap entry
//! count, over-cap byte budget) is REFUSED at the FIRST violation. The project bans
//! `panic!` even in tests, so loop failures collect into a `Vec` asserted empty and
//! single assertions use `assert!`/`assert_eq!` (never `panic!`).

use super::{EnvEntry, EnvPolicy, EnvPolicyError, EnvSource, SecretRef, MAX_ENV_ENTRIES};

fn lit(name: &str, value: &str) -> EnvEntry {
    EnvEntry::literal(name, value)
}

#[test]
fn a_well_formed_exact_table_validates() {
    let policy = EnvPolicy::Exact(vec![
        lit("PATH", "/usr/bin:/bin"),
        lit("LANG", "C.UTF-8"),
        EnvEntry::lease("TOKEN", SecretRef::new("vault://lease/abc")),
    ]);
    assert_eq!(policy.validate(), Ok(()));
}

#[test]
fn an_empty_table_validates() {
    // The minimal exact environment: nothing. A child gets a genuinely empty env.
    assert_eq!(EnvPolicy::Exact(Vec::new()).validate(), Ok(()));
}

#[test]
fn an_empty_name_is_refused() {
    let policy = EnvPolicy::Exact(vec![lit("", "x")]);
    assert_eq!(policy.validate(), Err(EnvPolicyError::EmptyName));
}

#[test]
fn a_name_with_equals_or_nul_is_refused() {
    let mut failures = Vec::new();
    for (name, byte) in [("A=B", b'='), ("A\0B", 0u8)] {
        let got = EnvPolicy::Exact(vec![lit(name, "x")]).validate();
        if got
            != Err(EnvPolicyError::NameHasReservedByte {
                name: name.to_string(),
                byte,
            })
        {
            failures.push(format!("name {name:?} byte {byte:#04x} got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "reserved-byte names: {failures:?}");
}

#[test]
fn a_literal_value_with_a_nul_is_refused() {
    let policy = EnvPolicy::Exact(vec![lit("K", "a\0b")]);
    assert_eq!(
        policy.validate(),
        Err(EnvPolicyError::ValueHasNul {
            name: "K".to_string()
        })
    );
}

#[test]
fn a_case_sensitive_duplicate_name_is_refused_but_case_variants_are_distinct() {
    // Exact duplicate ⇒ refused.
    let dup = EnvPolicy::Exact(vec![lit("PATH", "/a"), lit("PATH", "/b")]);
    assert_eq!(
        dup.validate(),
        Err(EnvPolicyError::DuplicateName {
            name: "PATH".to_string()
        })
    );
    // Case-sensitive: PATH and path are DISTINCT names, both admissible.
    let distinct = EnvPolicy::Exact(vec![lit("PATH", "/a"), lit("path", "/b")]);
    assert_eq!(distinct.validate(), Ok(()));
}

#[test]
fn a_lease_does_not_carry_a_value_and_validates() {
    // A SecretLease entry carries only an opaque ref — never a value — and is valid
    // even though its resolved value is unknown at admission.
    let policy = EnvPolicy::Exact(vec![EnvEntry::lease(
        "DB_PASSWORD",
        SecretRef::new("lease://db/primary"),
    )]);
    assert_eq!(policy.validate(), Ok(()));
    // The ref is opaque: it is the only thing carried by the source.
    let EnvPolicy::Exact(entries) = &policy;
    assert!(
        matches!(&entries[0].source, EnvSource::SecretLease(r) if r.id() == "lease://db/primary"),
        "expected a SecretLease carrying only the opaque ref, got {:?}",
        entries[0].source
    );
}

#[test]
fn over_cap_entry_count_is_refused() {
    let entries: Vec<EnvEntry> = (0..=MAX_ENV_ENTRIES)
        .map(|i| lit(&format!("K{i}"), "v"))
        .collect();
    let found = entries.len();
    assert_eq!(
        EnvPolicy::Exact(entries).validate(),
        Err(EnvPolicyError::TooManyEntries { found })
    );
}

#[test]
fn over_cap_total_bytes_is_refused() {
    // One entry whose value alone exceeds the byte budget.
    let big = "x".repeat(super::MAX_ENV_TOTAL_BYTES + 1);
    let result = EnvPolicy::Exact(vec![lit("BIG", &big)]).validate();
    assert!(
        matches!(result, Err(EnvPolicyError::TooManyBytes { .. })),
        "an over-budget value must be refused, got {result:?}"
    );
}
