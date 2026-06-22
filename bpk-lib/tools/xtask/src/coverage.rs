use crate::util::{cargo_target_dir, command_succeeds, run, run_output};
use crate::CoverArgs;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Single source of truth for the repo-wide coverage floor enforced on the
/// default PR path (`ci-fast`) and in the heavier `preflight` bundle. Both
/// callers MUST reference this constant rather than duplicating the literal so
/// the floor cannot silently diverge between lanes.
pub(crate) const COVERAGE_FLOOR_PCT: u32 = 80;

pub(crate) fn cover(args: CoverArgs) -> Result<()> {
    ensure_llvm_cov_available()?;

    let export_dir = coverage_export_dir()?;
    let staging_dir = coverage_staging_dir()?;
    let staged_coverage_json = staging_dir.join("coverage.json");
    let coverage_json = export_dir.join("coverage.json");
    let text_report = export_dir.join("text-report.txt");

    if !args.json {
        outln!("Running tests with coverage instrumentation...");
        outln!();
    }

    run_llvm_cov_nextest(&staging_dir, args.json)?;
    export_llvm_cov_json(&staged_coverage_json, &staging_dir, args.json)?;
    ensure_artifact_exists(&staged_coverage_json, &staging_dir, "coverage.json")?;
    fs::create_dir_all(&export_dir)
        .with_context(|| format!("recreate {}", export_dir.display()))?;
    fs::copy(&staged_coverage_json, &coverage_json).with_context(|| {
        format!(
            "copy {} -> {}",
            staged_coverage_json.display(),
            coverage_json.display()
        )
    })?;
    ensure_artifact_exists(&coverage_json, &export_dir, "coverage.json")?;

    if !args.json {
        outln!("Coverage export written to {}", coverage_json.display());
    }

    let json_text = fs::read_to_string(&coverage_json)
        .with_context(|| format!("read {}", coverage_json.display()))?;
    if args.json {
        out!("{json_text}");
        return Ok(());
    }

    let report_output = llvm_cov_text_report()?;
    fs::create_dir_all(&export_dir)
        .with_context(|| format!("recreate {}", export_dir.display()))?;
    fs::write(&text_report, &report_output)
        .with_context(|| format!("write {}", text_report.display()))?;
    ensure_artifact_exists(&text_report, &export_dir, "text-report.txt")?;

    let parsed_json: Value = serde_json::from_str(&json_text).context("parse coverage json")?;
    let summary = coverage_summary(&parsed_json)?;

    outln!();
    outln!("================================================================");
    outln!("  COVERAGE FEEDBACK");
    outln!("================================================================");
    outln!();
    outln!(
        "  Lines:     {} / {} ({}%)",
        summary.lines_covered,
        summary.lines_total,
        summary.line_pct
    );
    outln!(
        "  Functions: {} / {} ({}%)",
        summary.funcs_covered,
        summary.funcs_total,
        summary.func_pct
    );
    outln!();
    outln!("----------------------------------------------------------------");
    outln!("  UNCOVERED (the ping-back)");
    outln!("----------------------------------------------------------------");
    outln!();

    for file in uncovered_files(&report_output) {
        outln!(
            "  {:<50}  regions_miss={:<4}  lines_miss={:<4}",
            file.file,
            file.region_miss,
            file.line_miss
        );
    }

    outln!();
    outln!("----------------------------------------------------------------");
    outln!("  UNCOVERED FUNCTIONS");
    outln!("----------------------------------------------------------------");
    outln!();

    let uncovered = uncovered_functions(&parsed_json);
    if uncovered.is_empty() {
        outln!("  All functions covered!");
    } else {
        let mut current_file = String::new();
        for item in &uncovered {
            if item.location != current_file {
                current_file = item.location.clone();
                outln!("  {}:", current_file);
            }
            outln!("    -> {}::{}()", item.module_path, item.function_name);
        }
        outln!();
        outln!("  Total uncovered functions: {}", uncovered.len());
    }

    outln!();
    outln!("================================================================");

    if args.ci {
        let threshold = args.threshold.unwrap_or(70);
        if summary.line_pct < threshold || summary.func_pct < threshold {
            bail!(
                "FAIL: coverage below threshold {}% (lines={}%, functions={}%)\n\
                 Fix the uncovered functions listed above.",
                threshold,
                summary.line_pct,
                summary.func_pct
            );
        }
        outln!();
        outln!(
            "PASS: coverage meets threshold {}% (lines={}%, functions={}%)",
            threshold,
            summary.line_pct,
            summary.func_pct
        );
    }

    Ok(())
}

fn ensure_llvm_cov_available() -> Result<()> {
    if command_succeeds("cargo", ["llvm-cov", "--version"]) {
        return Ok(());
    }
    bail!(
        "cargo-llvm-cov is required for `cargo xtask cover`.\n\
         Install it with `cargo xtask setup --install-tools`, `cargo install cargo-llvm-cov --locked`, or `cargo binstall cargo-llvm-cov`."
    )
}

fn run_llvm_cov_nextest(export_dir: &Path, json_mode: bool) -> Result<()> {
    let profraw_dir = coverage_profraw_dir(export_dir)?;
    let mut command = Command::new("cargo");
    command.args([
        "llvm-cov",
        "nextest",
        "--profile",
        "ci",
        "-p",
        "batpak",
        "-p",
        "syncbat",
        "-p",
        "netbat",
        "--all-features",
        "--no-report",
    ]);
    command.env(
        "LLVM_PROFILE_FILE",
        profraw_dir.join("batpak-%p-%m.profraw"),
    );
    if json_mode {
        command.stdout(Stdio::null());
        command.stderr(Stdio::inherit());
    }
    run(command).with_context(|| {
        format!(
            "coverage test execution failed before report export; inspect retained artifacts in {}",
            export_dir.display()
        )
    })
}

fn export_llvm_cov_json(output_path: &Path, export_dir: &Path, json_mode: bool) -> Result<()> {
    let mut command = Command::new("cargo");
    command.args(["llvm-cov", "report", "--json", "--output-path"]);
    command.arg(output_path);
    if json_mode {
        command.stdout(Stdio::null());
        command.stderr(Stdio::inherit());
    }
    run(command).with_context(|| {
        format!(
            "coverage export failed after test execution; inspect retained artifacts in {}",
            export_dir.display()
        )
    })
}

fn llvm_cov_text_report() -> Result<String> {
    let mut command = Command::new("cargo");
    command.args(["llvm-cov", "report", "--text"]);
    let output = run_output(command)?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn coverage_export_dir() -> Result<PathBuf> {
    let path = cargo_target_dir()?.join("xtask-cover/last-run");
    if path.exists() {
        fs::remove_dir_all(&path).with_context(|| format!("clear {}", path.display()))?;
    }
    fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
    Ok(path)
}

fn coverage_staging_dir() -> Result<PathBuf> {
    let repo_slug = cargo_target_dir()?
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let path = std::env::temp_dir().join(format!(
        "batpak-xtask-cover-staging-{}-{}",
        repo_slug,
        std::process::id()
    ));
    if path.exists() {
        fs::remove_dir_all(&path).with_context(|| format!("clear {}", path.display()))?;
    }
    fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
    Ok(path)
}

fn coverage_profraw_dir(staging_dir: &Path) -> Result<PathBuf> {
    let path = staging_dir.join("profraw");
    if path.exists() {
        fs::remove_dir_all(&path).with_context(|| format!("clear {}", path.display()))?;
    }
    fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
    Ok(path)
}

fn ensure_artifact_exists(path: &Path, export_dir: &Path, name: &str) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "coverage run completed without producing {name}; inspect retained artifacts in {}",
            export_dir.display()
        )
    })?;
    if metadata.len() == 0 {
        bail!(
            "coverage run completed but produced an empty {name}; inspect retained artifacts in {}",
            export_dir.display()
        );
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct CoverageSummary {
    lines_total: u32,
    lines_covered: u32,
    funcs_total: u32,
    funcs_covered: u32,
    line_pct: u32,
    func_pct: u32,
}

fn coverage_summary(json: &Value) -> Result<CoverageSummary> {
    let totals = json
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("totals"))
        .context("coverage json missing data[0].totals")?;

    let lines_total = coverage_total_u32(totals, "lines", "count")?;
    let lines_covered = coverage_total_u32(totals, "lines", "covered")?;
    let funcs_total = coverage_total_u32(totals, "functions", "count")?;
    let funcs_covered = coverage_total_u32(totals, "functions", "covered")?;

    if lines_total == 0 {
        bail!("coverage json reported zero total lines");
    }
    if funcs_total == 0 {
        bail!("coverage json reported zero total functions");
    }

    Ok(CoverageSummary {
        lines_total,
        lines_covered,
        funcs_total,
        funcs_covered,
        line_pct: (lines_covered * 100) / lines_total,
        func_pct: (funcs_covered * 100) / funcs_total,
    })
}

fn coverage_total_u32(totals: &Value, section: &str, field: &str) -> Result<u32> {
    let value = totals
        .get(section)
        .and_then(|v| v.get(field))
        .and_then(Value::as_u64)
        .with_context(|| format!("coverage json missing {section}.{field}"))?;

    u32::try_from(value)
        .with_context(|| format!("coverage json value {section}.{field} exceeds u32"))
}

#[derive(Debug, PartialEq, Eq)]
struct UncoveredFile {
    file: String,
    region_miss: u32,
    line_miss: u32,
}

fn uncovered_files(text_report: &str) -> Vec<UncoveredFile> {
    let mut files = Vec::new();
    for line in text_report.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('-')
            || trimmed.starts_with("Filename")
            || trimmed.starts_with("TOTAL")
        {
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 7 {
            continue;
        }
        let region_miss = fields[2].parse::<u32>().ok();
        let line_miss = fields[5].parse::<u32>().ok();
        if let (Some(region_miss), Some(line_miss)) = (region_miss, line_miss) {
            if region_miss > 0 || line_miss > 0 {
                files.push(UncoveredFile {
                    file: fields[0].to_string(),
                    region_miss,
                    line_miss,
                });
            }
        }
    }
    files
}

#[derive(Debug, PartialEq, Eq)]
struct UncoveredFunction {
    location: String,
    module_path: String,
    function_name: String,
}

fn uncovered_functions(json: &Value) -> Vec<UncoveredFunction> {
    let mut uncovered = Vec::new();
    let Some(functions) = json
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("functions"))
        .and_then(Value::as_array)
    else {
        return uncovered;
    };

    for function in functions {
        let count = function.get("count").and_then(Value::as_u64).unwrap_or(0);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if count != 0 || name.to_ascii_lowercase().contains("test") {
            continue;
        }
        let location = function
            .get("filenames")
            .and_then(Value::as_array)
            .and_then(|files| files.first())
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let (module_path, function_name) = split_function_name(&name);
        uncovered.push(UncoveredFunction {
            location,
            module_path,
            function_name,
        });
    }

    uncovered.sort_by(|a, b| {
        a.location
            .cmp(&b.location)
            .then(a.module_path.cmp(&b.module_path))
            .then(a.function_name.cmp(&b.function_name))
    });
    uncovered
}

fn split_function_name(name: &str) -> (String, String) {
    match name.rsplit_once("::") {
        Some((module_path, function_name)) => (module_path.to_string(), function_name.to_string()),
        None => (String::new(), name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        coverage_export_dir, coverage_staging_dir, coverage_summary, split_function_name,
        uncovered_files, uncovered_functions,
    };
    use serde_json::json;

    #[test]
    fn coverage_summary_uses_totals_block() {
        let json = json!({
            "data": [{
                "totals": {
                    "lines": {"count": 200, "covered": 150},
                    "functions": {"count": 20, "covered": 15}
                }
            }]
        });
        // justifies: INV-TEST-PANIC-AS-ASSERTION; test-only in tools/xtask/src/coverage.rs; panic on setup failure is the test's signal of broken fixtures
        let summary = coverage_summary(&json).expect("summary");
        assert_eq!(summary.line_pct, 75);
        assert_eq!(summary.func_pct, 75);
    }

    #[test]
    fn uncovered_file_parser_filters_zero_miss_rows() {
        let report = "\
Filename Regions Miss Cover Lines Miss Cover\n\
src/lib.rs 10 0 100.00% 20 0 100.00%\n\
src/store/mod.rs 12 3 75.00% 30 4 86.67%\n\
TOTAL 22 3 86.36% 50 4 92.00%\n";
        let files = uncovered_files(report);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file, "src/store/mod.rs");
        assert_eq!(files[0].region_miss, 3);
        assert_eq!(files[0].line_miss, 4);
    }

    #[test]
    fn uncovered_function_parser_skips_tests_and_sorts() {
        let json = json!({
            "data": [{
                "functions": [
                    {"name": "batpak::mod_a::cold_path", "count": 0, "filenames": ["src/a.rs"]},
                    {"name": "batpak::mod_b::test_helper", "count": 0, "filenames": ["src/b.rs"]},
                    {"name": "batpak::mod_c::warm_path", "count": 1, "filenames": ["src/c.rs"]},
                    {"name": "standalone", "count": 0, "filenames": []}
                ]
            }]
        });
        let functions = uncovered_functions(&json);
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].location, "?");
        assert_eq!(functions[0].function_name, "standalone");
        assert_eq!(functions[1].location, "src/a.rs");
        assert_eq!(functions[1].module_path, "batpak::mod_a");
        assert_eq!(functions[1].function_name, "cold_path");
    }

    #[test]
    fn split_function_name_keeps_module_and_leaf() {
        assert_eq!(
            split_function_name("batpak::coverage::render"),
            ("batpak::coverage".to_string(), "render".to_string())
        );
        assert_eq!(
            split_function_name("single"),
            (String::new(), "single".to_string())
        );
    }

    #[test]
    fn coverage_export_dir_is_stable_under_target() {
        // justifies: INV-TEST-PANIC-AS-ASSERTION; test-only in tools/xtask/src/coverage.rs; panic on setup failure is the test's signal of broken fixtures
        let path = coverage_export_dir().expect("export dir");
        assert!(path.ends_with("target/xtask-cover/last-run"));
    }

    #[test]
    fn coverage_staging_dir_stays_outside_target_tree() {
        // justifies: INV-TEST-PANIC-AS-ASSERTION; test-only in tools/xtask/src/coverage.rs; panic on setup failure is the test's signal of broken fixtures
        let path = coverage_staging_dir().expect("staging dir");
        assert!(
            !path.starts_with(std::path::Path::new("target")),
            "staging dir should stay outside target so cargo-llvm-cov cleanup cannot remove it"
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("staging dir has utf8 leaf");
        assert!(name.starts_with("batpak-xtask-cover-staging-"));
    }
}
