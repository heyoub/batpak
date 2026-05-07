// justifies: INV-TEST-PANIC-AS-ASSERTION, ADR-0010; this test shells out to a downstream fixture and panics only on fixture execution failure.
#![allow(clippy::panic)]
//! PROVES: cross-crate EventPayload registry collisions are visible to a composing downstream binary.
//! CATCHES: dependency-crate `inventory::submit!` registrations lost behind `cfg(test)` boundaries.
//! SEEDED: deterministic / no randomness.

use std::path::PathBuf;
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
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/kind-collision-composer/Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = match Command::new(cargo)
        .args(args)
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("failed to run downstream fixture: {error}"),
    };
    if !output.status.success() {
        panic!(
            "downstream fixture failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
