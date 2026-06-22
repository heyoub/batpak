//! Gauntlet execution receipts — the binary-side writer (P1-3).
//!
//! DO-178B "the verification activity must leave evidence." Every integrity
//! gate that runs successfully writes `target/gauntlet-receipts/<slug>.json`
//! recording the REAL number of files it examined and assertions it ran, so a
//! downstream meta-check ([`crate::receipts::check_present`]) can prove no gate
//! "passed because it looked at nothing" (the vacuous-pass failure mode).
//!
//! # Why this is NOT `build_support/build_receipts.rs`
//!
//! The build-only writer lives in `crates/core/build_support/build_receipts.rs`
//! and is `include!`d ONLY from `build.rs`. It depends on `OUT_DIR` (a
//! build-script-only env var) and on `crc32fast` (a build-dependency, not a
//! dependency of the `batpak-integrity` binary). Pulling it into the binary
//! would either drag in an unused dependency or surface as dead code under the
//! repo's `-D warnings` gate. This module is the binary's own implementation of
//! the SAME on-disk contract: the JSON field set, order, and `verdict` vocabulary
//! are kept byte-compatible with `GauntletReceipt` so a single
//! `gauntlet-receipts-present` check reads both build-time and integrity-time
//! receipts uniformly.

use crate::repo_surface::repo_root;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// One gauntlet execution receipt. Field order / shape / `verdict` vocabulary is
/// the gauntlet-wide contract shared with the build-time writer
/// (`build_support/build_receipts.rs`); do NOT diverge it. `inputs_hash` is a
/// lowercase hex digest (or empty when no inputs were hashed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GateReceipt {
    pub gate: String,
    pub inputs_hash: String,
    pub files_examined: usize,
    pub assertions_run: usize,
    pub started: String,
    pub ended: String,
    pub verdict: String,
}

/// The directory CI reads receipts from for a plain (non-build-script) binary
/// run: `<target>/gauntlet-receipts`. Resolved from `CARGO_TARGET_DIR` when set,
/// else `<repo_root>/target` (the workspace's default target dir). This is the
/// same `<target>/gauntlet-receipts` leaf the build-time writer lands on for the
/// default profile, so both feed one presence check.
pub(crate) fn receipts_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir).join("gauntlet-receipts"));
    }
    Ok(repo_root()?.join("target").join("gauntlet-receipts"))
}

/// Best-effort ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`), dependency-free.
/// Receipt timestamps are audit metadata, not load-bearing for any assertion.
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

/// Howard Hinnant's `civil_from_days` (public-domain): Unix day count → (y,m,d).
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
    let d = u32::try_from(d).unwrap_or(0);
    let m = u32::try_from(m).unwrap_or(0);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// FNV-1a (64-bit) hex digest over the contents of `inputs` (sorted for
/// determinism). FNV is dependency-free and adequate as a receipt fingerprint —
/// the receipt is audit evidence, not a security boundary. Returns empty when
/// `inputs` is empty (matching the build writer's "empty when not computed").
fn fnv1a_inputs_hash(inputs: &BTreeSet<PathBuf>) -> String {
    if inputs.is_empty() {
        return String::new();
    }
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut absorb = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    for path in inputs {
        absorb(path.to_string_lossy().as_bytes());
        if let Ok(bytes) = fs::read(path) {
            absorb(&bytes);
        }
    }
    format!("{hash:016x}")
}

/// Write `receipt` to `receipts_dir()/<slug>.json`, serialized identically to the
/// build-time writer (`serde_json::to_vec_pretty`).
fn write(receipt: &GateReceipt) -> Result<()> {
    let dir = receipts_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("create receipts dir {}", dir.display()))?;
    let path = dir.join(format!("{}.json", receipt.gate));
    let bytes = serde_json::to_vec_pretty(receipt).context("serialize gauntlet receipt")?;
    fs::write(&path, bytes).with_context(|| format!("write receipt {}", path.display()))?;
    Ok(())
}

/// Record a non-vacuous PASS receipt for a gate that has already run
/// successfully. `inputs` is the exact set of files the gate read (used for the
/// `inputs_hash` and to count `files_examined`); `assertions_run` is the number
/// of distinct checks the gate executed. Both counts must be non-zero on a real
/// run — that is the property `check_present` enforces.
///
/// The receipt write is best-effort surfaced as an error so a genuine
/// filesystem failure is visible, but the heavy gate work has already happened
/// and passed by the time this is called.
pub(crate) fn record_pass(
    slug: &str,
    inputs: &BTreeSet<PathBuf>,
    files_examined: usize,
    assertions_run: usize,
    started: String,
) -> Result<()> {
    write(&GateReceipt {
        gate: slug.to_string(),
        inputs_hash: fnv1a_inputs_hash(inputs),
        files_examined,
        assertions_run,
        started,
        ended: iso8601_now(),
        verdict: "PASS".to_string(),
    })
}

/// Run a gate, timing it and emitting a non-vacuous PASS receipt on success.
/// The gate closure returns the work it performed ([`GateWork`]); the receipt is
/// only written when the gate returns `Ok`, so a failing gate leaves no PASS
/// receipt behind (a stale PASS receipt from a prior run would still be caught
/// because the gate's `Err` propagates and fails the whole run first).
pub(crate) fn run_gate<F>(slug: &str, gate: F) -> Result<()>
where
    F: FnOnce() -> Result<GateWork>,
{
    let started = iso8601_now();
    let work = gate()?;
    record_pass(
        slug,
        &work.inputs,
        work.files_examined,
        work.assertions_run,
        started,
    )
}

/// The measurable work a gate performed, returned so its receipt records real
/// (never-zero on a real run) counts. `files_examined` and `assertions_run` are
/// reported explicitly rather than derived from `inputs.len()` so a gate that
/// reads one file but runs many assertions (or vice versa) is recorded honestly.
pub(crate) struct GateWork {
    pub files_examined: usize,
    pub assertions_run: usize,
    pub inputs: BTreeSet<PathBuf>,
}

impl GateWork {
    pub(crate) fn new(
        files_examined: usize,
        assertions_run: usize,
        inputs: BTreeSet<PathBuf>,
    ) -> Self {
        GateWork {
            files_examined,
            assertions_run,
            inputs,
        }
    }
}

/// Read and parse a single receipt file. Returns `Err` with context on malformed
/// JSON so a corrupt receipt is a finding, not a silent skip.
pub(crate) fn read_receipt(path: &Path) -> Result<GateReceipt> {
    let bytes = fs::read(path).with_context(|| format!("read receipt {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse receipt {}", path.display()))
}

/// The `gauntlet-receipts-present` meta-check. Scans `receipts_dir()` and FAILS
/// when any gate in `required` is missing a receipt, or has a non-`SKIPPED_*`
/// verdict with `files_examined == 0` or `assertions_run == 0` (the vacuous-pass
/// guard). `SKIPPED_PACKAGED` receipts are allowed to carry zero counts because
/// they record a deliberate, audited skip rather than a vacuous pass.
///
/// Returns the receipts it validated so callers can report counts.
pub(crate) fn check_present(required: &[&str]) -> Result<Vec<GateReceipt>> {
    let dir = receipts_dir()?;
    let mut validated = Vec::new();
    for slug in required {
        let path = dir.join(format!("{slug}.json"));
        if !path.exists() {
            anyhow::bail!(
                "gauntlet-receipts-present: no receipt for registered gate `{slug}` at {}.\n\
                 The gate either did not run or did not emit a receipt. Run `cargo run -p \
                 batpak-integrity -- structural-check` (and the build) to (re)generate receipts.",
                path.display()
            );
        }
        let receipt = read_receipt(&path)?;
        validate_receipt(&receipt)?;
        validated.push(receipt);
    }
    Ok(validated)
}

/// Validate one receipt against the non-vacuity rule. Public to the crate so the
/// self-test can drive it directly on planted receipts.
pub(crate) fn validate_receipt(receipt: &GateReceipt) -> Result<()> {
    let skipped = receipt.verdict.starts_with("SKIPPED");
    if skipped {
        return Ok(());
    }
    if receipt.verdict != "PASS" {
        anyhow::bail!(
            "gauntlet-receipts-present: gate `{}` has verdict `{}` (expected PASS or SKIPPED_*).",
            receipt.gate,
            receipt.verdict
        );
    }
    if receipt.files_examined == 0 || receipt.assertions_run == 0 {
        anyhow::bail!(
            "gauntlet-receipts-present: gate `{}` produced a VACUOUS receipt \
             (files_examined={}, assertions_run={}). A passing gate must examine at least one \
             file and run at least one assertion; a zero count means it passed by looking at \
             nothing. Only a SKIPPED_* verdict may carry zero counts.",
            receipt.gate,
            receipt.files_examined,
            receipt.assertions_run
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(verdict: &str, files: usize, assertions: usize) -> GateReceipt {
        GateReceipt {
            gate: "demo".into(),
            inputs_hash: "deadbeef".into(),
            files_examined: files,
            assertions_run: assertions,
            started: "2026-06-20T00:00:00Z".into(),
            ended: "2026-06-20T00:00:01Z".into(),
            verdict: verdict.into(),
        }
    }

    // GREEN: a PASS receipt with real counts is accepted.
    #[test]
    fn nonvacuous_pass_receipt_is_accepted() {
        validate_receipt(&receipt("PASS", 12, 4)).expect("non-vacuous PASS must validate");
    }

    // RED: a PASS receipt with files_examined == 0 is rejected (vacuous).
    #[test]
    fn zero_files_pass_receipt_is_rejected() {
        let err = validate_receipt(&receipt("PASS", 0, 4))
            .expect_err("a PASS with files_examined==0 must fail");
        assert!(
            err.to_string().contains("VACUOUS"),
            "error must flag the vacuous pass, got: {err}"
        );
    }

    // RED: a PASS receipt with assertions_run == 0 is rejected (vacuous).
    #[test]
    fn zero_assertions_pass_receipt_is_rejected() {
        let err = validate_receipt(&receipt("PASS", 9, 0))
            .expect_err("a PASS with assertions_run==0 must fail");
        assert!(err.to_string().contains("VACUOUS"), "got: {err}");
    }

    // GREEN: a SKIPPED_PACKAGED receipt may carry zero counts (audited skip).
    #[test]
    fn skipped_packaged_receipt_allows_zero_counts() {
        validate_receipt(&receipt("SKIPPED_PACKAGED", 0, 0))
            .expect("a SKIPPED_PACKAGED receipt may carry zero counts");
    }

    // RED: an unexpected verdict is rejected.
    #[test]
    fn unknown_verdict_is_rejected() {
        let err = validate_receipt(&receipt("MAYBE", 3, 3)).expect_err("unknown verdict must fail");
        assert!(err.to_string().contains("verdict"), "got: {err}");
    }

    // The hash is empty for no inputs and stable + nonempty for real inputs.
    #[test]
    fn inputs_hash_is_empty_for_no_inputs_and_hex_for_inputs() {
        assert!(fnv1a_inputs_hash(&BTreeSet::new()).is_empty());
        let mut set = BTreeSet::new();
        set.insert(PathBuf::from(file!()));
        let hash = fnv1a_inputs_hash(&set);
        assert_eq!(hash.len(), 16, "fnv-1a 64-bit renders as 16 hex chars");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
