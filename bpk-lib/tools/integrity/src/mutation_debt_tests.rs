//! Tests for the mutation-debt ledger consumer, including the anti-vacuous RED
//! fixture (`malformed_mutation_debt_entry_is_rejected`).

use super::*;
use crate::assurance::load_manifest;
use crate::repo_surface::repo_root;

fn repo() -> std::path::PathBuf {
    repo_root().expect("repo root resolves from tools/integrity")
}

fn assurance_entries() -> Vec<crate::assurance::AssuranceEntry> {
    load_manifest(&repo()).expect("load assurance manifest for mutation-debt tests")
}

/// A well-formed entry pointing at a real tracked source file.
fn valid_entry() -> DebtEntry {
    DebtEntry {
        mutant: "replace == with != in unregister".to_owned(),
        file: "tools/integrity/src/mutation_debt.rs".to_owned(),
        line: 1,
        seam: "projection-flow".to_owned(),
        first_seen: "2026-06-20".to_owned(),
        reason: "no falsifying test yet — tracked in this debt ledger".to_owned(),
        proof: None,
    }
}

#[test]
fn empty_ledger_passes() {
    validate_entries(&repo(), &[], &assurance_entries())
        .expect("an empty debt ledger must validate");
}

#[test]
fn well_formed_entry_passes() {
    validate_entries(&repo(), &[valid_entry()], &assurance_entries())
        .expect("a well-formed entry must validate");
}

/// THE RED FIXTURE: a malformed entry (bad date, missing file, empty field, or
/// zero line) must be rejected. If any of these pass, the schema gate is vacuous.
#[test]
fn malformed_mutation_debt_entry_is_rejected() {
    let assurance = assurance_entries();
    let bad_date = DebtEntry {
        first_seen: "June 20".to_owned(),
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[bad_date], &assurance).is_err(),
        "a non-ISO first_seen must be rejected"
    );

    let missing_file = DebtEntry {
        file: "crates/core/src/THIS_FILE_DOES_NOT_EXIST.rs".to_owned(),
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[missing_file], &assurance).is_err(),
        "an entry naming a nonexistent file must be rejected (stale debt)"
    );

    let empty_mutant = DebtEntry {
        mutant: "   ".to_owned(),
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[empty_mutant], &assurance).is_err(),
        "an empty mutant description must be rejected"
    );

    let zero_line = DebtEntry {
        line: 0,
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[zero_line], &assurance).is_err(),
        "a zero line number must be rejected"
    );
}

/// RED fixture (#64-A): an L4 seam survivor without `proof:` must hard-fail.
#[test]
fn l4_survivor_without_proof_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let l4_no_proof = DebtEntry {
        seam: "hash-chain-replay".to_owned(),
        file: "crates/core/src/store/chain_walk.rs".to_owned(),
        ..valid_entry()
    };
    let err = match validate_entries(&repo(), &[l4_no_proof], &assurance_entries()) {
        Ok(()) => {
            return Err(std::io::Error::other(
                "PROPERTY: L4 survivor without proof must be rejected",
            )
            .into());
        }
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("hash-chain-replay") && msg.contains("proof"),
        "error must name the L4 seam and missing proof, got: {msg}"
    );
    Ok(())
}

/// GREEN fixture (#64-A): an L4 seam survivor with non-empty `proof:` passes.
#[test]
fn l4_survivor_with_proof_passes() {
    let l4_proven = DebtEntry {
        seam: "hash-chain-replay".to_owned(),
        file: "crates/core/src/store/chain_walk.rs".to_owned(),
        proof: Some(
            "equivalent mutant: mutation only affects debug formatting, not chain walk semantics"
                .to_owned(),
        ),
        ..valid_entry()
    };
    validate_entries(&repo(), &[l4_proven], &assurance_entries())
        .expect("L4 survivor with proof must validate");
}

#[test]
fn is_iso_date_accepts_only_yyyy_mm_dd() {
    assert!(is_iso_date("2026-06-20"));
    assert!(is_iso_date("1999-12-31"));
    assert!(!is_iso_date("2026-6-20"), "month must be zero-padded");
    assert!(!is_iso_date("2026-13-01"), "month out of range");
    assert!(!is_iso_date("2026-06-32"), "day out of range");
    assert!(!is_iso_date("2026/06/20"), "wrong separator");
    assert!(!is_iso_date("June 20, 2026"));
    assert!(!is_iso_date(""));
}

#[test]
fn seam_assurance_level_joins_manifest_seams() {
    let entries = assurance_entries();
    assert_eq!(
        seam_assurance_level("hash-chain-replay", &entries),
        crate::assurance::AssuranceLevel::L4
    );
    assert_eq!(
        seam_assurance_level("projection-flow", &entries),
        crate::assurance::AssuranceLevel::L3
    );
    assert_eq!(
        seam_assurance_level("repo-wide", &entries),
        crate::assurance::DEFAULT_LEVEL
    );
}

/// The live committed ledger parses and validates (it is the empty list today;
/// this keeps it honest as entries are added).
#[test]
fn live_committed_ledger_is_valid() {
    check(&repo()).expect("the committed mutation_debt.yaml must parse and schema-validate");
}
