use super::{check, derive_for_test, MATRIX_REL};
use crate::repo_surface::repo_root;

#[test]
fn committed_matrix_mirrors_source() {
    let repo = repo_root().expect("repo root");
    check(&repo).expect("committed platform qualification matrix must mirror source");
}

#[test]
fn incomplete_status_is_rejected() {
    let repo = repo_root().expect("repo root");
    let mut matrix = derive_for_test();
    matrix.cells[0].status = "incomplete".to_string();
    let err = super::validate_matrix(&repo, &matrix).expect_err("incomplete must fail");
    assert!(
        err.to_string().contains("incomplete"),
        "error must mention incomplete, got: {err:#}"
    );
}

#[test]
fn proven_without_receipts_is_rejected() {
    let repo = repo_root().expect("repo root");
    let mut matrix = derive_for_test();
    for cell in &mut matrix.cells {
        if cell.status == "proven" {
            cell.proof_receipts.clear();
            let err = super::validate_matrix(&repo, &matrix)
                .expect_err("proven without receipts must fail");
            assert!(
                err.to_string().contains("proof_receipts"),
                "error must mention proof_receipts, got: {err:#}"
            );
            return;
        }
    }
    assert!(
        std::hint::black_box(false),
        "PROPERTY: derive matrix must include a proven cell"
    );
}

#[test]
fn fail_closed_native_runner_is_rejected() {
    let repo = repo_root().expect("repo root");
    let mut matrix = derive_for_test();
    let cell = matrix
        .cells
        .iter_mut()
        .find(|c| c.status == "fail-closed" && c.backend == "windows")
        .expect("derive matrix must include a windows fail-closed cell");
    cell.runner = "windows-native".to_string();
    let err =
        super::validate_matrix(&repo, &matrix).expect_err("native runner on fail-closed must fail");
    assert!(
        err.to_string().contains("contract-any"),
        "error must mention contract-any, got: {err:#}"
    );
}

#[test]
fn mirror_drift_fails_check() {
    let repo = repo_root().expect("repo root");
    let committed = std::fs::read_to_string(repo.join(MATRIX_REL)).expect("read matrix");
    let mut derived = derive_for_test();
    let cell = derived
        .cells
        .iter_mut()
        .find(|c| c.status == "proven")
        .expect("derive matrix must include a proven cell");
    cell.status = "fail-closed".to_string();
    cell.proof_receipts.clear();
    let err = super::assert_mirror(&committed, &derived).expect_err("drift must fail mirror check");
    assert!(
        err.to_string().contains("STALE"),
        "error must mention STALE, got: {err:#}"
    );
}
