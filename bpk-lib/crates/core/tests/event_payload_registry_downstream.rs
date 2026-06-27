//! PROVES: cross-crate EventPayload registry collisions are visible to a composing downstream binary.
//! CATCHES: dependency-crate `inventory::submit!` registrations lost behind `cfg(test)` boundaries.
//! SEEDED: deterministic / no randomness.

use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn downstream_fixture_detects_dependency_event_kind_collision() {
    run_downstream_fixture(&["test", "--quiet"]);
}

#[test]
fn downstream_fixture_detects_dependency_event_kind_collision_in_release() {
    run_downstream_fixture(&["test", "--release", "--quiet"]);
}

fn run_downstream_fixture(args: &[&str]) {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = manifest_dir.join("fixtures/kind-collision-composer/Cargo.toml");
    assert!(
        manifest.exists(),
        "downstream fixture manifest is missing from repo checkout: {}",
        manifest.display()
    );
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(args)
        .arg("--manifest-path")
        .arg(&manifest)
        .env(
            "CARGO_TARGET_DIR",
            downstream_fixture_target_dir(&manifest_dir),
        )
        .output()
        .expect("failed to run downstream fixture");
    assert!(
        output.status.success(),
        "downstream fixture failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn downstream_fixture_target_dir(manifest_dir: &std::path::Path) -> PathBuf {
    downstream_fixture_target_dir_with_env(
        manifest_dir,
        std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from),
    )
}

fn downstream_fixture_target_dir_with_env(
    manifest_dir: &Path,
    configured_target_dir: Option<PathBuf>,
) -> PathBuf {
    let workspace_root = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap_or(manifest_dir);
    let root_target = match configured_target_dir {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join("target"),
    };
    root_target.join("downstream-fixtures")
}

#[test]
fn downstream_fixture_target_dir_defaults_to_workspace_target_not_repo_root() {
    let manifest_dir = PathBuf::from("/repo/bpk-lib/crates/core");
    let target = downstream_fixture_target_dir_with_env(&manifest_dir, None);
    assert_eq!(
        target,
        PathBuf::from("/repo/bpk-lib/target/downstream-fixtures")
    );
}
