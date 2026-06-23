#[macro_use]
mod cli_out;

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
    /// Run scoped ast-grep calipers for production store smell shapes.
    AstGrep,
    Doctor,
    Traceability,
    Structural,
    /// Cross-oracle triangulation gate (GAUNTLET-TRIANGULATION): workspace
    /// crate-graph acyclicity cross-checked by two independent derivations.
    Triangulation,
    /// Prove the gates bite (GAUNTLET-TQL): rebuild every `ProductionFlip` red
    /// fixture under `--cfg gauntlet_red_fixture` and assert each test FAILS. A
    /// fixture that cannot red has laundered blocking authority.
    ProveGatesBite,
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
    /// Emit the machine-readable architecture IR into target/ by default.
    ArchitectureIr(ArchitectureIrArgs),
    /// Run the bpk-ts (TypeScript) gate surface: frozen-lockfile install,
    /// build (tsc), lint, format check, and tests. The polyglot half of the
    /// monorepo; `just verify-all` runs this alongside the Rust `preflight`.
    VerifyTs,
    Check(CheckArgs),
    Test(TestArgs),
    Clippy(ClippyArgs),
    Fmt,
    Deny,
    Bench(BenchArgs),
    Cover(CoverArgs),
    Mutants(MutantsArgs),
    /// Smoke-test every standalone Cargo template under `templates/`.
    Templates,
    /// Emit a CycloneDX 1.5 SBOM JSON file per publishable crate into
    /// `target/sbom/<crate>.cdx.json` under the Cargo workspace target dir.
    ///
    /// `cargo-cyclonedx` is a separate install:
    /// `cargo install cargo-cyclonedx --locked`. The subcommand never
    /// auto-installs it; consulting clients run release gates inside
    /// clean containers and want deterministic tool versioning.
    Sbom,
    /// Detect dependencies declared in any workspace `Cargo.toml` that are
    /// never referenced from source. Each unused dep widens the supply-chain
    /// blast radius and slows builds; this gate keeps the dep set tight.
    ///
    /// Backed by `cargo-machete`, installed by `cargo xtask setup --install-tools`.
    UnusedDeps,
    /// Verify the publish crates (batpak, syncbat, netbat) compile under
    /// their declared `rust-version` MSRV. Requires the relevant
    /// toolchain installed via `rustup toolchain install <msrv>
    /// --profile minimal`.
    MsrvCheck,
    /// Focused alias for template smoke + generated-lock drift checks.
    TemplateFreshness,
    /// Inspect staged files for generated artifacts, retired paths, and conflict markers.
    StagedDiff,
    /// Record the current public API surface. Advisory unless --strict is supplied.
    PublicApi(PublicApiArgs),
    /// Run release-oriented semver checks. Advisory unless --strict is supplied.
    SemverCheck(SemverCheckArgs),
    /// Write a local release proof manifest under the Cargo workspace target dir.
    ReleaseManifest(ReleaseManifestArgs),
    /// Opt-in factory command proof ledger backed by a local BatPAK store.
    FactoryLedger(FactoryLedgerArgs),
    /// Emit a PCP-aligned handoff packet under `target/context/`.
    Context(ContextArgs),
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
    /// Agent-safety meta-gate (P1-4, "raccoon with commit access"): resolve the
    /// `base..HEAD` diff, gather PR labels + commit trailers, and FAIL if the
    /// diff WEAKENS the assurance machinery without the required human approval.
    /// Thin shell around `batpak-integrity meta-gate-check`.
    MetaGate(MetaGateArgs),
    /// Early PR signal: format, clippy, checks, tests, dependency gates, machine law.
    CiFast,
    /// Native Windows surface compatibility lane.
    CiWindowsSurface,
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

/// Narrow `cargo test` to a single crate / single test binary so the
/// per-feature dev loop doesn't pay the full-workspace tax.
///
/// `cargo xtask test`                 ← default, full workspace + doctests + per-family-crate
/// `cargo xtask test --pkg syncbat`   ← only syncbat unit + integration + doc tests
/// `cargo xtask test --pkg syncbat --test runtime`
///                                    ← single integration test binary
/// `cargo xtask test --pkg netbat --no-doc`
///                                    ← skip doctests for this scoped run
/// `cargo xtask test --features ""`   ← override the default `--all-features`
#[derive(Args, Clone, Debug, Default)]
pub(crate) struct TestArgs {
    /// Cargo `-p` package name. Implies `--no-workspace` unless overridden.
    #[arg(long)]
    pub(crate) pkg: Option<String>,
    /// Cargo `--test` name (integration-test binary). Only valid with `--pkg`.
    #[arg(long, requires = "pkg")]
    pub(crate) test: Option<String>,
    /// Feature flags to pass to cargo. Defaults to `--all-features` when
    /// unset.
    #[arg(long)]
    pub(crate) features: Option<String>,
    /// Skip the doctest pass.
    #[arg(long)]
    pub(crate) no_doc: bool,
    /// Skip the workspace-wide `nextest` step. Implied by `--pkg` unless
    /// you also pass `--workspace`.
    #[arg(long, conflicts_with = "workspace")]
    pub(crate) no_workspace: bool,
    /// Force the workspace-wide step to run even when `--pkg` is set.
    #[arg(long)]
    pub(crate) workspace: bool,
}

/// Narrow `cargo check`. Default behaviour matches the legacy
/// `XtaskCommand::Check` (workspace + per-family-crate).
#[derive(Args, Clone, Debug, Default)]
pub(crate) struct CheckArgs {
    /// Cargo `-p` package name. Implies `--no-workspace`.
    #[arg(long)]
    pub(crate) pkg: Option<String>,
    /// Feature flag override.
    #[arg(long)]
    pub(crate) features: Option<String>,
    /// Skip the `--no-default-features` half of the check.
    #[arg(long)]
    pub(crate) no_default_only: bool,
    /// Skip the `--all-features` half of the check.
    #[arg(long)]
    pub(crate) all_features_only: bool,
}

/// Narrow `cargo clippy`. Default matches the legacy
/// `XtaskCommand::Clippy`.
#[derive(Args, Clone, Debug, Default)]
pub(crate) struct ClippyArgs {
    /// Cargo `-p` package name. Implies `--no-workspace`.
    #[arg(long)]
    pub(crate) pkg: Option<String>,
    /// Feature flag override.
    #[arg(long)]
    pub(crate) features: Option<String>,
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
    /// Run one critical smoke seam (CI matrix shard). Mutually exclusive with `--repo-wide-only`.
    #[arg(long, conflicts_with = "repo_wide_only")]
    seam: Option<String>,
    /// Run only repo-wide smoke ratchet lanes. Mutually exclusive with `--seam`.
    #[arg(long, conflicts_with = "seam")]
    repo_wide_only: bool,
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
pub(crate) struct FactoryLedgerArgs {
    #[command(subcommand)]
    pub(crate) command: FactoryLedgerCommand,
}

#[derive(Subcommand, Clone)]
pub(crate) enum FactoryLedgerCommand {
    /// Append one ledger event explicitly (tests and future hooks).
    Record(FactoryLedgerRecordArgs),
    /// Query recent ledger events in global_sequence order.
    List(FactoryLedgerListArgs),
    /// Run a command, recording started/completed/failed proof events.
    Run(FactoryLedgerRunArgs),
}

#[derive(Subcommand, Clone)]
pub(crate) enum FactoryLedgerRecordCommand {
    Started(FactoryLedgerRecordStartedArgs),
    Completed(FactoryLedgerRecordCompletedArgs),
    Failed(FactoryLedgerRecordFailedArgs),
    GateCompleted(FactoryLedgerRecordGateCompletedArgs),
}

#[derive(Args, Clone)]
pub(crate) struct FactoryLedgerRecordArgs {
    #[command(subcommand)]
    pub(crate) command: FactoryLedgerRecordCommand,
}

#[derive(Args, Clone)]
pub(crate) struct FactoryLedgerRecordStartedArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) command: String,
    #[arg(long)]
    pub(crate) args: Vec<String>,
    #[arg(long)]
    pub(crate) cwd: Option<String>,
    #[arg(long)]
    pub(crate) branch: Option<String>,
    #[arg(long)]
    pub(crate) head: Option<String>,
    #[arg(long)]
    pub(crate) started_ms: Option<u64>,
}

#[derive(Args, Clone)]
pub(crate) struct FactoryLedgerRecordCompletedArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) command: String,
    #[arg(long, default_value_t = 0)]
    pub(crate) status_code: i32,
    #[arg(long)]
    pub(crate) duration_ms: u64,
    #[arg(long)]
    pub(crate) completed_ms: Option<u64>,
}

#[derive(Args, Clone)]
pub(crate) struct FactoryLedgerRecordFailedArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) command: String,
    #[arg(long)]
    pub(crate) status_code: i32,
    #[arg(long)]
    pub(crate) duration_ms: u64,
    #[arg(long, default_value = "")]
    pub(crate) stderr_tail: String,
    #[arg(long)]
    pub(crate) completed_ms: Option<u64>,
}

#[derive(Args, Clone)]
pub(crate) struct FactoryLedgerRecordGateCompletedArgs {
    #[arg(long)]
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) gate: String,
    #[arg(long)]
    pub(crate) command: String,
    #[arg(long, default_value_t = 0)]
    pub(crate) status_code: i32,
    #[arg(long)]
    pub(crate) duration_ms: u64,
    #[arg(long)]
    pub(crate) completed_ms: Option<u64>,
    #[arg(long)]
    pub(crate) branch: Option<String>,
    #[arg(long)]
    pub(crate) head: Option<String>,
    #[arg(long)]
    pub(crate) summary: String,
}

#[derive(Args, Clone)]
pub(crate) struct FactoryLedgerListArgs {
    #[arg(long, default_value_t = 50)]
    pub(crate) limit: usize,
}

#[derive(Args, Clone, Debug)]
pub(crate) struct FactoryLedgerRunArgs {
    #[arg(long)]
    pub(crate) gate: Option<String>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Args, Clone)]
pub(crate) struct ContextArgs {
    #[arg(long, default_value_t = 20)]
    pub(crate) ledger_limit: usize,
    #[arg(long)]
    pub(crate) notes: Option<String>,
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
pub(crate) struct ArchitectureIrArgs {
    /// Output path for the rendered IR. Defaults to target/architecture.ir.json.
    #[arg(long, value_name = "PATH")]
    pub(crate) out: Option<PathBuf>,
    /// Refuse drift instead of rewriting the IR file.
    #[arg(long)]
    pub(crate) check: bool,
}

#[derive(Args, Clone, Debug)]
pub(crate) struct MetaGateArgs {
    /// Base ref/sha to diff against. Defaults to the merge-base with
    /// `origin/main` (falling back to `main`).
    #[arg(long)]
    pub(crate) base: Option<String>,
    /// A PR label (repeatable). When omitted, labels are read from the
    /// `GAUNTLET_PR_LABELS` env var (comma- or newline-separated), so CI can pass
    /// `github.event.pull_request.labels.*.name` through one variable.
    #[arg(long = "label")]
    pub(crate) labels: Vec<String>,
    /// The PR author login. Defaults to the `GAUNTLET_PR_AUTHOR` env var.
    #[arg(long)]
    pub(crate) pr_author: Option<String>,
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
        XtaskCommand::AstGrep => commands::ast_grep(),
        XtaskCommand::Doctor => commands::doctor(),
        XtaskCommand::Traceability => commands::integrity("traceability-check", []),
        XtaskCommand::Structural => commands::integrity("structural-check", []),
        XtaskCommand::Triangulation => commands::integrity("triangulation-check", []),
        XtaskCommand::ProveGatesBite => commands::prove_gates_bite(),
        XtaskCommand::Layout => commands::integrity("structural-check", []),
        XtaskCommand::Boundary => commands::integrity("structural-check", []),
        XtaskCommand::StalePaths => commands::integrity("structural-check", []),
        XtaskCommand::DiskAudit => commands::disk_audit(),
        XtaskCommand::CleanGenerated(args) => commands::clean_generated(args),
        XtaskCommand::PackageLeakScan(args) => commands::package_leak_scan(args),
        XtaskCommand::CheckVersionPins => commands::check_version_pins(),
        XtaskCommand::EvidenceAudit => commands::integrity("evidence-audit", []),
        XtaskCommand::AgentDoctor => commands::integrity("agent-doctor", []),
        XtaskCommand::ArchitectureIr(args) => commands::architecture_ir(&args),
        XtaskCommand::VerifyTs => commands::verify_ts(),
        XtaskCommand::Check(args) => run_check(&args),
        XtaskCommand::Test(args) => run_test(&args),
        XtaskCommand::Clippy(args) => run_clippy(&args),
        XtaskCommand::Fmt => util::cargo(["fmt", "--check"]),
        XtaskCommand::Deny => commands::deny_split(),
        XtaskCommand::Bench(args) => bench::bench(args),
        XtaskCommand::Cover(args) => coverage::cover(args),
        XtaskCommand::Mutants(args) => commands::mutants(&args),
        XtaskCommand::Templates => commands::templates(),
        XtaskCommand::Sbom => commands::sbom(),
        XtaskCommand::UnusedDeps => commands::unused_deps(),
        XtaskCommand::MsrvCheck => commands::msrv_check(),
        XtaskCommand::TemplateFreshness => {
            commands::templates()?;
            commands::integrity("structural-check", [])
        }
        XtaskCommand::StagedDiff => commands::staged_diff(),
        XtaskCommand::PublicApi(args) => public_api::public_api(args),
        XtaskCommand::SemverCheck(args) => public_api::semver_check(args),
        XtaskCommand::ReleaseManifest(args) => commands::release_manifest(args),
        XtaskCommand::FactoryLedger(args) => commands::factory_ledger(args),
        XtaskCommand::Context(args) => commands::context(args),
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
        XtaskCommand::MetaGate(args) => commands::meta_gate(&args),
        XtaskCommand::CiFast => commands::ci_fast(),
        XtaskCommand::CiWindowsSurface => commands::ci_windows_surface(),
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
            util::cargo([
                "clippy",
                "-p",
                "netbat",
                "--no-deps",
                "--all-features",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ])?;
            commands::integrity("traceability-check", [])?;
            commands::integrity("structural-check", [])
        }
        XtaskCommand::Docs(args) => docs::docs(args),
        XtaskCommand::Release(args) => commands::release(args),
    }
}

fn features_flag(features: Option<&str>) -> Vec<String> {
    match features {
        None => vec!["--all-features".to_owned()],
        Some("") => Vec::new(),
        Some(spec) => vec!["--features".to_owned(), spec.to_owned()],
    }
}

fn run_check(args: &CheckArgs) -> Result<()> {
    if let Some(pkg) = args.pkg.as_deref() {
        let feature_args = features_flag(args.features.as_deref());
        if !args.no_default_only {
            let mut cmd = vec!["check".to_owned(), "-p".to_owned(), pkg.to_owned()];
            cmd.extend(feature_args.iter().cloned());
            util::cargo(cmd.iter().map(String::as_str))?;
        }
        if !args.all_features_only {
            util::cargo(["check", "-p", pkg, "--no-default-features"])?;
        }
        return Ok(());
    }
    util::cargo(["check", "--all-features"])?;
    util::cargo(["check", "--no-default-features"])?;
    for package in publish::FAMILY_CRATES {
        util::cargo(["check", "-p", package, "--all-features"])?;
        util::cargo(["check", "-p", package, "--no-default-features"])?;
    }
    Ok(())
}

fn run_test(args: &TestArgs) -> Result<()> {
    let scoped = args.pkg.is_some();
    let workspace_step = (!scoped && !args.no_workspace) || args.workspace;
    let feature_args = features_flag(args.features.as_deref());
    let feature_strs: Vec<&str> = feature_args.iter().map(String::as_str).collect();

    if workspace_step {
        let mut nextest_args: Vec<&str> = Vec::new();
        nextest_args.extend(feature_strs.iter().copied());
        commands::run_nextest_ci(nextest_args.iter().copied())?;
        if !args.no_doc {
            let mut doc_cmd = vec!["test".to_owned(), "--doc".to_owned()];
            doc_cmd.extend(feature_args.iter().cloned());
            util::cargo(doc_cmd.iter().map(String::as_str))?;
        }
        if !scoped {
            for package in publish::FAMILY_CRATES {
                let mut cmd = vec!["test".to_owned(), "-p".to_owned(), package.to_string()];
                cmd.extend(feature_args.iter().cloned());
                util::cargo(cmd.iter().map(String::as_str))?;
            }
            return Ok(());
        }
    }

    if let Some(pkg) = args.pkg.as_deref() {
        let mut cmd = vec!["test".to_owned(), "-p".to_owned(), pkg.to_owned()];
        if let Some(test_bin) = args.test.as_deref() {
            cmd.push("--test".to_owned());
            cmd.push(test_bin.to_owned());
        }
        cmd.extend(feature_args.iter().cloned());
        util::cargo(cmd.iter().map(String::as_str))?;
        if !args.no_doc && args.test.is_none() {
            let mut doc_cmd = vec![
                "test".to_owned(),
                "--doc".to_owned(),
                "-p".to_owned(),
                pkg.to_owned(),
            ];
            doc_cmd.extend(feature_args.iter().cloned());
            util::cargo(doc_cmd.iter().map(String::as_str))?;
        }
    }
    Ok(())
}

fn run_clippy(args: &ClippyArgs) -> Result<()> {
    let feature_args = features_flag(args.features.as_deref());
    if let Some(pkg) = args.pkg.as_deref() {
        let mut cmd = vec![
            "clippy".to_owned(),
            "-p".to_owned(),
            pkg.to_owned(),
            "--no-deps".to_owned(),
        ];
        cmd.extend(feature_args.iter().cloned());
        cmd.extend([
            "--all-targets".to_owned(),
            "--".to_owned(),
            "-D".to_owned(),
            "warnings".to_owned(),
        ]);
        util::cargo(cmd.iter().map(String::as_str))?;
        return Ok(());
    }
    let mut cmd = vec!["clippy".to_owned()];
    cmd.extend(feature_args.iter().cloned());
    cmd.extend([
        "--all-targets".to_owned(),
        "--".to_owned(),
        "-D".to_owned(),
        "warnings".to_owned(),
    ]);
    util::cargo(cmd.iter().map(String::as_str))?;
    for package in publish::FAMILY_CRATES {
        let mut per_cmd = vec![
            "clippy".to_owned(),
            "-p".to_owned(),
            package.to_string(),
            "--no-deps".to_owned(),
        ];
        per_cmd.extend(feature_args.iter().cloned());
        per_cmd.extend([
            "--all-targets".to_owned(),
            "--".to_owned(),
            "-D".to_owned(),
            "warnings".to_owned(),
        ]);
        util::cargo(per_cmd.iter().map(String::as_str))?;
    }
    Ok(())
}
