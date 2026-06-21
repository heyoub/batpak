//! Tests for the mutation-debt ledger consumer, including the anti-vacuous RED
//! fixture (`malformed_mutation_debt_entry_is_rejected`).

use super::*;
use crate::repo_surface::repo_root;

fn repo() -> std::path::PathBuf {
    repo_root().expect("repo root resolves from tools/integrity")
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
    }
}

#[test]
fn empty_ledger_passes() {
    validate_entries(&repo(), &[]).expect("an empty debt ledger must validate");
}

#[test]
fn well_formed_entry_passes() {
    validate_entries(&repo(), &[valid_entry()]).expect("a well-formed entry must validate");
}

/// THE RED FIXTURE: a malformed entry (bad date, missing file, empty field, or
/// zero line) must be rejected. If any of these pass, the schema gate is vacuous.
#[test]
fn malformed_mutation_debt_entry_is_rejected() {
    let bad_date = DebtEntry {
        first_seen: "June 20".to_owned(),
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[bad_date]).is_err(),
        "a non-ISO first_seen must be rejected"
    );

    let missing_file = DebtEntry {
        file: "crates/core/src/THIS_FILE_DOES_NOT_EXIST.rs".to_owned(),
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[missing_file]).is_err(),
        "an entry naming a nonexistent file must be rejected (stale debt)"
    );

    let empty_mutant = DebtEntry {
        mutant: "   ".to_owned(),
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[empty_mutant]).is_err(),
        "an empty mutant description must be rejected"
    );

    let zero_line = DebtEntry {
        line: 0,
        ..valid_entry()
    };
    assert!(
        validate_entries(&repo(), &[zero_line]).is_err(),
        "a zero line number must be rejected"
    );
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

/// The live committed ledger parses and validates (it is the empty list today;
/// this keeps it honest as entries are added).
#[test]
fn live_committed_ledger_is_valid() {
    check(&repo()).expect("the committed mutation_debt.yaml must parse and schema-validate");
}
