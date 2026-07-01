//! PROVES: item #133 — a release (non-test) binary that registers colliding
//! `EventPayload` kinds and NEVER opens a `Store` still fails fast on the
//! collision, via BOTH the always-on `verify_registry()` entry point (A4) and
//! the opt-in `startup-registry-check` constructor (which aborts before `main`).
//! CATCHES: a regression that drops the register-but-never-open collision catch,
//! letting a store-less release binary link two payload types that share wire
//! identity with no diagnostic (the derive's own collision test is
//! `#[cfg(test)]`-only and absent from a non-test binary).
//! SEEDED: deterministic / no randomness.
//!
//! The fixtures are separate crates (nested workspaces) built via
//! `cargo build --release --manifest-path ...` with a redirected
//! `CARGO_TARGET_DIR`, mirroring `event_payload_registry_downstream.rs`. The
//! built binary is then run DIRECTLY so exit status and the fixture's own stderr
//! are observed without cargo's build chatter. `--release` matches the "release
//! binary" framing of #133; the mechanism (`cfg(test) == false` in a `[[bin]]`
//! target, so no per-derive collision test) is profile-independent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A4 path: `verify_registry()` is a real, callable entry point that returns
/// `Ok` for a binary whose linked registry has no colliding kinds. Also serves
/// as the `pub-items-have-tests` witness that names `verify_registry` directly.
#[test]
fn verify_registry_is_ok_in_a_binary_without_collisions() {
    batpak::event::verify_registry()
        .expect("this test binary registers no colliding EventPayload kinds");
}

/// A4 path, RED: a non-test binary that registers a colliding pair and calls
/// `verify_registry()` in `main` exits non-zero and names the collision.
#[test]
fn release_binary_verify_registry_call_fails_on_collision() {
    let bin = build_fixture_bin("registry-startup-collision", "collide_verify");
    let output = run_bin(&bin);

    assert_eq!(
        output.status.code(),
        Some(1),
        "PROPERTY: a store-less release binary that registers a colliding pair and calls verify_registry() in main must exit 1; got {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("registry-startup-collision")
            && stderr.contains("duplicate kind assignment"),
        "PROPERTY: the failing binary must emit the collision diagnostic on stderr; got:\n{stderr}"
    );
}

/// RED confirmation: the same-shaped binary with NON-colliding registrations
/// exits 0. This proves the exit-1 above is caused by the seeded collision.
#[test]
fn release_binary_without_collision_exits_zero() {
    let bin = build_fixture_bin("registry-startup-collision", "clean_verify");
    let output = run_bin(&bin);

    assert_eq!(
        output.status.code(),
        Some(0),
        "PROPERTY: a clean registry must verify cleanly and exit 0; got {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ctor path, RED: with the `startup-registry-check` feature the before-`main`
/// constructor aborts on the collision. `main` is effectively empty (it would
/// exit 0 and print a sentinel if reached), so a non-zero exit with NO sentinel
/// proves the constructor fired before `main`.
#[test]
fn startup_registry_check_feature_aborts_before_main() {
    let bin = build_fixture_bin("registry-startup-ctor", "collide_ctor");
    let output = run_bin(&bin);

    assert!(
        !output.status.success(),
        "PROPERTY: the startup-registry-check constructor must abort the process on a collision; got success ({:?})",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("REACHED_MAIN_WITHOUT_ABORT"),
        "PROPERTY: the constructor must abort BEFORE main; main was reached (stdout: {stdout:?})"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("batpak startup-registry-check") && stderr.contains("aborting before main"),
        "PROPERTY: the constructor must emit its startup diagnostic on stderr; got:\n{stderr}"
    );
}

// ─── subprocess helpers (mirrors event_payload_registry_downstream.rs) ───────

fn build_fixture_bin(fixture: &str, bin: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = manifest_dir
        .join("fixtures")
        .join(fixture)
        .join("Cargo.toml");
    assert!(
        manifest.exists(),
        "startup fixture manifest is missing from repo checkout: {}",
        manifest.display()
    );
    let target_dir = startup_fixture_target_dir(&manifest_dir);
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .args(["build", "--release", "--quiet", "--bin", bin])
        .arg("--manifest-path")
        .arg(&manifest)
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .expect("failed to spawn cargo build for startup fixture");
    assert!(
        status.success(),
        "startup fixture `{fixture}` bin `{bin}` failed to build: {status}"
    );
    let bin_path = target_dir.join("release").join(bin);
    assert!(
        bin_path.exists(),
        "built startup fixture bin is missing: {}",
        bin_path.display()
    );
    bin_path
}

fn run_bin(bin: &Path) -> Output {
    Command::new(bin)
        .output()
        .expect("failed to run built startup fixture bin")
}

fn startup_fixture_target_dir(manifest_dir: &Path) -> PathBuf {
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest_dir);
    let root_target = match std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join("target"),
    };
    root_target.join("registry-startup-fixtures")
}
