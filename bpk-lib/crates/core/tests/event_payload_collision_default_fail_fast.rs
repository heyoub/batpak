//! PROVES: `Store::open` defaults to `EventPayloadValidation::FailFast`, so a
//! binary whose linked payload registry carries a `(category, type_id)` collision
//! REFUSES to open unless the caller explicitly opts back into the looser `Warn`
//! (log-and-proceed) policy.
//! CATCHES: a regression that re-defaults the payload-registry policy to `Warn`,
//! which would let two payload types silently share wire identity.
//! SEEDED: deterministic / no randomness.
//!
//! The colliding registrations live in a SEPARATE nested-workspace fixture crate
//! (`fixtures/store-open-collision`), built via `cargo build --release
//! --manifest-path ...` with a redirected `CARGO_TARGET_DIR` and run as a
//! subprocess, mirroring `event_payload_registry_startup.rs`. This deliberately
//! keeps the link-time collision OUT of this test binary: under `--all-features`
//! the opt-in `startup-registry-check` constructor aborts (before `main`) any
//! binary whose own linked registry collides, so an inline-collision test binary
//! could not even be enumerated by nextest. The fixture bin builds with the
//! crate's default features (no constructor), reaches `main`, opens a `Store`,
//! and encodes the store-open outcome in its exit code, so the store-open-time
//! DEFAULT-policy property stays proven in every feature lane.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The DEFAULT policy is `FailFast`: the fixture bin opens a `Store` with a
/// default `StoreConfig` over a colliding registry and exits 0 iff the open
/// failed closed with the registry error naming the seeded collision.
#[test]
fn default_policy_fail_fast_refuses_open_on_kind_collision() {
    let bin = build_fixture_bin("store-open-collision", "open_default_failfast");
    let output = run_bin(&bin);

    assert!(
        output.status.success(),
        "PROPERTY: default-policy `Store::open` must fail closed (FailFast) on a colliding registry; the fixture bin exited {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// RED control: the SAME colliding registry opened with an EXPLICIT
/// `EventPayloadValidation::Warn` opt-out must still open — proving the exit-0
/// above is the default policy, not the collision being unconditionally fatal.
#[test]
fn explicit_warn_opt_in_still_opens_on_kind_collision() {
    let bin = build_fixture_bin("store-open-collision", "open_warn_opens");
    let output = run_bin(&bin);

    assert!(
        output.status.success(),
        "PROPERTY: an explicit `EventPayloadValidation::Warn` open must still succeed on a colliding registry; the fixture bin exited {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

// ─── subprocess helpers (mirror event_payload_registry_startup.rs) ───────────

fn build_fixture_bin(fixture: &str, bin: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = manifest_dir
        .join("fixtures")
        .join(fixture)
        .join("Cargo.toml");
    assert!(
        manifest.exists(),
        "store-open fixture manifest is missing from repo checkout: {}",
        manifest.display()
    );
    let target_dir = fixture_target_dir(&manifest_dir);
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .args(["build", "--release", "--quiet", "--bin", bin])
        .arg("--manifest-path")
        .arg(&manifest)
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .expect("failed to spawn cargo build for store-open fixture");
    assert!(
        status.success(),
        "store-open fixture `{fixture}` bin `{bin}` failed to build: {status}"
    );
    let bin_path = target_dir.join("release").join(bin);
    assert!(
        bin_path.exists(),
        "built store-open fixture bin is missing: {}",
        bin_path.display()
    );
    bin_path
}

fn run_bin(bin: &Path) -> Output {
    Command::new(bin)
        .output()
        .expect("failed to run built store-open fixture bin")
}

fn fixture_target_dir(manifest_dir: &Path) -> PathBuf {
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest_dir);
    let root_target = match std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join("target"),
    };
    root_target.join("store-open-collision-fixtures")
}
