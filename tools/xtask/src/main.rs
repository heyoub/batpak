use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(author, version, about = "Root developer command surface for batpak")]
struct Cli {
    #[command(subcommand)]
    command: XtaskCommand,
}

#[derive(Subcommand)]
enum XtaskCommand {
    Setup(SetupArgs),
    Quickstart,
    Doctor,
    Traceability,
    Structural,
    Check,
    Test,
    Clippy,
    Fmt,
    Deny,
    BenchCompile,
    Bench(BenchArgs),
    Cover(CoverArgs),
    Mutants(MutantsArgs),
    Fuzz(FuzzArgs),
    Chaos(ChaosArgs),
    FuzzChaos,
    Stress,
    Ci,
    PreCommit,
    Docs(DocsArgs),
    Release(ReleaseArgs),
}

#[derive(Args)]
struct SetupArgs {
    #[arg(long)]
    install_tools: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum BenchSurface {
    Neutral,
    Native,
}

#[derive(Args)]
struct BenchArgs {
    #[arg(long, value_enum, default_value_t = BenchSurface::Neutral)]
    surface: BenchSurface,
    #[arg(long)]
    save: bool,
    #[arg(long)]
    compare: bool,
    #[arg(long)]
    compile: bool,
}

#[derive(Args)]
struct CoverArgs {
    #[arg(long)]
    ci: bool,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    threshold: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum MutantMode {
    Smoke,
    Full,
}

#[derive(Args)]
struct MutantsArgs {
    #[arg(value_enum)]
    mode: MutantMode,
}

#[derive(Args)]
struct FuzzArgs {
    #[arg(long)]
    deep: bool,
}

#[derive(Args)]
struct ChaosArgs {
    #[arg(long)]
    deep: bool,
}

#[derive(Args)]
struct DocsArgs {
    #[arg(long)]
    open: bool,
}

#[derive(Args)]
struct ReleaseArgs {
    #[arg(long)]
    dry_run: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        XtaskCommand::Setup(args) => setup(args),
        XtaskCommand::Quickstart => quickstart(),
        XtaskCommand::Doctor => integrity("doctor", ["--strict"]),
        XtaskCommand::Traceability => integrity("traceability-check", []),
        XtaskCommand::Structural => integrity("structural-check", []),
        XtaskCommand::Check => {
            cargo(["check", "--all-features"])?;
            cargo(["check", "--no-default-features"])
        }
        XtaskCommand::Test => {
            cargo(["nextest", "run", "--profile", "ci", "--all-features"])?;
            cargo(["test", "--doc", "--all-features"])
        }
        XtaskCommand::Clippy => cargo([
            "clippy",
            "--all-features",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
        XtaskCommand::Fmt => cargo(["fmt", "--check"]),
        XtaskCommand::Deny => cargo(["deny", "check"]),
        XtaskCommand::BenchCompile => cargo(["bench", "--no-run", "--all-features"]),
        XtaskCommand::Bench(args) => bench(args),
        XtaskCommand::Cover(args) => cover(args),
        XtaskCommand::Mutants(args) => mutants(args),
        XtaskCommand::Fuzz(args) => fuzz(args),
        XtaskCommand::Chaos(args) => chaos(args),
        XtaskCommand::FuzzChaos => cargo([
            "test",
            "--test",
            "fuzz_chaos_feedback",
            "--all-features",
            "--release",
            "--",
            "--nocapture",
        ]),
        XtaskCommand::Stress => {
            fuzz(FuzzArgs { deep: false })?;
            chaos(ChaosArgs { deep: false })?;
            cargo([
                "test",
                "--test",
                "fuzz_chaos_feedback",
                "--all-features",
                "--release",
                "--",
                "--nocapture",
            ])?;
            bench(BenchArgs {
                surface: BenchSurface::Neutral,
                save: false,
                compare: false,
                compile: false,
            })
        }
        XtaskCommand::Ci => ci(),
        XtaskCommand::PreCommit => {
            cargo(["fmt", "--check"])?;
            cargo([
                "clippy",
                "--all-features",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ])?;
            integrity("traceability-check", [])?;
            integrity("structural-check", [])
        }
        XtaskCommand::Docs(args) => docs(args),
        XtaskCommand::Release(args) => release(args),
    }
}

fn setup(args: SetupArgs) -> Result<()> {
    let required = [
        (
            "cargo-nextest",
            &["install", "--locked", "cargo-nextest"][..],
        ),
        (
            "cargo-deny",
            &["install", "--locked", "cargo-deny@0.19.0"][..],
        ),
        (
            "cargo-llvm-cov",
            &["install", "--locked", "cargo-llvm-cov@0.8.5"][..],
        ),
        (
            "cargo-mutants",
            &["install", "--locked", "cargo-mutants@27.0.0"][..],
        ),
        ("mdbook", &["install", "--locked", "mdbook@0.4.52"][..]),
    ];

    let mut missing = Vec::new();
    for (tool, _) in required {
        if !command_succeeds(tool, ["--version"]) {
            missing.push(tool);
        }
    }

    if missing.is_empty() {
        println!("All developer tools are installed.");
    } else if args.install_tools {
        for (tool, install_args) in required {
            if missing.contains(&tool) {
                cargo(install_args)?;
            }
        }
    } else {
        println!("Missing tools: {}", missing.join(", "));
        println!("Run `cargo xtask setup --install-tools` to install the standard toolchain.");
    }

    if cfg!(windows) {
        println!("Native Windows detected. `cargo xtask doctor` will validate the host toolchain.");
    } else {
        println!("Use the checked-in devcontainer for the canonical environment.");
    }
    Ok(())
}

fn quickstart() -> Result<()> {
    cargo(["run", "--example", "quickstart"])
}

fn integrity<const N: usize>(subcommand: &str, extra: [&str; N]) -> Result<()> {
    let mut args = vec!["run", "--package", "batpak-integrity", "--", subcommand];
    args.extend(extra);
    cargo(args)
}

fn bench(args: BenchArgs) -> Result<()> {
    if args.compile {
        return cargo(["bench", "--no-run", "--all-features"]);
    }
    let mut command = Command::new(python());
    command.arg("./scripts/bench-report");
    command.arg("--surface");
    command.arg(match args.surface {
        BenchSurface::Neutral => "neutral",
        BenchSurface::Native => "native",
    });
    if args.save {
        command.arg("--save");
    }
    if args.compare {
        command.arg("--compare");
    }
    run(command)
}

fn cover(args: CoverArgs) -> Result<()> {
    let mut command = if cfg!(windows) {
        let mut cmd = Command::new("bash");
        cmd.arg("./scripts/coverage-feedback");
        cmd
    } else {
        Command::new("./scripts/coverage-feedback")
    };
    if args.ci {
        command.arg("--ci");
    }
    if args.json {
        command.arg("--json");
    }
    if let Some(threshold) = args.threshold {
        command.arg("--threshold").arg(threshold.to_string());
    }
    run(command)
}

fn mutants(args: MutantsArgs) -> Result<()> {
    let base_all = [
        "mutants",
        "--file",
        "src/store/*.rs",
        "--file",
        "src/wire.rs",
        "--file",
        "src/guard/*.rs",
        "--file",
        "src/pipeline/*.rs",
        "--exclude",
        "src/store/ancestors_clock.rs",
        "--all-features",
        "--test-tool",
        "cargo",
    ];
    let base_min = [
        "mutants",
        "--file",
        "src/store/*.rs",
        "--exclude",
        "src/store/ancestors_hash.rs",
        "--no-default-features",
        "--test-tool",
        "cargo",
    ];

    match args.mode {
        MutantMode::Smoke => {
            cargo(
                base_all
                    .into_iter()
                    .chain(["--shard", "1/12"])
                    .collect::<Vec<_>>(),
            )?;
            cargo(
                base_min
                    .into_iter()
                    .chain(["--shard", "1/12"])
                    .collect::<Vec<_>>(),
            )
        }
        MutantMode::Full => {
            cargo(base_all)?;
            cargo(base_min)
        }
    }
}

fn fuzz(args: FuzzArgs) -> Result<()> {
    let cases = if args.deep { "100000" } else { "10000" };
    let mut command = Command::new("cargo");
    command.env("PROPTEST_CASES", cases);
    command.args([
        "test",
        "--test",
        "fuzz_targets",
        "--all-features",
        "--release",
        "--",
        "--nocapture",
    ]);
    run(command)
}

fn chaos(args: ChaosArgs) -> Result<()> {
    let iterations = if args.deep { "5000" } else { "2000" };
    let mut command = Command::new("cargo");
    command.env("CHAOS_ITERATIONS", iterations);
    command.args([
        "test",
        "--test",
        "chaos_testing",
        "--all-features",
        "--release",
        "--",
        "--nocapture",
    ]);
    run(command)
}

fn ci() -> Result<()> {
    integrity("doctor", ["--strict"])?;
    integrity("traceability-check", [])?;
    integrity("structural-check", [])?;
    cargo(["fmt", "--check"])?;
    cargo([
        "clippy",
        "--all-features",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])?;
    // Bans, licenses, and sources are hard gates — they protect against
    // license violations, banned dependencies, and unauthorized registries.
    cargo(["deny", "check", "bans", "licenses", "sources"])?;
    // Advisories are run separately and treated as warn-only because the
    // upstream RustSec/advisory-db currently ships a malformed
    // RUSTSEC-2020-0105.md (abi_stable) that crashes cargo-deny's parser
    // before any filtering can occur. cargo-deny stderr is still printed,
    // so legitimate advisories remain visible — they just don't fail CI
    // until the upstream parse bug or the malformed file is fixed.
    // TODO: revert to a single `cargo deny check` once upstream is fixed.
    if cargo(["deny", "check", "advisories"]).is_err() {
        eprintln!(
            "warning: cargo-deny advisories check failed (upstream advisory-db parse issue) — \
             continuing without enforcement. See xtask main.rs for context."
        );
    }
    cargo(["nextest", "run", "--profile", "ci", "--all-features"])?;
    cargo(["test", "--doc", "--all-features"])?;
    cargo(["check", "--all-features"])?;
    cargo(["check", "--no-default-features"])?;
    cargo(["bench", "--no-run", "--all-features"])
}

fn docs(args: DocsArgs) -> Result<()> {
    if !command_succeeds("mdbook", ["--version"]) {
        bail!("mdbook is required for docs. Run `cargo xtask setup --install-tools`.");
    }

    let site_dir = PathBuf::from("target/site");
    if site_dir.exists() {
        fs::remove_dir_all(&site_dir).context("clear target/site")?;
    }
    fs::create_dir_all(&site_dir).context("create target/site")?;

    let mut mdbook = Command::new("mdbook");
    mdbook.args(["build", "guide", "--dest-dir", "../target/site/guide"]);
    run(mdbook)?;
    let mut cargo_doc = Command::new("cargo");
    cargo_doc.env("RUSTDOCFLAGS", "--cfg docsrs -D warnings");
    cargo_doc.args(["doc", "--all-features", "--no-deps"]);
    run(cargo_doc)?;
    copy_dir(Path::new("target/doc"), &site_dir.join("api"))?;

    fs::write(
        site_dir.join("index.html"),
        "<!doctype html><meta charset=\"utf-8\"><title>batpak docs</title><h1>batpak docs</h1><ul><li><a href=\"guide/\">Guide</a></li><li><a href=\"api/batpak/\">API</a></li></ul>",
    )?;

    if args.open {
        open_in_browser(site_dir.join("index.html"))?;
    }
    Ok(())
}

fn release(args: ReleaseArgs) -> Result<()> {
    ci()?;
    docs(DocsArgs { open: false })?;
    if args.dry_run {
        cargo(["publish", "--dry-run"])
    } else {
        bail!("release without --dry-run is intentionally disabled in xtask")
    }
}

fn cargo<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut command = Command::new("cargo");
    for arg in args {
        command.arg(arg.as_ref());
    }
    run(command)
}

fn run(mut command: Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("run {:?}", command))?;
    if status.success() {
        Ok(())
    } else {
        bail!("command failed with status {status}")
    }
}

fn command_succeeds<const N: usize>(program: &str, args: [&str; N]) -> bool {
    Command::new(program)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let entry_path = entry.path();
        let dest_path = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry_path, &dest_path)?;
        } else {
            fs::copy(&entry_path, &dest_path)?;
        }
    }
    Ok(())
}

fn open_in_browser(path: PathBuf) -> Result<()> {
    if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", path.to_string_lossy().as_ref()]);
        run(command)
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(path);
        run(command)
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        run(command)
    }
}
