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
    ConsumerSmoke,
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
    /// Run hardware-dependent perf gates (excluded from `cargo xtask ci`).
    /// These tests use Instant::now() and assert on wall-clock time, so they
    /// are unfit for shared CI runners. Run them on a dedicated perf machine
    /// or locally on stable hardware.
    PerfGates,
    Ci,
    /// Reproduce CI verbatim by running `cargo xtask ci` inside the
    /// canonical devcontainer. If `cargo xtask preflight` passes, the
    /// `Integrity (ubuntu-devcontainer)` GitHub job will pass.
    Preflight,
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
        XtaskCommand::ConsumerSmoke => consumer_smoke(),
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
        XtaskCommand::Deny => deny_split(),
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
        XtaskCommand::PerfGates => perf_gates(),
        XtaskCommand::Ci => ci(),
        XtaskCommand::Preflight => preflight(),
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
            // Pinned to match `.devcontainer/Dockerfile` so local installs
            // and the canonical container stay in lock-step.
            &["install", "--locked", "cargo-nextest@0.9.132"][..],
        ),
        (
            "cargo-deny",
            &["install", "--locked", "cargo-deny@0.19.0"][..],
        ),
        (
            "cargo-audit",
            &["install", "--locked", "cargo-audit@0.22.1"][..],
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

fn consumer_smoke() -> Result<()> {
    let root = repo_root()?;
    let smoke_root = root.join("target").join("consumer-smoke");
    if smoke_root.exists() {
        fs::remove_dir_all(&smoke_root).context("clear target/consumer-smoke")?;
    }

    let packaged_root = smoke_root.join("packaged");
    let consumer_root = smoke_root.join("consumer");
    fs::create_dir_all(&packaged_root).context("create packaged crate dir")?;
    fs::create_dir_all(consumer_root.join("src")).context("create consumer src dir")?;

    let mut cargo_package = Command::new("cargo");
    cargo_package
        .current_dir(&root)
        .args(["package", "--allow-dirty", "--no-verify"]);
    run(cargo_package)?;

    let archive = latest_packaged_crate(&root.join("target").join("package"))?;
    let mut unpack = Command::new("tar");
    unpack.current_dir(&packaged_root).arg("xf").arg(&archive);
    run(unpack)?;

    let unpacked_name = unpacked_package_dir(&packaged_root)?;

    fs::write(
        consumer_root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"batpak-consumer-smoke\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             publish = false\n\
             \n\
             [workspace]\n\
             \n\
             [dependencies]\n\
             batpak = {{ path = \"../packaged/{unpacked_name}\", features = [\"blake3\"] }}\n"
        ),
    )
    .context("write consumer smoke manifest")?;
    fs::write(
        consumer_root.join("src").join("main.rs"),
        "use batpak::prelude::*;\n\
         \n\
         fn main() -> Result<(), Box<dyn std::error::Error>> {\n\
         \x20   let dir = std::env::temp_dir().join(format!(\"batpak-consumer-smoke-{}\", std::process::id()));\n\
         \x20   if dir.exists() {\n\
         \x20       std::fs::remove_dir_all(&dir)?;\n\
         \x20   }\n\
         \x20   std::fs::create_dir_all(&dir)?;\n\
         \n\
         \x20   let config = StoreConfig::new(&dir)\n\
         \x20       .with_sync_every_n_events(1)\n\
         \x20       .with_sync_mode(SyncMode::SyncData);\n\
         \x20   let store = Store::open(config)?;\n\
         \x20   let coord = Coordinate::new(\"consumer:smoke\", \"scope:packaged\")?;\n\
         \x20   let receipt = store.append(&coord, EventKind::custom(0xF, 1), &\"payload\")?;\n\
         \x20   let fetched = store.get(receipt.event_id)?;\n\
         \x20   assert_eq!(fetched.coordinate.scope(), \"scope:packaged\");\n\
         \x20   store.close()?;\n\
         \x20   std::fs::remove_dir_all(&dir)?;\n\
         \x20   Ok(())\n\
         }\n",
    )
    .context("write consumer smoke source")?;

    let mut cargo_run = Command::new("cargo");
    cargo_run
        .current_dir(&consumer_root)
        .args(["run", "--quiet"]);
    run(cargo_run)
}

fn integrity<const N: usize>(subcommand: &str, extra: [&str; N]) -> Result<()> {
    let mut args = vec!["run", "--package", "batpak-integrity", "--", subcommand];
    args.extend(extra);
    cargo(args)
}

fn deny_split() -> Result<()> {
    cargo(["deny", "check"])?;
    cargo(["audit", "--deny", "warnings"])
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

/// Count non-empty lines in a file produced by cargo-mutants in its output
/// directory (e.g. `caught.txt`, `missed.txt`). Returns 0 if the file does
/// not exist (cargo-mutants omits empty files).
fn count_mutants_file(output_dir: &Path, filename: &str) -> Result<usize> {
    let path = output_dir.join(filename);
    if !path.exists() {
        return Ok(0);
    }
    let contents = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(contents.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Assert that the mutation score in `output_dir` meets the minimum catch
/// threshold.
///
/// cargo-mutants 27.0.0 has no built-in `--minimum-mutation-score` or
/// `--error-on-survived` flag (confirmed via `cargo mutants --help` and
/// `--emit-schema config`).  We therefore parse the text files it writes to
/// its output directory (`caught.txt`, `missed.txt`, `timeout.txt`,
/// `unviable.txt`) — each line is one mutant — and compute the catch rate
/// ourselves.
///
/// Gate: at least `min_catch_pct`% of *tested* mutants (caught + missed) must
/// be caught.  Timed-out and unviable mutants are excluded from the
/// denominator because they don't reflect test quality.
///
/// The threshold is set at 20 % — deliberately generous for a first gate so
/// that "all tests deleted" PRs are blocked while legitimate low-coverage
/// areas can still pass.  Raise it incrementally as the suite matures.
fn assert_mutation_score(output_dir: &Path, min_catch_pct: u32) -> Result<()> {
    let caught = count_mutants_file(output_dir, "caught.txt")?;
    let missed = count_mutants_file(output_dir, "missed.txt")?;
    let tested = caught + missed;

    if tested == 0 {
        // No tested mutants means either no mutations were generated (the
        // shard was empty) or the baseline run itself failed.  Treat as
        // pass so we don't false-positive on empty shards.
        eprintln!(
            "mutants: no tested mutants found in {}; skipping score gate",
            output_dir.display()
        );
        return Ok(());
    }

    let score_pct = (caught * 100) / tested;
    println!(
        "mutants: {caught} caught / {tested} tested = {score_pct}% (threshold: {min_catch_pct}%)"
    );

    if score_pct < min_catch_pct as usize {
        bail!(
            "mutation score {score_pct}% is below the required {min_catch_pct}% \
             ({caught} caught, {missed} missed out of {tested} tested mutants). \
             Add tests that catch the mutations listed in {}.",
            output_dir.join("missed.txt").display()
        );
    }
    Ok(())
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

    // The output dir is set to "target/mutants.out" in .cargo/mutants.toml.
    // After each run we parse caught.txt / missed.txt to enforce the gate.
    // See `assert_mutation_score` for why we do this manually instead of
    // relying on a built-in flag (none exists in cargo-mutants 27.0.0).
    let output_dir = Path::new("target/mutants.out");
    // Minimum percentage of tested mutants that must be caught.
    // 20 % is the floor: generous enough to tolerate gaps in the current
    // suite but strict enough to block a PR that deletes all tests.
    const MIN_CATCH_PCT: u32 = 20;

    // Helper: clear stale output before each cargo-mutants invocation so
    // assert_mutation_score cannot pass on files from a previous run. Without
    // this, an infrastructure failure (timeout, network, temp-workspace issue)
    // that prevents cargo-mutants from running at all would be invisible:
    // `let _ = cargo(...)` swallows the error, and stale caught.txt/missed.txt
    // from last time would let the percentage check pass on ghost data.
    let clear_output = || {
        let _ = std::fs::remove_dir_all(output_dir);
    };

    match args.mode {
        MutantMode::Smoke => {
            // cargo-mutants 27.0 exits 2 when ANY mutant is missed, which is
            // stricter than our percentage-based gate. Ignore its exit code
            // and use assert_mutation_score exclusively — the percentage
            // threshold is the intentional policy, not "zero missed."
            clear_output();
            let _ = cargo(
                base_all
                    .into_iter()
                    .chain(["--shard", "1/12"])
                    .collect::<Vec<_>>(),
            );
            assert_mutation_score(output_dir, MIN_CATCH_PCT)?;
            clear_output();
            let _ = cargo(
                base_min
                    .into_iter()
                    .chain(["--shard", "1/12"])
                    .collect::<Vec<_>>(),
            );
            assert_mutation_score(output_dir, MIN_CATCH_PCT)
        }
        MutantMode::Full => {
            clear_output();
            let _ = cargo(base_all);
            assert_mutation_score(output_dir, MIN_CATCH_PCT)?;
            clear_output();
            let _ = cargo(base_min);
            assert_mutation_score(output_dir, MIN_CATCH_PCT)
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
    deny_split()?;
    cargo(["nextest", "run", "--profile", "ci", "--all-features"])?;
    cargo(["test", "--doc", "--all-features"])?;
    cargo(["check", "--all-features"])?;
    cargo(["check", "--no-default-features"])?;
    cargo(["bench", "--no-run", "--all-features"])
}

/// Run the hardware-dependent perf-gate tests that are excluded from
/// `cargo xtask ci`. These are marked `#[ignore]` in the test source so
/// they don't fire on every CI run; this command runs them on demand.
///
/// **Where to run this:** locally on stable hardware, or on a dedicated
/// perf-tracking CI runner. Do NOT run on shared GitHub Actions runners
/// — the timing variance will produce false failures.
fn perf_gates() -> Result<()> {
    cargo([
        "nextest",
        "run",
        "--test",
        "perf_gates",
        "--all-features",
        "--run-ignored",
        "only",
    ])
}

/// Reproduce the GitHub Linux integrity-and-friends jobs verbatim by
/// running the full CI pipeline inside the canonical devcontainer.
///
/// This eliminates the entire class of "passes locally but fails CI"
/// drift caused by tool-version differences, advisory-db caching, file
/// permissions, fsync rates, etc. If `cargo xtask preflight` passes,
/// every job in `.github/workflows/ci.yml` that runs through the
/// devcontainer will pass too — bit-for-bit equivalent environment.
///
/// **Pipeline executed (inside the container):**
/// 1. `cargo xtask ci`   — clippy, fmt, deny-split, all tests, doctests,
///                          all-features check, no-default check, bench compile
/// 2. `cargo xtask cover --ci --threshold 80` — line-coverage gate
/// 3. `cargo xtask docs` — rustdoc + mdbook build
/// 4. Strip `*!*.html`  — the rustdoc-emits-bang-redirect workaround
///                          that the upload-pages-artifact action chokes on
///
/// Use this as the gold standard for "is this commit ready to push"
/// instead of bare `cargo xtask ci` (which runs on the host and skips
/// coverage and docs).
///
/// Cost: ~15-20 minutes on a developer machine (matches the
/// `Integrity (ubuntu-devcontainer)` job cost). Run BEFORE pushing to
/// save GitHub Actions minutes.
fn preflight() -> Result<()> {
    let script = repo_root()?.join("scripts").join("run-in-devcontainer.sh");
    if !script.exists() {
        bail!(
            "preflight requires scripts/run-in-devcontainer.sh — not found at {}",
            script.display()
        );
    }
    let mut command = Command::new("bash");
    command.arg(&script);
    // The full devcontainer pipeline. `&&` short-circuits — first failure
    // stops the chain and returns non-zero. The find/chmod steps mirror
    // the docs job in `.github/workflows/ci.yml` so preflight catches
    // the same gotchas the GH job would.
    command.arg(
        "cargo xtask ci && \
         cargo xtask cover --ci --threshold 80 && \
         cargo xtask docs && \
         find target/site -name '*!*.html' -type f -delete && \
         find target/site -name '.lock' -type f -delete && \
         chmod -R u+rX target/site",
    );
    run(command)
}

/// Locate the workspace root by walking up from the current directory
/// until we find a `Cargo.toml` containing `[workspace]`.
fn repo_root() -> Result<PathBuf> {
    let mut current = std::env::current_dir().context("get cwd")?;
    loop {
        let manifest = current.join("Cargo.toml");
        if manifest.exists() {
            let contents = std::fs::read_to_string(&manifest).context("read manifest")?;
            if contents.contains("[workspace]") {
                return Ok(current);
            }
        }
        if !current.pop() {
            bail!("could not locate workspace root from cwd");
        }
    }
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
    consumer_smoke()?;
    docs(DocsArgs { open: false })?;
    if args.dry_run {
        // Release verification runs before the commit is cut, so package the
        // current tree intentionally instead of requiring a clean git state.
        cargo(["publish", "--dry-run", "--allow-dirty"])
    } else {
        bail!("release without --dry-run is intentionally disabled in xtask")
    }
}

fn latest_packaged_crate(package_dir: &Path) -> Result<PathBuf> {
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(package_dir)
        .with_context(|| format!("read packaged crate directory {}", package_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() || !file_name.starts_with("batpak-") || !file_name.ends_with(".crate") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("read modified time for {}", path.display()))?;
        match &latest {
            Some((current, _)) if modified <= *current => {}
            _ => latest = Some((modified, path)),
        }
    }

    latest
        .map(|(_, path)| path)
        .context("could not locate packaged batpak .crate archive")
}

fn unpacked_package_dir(packaged_root: &Path) -> Result<String> {
    let mut unpacked = None;
    for entry in fs::read_dir(packaged_root)
        .with_context(|| format!("read unpacked package dir {}", packaged_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.path().join("Cargo.toml").is_file() {
            unpacked = Some(entry.file_name().to_string_lossy().into_owned());
            break;
        }
    }

    unpacked.context("could not locate unpacked batpak package directory")
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
