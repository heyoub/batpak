//! Agent-safety meta-gate — "raccoon with commit access" (P1-4).
//!
//! A coding agent with commit access can make CI green the easy way: instead of
//! satisfying a gate, it *weakens the gate*. Lower a mutation threshold, raise a
//! file-size cap, add a waiver entry, re-bury a gate behind a label, delete a red
//! fixture — every one of those makes the assurance machinery accept a forged
//! reality, and every one of them is a one-line diff the agent can self-approve.
//!
//! This module is the separation-of-duties / two-person-rule control from
//! DO-178B and ISO 26262 applied to the gauntlet itself: a diff that WEAKENS an
//! assurance surface cannot merge on the agent's own authority. It requires an
//! explicit, human-applied approval that CI cannot self-grant.
//!
//! # Design for testability
//!
//! The detector is a PURE function over a unified-diff string plus an
//! [`ApprovalContext`] (labels + commit trailers). No git, no filesystem, no
//! environment — so the red fixtures in this file's test module are synthetic
//! diff strings. The CLI / git / CI wiring (`cargo xtask meta-gate`) is a thin
//! shell that produces the diff and context and calls [`evaluate`].
//!
//! # What counts as a weakening (and what does NOT)
//!
//! Only DECREASES, REMOVALS, new waiver/allowlist ENTRIES, cap/budget INCREASES,
//! gate re-burial, red-fixture deletion, and assurance DOWNGRADES count. ADDING a
//! new threshold constant, a new gate, a new red fixture, or new waiver *schema*
//! (comments / empty containers) is strengthening and is explicitly NOT flagged —
//! see [`classify_weakening`] and its tests. This is what lets a strengthening PR
//! (like the one that introduced this gate) pass its own meta-gate.

use crate::assurance::{self, AssuranceEntry, AssuranceLevel};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

/// The severity / blast radius of a detected weakening. `L4` weakenings (an L4
/// threshold, an L4 typed-waiver, an assurance downgrade of an L4 file) trigger
/// the two-person rule on top of the label+trailer requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlastRadius {
    /// A weakening of a non-L4 assurance surface. Needs label + trailer.
    Standard,
    /// A weakening touching the L4 "lies-downstream" surface. Needs label +
    /// trailer AND a trailer author distinct from the PR author.
    L4,
}

/// One detected weakening: a machine-named kind, a human-readable detail, the
/// repo-relative file it was found in, and its blast radius.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Weakening {
    pub kind: WeakeningKind,
    pub detail: String,
    pub file: String,
    pub blast_radius: BlastRadius,
}

/// The closed set of weakening signals the meta-gate detects. Stable names so a
/// downstream tool (or a human reading CI output) can grep for a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WeakeningKind {
    /// A watched numeric threshold constant DECREASED (e.g.
    /// `CRITICAL_SEAM_MIN_CATCH_PCT`, `COVERAGE_FLOOR_PCT`, an L4 threshold).
    ThresholdLowered,
    /// A watched size/debt budget INCREASED (e.g. `DEFAULT_LINE_BUDGET`,
    /// `DEFAULT_TEST_ISLAND_BUDGET`, a `max_nonblank` ceiling).
    BudgetRaised,
    /// `REPO_MUTATION_PHASE` moved to a lower phase, or a
    /// `MutationEnforcement::Threshold` became `RecordOnly`.
    MutationEnforcementWeakened,
    /// A new entry was ADDED to an allowlist / waiver file or array.
    WaiverEntryAdded,
    /// An `assurance_levels.yaml` entry DOWNGRADED a file's level.
    AssuranceDowngraded,
    /// A gate was removed from the default PR path (`ci_fast` markers / a ci.yml
    /// `if:` flipped from default-on to label-gated).
    GateReburied,
    /// A `red_fixture_test` was deleted, or `has_blocking_authority: true`
    /// flipped to `false`, in the gate registry.
    BlockingAuthorityRemoved,
}

impl WeakeningKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            WeakeningKind::ThresholdLowered => "threshold-lowered",
            WeakeningKind::BudgetRaised => "budget-raised",
            WeakeningKind::MutationEnforcementWeakened => "mutation-enforcement-weakened",
            WeakeningKind::WaiverEntryAdded => "waiver-entry-added",
            WeakeningKind::AssuranceDowngraded => "assurance-downgraded",
            WeakeningKind::GateReburied => "gate-reburied",
            WeakeningKind::BlockingAuthorityRemoved => "blocking-authority-removed",
        }
    }
}

/// The approval signals read from the PR context. Produced by the CLI shell from
/// `--label` / `--approved` / `GITHUB_*` env; a pure value here for testability.
#[derive(Debug, Clone, Default)]
pub(crate) struct ApprovalContext {
    /// PR labels (e.g. from `github.event.pull_request.labels`). The meta-gate
    /// only READS these; CI cannot self-apply `gauntlet-weaken-approved`.
    pub labels: Vec<String>,
    /// Login / identity of the PR author (`github.event.pull_request.user.login`).
    pub pr_author: Option<String>,
    /// Authors of `GAUNTLET-WEAKEN-OK:` trailers found across the PR's commits,
    /// paired with the reason text. Used for the trailer presence check and the
    /// two-person rule (a trailer author `!=` the PR author).
    pub weaken_ok_trailers: Vec<WeakenTrailer>,
}

/// One `GAUNTLET-WEAKEN-OK: <reason>` approval trailer and who authored the
/// commit carrying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeakenTrailer {
    pub reason: String,
    pub author: Option<String>,
}

/// The human-applied label that authorizes a weakening. CI cannot apply it.
pub(crate) const WEAKEN_APPROVED_LABEL: &str = "gauntlet-weaken-approved";
/// The commit trailer key that records the weakening reason.
pub(crate) const WEAKEN_OK_TRAILER_KEY: &str = "GAUNTLET-WEAKEN-OK";

/// The watched numeric threshold constants. A DECREASE in any of these (in any
/// watched file) is a [`WeakeningKind::ThresholdLowered`]. The `l4` flag marks
/// thresholds whose weakening is L4 blast radius (two-person rule).
struct ThresholdConst {
    name: &'static str,
    l4: bool,
}

/// Threshold constants whose DECREASE is a weakening. Names are matched as the
/// left side of a `const NAME ... = <int>;` assignment in any watched file.
const WATCHED_THRESHOLDS: &[ThresholdConst] = &[
    // The critical-seam mutation catch floor (mutants/lanes.rs). L3 today; its
    // weakening blasts the whole runtime mutation grade.
    ThresholdConst {
        name: "CRITICAL_SEAM_MIN_CATCH_PCT",
        l4: false,
    },
    // The repo-wide coverage floor (coverage.rs).
    ThresholdConst {
        name: "COVERAGE_FLOOR_PCT",
        l4: false,
    },
    // The L4 mutation threshold const, if/when one is introduced (spec AL-DEF).
    // Watched proactively so adding-then-lowering it cannot slip the gate.
    ThresholdConst {
        name: "L4_SEAM_MIN_CATCH_PCT",
        l4: true,
    },
    // Aggregate typed-waiver debt budget (P0-2), ratchet-down only.
    ThresholdConst {
        name: "DEBT_BUDGET",
        l4: false,
    },
];

/// Size / debt budget constants whose INCREASE is a weakening
/// ([`WeakeningKind::BudgetRaised`]). Distinct from thresholds because the
/// weakening direction is reversed (up, not down).
const WATCHED_BUDGETS: &[&str] = &["DEFAULT_LINE_BUDGET", "DEFAULT_TEST_ISLAND_BUDGET"];

/// Files whose new array/list entries are allowlist / waiver additions. A pure
/// substring match on the repo-relative `+`-side path; matched loosely so the
/// classifier needs no full path table.
const WAIVER_FILE_MARKERS: &[&str] = &[
    "traceability/typed_waivers.yaml",
    "traceability/dead_code_silencer_allowlist.yaml",
    "traceability/pub_item_allowlist.yaml",
    "traceability/invariant_citation_waivers.yaml",
    "traceability/ledger_prose_waivers.yaml",
];

/// Source files carrying allowlist ARRAYS (lanes.rs exclude/equivalent arrays,
/// harness_lints header allowlist). A new quoted-string element added inside one
/// of those arrays is a waiver addition; see [`detect_source_array_entry`].
const SOURCE_ALLOWLIST_FILES: &[&str] = &[
    "tools/xtask/src/commands/mutants/lanes.rs",
    "tools/integrity/src/harness_lints.rs",
    "tools/integrity/src/public_surface.rs",
];

/// Substrings on a removed/added const line that name a source allowlist array.
const SOURCE_ALLOWLIST_ARRAY_MARKERS: &[&str] = &[
    "MUTANT_EXCLUDE_RES",
    "EQUIVALENT_MUTANT",
    "HEADER_DEBT_ALLOWLIST",
    "OVERSIZE_HARNESS_ALLOWLIST",
];

/// A single file's diff: its repo-relative path and the added / removed lines
/// (content only, leading `+`/`-` stripped, hunk headers excluded).
#[derive(Debug, Default)]
struct FileDiff {
    path: String,
    added: Vec<String>,
    removed: Vec<String>,
}

/// Parse a unified diff into per-file added/removed line sets. Robust to the
/// `git diff` shape: `diff --git a/<p> b/<p>` headers, `+++ b/<path>` lines, and
/// `+`/`-` content lines (excluding the `+++`/`---` file markers and `@@` hunk
/// headers). The path is taken from the `+++ b/...` line (or the `--- a/...`
/// line for deletions) so renames/creations are attributed correctly.
fn parse_unified_diff(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                files.push(file);
            }
            // `a/<path> b/<path>` — take the b-side as the canonical path; fall
            // back to the a-side if the b-side is `/dev/null`.
            let path = rest
                .split_whitespace()
                .nth(1)
                .or_else(|| rest.split_whitespace().next())
                .map(strip_ab_prefix)
                .unwrap_or_default()
                .to_string();
            current = Some(FileDiff {
                path,
                ..FileDiff::default()
            });
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if let Some(b) = line.strip_prefix("+++ ") {
            let p = strip_ab_prefix(b.trim());
            if p != "/dev/null" && !p.is_empty() {
                file.path = p.to_string();
            }
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("@@") {
            continue;
        }
        if let Some(content) = line.strip_prefix('+') {
            file.added.push(content.to_string());
        } else if let Some(content) = line.strip_prefix('-') {
            file.removed.push(content.to_string());
        }
    }
    if let Some(file) = current.take() {
        files.push(file);
    }
    files
}

/// Strip a leading `a/` or `b/` git diff path prefix.
fn strip_ab_prefix(p: &str) -> &str {
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
}

/// Parse the integer literal a `const NAME ... = <int>;` (or `static`, or a YAML
/// `key: <int>`) line assigns to `name`, if the line is an assignment to `name`.
/// Returns `None` when the line does not assign `name` or has no integer RHS.
/// Tolerant of underscores in the literal (`1_000`) and type suffixes (`850usize`).
fn parse_named_int(line: &str, name: &str) -> Option<i64> {
    // The name must appear as a whole token followed (eventually) by `=` or `:`.
    let trimmed = line.trim();
    let after_name = trimmed.split_once(name).map(|(_, rest)| rest)?;
    // Guard against substring hits (e.g. `MY_NAME_X`): the char before `name`
    // and after `name` must not be an identifier char.
    let before_ok = trimmed
        .find(name)
        .map(|idx| {
            idx == 0
                || !trimmed.as_bytes()[idx - 1].is_ascii_alphanumeric()
                    && trimmed.as_bytes()[idx - 1] != b'_'
        })
        .unwrap_or(false);
    let after_ok = after_name
        .chars()
        .next()
        .map(|c| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(true);
    if !before_ok || !after_ok {
        return None;
    }
    // Find the assignment operator: `=` (Rust const) or `:` (YAML / struct field).
    let rhs = after_name
        .split_once('=')
        .map(|(_, r)| r)
        .or_else(|| after_name.split_once(':').map(|(_, r)| r))?;
    extract_leading_int(rhs)
}

/// Pull the first integer literal out of `s`, ignoring underscores and a numeric
/// type suffix (e.g. ` 850usize, // note` -> 850; ` 1_000` -> 1000).
fn extract_leading_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if c == '_' {
            continue;
        } else {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// THE pure classifier. Given a unified diff string and the PR's assurance
/// context (the L4 file set, derived from the assurance manifest), return every
/// weakening signal the diff carries. Empty result == not a weakening.
///
/// `l4_entries` is the loaded assurance manifest; the classifier uses it to
/// decide whether a weakening that touches a watched file lands on an L4 surface
/// (two-person rule). Pass an empty slice to treat every weakening as Standard.
pub(crate) fn classify_weakening(diff: &str, l4_entries: &[AssuranceEntry]) -> Vec<Weakening> {
    let mut findings = Vec::new();
    for file in parse_unified_diff(diff) {
        classify_file(&file, l4_entries, &mut findings);
    }
    findings
}

/// True when `rel` resolves to assurance level L4 under the manifest. Empty
/// manifest -> false (no L4 surface known).
fn is_l4_path(entries: &[AssuranceEntry], rel: &str) -> bool {
    !entries.is_empty() && assurance::resolve_level(entries, rel) == AssuranceLevel::L4
}

fn classify_file(file: &FileDiff, entries: &[AssuranceEntry], findings: &mut Vec<Weakening>) {
    detect_threshold_and_budget_changes(file, entries, findings);
    detect_mutation_enforcement(file, entries, findings);
    detect_waiver_additions(file, findings);
    detect_assurance_downgrade(file, findings);
    detect_gate_rebury(file, findings);
    detect_blocking_authority_removal(file, findings);
}

/// Numeric thresholds (DECREASE = weakening) and budgets (INCREASE = weakening).
/// A change is only flagged when the SAME constant appears on both a removed and
/// an added line — i.e. the value was EDITED, not merely added or deleted.
/// Adding a brand-new threshold const (only a `+` line) is strengthening and is
/// NOT flagged; this is the key false-positive guard for strengthening PRs.
fn detect_threshold_and_budget_changes(
    file: &FileDiff,
    entries: &[AssuranceEntry],
    findings: &mut Vec<Weakening>,
) {
    for threshold in WATCHED_THRESHOLDS {
        if let Some((old, new)) = paired_int_change(file, threshold.name) {
            if new < old {
                let l4 = threshold.l4 || is_l4_path(entries, &file.path);
                findings.push(Weakening {
                    kind: WeakeningKind::ThresholdLowered,
                    detail: format!("{} lowered {old} -> {new}", threshold.name),
                    file: file.path.clone(),
                    blast_radius: if l4 {
                        BlastRadius::L4
                    } else {
                        BlastRadius::Standard
                    },
                });
            }
        }
    }
    for budget in WATCHED_BUDGETS {
        if let Some((old, new)) = paired_int_change(file, budget) {
            if new > old {
                findings.push(Weakening {
                    kind: WeakeningKind::BudgetRaised,
                    detail: format!("{budget} raised {old} -> {new}"),
                    file: file.path.clone(),
                    blast_radius: if is_l4_path(entries, &file.path) {
                        BlastRadius::L4
                    } else {
                        BlastRadius::Standard
                    },
                });
            }
        }
    }
    // `max_nonblank` ceilings (file_size_ceilings.lock, if it exists): any
    // INCREASE of a `max_nonblank:` value is a budget raise. Keyed by line shape
    // rather than a const name since each lock row repeats the key.
    detect_max_nonblank_raise(file, findings);
}

/// Find a constant that was edited: present on a removed line AND an added line,
/// returning `(old_value, new_value)`. Picks the first removed value and first
/// added value for the named constant.
fn paired_int_change(file: &FileDiff, name: &str) -> Option<(i64, i64)> {
    let old = file.removed.iter().find_map(|l| parse_named_int(l, name))?;
    let new = file.added.iter().find_map(|l| parse_named_int(l, name))?;
    Some((old, new))
}

/// Detect a raised `max_nonblank:` ceiling in `file_size_ceilings.lock`: the max
/// added `max_nonblank` value exceeds the max removed one (the raise the P0-1
/// ratchet forbids). A purely-added ceiling (new file) is not flagged.
fn detect_max_nonblank_raise(file: &FileDiff, findings: &mut Vec<Weakening>) {
    if !file.path.contains("file_size_ceilings.lock") {
        return;
    }
    let max_removed = file
        .removed
        .iter()
        .filter_map(|l| parse_named_int(l, "max_nonblank"))
        .max();
    let max_added = file
        .added
        .iter()
        .filter_map(|l| parse_named_int(l, "max_nonblank"))
        .max();
    if let (Some(old), Some(new)) = (max_removed, max_added) {
        if new > old {
            findings.push(Weakening {
                kind: WeakeningKind::BudgetRaised,
                detail: format!("max_nonblank ceiling raised {old} -> {new}"),
                file: file.path.clone(),
                blast_radius: BlastRadius::Standard,
            });
        }
    }
}

/// `REPO_MUTATION_PHASE` lowered (e.g. `Phase1` -> `Phase0`) or a
/// `MutationEnforcement::Threshold` becoming `RecordOnly`.
fn detect_mutation_enforcement(
    file: &FileDiff,
    entries: &[AssuranceEntry],
    findings: &mut Vec<Weakening>,
) {
    // Phase regression: removed line sets a higher phase than the added line.
    let removed_phase = file.removed.iter().find_map(|l| parse_phase(l));
    let added_phase = file.added.iter().find_map(|l| parse_phase(l));
    if let (Some(old), Some(new)) = (removed_phase, added_phase) {
        if new < old {
            findings.push(Weakening {
                kind: WeakeningKind::MutationEnforcementWeakened,
                detail: format!("REPO_MUTATION_PHASE lowered Phase{old} -> Phase{new}"),
                file: file.path.clone(),
                blast_radius: BlastRadius::Standard,
            });
        }
    }
    // Threshold -> RecordOnly: a removed line names `MutationEnforcement::Threshold`
    // and an added line at the corresponding site names `RecordOnly`.
    let removed_threshold = file
        .removed
        .iter()
        .any(|l| l.contains("MutationEnforcement::Threshold"));
    let added_record_only = file
        .added
        .iter()
        .any(|l| l.contains("MutationEnforcement::RecordOnly"));
    let removed_record_only = file
        .removed
        .iter()
        .any(|l| l.contains("MutationEnforcement::RecordOnly"));
    if removed_threshold && added_record_only && !removed_record_only {
        findings.push(Weakening {
            kind: WeakeningKind::MutationEnforcementWeakened,
            detail: "MutationEnforcement::Threshold downgraded to RecordOnly".to_string(),
            file: file.path.clone(),
            blast_radius: if is_l4_path(entries, &file.path) {
                BlastRadius::L4
            } else {
                BlastRadius::Standard
            },
        });
    }
}

/// Parse the `N` from a `RepoMutationPhase::PhaseN` mention on a line. Scans for
/// every `Phase` occurrence and returns the first one immediately followed by a
/// digit, so the `Phase` inside `RepoMutationPhase` (followed by `:`) is skipped.
fn parse_phase(line: &str) -> Option<i64> {
    let mut search_from = 0usize;
    while let Some(rel) = line[search_from..].find("Phase") {
        let idx = search_from + rel;
        let after = &line[idx + "Phase".len()..];
        if let Some(n) = extract_leading_int(after) {
            return Some(n);
        }
        search_from = idx + "Phase".len();
    }
    None
}

/// A new entry added to a waiver / allowlist file or source array. We flag an
/// added, non-comment, content-bearing line in a waiver FILE, or an added
/// quoted-string element near a watched source allowlist array. Schema-only
/// additions (comments, the empty `[]` container, blank lines, the YAML doc
/// markers) are NOT flagged — adding a waiver SCHEMA is strengthening.
fn detect_waiver_additions(file: &FileDiff, findings: &mut Vec<Weakening>) {
    if WAIVER_FILE_MARKERS.iter().any(|m| file.path.contains(m)) {
        for line in &file.added {
            if is_yaml_entry_addition(line) {
                findings.push(Weakening {
                    kind: WeakeningKind::WaiverEntryAdded,
                    detail: format!("new waiver/allowlist entry: {}", line.trim()),
                    file: file.path.clone(),
                    blast_radius: waiver_blast_radius(file),
                });
                break; // one finding per file is enough to require approval
            }
        }
    }
    if SOURCE_ALLOWLIST_FILES.iter().any(|f| file.path.contains(f)) {
        detect_source_array_entry(file, findings);
    }
}

/// Heuristic: an added YAML line that introduces a NEW list/map entry (a `- `
/// sequence item or an `id:`/`target:` field) rather than a comment, the empty
/// `[]` container, or whitespace. This is what distinguishes "added an entry"
/// from "added the schema comment block".
fn is_yaml_entry_addition(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return false;
    }
    // The literal empty-container marker is schema, not an entry.
    if t == "[]" {
        return false;
    }
    // A new sequence item (`- id: ...`) or a keyed field that names a waiver.
    t.starts_with("- ") || t.starts_with("id:") || t.starts_with("- id:")
}

/// A `blast_radius: L4` line in the added waiver content elevates the whole
/// waiver-add to L4 severity (the spec's raccoon rule). Default Standard.
fn waiver_blast_radius(file: &FileDiff) -> BlastRadius {
    let mentions_l4 = file.added.iter().any(|l| {
        let t = l.trim();
        t.starts_with("blast_radius:") && t.contains("L4")
    });
    if mentions_l4 {
        BlastRadius::L4
    } else {
        BlastRadius::Standard
    }
}

/// Detect a new quoted-string / `r"..."` element added to one of the watched
/// source allowlist arrays. Requires a context signal — an array marker present
/// in any of the file's diff lines — then flags an added bare quoted list
/// element as a new exclusion entry.
fn detect_source_array_entry(file: &FileDiff, findings: &mut Vec<Weakening>) {
    let touches_allowlist_array = file
        .added
        .iter()
        .chain(file.removed.iter())
        .any(|l| SOURCE_ALLOWLIST_ARRAY_MARKERS.iter().any(|m| l.contains(m)));
    if !touches_allowlist_array {
        return;
    }
    for line in &file.added {
        if is_quoted_list_element(line) {
            findings.push(Weakening {
                kind: WeakeningKind::WaiverEntryAdded,
                detail: format!("new mutant-exclusion / allowlist entry: {}", line.trim()),
                file: file.path.clone(),
                blast_radius: BlastRadius::Standard,
            });
            break;
        }
    }
}

/// True for an added line that is a standalone quoted-string list element:
/// `"..."` or `r"..."` optionally with a trailing comma. NOT a `const`
/// declaration line (that is the array itself, not an entry) and NOT a comment.
fn is_quoted_list_element(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("//") || t.contains("const ") || t.contains('=') {
        return false;
    }
    (t.starts_with('"') || t.starts_with("r\"") || t.starts_with("r#\""))
        && (t.ends_with("\",") || t.ends_with('"') || t.ends_with("\"#,"))
}

/// An `assurance_levels.yaml` DOWNGRADE: fires when the max `level:` on the added
/// side is strictly below the max on the removed side (a level was lowered, not
/// merely reordered). An L4 downgrade carries L4 blast radius.
fn detect_assurance_downgrade(file: &FileDiff, findings: &mut Vec<Weakening>) {
    if !file.path.contains("traceability/assurance_levels.yaml") {
        return;
    }
    let removed_levels: BTreeSet<u8> = file
        .removed
        .iter()
        .filter_map(|l| parse_assurance_level(l))
        .collect();
    let added_levels: BTreeSet<u8> = file
        .added
        .iter()
        .filter_map(|l| parse_assurance_level(l))
        .collect();
    // A downgrade: some level present on the removed side is gone from the added
    // side and was replaced by a strictly lower level. If the added set's max is
    // below the removed set's max, a downgrade occurred.
    if let (Some(&max_removed), Some(&max_added)) =
        (removed_levels.iter().max(), added_levels.iter().max())
    {
        if max_added < max_removed {
            findings.push(Weakening {
                kind: WeakeningKind::AssuranceDowngraded,
                detail: format!("assurance level downgraded L{max_removed} -> L{max_added}"),
                file: file.path.clone(),
                blast_radius: if max_removed >= 4 {
                    BlastRadius::L4
                } else {
                    BlastRadius::Standard
                },
            });
        }
    }
}

/// Parse the digit from a `level: L<n>` line (`- level: L4` -> 4).
fn parse_assurance_level(line: &str) -> Option<u8> {
    let t = line.trim();
    let idx = t.find("level:")?;
    let after = t[idx + "level:".len()..].trim_start();
    let after = after.strip_prefix('L')?;
    // 0..=9 fits u8; `to_digit(10)` only yields 0..=9.
    after
        .chars()
        .next()?
        .to_digit(10)
        .and_then(|d| u8::try_from(d).ok())
}

/// A gate re-buried off the default PR path: a `CI_FAST_REQUIRED_GATE_MARKERS`
/// marker removed from `ci.rs`, or a ci.yml step gaining a label-gate `if:`.
fn detect_gate_rebury(file: &FileDiff, findings: &mut Vec<Weakening>) {
    // ci_fast() marker removal: a removed line carries a known gate marker and
    // no added line restores it.
    if file.path.contains("tools/xtask/src/commands/ci.rs") {
        for marker in CI_FAST_GATE_MARKERS {
            let removed = file.removed.iter().any(|l| l.contains(marker));
            let added = file.added.iter().any(|l| l.contains(marker));
            if removed && !added {
                findings.push(Weakening {
                    kind: WeakeningKind::GateReburied,
                    detail: format!("default-path gate marker removed from ci_fast(): {marker}"),
                    file: file.path.clone(),
                    blast_radius: BlastRadius::Standard,
                });
            }
        }
    }
    // ci.yml: an added `if:` line that gates a step on a label (re-burying a
    // default-on gate behind a label). We flag an ADDED `if:` mentioning a
    // `run-`/`gauntlet-` label condition on `contains(... labels ...)`.
    if file.path.ends_with(".github/workflows/ci.yml") || file.path.contains("workflows/ci.yml") {
        for line in &file.added {
            let t = line.trim();
            if t.starts_with("if:")
                && t.contains("labels")
                && (t.contains("run-") || t.contains("gauntlet-") || t.contains("heavy"))
            {
                findings.push(Weakening {
                    kind: WeakeningKind::GateReburied,
                    detail: format!("ci.yml step gained a label-gate condition: {t}"),
                    file: file.path.clone(),
                    blast_radius: BlastRadius::Standard,
                });
                break;
            }
        }
    }
}

/// The distinctive ci_fast() gate markers whose removal re-buries an L2+ gate.
/// Kept in lockstep with `ci_parity::CI_FAST_REQUIRED_GATE_MARKERS` by intent;
/// duplicated here because the meta-gate works on diff text, not on the live
/// const. (A future test could assert the two lists agree.)
const CI_FAST_GATE_MARKERS: &[&str] = &[
    "coverage::cover(CoverArgs",
    "crate::public_api::public_api(PublicApiArgs",
    "super::package_leak_scan(PackageLeakScanArgs",
    "integrity(\"doctor\", [\"--strict\"])",
    "integrity(\"gauntlet-receipts-present\"",
];

/// A red fixture deleted from the gate registry, or `has_blocking_authority`
/// flipped from `true` to `false`.
fn detect_blocking_authority_removal(file: &FileDiff, findings: &mut Vec<Weakening>) {
    if !file.path.contains("tools/integrity/src/gate_registry.rs") {
        return;
    }
    // has_blocking_authority: true -> false. Pair a removed `: true` with an
    // added `: false` on the `has_blocking_authority` field.
    let removed_true = file
        .removed
        .iter()
        .any(|l| l.contains("has_blocking_authority: true"));
    let added_false = file
        .added
        .iter()
        .any(|l| l.contains("has_blocking_authority: false"));
    let added_true = file
        .added
        .iter()
        .any(|l| l.contains("has_blocking_authority: true"));
    if removed_true && added_false && !added_true {
        findings.push(Weakening {
            kind: WeakeningKind::BlockingAuthorityRemoved,
            detail: "has_blocking_authority flipped true -> false".to_string(),
            file: file.path.clone(),
            blast_radius: BlastRadius::L4,
        });
    }
    // A red_fixture_test removed (a `red_fixture_test: Some(...)` line removed
    // with no added replacement Some(...)).
    let removed_red = file
        .removed
        .iter()
        .any(|l| l.contains("red_fixture_test: Some("));
    let added_red = file
        .added
        .iter()
        .any(|l| l.contains("red_fixture_test: Some("));
    let added_none = file
        .added
        .iter()
        .any(|l| l.contains("red_fixture_test: None"));
    if removed_red && !added_red && (added_none || file.added.iter().all(|l| l.trim().is_empty())) {
        findings.push(Weakening {
            kind: WeakeningKind::BlockingAuthorityRemoved,
            detail: "a red_fixture_test was deleted from the gate registry".to_string(),
            file: file.path.clone(),
            blast_radius: BlastRadius::L4,
        });
    }
}

/// Whether the context carries the human-applied approval label.
fn has_approval_label(ctx: &ApprovalContext) -> bool {
    ctx.labels.iter().any(|l| l == WEAKEN_APPROVED_LABEL)
}

/// Whether the context carries at least one `GAUNTLET-WEAKEN-OK:` trailer.
fn has_weaken_trailer(ctx: &ApprovalContext) -> bool {
    !ctx.weaken_ok_trailers.is_empty()
}

/// Whether some `GAUNTLET-WEAKEN-OK` trailer was authored by a person OTHER than
/// the PR author (the two-person rule for L4 blast-radius weakenings). When the
/// PR author is unknown we conservatively require a trailer author to be present
/// and treat an unknown trailer author as NOT satisfying the rule.
fn has_independent_trailer(ctx: &ApprovalContext) -> bool {
    let pr_author = ctx.pr_author.as_deref();
    ctx.weaken_ok_trailers
        .iter()
        .any(|t| match (&t.author, pr_author) {
            (Some(trailer_author), Some(pr)) => trailer_author != pr,
            // Trailer author present, PR author unknown: cannot prove distinctness.
            (Some(_), None) => false,
            // No trailer author recorded: cannot satisfy the two-person rule.
            (None, _) => false,
        })
}

/// Evaluate a diff against the approval context. `Ok(())` when the diff is not a
/// weakening, or is a weakening WITH the required approval. `Err` with a precise,
/// per-weakening message otherwise.
///
/// Approval rules:
///   * any weakening requires the `gauntlet-weaken-approved` label AND a
///     `GAUNTLET-WEAKEN-OK: <reason>` trailer;
///   * an L4-blast-radius weakening additionally requires the trailer author to
///     differ from the PR author (two-person rule).
pub(crate) fn evaluate(
    diff: &str,
    l4_entries: &[AssuranceEntry],
    ctx: &ApprovalContext,
) -> Result<()> {
    let findings = classify_weakening(diff, l4_entries);
    if findings.is_empty() {
        return Ok(());
    }
    let label = has_approval_label(ctx);
    let trailer = has_weaken_trailer(ctx);
    let independent = has_independent_trailer(ctx);
    let any_l4 = findings.iter().any(|w| w.blast_radius == BlastRadius::L4);

    let approved_standard = label && trailer;
    let approved_l4 = approved_standard && independent;
    let satisfied = if any_l4 {
        approved_l4
    } else {
        approved_standard
    };
    if satisfied {
        return Ok(());
    }

    let mut msg = String::new();
    msg.push_str(
        "meta-gate: this diff WEAKENS the assurance machinery and lacks the required approval.\n",
    );
    for w in &findings {
        let radius = match w.blast_radius {
            BlastRadius::L4 => "L4",
            BlastRadius::Standard => "standard",
        };
        msg.push_str(&format!(
            "  - [{}] {} (in {}) [blast_radius={radius}]\n",
            w.kind.as_str(),
            w.detail,
            w.file
        ));
    }
    msg.push_str("\nTo land a weakening you MUST:\n");
    msg.push_str(&format!(
        "  1. add a `{WEAKEN_OK_TRAILER_KEY}: <reason>` trailer to a commit on this PR{}\n",
        if trailer { " [present]" } else { " [MISSING]" }
    ));
    msg.push_str(&format!(
        "  2. have a human apply the `{WEAKEN_APPROVED_LABEL}` PR label (CI cannot self-apply it){}\n",
        if label { " [present]" } else { " [MISSING]" }
    ));
    if any_l4 {
        msg.push_str(&format!(
            "  3. (L4 blast radius) the trailer author MUST differ from the PR author \
             (two-person rule){}\n",
            if independent {
                " [satisfied]"
            } else {
                " [NOT satisfied]"
            }
        ));
    }
    anyhow::bail!(msg);
}

/// Parse `GAUNTLET-WEAKEN-OK:` trailers and their commit authors from a
/// commits-file. The CLI shell produces this with
/// `git log <base>..HEAD --format=%an%x00%B%x01`: each commit is one record
/// terminated by `\x01`, and within a record the author and the message body are
/// separated by `\x00`. Any line in a commit body whose (trimmed) form is
/// `GAUNTLET-WEAKEN-OK: <reason>` yields a [`WeakenTrailer`] attributed to that
/// commit's author. This pairing is what powers the two-person rule.
pub(crate) fn parse_weaken_trailers(commits: &str) -> Vec<WeakenTrailer> {
    let mut trailers = Vec::new();
    for record in commits.split('\u{1}') {
        if record.trim().is_empty() {
            continue;
        }
        let (author, body) = match record.split_once('\u{0}') {
            Some((a, b)) => (a.trim().to_string(), b),
            // No NUL separator: treat the whole record as body, author unknown.
            None => (String::new(), record),
        };
        let author = if author.is_empty() {
            None
        } else {
            Some(author)
        };
        for line in body.lines() {
            let t = line.trim();
            let prefix = format!("{WEAKEN_OK_TRAILER_KEY}:");
            if let Some(reason) = t.strip_prefix(&prefix) {
                trailers.push(WeakenTrailer {
                    reason: reason.trim().to_string(),
                    author: author.clone(),
                });
            }
        }
    }
    trailers
}

/// Load the assurance manifest for the meta-gate's L4 classification. Falls back
/// to an empty manifest (every weakening treated as Standard) if the manifest is
/// absent, so the meta-gate still runs in a packaged tree.
pub(crate) fn load_l4_entries(repo_root: &Path) -> Vec<AssuranceEntry> {
    assurance::load_manifest(repo_root).unwrap_or_default()
}

#[cfg(test)]
#[path = "meta_gate_tests.rs"]
mod meta_gate_tests;
