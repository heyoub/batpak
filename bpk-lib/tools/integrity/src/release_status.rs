//! Release terminal-status ledger (Truth Gate, Slice 0).
//!
//! `traceability/releases/*.yaml` is the machine-readable program board for a cut.
//! Each row names a release blocker or proven milestone with a terminal status:
//! `PROVEN`, `FAIL-CLOSED`, `WAIVED`, `FAULT-INJECTED`, or `INCOMPLETE`.
//!
//! Split of authority:
//! - **Normal `structural-check`**: validate every release file's schema, witness
//!   references, waiver expiry shape, and that `justifies: INV-*` test headers
//!   resolve to `invariants.yaml`. `INCOMPLETE` rows are allowed.
//! - **Strict release mode** (`release-status --strict`, folded into `just seal` and
//!   `cargo xtask release --dry-run`): additionally fail when any
//!   `terminal_required: true` row is not in a terminal status, when a `WAIVED`
//!   row is expired, or when `--active` finds other than exactly one `active: true`
//!   release target.

use crate::assurance::{load_seam_registry, SeamRegistryEntry};
use crate::docs_catalog::{file_declares_test_fn, load_catalog};
use crate::gate_registry;
use crate::receipts::GateWork;
use crate::repo_surface::{
    core_tests_root, ensure, load_yaml, relative, resolve_repo_or_core_path, rust_files,
};
use crate::source_cache::SourceCache;
use crate::typed_waivers::IsoDate;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Repo-relative directory holding release target ledgers.
pub(crate) const RELEASES_DIR: &str = "traceability/releases";

/// Maximum calendar days a release-blocking (`L4`) waiver may live.
pub(crate) const L4_WAIVER_MAX_DAYS: u32 = 30;
/// Maximum calendar days a non-L4 waiver may live.
pub(crate) const NON_L4_WAIVER_MAX_DAYS: u32 = 90;

/// How a release-status check is enforced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseCheckOptions {
    /// When true, `terminal_required` rows must reach a terminal status and waivers
    /// must not be expired.
    pub strict: bool,
    /// When set, only this `release:` version is checked (strict completeness applies
    /// only within the selected target(s)).
    pub target: Option<String>,
    /// When true with strict mode, exactly one release file must carry `active: true`.
    pub active_only: bool,
}

impl ReleaseCheckOptions {
    pub(crate) fn structural() -> Self {
        Self {
            strict: false,
            target: None,
            active_only: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
enum ReleaseStatus {
    #[serde(rename = "PROVEN")]
    Proven,
    #[serde(rename = "FAIL-CLOSED")]
    FailClosed,
    #[serde(rename = "WAIVED")]
    Waived,
    #[serde(rename = "FAULT-INJECTED")]
    FaultInjected,
    #[serde(rename = "INCOMPLETE")]
    Incomplete,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseTarget {
    release: String,
    #[serde(default)]
    active: bool,
    rows: Vec<ReleaseRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRow {
    id: String,
    title: String,
    status: ReleaseStatus,
    #[serde(default)]
    terminal_required: bool,
    surface: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    expiry: Option<String>,
    #[serde(default)]
    blast_radius: Option<String>,
    #[serde(default)]
    why_no_witness: Option<String>,
    #[serde(default)]
    witness_invariants: Vec<String>,
    #[serde(default)]
    witness_tests: Vec<String>,
    #[serde(default)]
    witness_gates: Vec<String>,
    #[serde(default)]
    witness_seams: Vec<String>,
    #[serde(default)]
    proof_commands: Vec<String>,
    #[serde(default)]
    blocked_by: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug)]
struct LoadedRelease {
    path: PathBuf,
    target: ReleaseTarget,
}

pub(crate) fn releases_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(RELEASES_DIR)
}

pub(crate) fn discover_release_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let dir = releases_dir(repo_root);
    ensure(
        dir.is_dir(),
        format!(
            "{RELEASES_DIR}/ must exist — add at least one release target ledger \
             (e.g. traceability/releases/0.9.0.yaml)"
        ),
    )?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("yaml") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    files.sort();
    ensure(
        !files.is_empty(),
        format!("{RELEASES_DIR}/ contains no .yaml release target files"),
    )?;
    Ok(files)
}

fn load_release_file(path: &Path) -> Result<ReleaseTarget> {
    load_yaml(path).with_context(|| format!("parse release ledger {}", path.display()))
}

fn load_all_releases(repo_root: &Path) -> Result<Vec<LoadedRelease>> {
    discover_release_files(repo_root)?
        .into_iter()
        .map(|path| {
            let target = load_release_file(&path)?;
            Ok(LoadedRelease { path, target })
        })
        .collect()
}

fn filter_releases<'a>(
    releases: &'a [LoadedRelease],
    opts: &ReleaseCheckOptions,
) -> Vec<&'a LoadedRelease> {
    releases
        .iter()
        .filter(|loaded| {
            opts.target
                .as_ref()
                .is_none_or(|want| loaded.target.release == *want)
        })
        .collect()
}

fn gate_slugs() -> BTreeSet<&'static str> {
    gate_registry::GATES.iter().map(|gate| gate.slug).collect()
}

fn catalog_ids(repo_root: &Path) -> Result<BTreeSet<String>> {
    Ok(load_catalog(repo_root)?
        .into_iter()
        .map(|inv| inv.id)
        .collect())
}

fn seam_slugs(entries: &[SeamRegistryEntry]) -> BTreeSet<String> {
    entries.iter().map(|entry| entry.slug.clone()).collect()
}

fn validate_witness_test(
    repo_root: &Path,
    cache: &mut SourceCache,
    tag: &str,
    witness: &str,
) -> Result<()> {
    let (rel_path, fn_name) = witness
        .rsplit_once("::")
        .with_context(|| format!("{tag}: witness `{witness}` must be repo-relative `path::fn`"))?;
    let full = resolve_repo_or_core_path(repo_root, rel_path);
    ensure(
        full.is_file(),
        format!("{tag}: witness `{witness}` points at missing file `{rel_path}`"),
    )?;
    ensure(
        file_declares_test_fn(cache, &full, fn_name)?,
        format!("{tag}: witness `{witness}` names no `#[test]`/`fn {fn_name}` in `{rel_path}`"),
    )
}

fn validate_row_schema(row: &ReleaseRow, tag: &str) -> Result<()> {
    ensure(
        !row.id.trim().is_empty(),
        format!("{tag}: row id must be non-empty"),
    )?;
    ensure(
        !row.title.trim().is_empty(),
        format!("{tag}: title must be non-empty"),
    )?;
    ensure(
        !row.surface.trim().is_empty(),
        format!("{tag}: surface must be non-empty"),
    )?;
    Ok(())
}

fn waiver_duration_days(created: IsoDate, expiry: IsoDate) -> Option<u32> {
    let created_days =
        u32::from(created.year) * 365 + u32::from(created.month) * 31 + u32::from(created.day);
    let expiry_days =
        u32::from(expiry.year) * 365 + u32::from(expiry.month) * 31 + u32::from(expiry.day);
    expiry_days.checked_sub(created_days)
}

fn validate_waiver(row: &ReleaseRow, tag: &str, today: IsoDate, strict: bool) -> Result<()> {
    let owner = row.owner.as_deref().unwrap_or("").trim();
    let reason = row.reason.as_deref().unwrap_or("").trim();
    let created = row
        .created
        .as_deref()
        .and_then(IsoDate::parse)
        .with_context(|| format!("{tag}: WAIVED row requires valid created: YYYY-MM-DD"))?;
    let expiry = row
        .expiry
        .as_deref()
        .and_then(IsoDate::parse)
        .with_context(|| format!("{tag}: WAIVED row requires valid expiry: YYYY-MM-DD"))?;
    let blast = row
        .blast_radius
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    let why_no_witness = row.why_no_witness.as_deref().unwrap_or("").trim();

    ensure(
        !owner.is_empty(),
        format!("{tag}: WAIVED row requires non-empty owner"),
    )?;
    ensure(
        !reason.is_empty(),
        format!("{tag}: WAIVED row requires non-empty reason"),
    )?;
    ensure(
        matches!(blast.as_str(), "L0" | "L1" | "L2" | "L3" | "L4"),
        format!("{tag}: WAIVED row blast_radius must be L0..L4"),
    )?;
    ensure(
        !why_no_witness.is_empty(),
        format!(
            "{tag}: WAIVED row requires why_no_witness explaining why no mechanical witness exists"
        ),
    )?;

    let max_days = if blast == "L4" {
        L4_WAIVER_MAX_DAYS
    } else {
        NON_L4_WAIVER_MAX_DAYS
    };
    let span = waiver_duration_days(created, expiry)
        .with_context(|| format!("{tag}: WAIVED row expiry must be on or after created date"))?;
    ensure(
        span <= max_days,
        format!(
            "{tag}: WAIVED row spans {span} day(s); max for blast_radius {blast} is {max_days}"
        ),
    )?;

    if strict && expiry < today {
        bail!(
            "{tag}: WAIVED row expired on {} (today is {})",
            row.expiry.as_deref().unwrap_or("?"),
            format_iso(today)
        );
    }
    Ok(())
}

fn format_iso(date: IsoDate) -> String {
    format!("{:04}-{:02}-{:02}", date.year, date.month, date.day)
}

fn row_has_witness_refs(row: &ReleaseRow) -> bool {
    !row.witness_invariants.is_empty()
        || !row.witness_tests.is_empty()
        || !row.witness_gates.is_empty()
        || !row.witness_seams.is_empty()
}

fn validate_row_references(
    repo_root: &Path,
    cache: &mut SourceCache,
    row: &ReleaseRow,
    tag: &str,
    catalog: &BTreeSet<String>,
    gates: &BTreeSet<&str>,
    seams: &BTreeSet<String>,
) -> Result<()> {
    validate_row_schema(row, tag)?;

    for inv in &row.witness_invariants {
        ensure(
            catalog.contains(inv),
            format!("{tag}: witness_invariants references unknown catalog id `{inv}`"),
        )?;
    }
    for witness in &row.witness_tests {
        validate_witness_test(repo_root, cache, tag, witness)?;
    }
    for gate in &row.witness_gates {
        ensure(
            gates.contains(gate.as_str()),
            format!("{tag}: witness_gates references unknown gate slug `{gate}`"),
        )?;
    }
    for seam in &row.witness_seams {
        ensure(
            seams.contains(seam),
            format!("{tag}: witness_seams references unknown seam slug `{seam}`"),
        )?;
    }

    match row.status {
        ReleaseStatus::Proven | ReleaseStatus::FaultInjected => ensure(
            row_has_witness_refs(row),
            format!(
                "{tag}: status {:?} requires at least one witness_invariants/tests/gates/seams ref",
                row.status
            ),
        )?,
        ReleaseStatus::FailClosed => ensure(
            !row.witness_tests.is_empty() || !row.witness_gates.is_empty(),
            format!("{tag}: FAIL-CLOSED row must cite witness_tests and/or witness_gates"),
        )?,
        ReleaseStatus::Waived => validate_waiver(row, tag, today(), false)?,
        ReleaseStatus::Incomplete => {}
    }
    Ok(())
}

fn validate_row_terminal(row: &ReleaseRow, tag: &str) -> Result<()> {
    if !row.terminal_required {
        return Ok(());
    }
    match row.status {
        ReleaseStatus::Incomplete => bail!(
            "{tag}: terminal_required row `{id}` is INCOMPLETE — release blocked",
            id = row.id
        ),
        ReleaseStatus::Waived => {
            validate_waiver(row, tag, today(), true)?;
            Ok(())
        }
        ReleaseStatus::Proven | ReleaseStatus::FailClosed | ReleaseStatus::FaultInjected => Ok(()),
    }
}

fn today() -> IsoDate {
    crate::typed_waivers::today_utc()
}

pub(crate) fn extract_justifies_invariants(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let tail = trimmed
            .strip_prefix("//! justifies:")
            .or_else(|| trimmed.strip_prefix("// justifies:"))
            .or_else(|| {
                trimmed
                    .find("// justifies:")
                    .map(|idx| &trimmed[idx + "// justifies:".len()..])
            });
        let Some(tail) = tail else {
            continue;
        };
        for tok in tail.split(|c: char| c.is_whitespace() || c == ';' || c == ',') {
            let tok = tok.trim_matches(|c: char| {
                c == '(' || c == ')' || c == '\'' || c == '"' || c == '.' || c == '`'
            });
            if let Some(rest) = tok.strip_prefix("INV-") {
                if !rest.is_empty()
                    && rest.chars().all(|c| {
                        c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-' || c == '_'
                    })
                {
                    found.insert(format!("INV-{rest}"));
                }
            }
        }
    }
    found
}

fn justifies_scan_roots(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let candidates = [
        core_tests_root(repo_root),
        repo_root.join("crates/syncbat/tests"),
        repo_root.join("crates/netbat/tests"),
        repo_root.join("crates/bvisor/tests"),
        repo_root.join("crates/hostbat/tests"),
    ];
    Ok(candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .collect())
}

pub(crate) fn check_unresolved_justifies(
    repo_root: &Path,
    catalog: &BTreeSet<String>,
) -> Result<Vec<(PathBuf, String)>> {
    let mut unresolved = Vec::new();
    for root in justifies_scan_roots(repo_root)? {
        for path in rust_files(&root) {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            for inv in extract_justifies_invariants(&text) {
                if !catalog.contains(&inv) {
                    unresolved.push((path.clone(), inv));
                }
            }
        }
    }
    unresolved.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(unresolved)
}

fn validate_duplicate_row_ids(releases: &[&LoadedRelease]) -> Result<()> {
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for loaded in releases {
        for row in &loaded.target.rows {
            if let Some(first) = seen.get(&row.id) {
                bail!(
                    "duplicate release row id `{}` in {} and {}",
                    row.id,
                    first.display(),
                    loaded.path.display()
                );
            }
            seen.insert(row.id.clone(), loaded.path.clone());
        }
    }
    Ok(())
}

pub(crate) fn check(repo_root: &Path, opts: &ReleaseCheckOptions) -> Result<GateWork> {
    let releases = load_all_releases(repo_root)?;
    let selected = filter_releases(&releases, opts);
    ensure(
        !selected.is_empty(),
        format!(
            "release-status: no release target matched filter {:?}",
            opts.target
        ),
    )?;

    if opts.strict && opts.active_only {
        let active: Vec<_> = releases
            .iter()
            .filter(|loaded| loaded.target.active)
            .collect();
        ensure(
            active.len() == 1,
            format!(
                "release-status: strict active mode requires exactly one active release target, found {}",
                active.len()
            ),
        )?;
    }

    validate_duplicate_row_ids(&selected)?;

    let catalog = catalog_ids(repo_root)?;
    let gates = gate_slugs();
    let seam_registry = load_seam_registry(repo_root)?;
    let seam_ids = seam_slugs(&seam_registry);

    let mut cache = SourceCache::new(repo_root);
    let mut inputs: BTreeSet<PathBuf> = selected.iter().map(|loaded| loaded.path.clone()).collect();
    let mut assertions = 0usize;

    for loaded in &selected {
        for (index, row) in loaded.target.rows.iter().enumerate() {
            let tag = format!(
                "{}:{}[{}]",
                relative(repo_root, &loaded.path),
                row.id,
                index
            );
            validate_row_references(
                repo_root, &mut cache, row, &tag, &catalog, &gates, &seam_ids,
            )?;
            assertions += 1
                + row.witness_invariants.len()
                + row.witness_tests.len()
                + row.witness_gates.len()
                + row.witness_seams.len()
                + row.proof_commands.len()
                + row.blocked_by.len()
                + usize::from(
                    row.notes
                        .as_ref()
                        .is_some_and(|note| !note.trim().is_empty()),
                );
            if opts.strict {
                validate_row_terminal(row, &tag)?;
                assertions += 1;
            }
        }
    }

    let unresolved = check_unresolved_justifies(repo_root, &catalog)?;
    if let Some((path, inv)) = unresolved.first() {
        inputs.insert(path.clone());
        bail!(
            "release-status: `{}` claims `justifies: {inv}` but `{inv}` is absent from traceability/invariants.yaml",
            relative(repo_root, path)
        );
    }
    assertions += unresolved.len().max(1);

    let files = inputs.len().max(1);
    Ok(GateWork::new(files, assertions.max(1), inputs))
}

#[cfg(test)]
#[path = "release_status_tests.rs"]
mod release_status_tests;
