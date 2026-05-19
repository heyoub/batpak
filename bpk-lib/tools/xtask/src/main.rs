// justifies: INV-ALLOW-IS-DESIGN; xtask is the repository-owned command-line tool and its subcommands intentionally write human and CI status messages to stdout/stderr from tools/xtask/src/main.rs.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod bench;
mod commands;
mod coverage;
mod devcontainer;
mod docs;
mod preflight;
mod public_api;
mod publish;
mod util;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about = "Root developer command surface for batpak")]
struct Cli {
    #[command(subcommand)]
    command: XtaskCommand,
}

#[derive(Subcommand)]
enum XtaskCommand {
    Setup(SetupArgs),
    InstallHooks,
    Quickstart,
    ConsumerSmoke,
    Doctor,
    Traceability,
    Structural,
    /// Check the repo layout contract: root docs/cookbook plus bpk-lib workspace.
    Layout,
    /// Check stack dependency direction and runtime boundary discipline.
    Boundary,
    /// Check moved/retired path references after repo layout changes.
    StalePaths,
    /// Read-only report for repo-local artifact/cache sprawl.
    DiskAudit,
    /// Remove generated repo-local artifact/cache sprawl. Dry-run by default.
    CleanGenerated(CleanGeneratedArgs),
    /// Build the batpak package locally and scan the .crate for leak-shaped text.
    PackageLeakScan(PackageLeakScanArgs),
    /// Check workspace package versions and path-dependency version pins.
    CheckVersionPins,
    /// Static evidence-report hygiene (schema anchors, export vocabulary).
    EvidenceAudit,
    /// Fast agent-oriented repository doctor with stable repair IDs.
    AgentDoctor,
    Check,
    Test,
    Clippy,
    Fmt,
    Deny,
    Bench(BenchArgs),
    Cover(CoverArgs),
    Mutants(MutantsArgs),
    /// Smoke-test every standalone Cargo template under `templates/`.
    Templates,
    /// Focused alias for template smoke + generated-lock drift checks.
    TemplateFreshness,
    /// Inspect staged files for generated artifacts, retired paths, and conflict markers.
    StagedDiff,
    /// Record the current public API surface. Advisory by default during 0.7.6 cleanup.
    PublicApi(PublicApiArgs),
    /// Run release-oriented semver checks. Advisory by default during 0.7.6 cleanup.
    SemverCheck(SemverCheckArgs),
    /// Write a local release proof manifest under target/.
    ReleaseManifest(ReleaseManifestArgs),
    /// Export the BatPAK TypeScript SDK manifest from the reference host
    /// descriptors. Consumed by `bpk-ts/packages/codegen`.
    ExportTsManifest(ExportTsManifestArgs),
    /// Copy a golden batpak starter template into a local project directory.
    Scaffold(ScaffoldArgs),
    Platform(PlatformArgs),
    Fuzz(FuzzArgs),
    Chaos(ChaosArgs),
    FuzzChaos,
    /// Run deterministic loom schedule proofs under --cfg loom.
    Loom,
    Stress,
    /// Run hardware-dependent perf gates (excluded from `cargo xtask ci`).
    /// These tests are loose catastrophic-regression guards, not precision
    /// performance gates: no current environment is both canonical and
    /// timing-stable. Run them on a dedicated perf machine or locally on
    /// stable hardware when you need real interpretation.
    PerfGates,
    DevcontainerExec(DevcontainerExecArgs),
    Ci,
    /// Reproduce the canonical verification bundle inside the devcontainer.
    /// The host enters the container once, then CI, coverage, and docs run
    /// from that same session.
    Preflight,
    PreCommit,
    Docs(DocsArgs),
    Release(ReleaseArgs),
}

#[derive(Args, Clone, Copy)]
pub(crate) struct SetupArgs {
    #[arg(long)]
    install_tools: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum BenchSurface {
    Neutral,
    Native,
}

#[derive(Args, Clone)]
pub(crate) struct BenchArgs {
    #[arg(long, value_enum, default_value_t = BenchSurface::Neutral)]
    surface: BenchSurface,
    #[arg(long)]
    save: Option<String>,
    #[arg(long)]
    compare: bool,
    #[arg(long)]
    compile: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct CoverArgs {
    #[arg(long)]
    ci: bool,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    threshold: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum MutantMode {
    /// Print the repo-owned mutation policy without running cargo-mutants.
    Policy,
    /// Run the CI smoke lane: hard critical seams plus repo-wide ratchet shards.
    Smoke,
    /// Run repo-wide lanes, or the full policy when no overrides are passed.
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum MutantSurface {
    AllFeatures,
    NoDefaultFeatures,
}

#[derive(Args, Clone)]
pub(crate) struct MutantsArgs {
    #[arg(value_enum)]
    mode: MutantMode,
    #[arg(long, value_enum)]
    surface: Option<MutantSurface>,
    #[arg(long)]
    shard: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ScaffoldPattern {
    TypedStore,
    Reactor,
    EvidenceRead,
    ProjectionCache,
    ArtifactEnvelope,
    RegistryRow,
    BackupEnvelope,
    StateTransition,
    ReservationLedger,
}

#[derive(Args, Clone, Debug)]
pub(crate) struct ScaffoldArgs {
    #[arg(value_enum)]
    pattern: ScaffoldPattern,
    #[arg(long)]
    name: String,
    #[arg(long)]
    path: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

#[derive(Args, Clone)]
pub(crate) struct PlatformArgs {
    #[command(subcommand)]
    command: PlatformCommand,
}

#[derive(Subcommand, Clone)]
pub(crate) enum PlatformCommand {
    /// Report whether a store path can produce a platform profile.
    Doctor(PlatformStorePathArgs),
    /// Write a platform profile for a store path.
    Probe(PlatformProfileIoArgs),
    /// Compare current platform evidence with a profile.
    Verify(PlatformProfileIoArgs),
    /// Intentionally refresh a platform profile fixture.
    Bless(PlatformProfileIoArgs),
    /// Run platform boundary structural checks.
    Audit,
}

#[derive(Args, Clone)]
pub(crate) struct PlatformStorePathArgs {
    #[arg(long, default_value = ".")]
    store_path: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct PlatformProfileIoArgs {
    #[arg(long)]
    store_path: PathBuf,
    #[arg(long)]
    profile: PathBuf,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct FuzzArgs {
    #[arg(long)]
    deep: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct ChaosArgs {
    #[arg(long)]
    deep: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct DocsArgs {
    #[arg(long)]
    open: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct ReleaseArgs {
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct CleanGeneratedArgs {
    /// Actually remove generated sprawl. Without this flag, only print actions.
    #[arg(long)]
    apply: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct PackageLeakScanArgs {
    /// Allow packaging from the current dirty worktree.
    #[arg(long)]
    allow_dirty: bool,
    /// Treat broad public-language warnings as release-blocking.
    #[arg(long)]
    strict_language: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct PublicApiArgs {
    /// Fail when cargo-public-api is missing or the public-api run fails.
    #[arg(long)]
    strict: bool,
    /// Compare the current public API against the checked-in baseline.
    #[arg(long)]
    check_baseline: bool,
    /// Replace the checked-in public API baseline with the current surface.
    #[arg(long)]
    bless_baseline: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct SemverCheckArgs {
    /// Fail when cargo-semver-checks is missing or reports an incompatibility.
    #[arg(long)]
    strict: bool,
}

#[derive(Args, Clone, Copy)]
pub(crate) struct ReleaseManifestArgs {
    /// Refuse to write a manifest from a dirty worktree.
    #[arg(long)]
    strict: bool,
    /// Record dirty-worktree state in the local manifest.
    #[arg(long)]
    allow_dirty: bool,
}

#[derive(Args, Clone, Debug)]
pub(crate) struct ExportTsManifestArgs {
    /// Output path for the rendered manifest. The parent directory is
    /// created on demand.
    #[arg(long, value_name = "PATH")]
    pub(crate) out: PathBuf,
}

#[derive(Args, Clone, Debug)]
pub(crate) struct DevcontainerExecArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        XtaskCommand::Setup(args) => commands::setup(args),
        XtaskCommand::InstallHooks => commands::install_hooks(),
        XtaskCommand::Quickstart => commands::quickstart(),
        XtaskCommand::ConsumerSmoke => commands::consumer_smoke(),
        XtaskCommand::Doctor => commands::doctor(),
        XtaskCommand::Traceability => commands::integrity("traceability-check", []),
        XtaskCommand::Structural => commands::integrity("structural-check", []),
        XtaskCommand::Layout => commands::integrity("structural-check", []),
        XtaskCommand::Boundary => commands::integrity("structural-check", []),
        XtaskCommand::StalePaths => commands::integrity("structural-check", []),
        XtaskCommand::DiskAudit => commands::disk_audit(),
        XtaskCommand::CleanGenerated(args) => commands::clean_generated(args),
        XtaskCommand::PackageLeakScan(args) => commands::package_leak_scan(args),
        XtaskCommand::CheckVersionPins => commands::check_version_pins(),
        XtaskCommand::EvidenceAudit => commands::integrity("evidence-audit", []),
        XtaskCommand::AgentDoctor => commands::integrity("agent-doctor", []),
        XtaskCommand::Check => {
            util::cargo(["check", "--all-features"])?;
            util::cargo(["check", "--no-default-features"])?;
            for package in publish::FAMILY_CRATES {
                util::cargo(["check", "-p", package, "--all-features"])?;
                util::cargo(["check", "-p", package, "--no-default-features"])?;
            }
            Ok(())
        }
        XtaskCommand::Test => {
            commands::run_nextest_ci(["--all-features"])?;
            util::cargo(["test", "--doc", "--all-features"])?;
            for package in publish::FAMILY_CRATES {
                util::cargo(["test", "-p", package, "--all-features"])?;
            }
            Ok(())
        }
        XtaskCommand::Clippy => {
            util::cargo([
                "clippy",
                "--all-features",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ])?;
            for package in publish::FAMILY_CRATES {
                util::cargo([
                    "clippy",
                    "-p",
                    package,
                    "--no-deps",
                    "--all-features",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ])?;
            }
            Ok(())
        }
        XtaskCommand::Fmt => util::cargo(["fmt", "--check"]),
        XtaskCommand::Deny => commands::deny_split(),
        XtaskCommand::Bench(args) => bench::bench(args),
        XtaskCommand::Cover(args) => coverage::cover(args),
        XtaskCommand::Mutants(args) => commands::mutants(&args),
        XtaskCommand::Templates => commands::templates(),
        XtaskCommand::TemplateFreshness => {
            commands::templates()?;
            commands::integrity("structural-check", [])
        }
        XtaskCommand::StagedDiff => commands::staged_diff(),
        XtaskCommand::PublicApi(args) => public_api::public_api(args),
        XtaskCommand::SemverCheck(args) => public_api::semver_check(args),
        XtaskCommand::ReleaseManifest(args) => commands::release_manifest(args),
        XtaskCommand::ExportTsManifest(args) => commands::export_ts_manifest(&args),
        XtaskCommand::Scaffold(args) => commands::scaffold(args),
        XtaskCommand::Platform(args) => commands::platform(args),
        XtaskCommand::Fuzz(args) => commands::fuzz(args),
        XtaskCommand::Chaos(args) => commands::chaos(args),
        XtaskCommand::FuzzChaos => util::cargo([
            "test",
            "--test",
            "fuzz_chaos_feedback",
            "--all-features",
            "--release",
            "--",
            "--ignored",
            "--nocapture",
        ]),
        XtaskCommand::Loom => commands::loom(),
        XtaskCommand::Stress => {
            commands::fuzz(FuzzArgs { deep: false })?;
            commands::chaos(ChaosArgs { deep: false })?;
            util::cargo([
                "test",
                "--test",
                "fuzz_chaos_feedback",
                "--all-features",
                "--release",
                "--",
                "--ignored",
                "--nocapture",
            ])?;
            bench::bench(BenchArgs {
                surface: BenchSurface::Neutral,
                save: None,
                compare: false,
                compile: false,
            })
        }
        XtaskCommand::PerfGates => commands::perf_gates(),
        XtaskCommand::DevcontainerExec(args) => devcontainer::devcontainer_exec(&args),
        XtaskCommand::Ci => commands::ci(),
        XtaskCommand::Preflight => preflight::preflight(),
        XtaskCommand::PreCommit => {
            util::cargo(["fmt", "--check"])?;
            util::cargo([
                "clippy",
                "--all-features",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ])?;
            util::cargo([
                "clippy",
                "-p",
                "syncbat",
                "--no-deps",
                "--all-features",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ])?;
            for package in ["netbat"] {
                util::cargo([
                    "clippy",
                    "-p",
                    package,
                    "--no-deps",
                    "--all-features",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ])?;
            }
            commands::integrity("traceability-check", [])?;
            commands::integrity("structural-check", [])
        }
        XtaskCommand::Docs(args) => docs::docs(args),
        XtaskCommand::Release(args) => commands::release(args),
    }
}
