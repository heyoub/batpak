//! Mutation-debt ledger consumer (GAUNT-MUT-4, slug `mutation-debt-schema`).
//!
//! `traceability/mutation_debt.yaml` is the typed record of surviving (missed)
//! mutants the repo-wide cloud lane currently fails to catch. Before this module
//! the file claimed "a structural check consumes this file" but NONE existed — a
//! false claim that left the debt ledger un-enforced. This is that consumer.
//!
//! SPLIT OF AUTHORITY (honest scoping):
//! - LOCAL (here, every `structural-check`): SCHEMA validation — every entry is
//!   well-formed (all fields present and non-empty, `first_seen` is an ISO date,
//!   `file` points at a real tracked source file, `line` is positive). A malformed
//!   or rotted entry (file deleted/moved) reds the build. Works on the empty list
//!   too (vacuously passes), so it bites the moment any entry is added.
//! - CLOUD (the repo-wide mutation lane): the NEW-missed-mutant comparison — a
//!   surviving mutant NOT recorded here reds the lane. That needs the cloud
//!   `missed.txt`, so it cannot be a pure structural check; it lives with the
//!   mutation runner (decision-1 baseline).

use crate::repo_surface::ensure;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Repo-relative path to the typed mutation-debt ledger.
pub(crate) const MUTATION_DEBT_REL: &str = "traceability/mutation_debt.yaml";

/// One tolerated surviving mutant. Mirrors the schema documented in the yaml.
#[derive(Debug, Deserialize)]
struct DebtEntry {
    /// The cargo-mutants mutant description (the exact `missed.txt` line).
    mutant: String,
    /// Repo-relative path to the source file the mutant lives in.
    file: String,
    /// 1-based line number of the mutated expression.
    line: u32,
    /// The mutation seam/lane slug that covers this file.
    seam: String,
    /// ISO-8601 (YYYY-MM-DD) date the mutant first survived.
    first_seen: String,
    /// Why it is currently tolerated + the plan to kill it.
    reason: String,
}

/// Production entry: parse + schema-validate the live ledger against the tree.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let path = repo_root.join(MUTATION_DEBT_REL);
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read {MUTATION_DEBT_REL}"))?;
    let entries: Vec<DebtEntry> = yaml_serde::from_str(&content)
        .with_context(|| format!("parse {MUTATION_DEBT_REL} as a list of debt entries"))?;
    validate_entries(repo_root, &entries)
}

/// Testable core: assert every entry is well-formed and not rotted. A RED fixture
/// drives synthetic entries (malformed date / missing file) against the real tree.
fn validate_entries(repo_root: &Path, entries: &[DebtEntry]) -> Result<()> {
    for (i, entry) in entries.iter().enumerate() {
        let tag = format!("mutation_debt.yaml[{i}]");
        ensure(
            !entry.mutant.trim().is_empty(),
            format!("structural-check (mutation-debt-schema): {tag} has an empty `mutant`"),
        )?;
        ensure(
            !entry.seam.trim().is_empty(),
            format!("structural-check (mutation-debt-schema): {tag} has an empty `seam`"),
        )?;
        ensure(
            !entry.reason.trim().is_empty(),
            format!("structural-check (mutation-debt-schema): {tag} has an empty `reason`"),
        )?;
        ensure(
            entry.line >= 1,
            format!(
                "structural-check (mutation-debt-schema): {tag} (`{}`) has line {} — must be >= 1",
                entry.mutant, entry.line
            ),
        )?;
        ensure(
            is_iso_date(&entry.first_seen),
            format!(
                "structural-check (mutation-debt-schema): {tag} (`{}`) has first_seen `{}` — must be \
                 an ISO-8601 date (YYYY-MM-DD) so debt age is auditable",
                entry.mutant, entry.first_seen
            ),
        )?;
        // Anti-rot: a recorded mutant whose file no longer exists is stale debt
        // masking a moved/deleted seam — fail so the ledger stays honest.
        ensure(
            repo_root.join(&entry.file).is_file(),
            format!(
                "structural-check (mutation-debt-schema): {tag} (`{}`) names file `{}` which does \
                 not exist — remove the stale debt entry or fix the path",
                entry.mutant, entry.file
            ),
        )?;
    }
    Ok(())
}

/// Strict `YYYY-MM-DD` check (no external date dependency): three `-`-separated
/// all-digit fields of widths 4/2/2 with month 1..=12 and day 1..=31.
fn is_iso_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return false;
    };
    let widths_ok = y.len() == 4 && m.len() == 2 && d.len() == 2;
    let digits_ok = [y, m, d]
        .iter()
        .all(|f| f.bytes().all(|b| b.is_ascii_digit()));
    if !(widths_ok && digits_ok) {
        return false;
    }
    let (month, day) = match (m.parse::<u8>(), d.parse::<u8>()) {
        (Ok(month), Ok(day)) => (month, day),
        _ => return false,
    };
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

#[cfg(test)]
#[path = "mutation_debt_tests.rs"]
mod tests;
