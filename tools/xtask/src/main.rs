mod bench;
mod commands;
mod coverage;
mod devcontainer;
mod docs;
mod preflight;
mod util;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

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
    Check,
    Test,
    Clippy,
    Fmt,
    Deny,
    Bench(BenchArgs),
    Cover(CoverArgs),
    Mutants(MutantsArgs),
    Fuzz(FuzzArgs),
    Chaos(ChaosArgs),
    FuzzChaos,
    Stress,
    /// Run hardware-dependent perf gates (excluded from `cargo xtask ci`).
    /// These tests are loose catastrophic-regression guards, not precision
    /// performance gates: no current environment is both canonical and
    /// timing-stable. Run them on a dedicated perf machine or locally on
    /// stable hardware when you need real interpretation.
    PerfGates,
    DevcontainerExec(DevcontainerExecArgs),
    Ci,
    /// Reproduce the full proof chain inside the canonical devcontainer.
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

#[derive(Args, Clone, Copy)]
pub(crate) struct BenchArgs {
    #[arg(long, value_enum, default_value_t = BenchSurface::Neutral)]
    surface: BenchSurface,
    #[arg(long)]
    save: bool,
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
    Smoke,
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
        XtaskCommand::Check => {
            util::cargo(["check", "--all-features"])?;
            util::cargo(["check", "--no-default-features"])
        }
        XtaskCommand::Test => {
            util::cargo(["nextest", "run", "--profile", "ci", "--all-features"])?;
            util::cargo(["test", "--doc", "--all-features"])
        }
        XtaskCommand::Clippy => util::cargo([
            "clippy",
            "--all-features",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
        XtaskCommand::Fmt => util::cargo(["fmt", "--check"]),
        XtaskCommand::Deny => commands::deny_split(),
        XtaskCommand::Bench(args) => bench::bench(args),
        XtaskCommand::Cover(args) => coverage::cover(args),
        XtaskCommand::Mutants(args) => commands::mutants(args),
        XtaskCommand::Fuzz(args) => commands::fuzz(args),
        XtaskCommand::Chaos(args) => commands::chaos(args),
        XtaskCommand::FuzzChaos => util::cargo([
            "test",
            "--test",
            "fuzz_chaos_feedback",
            "--all-features",
            "--release",
            "--",
            "--nocapture",
        ]),
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
                "--nocapture",
            ])?;
            bench::bench(BenchArgs {
                surface: BenchSurface::Neutral,
                save: false,
                compare: false,
                compile: false,
            })
        }
        XtaskCommand::PerfGates => commands::perf_gates(),
        XtaskCommand::DevcontainerExec(args) => devcontainer::devcontainer_exec(args),
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
            commands::integrity("traceability-check", [])?;
            commands::integrity("structural-check", [])
        }
        XtaskCommand::Docs(args) => docs::docs(args),
        XtaskCommand::Release(args) => commands::release(args),
    }
}
