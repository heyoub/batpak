use crate::bench;
use crate::docs;
use crate::util::{cargo, command_succeeds, repo_root, run};
use crate::{
    BenchSurface, ChaosArgs, DocsArgs, FuzzArgs, MutantMode, MutantSurface, MutantsArgs,
    ReleaseArgs, SetupArgs,
};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Eq, PartialEq)]
enum InstallStrategy {
    PreferBinstall,
    SourceOnly,
}

const REPO_HOOKS_PATH: &str = ".githooks";
const PRE_COMMIT_HOOK: &str = ".githooks/pre-commit";
const MUTANTS_OUTPUT_ROOT: &str = "tools/xtask/target/mutants";
const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 85;
const REPO_MUTATION_PHASE: RepoMutationPhase = RepoMutationPhase::Phase0;
const CRITICAL_SMOKE_SHARD: &str = "1/8";
const REPO_WIDE_SMOKE_SHARD: &str = "1/24";
const REPO_MUTATION_THRESHOLDS: &[(RepoMutationPhase, u32)] = &[
    (RepoMutationPhase::Phase1, 35),
    (RepoMutationPhase::Phase2, 50),
    (RepoMutationPhase::Phase3, 65),
    (RepoMutationPhase::Phase4, 75),
    (RepoMutationPhase::Phase5, 85),
];
const REPO_WIDE_ALL_FEATURES_MUTANT_FILES: &[&str] = &[
    "src/store/**/*.rs",
    "src/wire.rs",
    "src/guard/*.rs",
    "src/pipeline/*.rs",
];
const REPO_WIDE_NO_DEFAULT_MUTANT_FILES: &[&str] = &["src/store/**/*.rs"];
const WRITER_COMMIT_MUTANT_FILES: &[&str] = &["src/store/write/*.rs"];
const CURSOR_MUTANT_FILES: &[&str] = &["src/store/delivery/cursor.rs"];
const PROJECTION_MUTANT_FILES: &[&str] = &["src/store/projection/flow.rs"];
const SEGMENT_SCAN_MUTANT_FILES: &[&str] = &["src/store/segment/scan.rs"];
const HASH_CHAIN_REPLAY_ALL_FEATURES_MUTANT_FILES: &[&str] = &[
    "src/store/ancestry/by_hash.rs",
    "src/store/cold_start/rebuild.rs",
];
const HASH_CHAIN_REPLAY_NO_DEFAULT_MUTANT_FILES: &[&str] = &[
    "src/store/ancestry/by_clock.rs",
    "src/store/cold_start/rebuild.rs",
];
const ALL_FEATURES_MUTANT_EXCLUDES: &[&str] = &["src/store/ancestry/by_clock.rs"];
const NO_DEFAULT_FEATURES_MUTANT_EXCLUDES: &[&str] = &["src/store/ancestry/by_hash.rs"];

#[derive(Clone, Debug, Eq, PartialEq)]
enum HookStatus {
    Installed,
    Default,
    Custom(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationScope {
    CriticalSeam,
    RepoWide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationEnforcement {
    Threshold { min_catch_pct: u32 },
    RecordOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoMutationPhase {
    Phase0,
    Phase1,
    Phase2,
    Phase3,
    Phase4,
    Phase5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationBaseline {
    Run,
    Skip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationSharding {
    RoundRobin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CriticalMutationSeam {
    slug: &'static str,
    label: &'static str,
    description: &'static str,
    surface: MutantSurface,
    paths: &'static [&'static str],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MutationLane {
    label: String,
    slug: String,
    description: &'static str,
    scope: MutationScope,
    surface: MutantSurface,
    baseline: MutationBaseline,
    shard: Option<String>,
    sharding: Option<MutationSharding>,
    enforcement: MutationEnforcement,
    paths: &'static [&'static str],
    excludes: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MutationScore {
    caught: usize,
    missed: usize,
    timed_out: usize,
    unviable: usize,
    executed: usize,
    scored: usize,
    score_pct: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MutantExecutionPlan {
    DescribePolicy,
    Run(Vec<MutationLane>),
}

pub(crate) fn setup(args: SetupArgs) -> Result<()> {
    let required = [
        (
            "cargo-nextest",
            "cargo-nextest@0.9.132",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-deny",
            "cargo-deny@0.19.0",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-audit",
            "cargo-audit@0.22.1",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-llvm-cov",
            "cargo-llvm-cov@0.8.5",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-mutants",
            "cargo-mutants@27.0.0",
            InstallStrategy::SourceOnly,
        ),
    ];

    let mut missing = Vec::new();
    for (tool, _, _) in required {
        if !command_succeeds(tool, ["--version"]) {
            missing.push(tool);
        }
    }

    if missing.is_empty() {
        println!("All developer tools are installed.");
    } else if args.install_tools {
        if required.iter().any(|(tool, _, strategy)| {
            missing.contains(tool) && *strategy == InstallStrategy::PreferBinstall
        }) {
            ensure_binstall_helper()?;
        }
        for (tool, spec, strategy) in required {
            if missing.contains(&tool) {
                install_tool(spec, strategy)?;
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
    let hook_status = if args.install_tools {
        maybe_install_repo_hooks().map(|status| (status, true))
    } else {
        repo_hook_status().map(|status| (status, false))
    };
    match hook_status {
        Ok((status, attempted_install)) => report_hook_install_result(status, attempted_install),
        Err(err) => eprintln!("setup: warning: could not inspect/install repo hooks: {err:#}"),
    }
    Ok(())
}

pub(crate) fn install_hooks() -> Result<()> {
    report_hook_install_result(maybe_install_repo_hooks()?, true);
    Ok(())
}

pub(crate) fn doctor() -> Result<()> {
    integrity("doctor", ["--strict"])?;
    match repo_hook_status() {
        Ok(HookStatus::Installed) => {}
        Ok(HookStatus::Default) => {
            eprintln!(
                "doctor: warning: repo-managed hooks are not installed. Run `cargo xtask install-hooks` to wire `.githooks/pre-commit`."
            );
        }
        Ok(HookStatus::Custom(path)) => {
            eprintln!(
                "doctor: warning: custom git hooks path `{path}` is active, so `.githooks/pre-commit` is not managing pre-commit checks. Clear or change `core.hooksPath`, then run `cargo xtask install-hooks` if you want the repo hook surface."
            );
        }
        Err(err) => {
            eprintln!("doctor: warning: could not inspect git hooks path: {err:#}");
        }
    }
    Ok(())
}

fn maybe_install_repo_hooks() -> Result<HookStatus> {
    let root = repo_root()?;
    let hook = root.join(PRE_COMMIT_HOOK);
    if !hook.exists() {
        bail!(
            "repo hook surface is missing `{}`; restore the tracked hook before installing",
            hook.display()
        );
    }

    match repo_hook_status()? {
        HookStatus::Installed => Ok(HookStatus::Installed),
        HookStatus::Custom(path) => Ok(HookStatus::Custom(path)),
        HookStatus::Default => {
            let mut command = Command::new("git");
            command
                .current_dir(&root)
                .args(["config", "core.hooksPath", REPO_HOOKS_PATH]);
            run(command)?;
            Ok(HookStatus::Installed)
        }
    }
}

fn repo_hook_status() -> Result<HookStatus> {
    let root = repo_root()?;
    let output = Command::new("git")
        .current_dir(&root)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .context("inspect git core.hooksPath")?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if path.is_empty() || is_default_hooks_path(&root, &path) {
            return Ok(HookStatus::Default);
        }
        if matches_repo_hooks_path(&root, &path) {
            let hook = root.join(PRE_COMMIT_HOOK);
            if hook.exists() {
                return Ok(HookStatus::Installed);
            }
            return Ok(HookStatus::Custom(path));
        }
        return Ok(HookStatus::Custom(path));
    }

    if output.status.code() == Some(1) {
        return Ok(HookStatus::Default);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "git config --get core.hooksPath failed with status {}: {}",
        output.status,
        stderr.trim()
    )
}

fn is_default_hooks_path(root: &Path, configured_path: &str) -> bool {
    configured_path == ".git/hooks"
        || resolved_git_path(root, configured_path)
            == normalize_path(&root.join(".git").join("hooks"))
}

fn matches_repo_hooks_path(root: &Path, configured_path: &str) -> bool {
    resolved_git_path(root, configured_path) == normalize_path(&root.join(REPO_HOOKS_PATH))
}

fn resolved_git_path(root: &Path, configured_path: &str) -> PathBuf {
    let path = Path::new(configured_path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    normalize_path(&resolved)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn report_hook_install_result(status: HookStatus, attempted_install: bool) {
    match status {
        HookStatus::Installed if attempted_install => {
            println!("Repo hooks are installed at `{REPO_HOOKS_PATH}`.");
        }
        HookStatus::Installed => {
            println!("Repo hooks are already installed at `{REPO_HOOKS_PATH}`.");
        }
        HookStatus::Default => {
            println!(
                "Repo hooks are not installed. Run `cargo xtask install-hooks` to wire `.githooks/pre-commit`."
            );
        }
        HookStatus::Custom(path) => {
            println!(
                "Custom git hooks path `{path}` is active; leaving it unchanged. To opt into the repo-managed hook surface, set `git config core.hooksPath {REPO_HOOKS_PATH}` or clear the custom path first, then run `cargo xtask install-hooks`."
            );
        }
    }
}

fn ensure_binstall_helper() -> Result<()> {
    if command_succeeds("cargo", ["binstall", "--version"]) {
        return Ok(());
    }

    let mut install = Command::new("cargo");
    install.args(["install", "cargo-binstall"]);
    run(install)
}

fn install_tool(spec: &str, strategy: InstallStrategy) -> Result<()> {
    // Prefer prebuilt binaries when available so `cargo xtask setup --install-tools` and the
    // devcontainer bootstrap do not spend unnecessary time compiling the
    // helper toolchain from source.
    if strategy == InstallStrategy::PreferBinstall
        && command_succeeds("cargo", ["binstall", "--version"])
    {
        let mut binstall = Command::new("cargo");
        binstall.args(["binstall", "--no-confirm", spec]);
        if run(binstall).is_ok() {
            return Ok(());
        }
        eprintln!("binstall fallback: `{spec}` binary install failed; retrying with cargo install");
    }

    cargo(["install", "--locked", spec])
}

pub(crate) fn quickstart() -> Result<()> {
    cargo(["run", "--example", "quickstart"])
}

pub(crate) fn consumer_smoke() -> Result<()> {
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

pub(crate) fn integrity<const N: usize>(subcommand: &str, extra: [&str; N]) -> Result<()> {
    let mut args = vec!["run", "--package", "batpak-integrity", "--", subcommand];
    args.extend(extra);
    cargo(args)
}

pub(crate) fn deny_split() -> Result<()> {
    cargo(["deny", "check"])?;
    cargo(["audit", "--deny", "warnings"])
}

fn count_mutants_file(output_dir: &Path, filename: &str) -> Result<usize> {
    let path = output_dir.join(filename);
    if !path.exists() {
        return Ok(0);
    }
    let contents = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(contents.lines().filter(|l| !l.trim().is_empty()).count())
}

fn mutation_score(output_dir: &Path) -> Result<MutationScore> {
    let caught = count_mutants_file(output_dir, "caught.txt")?;
    let missed = count_mutants_file(output_dir, "missed.txt")?;
    let timed_out = count_mutants_file(output_dir, "timeout.txt")?;
    let unviable = count_mutants_file(output_dir, "unviable.txt")?;
    let scored = caught + missed;
    let executed = scored + timed_out + unviable;
    let score_pct = if scored == 0 {
        None
    } else {
        Some((caught * 100) / scored)
    };
    Ok(MutationScore {
        caught,
        missed,
        timed_out,
        unviable,
        executed,
        scored,
        score_pct,
    })
}

impl MutationScope {
    fn name(self) -> &'static str {
        match self {
            MutationScope::CriticalSeam => "critical seam",
            MutationScope::RepoWide => "repo-wide",
        }
    }
}

impl MutationLane {
    fn critical(seam: CriticalMutationSeam) -> Self {
        Self {
            label: format!("{} ({})", seam.label, surface_name(seam.surface)),
            slug: seam.slug.to_owned(),
            description: seam.description,
            scope: MutationScope::CriticalSeam,
            surface: seam.surface,
            baseline: MutationBaseline::Run,
            shard: None,
            sharding: None,
            enforcement: MutationEnforcement::Threshold {
                min_catch_pct: CRITICAL_SEAM_MIN_CATCH_PCT,
            },
            paths: seam.paths,
            excludes: surface_excludes(seam.surface),
        }
    }

    fn critical_smoke(seam: CriticalMutationSeam) -> Self {
        Self {
            label: format!(
                "{} ({}, smoke shard {CRITICAL_SMOKE_SHARD})",
                seam.label,
                surface_name(seam.surface)
            ),
            slug: seam.slug.to_owned(),
            description: seam.description,
            scope: MutationScope::CriticalSeam,
            surface: seam.surface,
            baseline: MutationBaseline::Run,
            shard: Some(CRITICAL_SMOKE_SHARD.to_owned()),
            sharding: Some(MutationSharding::RoundRobin),
            enforcement: MutationEnforcement::Threshold {
                min_catch_pct: CRITICAL_SEAM_MIN_CATCH_PCT,
            },
            paths: seam.paths,
            excludes: surface_excludes(seam.surface),
        }
    }

    fn repo_wide(surface: MutantSurface, shard: Option<&str>) -> Self {
        Self {
            label: match shard {
                Some(shard) => format!("repo-wide ({}, shard {shard})", surface_name(surface)),
                None => format!("repo-wide ({})", surface_name(surface)),
            },
            slug: "repo-wide".to_owned(),
            description: "repo-wide mutation ratchet lane",
            scope: MutationScope::RepoWide,
            surface,
            baseline: MutationBaseline::Run,
            shard: shard.map(str::to_owned),
            sharding: None,
            enforcement: current_repo_mutation_enforcement(),
            paths: repo_wide_paths(surface),
            excludes: surface_excludes(surface),
        }
    }

    fn repo_wide_smoke(surface: MutantSurface) -> Self {
        Self {
            label: format!(
                "repo-wide ({}, smoke shard {REPO_WIDE_SMOKE_SHARD})",
                surface_name(surface)
            ),
            slug: "repo-wide".to_owned(),
            description: "repo-wide mutation ratchet lane",
            scope: MutationScope::RepoWide,
            surface,
            baseline: MutationBaseline::Run,
            shard: Some(REPO_WIDE_SMOKE_SHARD.to_owned()),
            sharding: Some(MutationSharding::RoundRobin),
            enforcement: current_repo_mutation_enforcement(),
            paths: repo_wide_paths(surface),
            excludes: surface_excludes(surface),
        }
    }

    #[cfg(test)]
    fn with_baseline(self, baseline: MutationBaseline) -> Self {
        Self { baseline, ..self }
    }

    fn output_dir(&self) -> PathBuf {
        Path::new(MUTANTS_OUTPUT_ROOT).join(self.slug())
    }

    fn slug(&self) -> String {
        let surface = surface_slug(self.surface);
        match self.shard.as_deref() {
            Some(shard) => format!("{}-{surface}-{}", self.slug, shard.replace('/', "-of-")),
            None => format!("{}-{surface}", self.slug),
        }
    }

    fn allows_nonzero_exit(&self, score: MutationScore) -> bool {
        matches!(
            self.enforcement,
            MutationEnforcement::Threshold { .. } | MutationEnforcement::RecordOnly
        ) && score.executed > 0
    }

    fn policy_line(&self) -> String {
        match self.enforcement {
            MutationEnforcement::Threshold { min_catch_pct } => match self.shard.as_deref() {
                Some(shard) => format!(
                    "{} `{}` on {} shard {shard}: threshold {}%",
                    self.scope.name(),
                    self.label,
                    surface_name(self.surface),
                    min_catch_pct,
                ),
                None => format!(
                    "{} `{}` on {}: threshold {}%",
                    self.scope.name(),
                    self.label,
                    surface_name(self.surface),
                    min_catch_pct,
                ),
            },
            MutationEnforcement::RecordOnly => format!(
                "{} `{}` on {}: record-only for current ratchet phase",
                self.scope.name(),
                self.label,
                surface_name(self.surface)
            ),
        }
    }
}

fn surface_name(surface: MutantSurface) -> &'static str {
    match surface {
        MutantSurface::AllFeatures => "all-features",
        MutantSurface::NoDefaultFeatures => "no-default-features",
    }
}

fn surface_slug(surface: MutantSurface) -> &'static str {
    surface_name(surface)
}

fn repo_wide_paths(surface: MutantSurface) -> &'static [&'static str] {
    match surface {
        MutantSurface::AllFeatures => REPO_WIDE_ALL_FEATURES_MUTANT_FILES,
        MutantSurface::NoDefaultFeatures => REPO_WIDE_NO_DEFAULT_MUTANT_FILES,
    }
}

fn surface_excludes(surface: MutantSurface) -> &'static [&'static str] {
    match surface {
        MutantSurface::AllFeatures => ALL_FEATURES_MUTANT_EXCLUDES,
        MutantSurface::NoDefaultFeatures => NO_DEFAULT_FEATURES_MUTANT_EXCLUDES,
    }
}

fn critical_mutation_seams() -> &'static [CriticalMutationSeam] {
    &[
        CriticalMutationSeam {
            slug: "writer-commit",
            label: "writer commit protocol",
            description: "writer commit protocol and staging/publish ordering",
            surface: MutantSurface::AllFeatures,
            paths: WRITER_COMMIT_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "cursor-delivery",
            label: "cursor delivery and checkpoints",
            description: "cursor delivery/checkpoint logic",
            surface: MutantSurface::AllFeatures,
            paths: CURSOR_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "projection-flow",
            label: "projection replay and freshness",
            description: "projection replay/freshness logic",
            surface: MutantSurface::AllFeatures,
            paths: PROJECTION_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "segment-scan",
            label: "segment scan corruption handling",
            description: "segment scan and corruption handling",
            surface: MutantSurface::AllFeatures,
            paths: SEGMENT_SCAN_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "hash-chain-replay-all-features",
            label: "hash-chain and replay consistency",
            description: "hash-chain / replay consistency logic (blake3 lane)",
            surface: MutantSurface::AllFeatures,
            paths: HASH_CHAIN_REPLAY_ALL_FEATURES_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "hash-chain-replay-no-default",
            label: "hash-chain and replay consistency",
            description: "hash-chain / replay consistency logic (no-default lane)",
            surface: MutantSurface::NoDefaultFeatures,
            paths: HASH_CHAIN_REPLAY_NO_DEFAULT_MUTANT_FILES,
        },
    ]
}

fn current_repo_mutation_floor() -> Option<u32> {
    match REPO_MUTATION_PHASE {
        RepoMutationPhase::Phase0 => None,
        RepoMutationPhase::Phase1 => Some(35),
        RepoMutationPhase::Phase2 => Some(50),
        RepoMutationPhase::Phase3 => Some(65),
        RepoMutationPhase::Phase4 => Some(75),
        RepoMutationPhase::Phase5 => Some(85),
    }
}

fn current_repo_mutation_enforcement() -> MutationEnforcement {
    match current_repo_mutation_floor() {
        Some(min_catch_pct) => MutationEnforcement::Threshold { min_catch_pct },
        None => MutationEnforcement::RecordOnly,
    }
}

fn assert_mutation_policy(
    lane: &MutationLane,
    output_dir: &Path,
    score: MutationScore,
) -> Result<()> {
    if score.executed == 0 {
        bail!(
            "mutants: `{}` produced no executed mutants in {}. Treating this as a failure because \
             the mutation surface produced no evidence.",
            lane.label,
            output_dir.display()
        );
    }

    match score.score_pct {
        Some(score_pct) => println!(
            "mutants: `{}` => {} caught / {} scored = {}% (executed: {}, missed: {}, timed out: {}, unviable: {})",
            lane.label,
            score.caught,
            score.scored,
            score_pct,
            score.executed,
            score.missed,
            score.timed_out,
            score.unviable,
        ),
        None => println!(
            "mutants: `{}` => no scoreable mutants (executed: {}, timed out: {}, unviable: {})",
            lane.label,
            score.executed,
            score.timed_out,
            score.unviable,
        ),
    }

    if score.timed_out > 0 {
        bail!(
            "mutation lane `{}` timed out on {} mutants. Investigate {}.",
            lane.label,
            score.timed_out,
            output_dir.join("timeout.txt").display()
        );
    }

    if score.unviable > 0 {
        println!(
            "mutants: `{}` recorded {} unviable mutants in {}.",
            lane.label,
            score.unviable,
            output_dir.join("unviable.txt").display()
        );
    }

    let Some(score_pct) = score.score_pct else {
        println!(
            "mutants: `{}` produced execution evidence but no scoreable caught/missed mutants, so threshold math is not applied for this lane.",
            lane.label
        );
        return Ok(());
    };

    match lane.enforcement {
        MutationEnforcement::Threshold { min_catch_pct } => {
            if score_pct < min_catch_pct as usize {
                bail!(
                    "mutation score for `{}` is {}%, below the required {}% \
                     ({} caught, {} missed out of {} scored mutants; {} executed total). Add tests that catch the \
                     mutations listed in {}.",
                    lane.label,
                    score_pct,
                    min_catch_pct,
                    score.caught,
                    score.missed,
                    score.scored,
                    score.executed,
                    output_dir.join("missed.txt").display()
                );
            }
            if lane.scope == MutationScope::RepoWide {
                if let Some(next_floor) = next_ratchet_floor(score_pct, Some(min_catch_pct)) {
                    println!(
                        "mutants: `{}` is above the current repo-wide ratchet floor; a future raise to {}% is available.",
                        lane.label, next_floor
                    );
                }
            }
        }
        MutationEnforcement::RecordOnly => {
            if let Some(next_floor) = next_ratchet_floor(score_pct, None) {
                println!(
                    "mutants: `{}` is in repo-wide record-only mode for this phase. Current score {}% supports a future ratchet to {}%.",
                    lane.label, score_pct, next_floor
                );
            }
        }
    }

    Ok(())
}

fn next_ratchet_floor(score_pct: usize, current_floor: Option<u32>) -> Option<u32> {
    REPO_MUTATION_THRESHOLDS
        .iter()
        .map(|(_, floor)| *floor)
        .filter(|floor| Some(*floor) > current_floor && score_pct >= *floor as usize)
        .max()
}

fn with_batched_baseline(mut lanes: Vec<MutationLane>) -> Vec<MutationLane> {
    let mut first = true;
    for lane in &mut lanes {
        lane.baseline = if first {
            first = false;
            MutationBaseline::Run
        } else {
            MutationBaseline::Skip
        };
    }
    lanes
}

fn mutants_command(lane: &MutationLane, output_dir: &Path) -> Vec<String> {
    let mut args = vec![
        "mutants".to_owned(),
        "--output".to_owned(),
        output_dir.display().to_string(),
        "--in-place".to_owned(),
    ];

    args.push("--baseline".to_owned());
    args.push(match lane.baseline {
        MutationBaseline::Run => "run".to_owned(),
        MutationBaseline::Skip => "skip".to_owned(),
    });

    for pattern in lane.paths {
        args.push("--file".to_owned());
        args.push((*pattern).to_owned());
    }

    for exclude in lane.excludes {
        args.push("--exclude".to_owned());
        args.push((*exclude).to_owned());
    }

    match lane.surface {
        MutantSurface::AllFeatures => args.push("--all-features".to_owned()),
        MutantSurface::NoDefaultFeatures => args.push("--no-default-features".to_owned()),
    }
    args.push("--cargo-arg".to_owned());
    args.push("--locked".to_owned());
    args.push("--test-tool".to_owned());
    args.push("cargo".to_owned());

    if let Some(shard) = lane.shard.as_deref() {
        args.push("--shard".to_owned());
        args.push(shard.to_owned());
    }

    if let Some(sharding) = lane.sharding {
        args.push("--sharding".to_owned());
        args.push(match sharding {
            MutationSharding::RoundRobin => "round-robin".to_owned(),
        });
    }

    args
}

fn critical_mutation_lanes() -> Vec<MutationLane> {
    critical_mutation_seams()
        .iter()
        .copied()
        .map(MutationLane::critical)
        .collect()
}

fn critical_mutation_smoke_lanes() -> Vec<MutationLane> {
    critical_mutation_seams()
        .iter()
        .copied()
        .map(MutationLane::critical_smoke)
        .collect()
}

fn repo_wide_mutation_lanes(
    surfaces: Vec<MutantSurface>,
    shard: Option<&str>,
) -> Vec<MutationLane> {
    surfaces
        .into_iter()
        .map(|surface| MutationLane::repo_wide(surface, shard))
        .collect()
}

fn build_mutant_execution_plan(args: &MutantsArgs) -> Result<MutantExecutionPlan> {
    match args.mode {
        MutantMode::Policy => {
            if args.surface.is_some() || args.shard.is_some() {
                bail!(
                    "`cargo xtask mutants policy` only describes repo-owned policy; do not pass \
                     --surface or --shard"
                );
            }
            Ok(MutantExecutionPlan::DescribePolicy)
        }
        MutantMode::Smoke => {
            if args.surface.is_some() || args.shard.is_some() {
                bail!(
                    "`cargo xtask mutants smoke` owns its fixed policy lanes; do not pass \
                     --surface or --shard"
                );
            }

            let mut lanes = critical_mutation_smoke_lanes();
            lanes.extend([
                MutationLane::repo_wide_smoke(MutantSurface::AllFeatures),
                MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures),
            ]);
            Ok(MutantExecutionPlan::Run(with_batched_baseline(lanes)))
        }
        MutantMode::Full => {
            let surfaces = args.surface.map_or_else(
                || vec![MutantSurface::AllFeatures, MutantSurface::NoDefaultFeatures],
                |surface| vec![surface],
            );

            if args.surface.is_some() || args.shard.is_some() {
                return Ok(MutantExecutionPlan::Run(repo_wide_mutation_lanes(
                    surfaces,
                    args.shard.as_deref(),
                )));
            }

            let mut lanes = critical_mutation_lanes();
            lanes.extend(repo_wide_mutation_lanes(surfaces, None));
            Ok(MutantExecutionPlan::Run(with_batched_baseline(lanes)))
        }
    }
}

fn print_mutation_policy() {
    println!("Mutation policy:");
    println!(
        "- `cargo xtask mutants smoke`: run representative round-robin shards of every critical seam at {}%, then repo-wide {} lanes using the current ratchet phase. Only the first lane runs a fresh baseline; later lanes reuse it with `--baseline skip`.",
        CRITICAL_SEAM_MIN_CATCH_PCT,
        REPO_WIDE_SMOKE_SHARD,
    );
    println!(
        "- `cargo xtask mutants full`: with no overrides, run the full policy; with `--surface` and/or `--shard`, run only the requested repo-wide ratchet lane."
    );
    match current_repo_mutation_floor() {
        Some(floor) => println!(
            "- Repo-wide ratchet phase: {:?} (current floor: {floor}%).",
            REPO_MUTATION_PHASE
        ),
        None => println!(
            "- Repo-wide ratchet phase: {:?} (record-only; no floor enforced yet).",
            REPO_MUTATION_PHASE
        ),
    }
    println!("- Repo-wide ratchet phases staged in code:");
    for (phase, floor) in REPO_MUTATION_THRESHOLDS {
        println!("  {:?} => {floor}%", phase);
    }
    for lane in critical_mutation_smoke_lanes() {
        println!("- {}", lane.policy_line());
    }
    for lane in [
        MutationLane::repo_wide_smoke(MutantSurface::AllFeatures),
        MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures),
    ] {
        println!("- {}", lane.policy_line());
    }
    println!("- Critical seam surfaces:");
    for seam in critical_mutation_seams() {
        println!(
            "  {} [{} on {}]: {}",
            seam.label,
            seam.slug,
            surface_name(seam.surface),
            seam.description
        );
        for pattern in seam.paths {
            println!("    {pattern}");
        }
    }
    println!("- Repo-wide patterns:");
    for pattern in REPO_WIDE_ALL_FEATURES_MUTANT_FILES {
        println!("  all-features: {pattern}");
    }
    for pattern in REPO_WIDE_NO_DEFAULT_MUTANT_FILES {
        println!("  no-default-features: {pattern}");
    }
    println!(
        "- Surface-specific excludes: all-features => {}, no-default-features => {}.",
        ALL_FEATURES_MUTANT_EXCLUDES.join(", "),
        NO_DEFAULT_FEATURES_MUTANT_EXCLUDES.join(", ")
    );
    println!(
        "- Mutation artifacts live under `{MUTANTS_OUTPUT_ROOT}` so xtask owns the scratch surface."
    );
}

fn run_mutation_lane(lane: &MutationLane) -> Result<()> {
    let output_dir = lane.output_dir();
    let _ = std::fs::remove_dir_all(&output_dir);
    if let Some(parent) = output_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create cargo-mutants output parent directory {} for `{}`",
                parent.display(),
                lane.label
            )
        })?;
    }

    let mut command = Command::new("cargo");
    // cargo-mutants in `--in-place` mode can interact badly with incremental
    // artifacts and produce linker-only failures that disappear under a fresh
    // rebuild. Keep mutation receipts honest by forcing clean codegen for the
    // lane instead of inheriting ambient incremental state.
    command.env("CARGO_INCREMENTAL", "0");
    command.args(mutants_command(lane, &output_dir));
    let status = command
        .status()
        .with_context(|| format!("run cargo-mutants lane `{}`", lane.label))?;

    let score = mutation_score(&output_dir).with_context(|| {
        format!(
            "read cargo-mutants results for `{}` from {}",
            lane.label,
            output_dir.display()
        )
    })?;

    let policy_result = assert_mutation_policy(lane, &output_dir, score);
    if status.success() || lane.allows_nonzero_exit(score) {
        return policy_result;
    }

    match policy_result {
        Ok(()) => bail!(
            "cargo-mutants exited with status {status} for `{}` even though the xtask policy \
             checks passed. Inspect {}.",
            lane.label,
            output_dir.display()
        ),
        Err(err) => Err(err).context(format!(
            "cargo-mutants exited with status {status} for `{}`; inspect {}",
            lane.label,
            output_dir.display()
        )),
    }
}

pub(crate) fn mutants(args: MutantsArgs) -> Result<()> {
    match build_mutant_execution_plan(&args)? {
        MutantExecutionPlan::DescribePolicy => {
            print_mutation_policy();
            Ok(())
        }
        MutantExecutionPlan::Run(lanes) => {
            for lane in lanes {
                run_mutation_lane(&lane)?;
            }
            Ok(())
        }
    }
}

pub(crate) fn fuzz(args: FuzzArgs) -> Result<()> {
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

pub(crate) fn chaos(args: ChaosArgs) -> Result<()> {
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

pub(crate) fn ci() -> Result<()> {
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
    bench::bench_compile(BenchSurface::Neutral)?;
    bench::bench_compile(BenchSurface::Native)
}

pub(crate) fn perf_gates() -> Result<()> {
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

pub(crate) fn release(args: ReleaseArgs) -> Result<()> {
    ci()?;
    consumer_smoke()?;
    docs::docs(DocsArgs { open: false })?;
    if args.dry_run {
        cargo(["publish", "--dry-run", "--allow-dirty"])
    } else {
        bail!("release without --dry-run is intentionally disabled in xtask")
    }
}

fn latest_packaged_crate(package_dir: &Path) -> Result<std::path::PathBuf> {
    let mut latest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
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

#[cfg(test)]
mod tests {
    use super::{
        assert_mutation_policy, build_mutant_execution_plan, critical_mutation_lanes,
        critical_mutation_smoke_lanes, is_default_hooks_path, matches_repo_hooks_path,
        mutants_command, next_ratchet_floor, surface_excludes, MutantExecutionPlan,
        MutationBaseline, MutationLane, MutationScope, MutationScore, MutationSharding,
        RepoMutationPhase, CURSOR_MUTANT_FILES, PROJECTION_MUTANT_FILES, REPO_MUTATION_PHASE,
        REPO_WIDE_ALL_FEATURES_MUTANT_FILES, REPO_WIDE_NO_DEFAULT_MUTANT_FILES,
        WRITER_COMMIT_MUTANT_FILES,
    };
    use crate::{MutantMode, MutantSurface, MutantsArgs};
    use std::path::{Path, PathBuf};

    #[test]
    fn repo_hooks_path_matches_relative_and_absolute_spellings() {
        let root = Path::new("/workspace/batpak");
        assert!(matches_repo_hooks_path(root, ".githooks"));
        assert!(matches_repo_hooks_path(root, "./.githooks"));
        assert!(matches_repo_hooks_path(root, "/workspace/batpak/.githooks"));
    }

    #[test]
    fn default_git_hooks_path_matches_relative_and_absolute_spellings() {
        let root = Path::new("/workspace/batpak");
        assert!(is_default_hooks_path(root, ".git/hooks"));
        assert!(is_default_hooks_path(root, "/workspace/batpak/.git/hooks"));
    }

    #[test]
    fn mutants_smoke_plan_runs_critical_then_repo_wide_ratchet_lanes() {
        let plan = build_mutant_execution_plan(&MutantsArgs {
            mode: MutantMode::Smoke,
            surface: None,
            shard: None,
        })
        .expect("smoke plan");

        assert_eq!(
            plan,
            MutantExecutionPlan::Run(vec![
                critical_mutation_smoke_lanes()[0]
                    .clone()
                    .with_baseline(MutationBaseline::Run),
                critical_mutation_smoke_lanes()[1]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[2]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[3]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[4]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[5]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                MutationLane::repo_wide_smoke(MutantSurface::AllFeatures)
                    .with_baseline(MutationBaseline::Skip),
                MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures)
                    .with_baseline(MutationBaseline::Skip),
            ])
        );
    }

    #[test]
    fn mutants_full_with_overrides_stays_repo_wide_only() {
        let plan = build_mutant_execution_plan(&MutantsArgs {
            mode: MutantMode::Full,
            surface: Some(MutantSurface::AllFeatures),
            shard: Some("3/12".to_owned()),
        })
        .expect("full plan");

        assert_eq!(
            plan,
            MutantExecutionPlan::Run(vec![MutationLane::repo_wide(
                MutantSurface::AllFeatures,
                Some("3/12"),
            )])
        );
    }

    #[test]
    fn mutants_writer_commit_surface_stays_xtask_owned() {
        let lane = critical_mutation_lanes()
            .into_iter()
            .find(|lane| lane.slug == "writer-commit")
            .expect("writer commit seam");
        assert_eq!(
            mutants_command(
                &lane,
                Path::new("tools/xtask/target/mutants/writer-commit-all-features")
            ),
            vec![
                "mutants",
                "--output",
                "tools/xtask/target/mutants/writer-commit-all-features",
                "--in-place",
                "--baseline",
                "run",
                "--file",
                "src/store/write/*.rs",
                "--exclude",
                "src/store/ancestry/by_clock.rs",
                "--all-features",
                "--cargo-arg",
                "--locked",
                "--test-tool",
                "cargo",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mutants_full_no_default_surface_stays_xtask_owned() {
        let lane = MutationLane::repo_wide(MutantSurface::NoDefaultFeatures, None);
        assert_eq!(
            mutants_command(
                &lane,
                Path::new("tools/xtask/target/mutants/repo-wide-no-default-features")
            ),
            vec![
                "mutants",
                "--output",
                "tools/xtask/target/mutants/repo-wide-no-default-features",
                "--in-place",
                "--baseline",
                "run",
                "--file",
                "src/store/**/*.rs",
                "--exclude",
                "src/store/ancestry/by_hash.rs",
                "--no-default-features",
                "--cargo-arg",
                "--locked",
                "--test-tool",
                "cargo",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mutants_smoke_lane_uses_round_robin_shard_and_skip_baseline_after_first_lane() {
        let lanes = critical_mutation_smoke_lanes();
        assert_eq!(lanes[0].shard.as_deref(), Some("1/8"));
        assert_eq!(lanes[0].sharding, Some(MutationSharding::RoundRobin));
        assert_eq!(
            MutationLane::repo_wide_smoke(MutantSurface::AllFeatures).sharding,
            Some(MutationSharding::RoundRobin)
        );

        let plan = build_mutant_execution_plan(&MutantsArgs {
            mode: MutantMode::Smoke,
            surface: None,
            shard: None,
        })
        .expect("smoke plan");

        let MutantExecutionPlan::Run(lanes) = plan else {
            panic!("expected runnable smoke plan");
        };

        assert_eq!(lanes[0].baseline, MutationBaseline::Run);
        assert!(lanes[1..]
            .iter()
            .all(|lane| lane.baseline == MutationBaseline::Skip));
    }

    #[test]
    fn ratchet_floor_only_advances_to_staged_thresholds() {
        assert_eq!(next_ratchet_floor(34, None), None);
        assert_eq!(next_ratchet_floor(35, None), Some(35));
        assert_eq!(next_ratchet_floor(74, Some(50)), Some(65));
        assert_eq!(next_ratchet_floor(86, Some(75)), Some(85));
    }

    #[test]
    fn current_phase_starts_repo_wide_in_record_only_mode() {
        assert_eq!(REPO_MUTATION_PHASE, RepoMutationPhase::Phase0);
    }

    #[test]
    fn critical_seam_lane_keeps_owned_paths() {
        let lanes = critical_mutation_lanes();
        let cursor_lane = lanes
            .iter()
            .find(|lane| lane.slug == "cursor-delivery")
            .expect("cursor lane");
        let projection_lane = lanes
            .iter()
            .find(|lane| lane.slug == "projection-flow")
            .expect("projection lane");

        assert_eq!(cursor_lane.scope, MutationScope::CriticalSeam);
        assert_eq!(cursor_lane.paths, CURSOR_MUTANT_FILES);
        assert_eq!(projection_lane.paths, PROJECTION_MUTANT_FILES);
        assert_eq!(
            MutationLane::repo_wide(MutantSurface::AllFeatures, None).paths,
            REPO_WIDE_ALL_FEATURES_MUTANT_FILES
        );
        assert_eq!(
            MutationLane::repo_wide(MutantSurface::NoDefaultFeatures, None).paths,
            REPO_WIDE_NO_DEFAULT_MUTANT_FILES
        );
        assert_eq!(
            MutationLane::repo_wide(MutantSurface::AllFeatures, None).excludes,
            surface_excludes(MutantSurface::AllFeatures)
        );
        assert_eq!(lanes[0].paths, WRITER_COMMIT_MUTANT_FILES);
    }

    fn fake_lane() -> MutationLane {
        critical_mutation_lanes()
            .into_iter()
            .find(|lane| lane.slug == "writer-commit")
            .expect("writer lane")
    }

    fn fake_output_dir() -> PathBuf {
        PathBuf::from("tools/xtask/target/mutants/fake-lane")
    }

    #[test]
    fn mutation_lane_allows_nonzero_exit_when_unviable_is_execution_evidence() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 0,
            timed_out: 0,
            unviable: 2,
            executed: 2,
            scored: 0,
            score_pct: None,
        };

        assert!(lane.allows_nonzero_exit(score));
    }

    #[test]
    fn mutation_policy_accepts_unviable_only_lane_as_execution_evidence() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 0,
            timed_out: 0,
            unviable: 3,
            executed: 3,
            scored: 0,
            score_pct: None,
        };

        assert!(assert_mutation_policy(&lane, &fake_output_dir(), score).is_ok());
    }

    #[test]
    fn mutation_policy_rejects_truly_empty_execution() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 0,
            timed_out: 0,
            unviable: 0,
            executed: 0,
            scored: 0,
            score_pct: None,
        };

        let err = assert_mutation_policy(&lane, &fake_output_dir(), score).expect_err("must fail");
        assert!(
            err.to_string().contains("no executed mutants"),
            "empty mutation lanes must fail as no-evidence lanes, got: {err:#}"
        );
    }
}
