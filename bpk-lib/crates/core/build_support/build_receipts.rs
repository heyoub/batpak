// ---------------------------------------------------------------------------
// Gauntlet execution receipts + per-file rerun emission (P1-3b)
// ---------------------------------------------------------------------------
//
// BUILD-SCRIPT-ONLY. This file is `include!`d from `crates/core/build.rs`
// (inside its `shared_checks` module) and MUST NOT be pulled into the
// `batpak-integrity` binary: the integrity tool never emits receipts or rerun
// lines, so under the repo's `-D warnings` gate these items would be dead code
// there. The genuinely-shared lint helpers (extract_anchors,
// load_known_invariants, public_item_names, line_carries_justification, ...)
// live in the sibling `shared_checks.rs`, which BOTH build.rs and the integrity
// binary include.
//
// These helpers back two build-time anti-vacuity guarantees:
//
//   1. Per-file `cargo:rerun-if-changed`: Cargo treats a *directory* rerun line
//      as "rerun if the directory's immediate entries change", so editing a
//      nested file does NOT reliably retrigger build.rs. Emitting one line per
//      `.rs` file actually read is the documented-correct form and makes the
//      build-time lints incapable of stale-passing.
//
//   2. Execution receipts: when build.rs runs in a PACKAGED crate the repo
//      invariant surface (`../../traceability/`, `tools/`) is absent and three
//      lints would otherwise be SILENTLY skipped (a vacuous pass). Instead each
//      skipped lint emits a `cargo:warning=` and writes an auditable receipt
//      with verdict `SKIPPED_PACKAGED`. On a real workspace run the same lints
//      write a `PASS` receipt carrying the REAL files_examined / assertions_run
//      counts, so a downstream CI step can distinguish "ran and passed" from
//      "did not run". The JSON shape is shared across the whole gauntlet.

/// Non-vacuity counters returned by each surface-dependent lint so its receipt
/// can record real (never-zero on a real run) work. `inputs` is the exact set
/// of files the lint opened, used to compute the receipt `inputs_hash`.
#[derive(Default)]
pub(crate) struct LintCounts {
    pub files_examined: usize,
    pub assertions_run: usize,
    pub inputs: BTreeSet<PathBuf>,
}

/// Run one surface-dependent lint, recording a non-vacuous execution receipt.
/// When the repo invariant surface is absent (packaged crate) the lint is
/// skipped, but the skip is made auditable: a `cargo:warning=` is emitted and a
/// `SKIPPED_PACKAGED` receipt is written so a downstream CI step can prove the
/// gate did NOT silently vacuous-pass. When present, the lint runs and a `PASS`
/// receipt records its real files_examined / assertions_run counts.
pub(crate) fn run_surface_lint(available: bool, slug: &str, lint: fn() -> LintCounts) {
    let started = iso8601_now();
    if !available {
        println!("cargo:warning={slug} skipped: repo invariant surface absent (packaged crate)");
        write_gauntlet_receipt(&GauntletReceipt {
            gate: slug.to_string(),
            inputs_hash: String::new(),
            files_examined: 0,
            assertions_run: 0,
            started,
            ended: iso8601_now(),
            verdict: "SKIPPED_PACKAGED".to_string(),
        });
        return;
    }
    let counts = lint();
    write_gauntlet_receipt(&GauntletReceipt {
        gate: slug.to_string(),
        inputs_hash: crc32_inputs_hash(&counts.inputs),
        files_examined: counts.files_examined,
        assertions_run: counts.assertions_run,
        started,
        ended: iso8601_now(),
        verdict: "PASS".to_string(),
    });
}

/// One gauntlet execution receipt. Field order/shape is the gauntlet-wide
/// contract; do not diverge it. `inputs_hash` is a crc32 hex digest (or empty
/// when not computed — no sha2/blake3 is available as a build-dependency).
#[derive(Debug, serde::Serialize)]
pub(crate) struct GauntletReceipt {
    pub gate: String,
    pub inputs_hash: String,
    pub files_examined: usize,
    pub assertions_run: usize,
    pub started: String,
    pub ended: String,
    pub verdict: String,
}

/// Resolve the directory CI reads receipts from, deterministically, for ANY
/// build context (workspace or packaged-consumer).
///
/// Choice of location: we derive it from `OUT_DIR`, which Cargo always sets for
/// build scripts and which always lives at
/// `<target>/<profile>/build/<pkg-hash>/out`. Walking three parents up lands on
/// `<target>/<profile>`, so receipts go to `<target>/<profile>/gauntlet-receipts/`.
/// This is robust because it follows whatever `CARGO_TARGET_DIR` / profile the
/// invoking build actually used — including the consumer-smoke build whose
/// target dir is `<workspace>/target/consumer-smoke-build`, giving
/// `target/consumer-smoke-build/debug/gauntlet-receipts/`. A CI step that knows
/// the build's target+profile can read the files without guessing. Falls back to
/// `CARGO_TARGET_DIR`/`target` when `OUT_DIR` is somehow unset.
pub(crate) fn gauntlet_receipts_dir() -> PathBuf {
    if let Some(out_dir) = std::env::var_os("OUT_DIR") {
        let out = PathBuf::from(out_dir);
        // out -> <pkg-hash> -> build -> <profile>
        if let Some(profile_dir) = out.ancestors().nth(3) {
            return profile_dir.join("gauntlet-receipts");
        }
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    target.join("gauntlet-receipts")
}

/// Best-effort ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) without pulling
/// in chrono. Computed from `SystemTime::now()`; receipt timestamps are audit
/// metadata, not load-bearing for any assertion.
pub(crate) fn iso8601_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's days-from-civil inverse (`civil_from_days`); converts a
/// Unix day count into (year, month, day). Public-domain algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    // d is 1..=31 and m is 1..=12 by construction; the conversions cannot fail.
    let d = u32::try_from(d).unwrap_or(0);
    let m = u32::try_from(m).unwrap_or(0);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Write a gauntlet receipt to `gauntlet_receipts_dir()/<slug>.json`. Best
/// effort: a receipt-write failure must not fail the build (the warning + the
/// downstream presence check are the audit teeth), so errors are surfaced as a
/// `cargo:warning=` rather than a panic.
pub(crate) fn write_gauntlet_receipt(receipt: &GauntletReceipt) {
    let dir = gauntlet_receipts_dir();
    if let Err(err) = fs::create_dir_all(&dir) {
        println!(
            "cargo:warning=could not create gauntlet receipts dir {}: {err}",
            dir.display()
        );
        return;
    }
    let path = dir.join(format!("{}.json", receipt.gate));
    match serde_json::to_vec_pretty(receipt) {
        Ok(bytes) => {
            if let Err(err) = fs::write(&path, bytes) {
                println!(
                    "cargo:warning=could not write gauntlet receipt {}: {err}",
                    path.display()
                );
            }
        }
        Err(err) => {
            println!("cargo:warning=could not serialize gauntlet receipt {}: {err}", receipt.gate);
        }
    }
}

/// Emit ONE `cargo:rerun-if-changed` per `.rs` file the build-time lints read
/// (per-file is the only form Cargo honors for nested edits), plus the explicit
/// non-`.rs` inputs (Cargo.toml, traceability YAML, doc/ADR markdown). The
/// surface-dependent roots are only emitted when present.
pub(crate) fn emit_build_rerun_lines(
    core_root: &Path,
    repo_root: &Path,
    repo_invariants_available: bool,
) {
    println!("cargo:rerun-if-env-changed=BATPAK_PLATFORM_PROFILE");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build_support/shared_checks.rs");
    println!("cargo:rerun-if-changed=build_support/build_receipts.rs");
    // Always-read source surface.
    emit_rerun_for_rs_files(&core_root.join("src"));
    emit_rerun_for_rs_files(&core_root.join("examples"));
    if !repo_invariants_available {
        return;
    }
    emit_rerun_for_path(&repo_root.join("Cargo.toml"));
    // Every `.rs` file the surface-dependent lints open.
    for rel in [
        "crates/core/tests",
        "crates/core/benches",
        "crates/macros/src",
        "crates/macros-support/src",
        "tools/xtask/src",
        "tools/integrity/src",
    ] {
        emit_rerun_for_rs_files(&repo_root.join(rel));
    }
    // build.rs itself is read by the allow/dead-code lints.
    emit_rerun_for_path(&core_root.join("build.rs"));
    for rel in [
        "traceability/dead_code_silencer_allowlist.yaml",
        "traceability/typed_waivers.yaml",
        "traceability/invariants.yaml",
    ] {
        emit_rerun_for_path(&repo_root.join(rel));
    }
    let project_root = repo_root.join("..");
    for rel in [
        "README.md",
        "MODEL.md",
        "INVARIANTS.md",
        "CONFORMANCE.md",
        "archive/decisions/100_ADR_0001_SYNC_ONLY_STORE.md",
    ] {
        emit_rerun_for_path(&project_root.join(rel));
    }
}

/// Emit one `cargo:rerun-if-changed` line per `.rs` file under `dir` (recursive),
/// returning the count emitted. Per-file (not directory) lines are the only form
/// Cargo honors for nested edits.
pub(crate) fn emit_rerun_for_rs_files(dir: &Path) -> usize {
    let mut count = 0usize;
    emit_rerun_for_rs_files_inner(dir, &mut count);
    count
}

fn emit_rerun_for_rs_files_inner(dir: &Path, count: &mut usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            emit_rerun_for_rs_files_inner(&path, count);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            println!("cargo:rerun-if-changed={}", path.display());
            *count += 1;
        }
    }
}

/// Emit a single `cargo:rerun-if-changed` line for `path` if it exists.
pub(crate) fn emit_rerun_for_path(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// crc32 hex digest over the contents of `paths` (sorted for determinism). Used
/// as the receipt `inputs_hash`; crc32fast is the only hashing build-dependency.
pub(crate) fn crc32_inputs_hash(paths: &BTreeSet<PathBuf>) -> String {
    let mut hasher = crc32fast::Hasher::new();
    for path in paths {
        if let Ok(bytes) = fs::read(path) {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(&bytes);
        }
    }
    format!("{:08x}", hasher.finalize())
}
