mod ci;
mod disk_audit;
mod export_ts_manifest;
mod loom;
mod msrv_check;
mod mutants;
mod package_scan;
mod platform;
mod release;
mod release_manifest;
mod sbom;
mod scaffold;
mod setup;
mod staged;
mod stress;
mod templates;
mod unused_deps;
mod version_pins;

use crate::util::cargo;
use crate::CleanGeneratedArgs;
use crate::{
    ChaosArgs, ExportTsManifestArgs, FuzzArgs, MutantsArgs, PackageLeakScanArgs, PlatformArgs,
    ReleaseArgs, ScaffoldArgs, SetupArgs,
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

pub(crate) fn integrity<const N: usize>(subcommand: &str, extra: [&str; N]) -> Result<()> {
    let mut args = vec!["run", "--package", "batpak-integrity", "--", subcommand];
    args.extend(extra);
    cargo(args)
}

pub(crate) fn scaffold(args: ScaffoldArgs) -> Result<()> {
    scaffold::scaffold(args)
}

pub(crate) fn templates() -> Result<()> {
    templates::templates()
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

/// Detect dependencies declared in `Cargo.toml` that are never referenced
/// from source. Backed by `cargo-machete`. Install with
/// `cargo install cargo-machete --locked`.
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

pub(crate) fn export_ts_manifest(args: &ExportTsManifestArgs) -> Result<()> {
    export_ts_manifest::export_ts_manifest(args)
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
