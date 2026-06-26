mod ast_grep;
mod ast_grep_family_version;
mod ci;
mod context;
mod disk_audit;
mod factory_ledger;
mod loom;
mod manifest;
mod meta_gate;
mod msrv_check;
mod mutants;
mod package_scan;
mod platform;
mod prove_gates_bite;
mod release;
mod release_manifest;
mod sbom;
mod scaffold;
mod setup;
mod staged;
mod stress;
mod templates;
mod unused_deps;
mod verify_ts;
mod version_pins;

use crate::util::{cargo, cargo_target_dir};
use crate::CleanGeneratedArgs;
use crate::{
    ArchitectureIrArgs, ChaosArgs, ContextArgs, FactoryLedgerArgs, FuzzArgs, MutantsArgs,
    PackageLeakScanArgs, PlatformArgs, ReleaseArgs, ScaffoldArgs, SetupArgs,
};
use anyhow::Result;

pub(crate) fn setup(args: SetupArgs) -> Result<()> {
    setup::setup(args)
}

/// Wire the tracked `.githooks/pre-commit` surface into git.
///
/// Run `cargo xtask install-hooks` to opt into the repo-managed hook surface.
pub(crate) fn install_hooks() -> Result<()> {
    setup::install_hooks()
}

pub(crate) fn doctor() -> Result<()> {
    setup::doctor()
}

pub(crate) fn quickstart() -> Result<()> {
    release::quickstart()
}

pub(crate) fn consumer_smoke() -> Result<()> {
    release::consumer_smoke()
}

pub(crate) fn ast_grep() -> Result<()> {
    ast_grep::ast_grep()
}

pub(crate) fn integrity<const N: usize>(subcommand: &str, extra: [&str; N]) -> Result<()> {
    let mut args = vec!["run", "--package", "batpak-integrity", "--", subcommand];
    args.extend(extra);
    cargo(args)
}

pub(crate) fn release_status(args: crate::ReleaseStatusArgs) -> Result<()> {
    let mut extra: Vec<&str> = Vec::new();
    if args.strict {
        extra.push("--strict");
    }
    if args.active {
        extra.push("--active");
    }
    let target_buf;
    if let Some(target) = args.target {
        target_buf = target;
        extra.push("--target");
        extra.push(target_buf.as_str());
    }
    let mut cargo_args = vec![
        "run",
        "--package",
        "batpak-integrity",
        "--",
        "release-status-check",
    ];
    cargo_args.extend(extra);
    cargo(cargo_args)
}

pub(crate) fn release_status_strict_active() -> Result<()> {
    release_status(crate::ReleaseStatusArgs {
        target: None,
        active: true,
        strict: true,
    })
}

pub(crate) fn scaffold(args: ScaffoldArgs) -> Result<()> {
    scaffold::scaffold(args)
}

pub(crate) fn templates() -> Result<()> {
    templates::templates()
}

/// Drive the bpk-ts (TypeScript) gate surface — the polyglot half of the
/// monorepo — so `just verify-all` clears both halves in one command.
pub(crate) fn verify_ts() -> Result<()> {
    verify_ts::verify_ts()
}

/// Drive `cargo cyclonedx` over every publishable crate and emit a
/// CycloneDX 1.5 SBOM JSON under the Cargo workspace `target/sbom/`.
///
/// `cargo-cyclonedx` is a separate install: `cargo install cargo-cyclonedx
/// --locked`. The subcommand fails fast with a clear install hint when
/// the binary is missing rather than auto-installing or no-opping.
pub(crate) fn sbom() -> Result<()> {
    sbom::sbom()
}

/// Prove every `ProductionFlip` gate's red fixture actually reds under
/// `--cfg gauntlet_red_fixture` (the anti-laundering "prove the gates bite" lane).
pub(crate) fn prove_gates_bite() -> Result<()> {
    prove_gates_bite::run()
}

/// Detect dependencies declared in `Cargo.toml` that are never referenced
/// from source. Backed by `cargo-machete`, installed by
/// `cargo xtask setup --install-tools`.
pub(crate) fn unused_deps() -> Result<()> {
    unused_deps::unused_deps()
}

/// Verify each publish crate compiles under its declared
/// `rust-version`. Requires the relevant toolchain installed via
/// `rustup toolchain install <msrv>`. Fails fast with an install
/// hint when the toolchain is missing.
pub(crate) fn msrv_check() -> Result<()> {
    msrv_check::msrv_check()
}

pub(crate) fn disk_audit() -> Result<()> {
    disk_audit::disk_audit()
}

pub(crate) fn clean_generated(args: CleanGeneratedArgs) -> Result<()> {
    disk_audit::clean_generated(args)
}

pub(crate) fn package_leak_scan(args: PackageLeakScanArgs) -> Result<()> {
    package_scan::package_leak_scan(args)
}

pub(crate) fn check_version_pins() -> Result<()> {
    version_pins::check_version_pins()
}

pub(crate) fn staged_diff() -> Result<()> {
    staged::staged_diff()
}

pub(crate) fn release_manifest(args: crate::ReleaseManifestArgs) -> Result<()> {
    release_manifest::release_manifest(args)
}

pub(crate) fn factory_ledger(args: FactoryLedgerArgs) -> Result<()> {
    factory_ledger::factory_ledger(args)
}

pub(crate) fn context(args: ContextArgs) -> Result<()> {
    context::context(args)
}

pub(crate) fn architecture_ir(args: &ArchitectureIrArgs) -> Result<()> {
    let out = args
        .out
        .clone()
        .unwrap_or(cargo_target_dir()?.join("architecture.ir.json"));
    let out_arg = out.to_string_lossy().into_owned();
    let mut cargo_args = vec![
        "run".to_owned(),
        "--package".to_owned(),
        "batpak-integrity".to_owned(),
        "--".to_owned(),
        "architecture-ir".to_owned(),
        "--out".to_owned(),
        out_arg,
    ];
    if args.check {
        cargo_args.push("--check".to_owned());
    }
    cargo(cargo_args.iter().map(String::as_str))
}

pub(crate) fn deny_split() -> Result<()> {
    cargo(["deny", "check"])?;
    cargo(["audit", "--deny", "warnings"])
}

pub(crate) fn mutants(args: &MutantsArgs) -> Result<()> {
    mutants::mutants(args)
}

pub(crate) fn platform(args: PlatformArgs) -> Result<()> {
    platform::platform(args)
}

pub(crate) fn fuzz(args: FuzzArgs) -> Result<()> {
    stress::fuzz(args)
}

pub(crate) fn chaos(args: ChaosArgs) -> Result<()> {
    stress::chaos(args)
}

pub(crate) fn ci() -> Result<()> {
    ci::ci()
}

pub(crate) fn ci_fast() -> Result<()> {
    ci::ci_fast()
}

pub(crate) fn meta_gate(args: &crate::MetaGateArgs) -> Result<()> {
    meta_gate::meta_gate(args)
}

pub(crate) fn ci_windows_surface() -> Result<()> {
    ci::ci_windows_surface()
}

pub(crate) fn loom() -> Result<()> {
    loom::loom()
}

pub(crate) fn run_nextest_ci<'a, I>(args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    ci::run_nextest_ci(args)
}

pub(crate) fn perf_gates() -> Result<()> {
    ci::perf_gates()
}

pub(crate) fn release(args: ReleaseArgs) -> Result<()> {
    release::release(args)
}

#[cfg(test)]
mod tests {
    use super::setup;
    use std::path::Path;

    #[test]
    fn repo_hooks_path_matches_relative_and_absolute_spellings() {
        let root = Path::new("/workspace/batpak");
        assert!(setup::matches_repo_hooks_path(root, ".githooks"));
        assert!(setup::matches_repo_hooks_path(root, "./.githooks"));
        assert!(setup::matches_repo_hooks_path(
            root,
            "/workspace/batpak/.githooks"
        ));
    }

    #[test]
    fn default_git_hooks_path_matches_relative_and_absolute_spellings() {
        let root = Path::new("/workspace/batpak");
        assert!(setup::is_default_hooks_path(root, ".git/hooks"));
        assert!(setup::is_default_hooks_path(
            root,
            "/workspace/batpak/.git/hooks"
        ));
    }
}
