//! Red fixtures for the release terminal-status gate.

use super::{
    check, check_unresolved_justifies, discover_release_files, extract_justifies_invariants,
    ReleaseCheckOptions,
};
use crate::repo_surface::{ensure, repo_root};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn temp_release_root(name: &str) -> Result<PathBuf, std::io::Error> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!("batpak-release-status-{name}-{nanos}"));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(root.join("traceability/releases"))?;
    Ok(root)
}

fn write_release(root: &Path, yaml: &str) -> Result<(), std::io::Error> {
    fs::write(root.join("traceability/releases/0.9.0.yaml"), yaml)
}

fn write_invariants(root: &Path, ids: &[&str]) -> Result<(), std::io::Error> {
    fs::create_dir_all(root.join("traceability"))?;
    let mut body = String::new();
    for id in ids {
        body.push_str(&format!("- id: {id}\n  statement: fixture\n"));
    }
    fs::write(root.join("traceability/invariants.yaml"), body)
}

fn write_seam_registry(root: &Path) -> Result<(), std::io::Error> {
    fs::write(root.join("traceability/seam_registry.yaml"), "[]")
}

#[test]
fn release_status_strict_rejects_incomplete_terminal_row() -> Result<(), Box<dyn std::error::Error>>
{
    let root = temp_release_root("strict-incomplete")?;
    write_invariants(&root, &["INV-FIXTURE"])?;
    write_seam_registry(&root)?;
    write_release(
        &root,
        r#"release: "0.9.0"
active: true
rows:
  - id: STORE-FIXTURE-BLOCKER
    title: fixture blocker
    status: INCOMPLETE
    terminal_required: true
    surface: batpak
"#,
    )?;
    let err = match check(
        &root,
        &ReleaseCheckOptions {
            strict: true,
            target: Some("0.9.0".to_owned()),
            active_only: false,
        },
    ) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: strict mode must reject terminal_required INCOMPLETE rows",
            )
            .into())
        }
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("terminal_required"),
        "wrong error: {err:#}"
    );
    Ok(())
}

#[test]
fn release_status_rejects_proven_without_witness_refs() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_release_root("proven-no-witness")?;
    write_invariants(&root, &["INV-FIXTURE"])?;
    write_seam_registry(&root)?;
    write_release(
        &root,
        r#"release: "0.9.0"
active: true
rows:
  - id: STORE-FIXTURE-PROVEN
    title: proven without witnesses
    status: PROVEN
    terminal_required: false
    surface: batpak
"#,
    )?;
    let err = match check(&root, &ReleaseCheckOptions::structural()) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: PROVEN rows must cite witness refs").into(),
            )
        }
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("requires at least one witness"),
        "wrong error: {err:#}"
    );
    Ok(())
}

#[test]
fn unresolved_justifies_header_fails_when_inv_missing_from_catalog(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_release_root("justifies-missing-inv")?;
    write_invariants(&root, &["INV-REAL"])?;
    fs::create_dir_all(root.join("crates/core/tests"))?;
    fs::write(
        root.join("crates/core/tests/planted_justifies.rs"),
        "//! justifies: INV-MISSING-FROM-CATALOG\n#[test]\nfn t() {}\n",
    )?;
    let catalog: BTreeSet<String> = BTreeSet::from(["INV-REAL".to_owned()]);
    let unresolved = check_unresolved_justifies(&root, &catalog)?;
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].1, "INV-MISSING-FROM-CATALOG");
    Ok(())
}

#[test]
fn extract_justifies_invariants_reads_test_header() {
    let found = extract_justifies_invariants(
        "//! justifies: INV-FORK-CRASH-ATOMIC\n// unrelated INV-OTHER mention ignored unless justified",
    );
    assert!(found.contains("INV-FORK-CRASH-ATOMIC"));
    assert!(!found.contains("INV-OTHER"));
}

#[test]
fn committed_release_ledger_passes_structural_validation() -> Result<(), Box<dyn std::error::Error>>
{
    let root = repo_root()?;
    discover_release_files(&root)?;
    check(&root, &ReleaseCheckOptions::structural())?;
    Ok(())
}

#[test]
fn committed_active_release_has_exactly_one_active_file() -> Result<(), Box<dyn std::error::Error>>
{
    let root = repo_root()?;
    let files = discover_release_files(&root)?;
    let active = files
        .into_iter()
        .filter(|path| {
            fs::read_to_string(path)
                .map(|text| text.contains("active: true"))
                .unwrap_or(false)
        })
        .count();
    ensure(
        active == 1,
        format!("expected exactly one active release ledger, found {active}"),
    )?;
    Ok(())
}
