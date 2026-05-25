//! PCP-aligned handoff packet capture for agent/operator context.
//!
//! Writes `target/context/latest.json` and `target/context/latest.md`.
//! Read-only toward git and the factory ledger store.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::commands::factory_ledger::collect_ledger_lines;
use crate::util::{cargo_target_dir, command_succeeds, project_root, run_output};
use crate::ContextArgs;

const SCHEMA_VERSION: u32 = 1;

const FACTORY_STACK_PARENTS: &[(&str, &str)] = &[
    ("factory/host-dev-profile", "factory/ordnance-cut"),
    ("factory/audit-loop", "factory/host-dev-profile"),
    ("factory/descriptor-inventory", "factory/audit-loop"),
    ("factory/factory-ledger", "factory/descriptor-inventory"),
    ("factory/host-proof-verbs", "factory/factory-ledger"),
    ("factory/context-packets", "factory/host-proof-verbs"),
];

const BOUNDARY_REMINDERS: &[&str] = &[
    "Moonwalker graph law lives in a separate repo; not BatPAK substrate traversal.",
    "PCP_SPEC is a separate spec; this packet is PCP-aligned handoff tooling, not PCP-Core.",
    "BatPAK preserves opaque extension bytes; it does not validate PCP schemas at runtime.",
    "event.walk is hash-chain ancestry only; event.query is commit-order pagination.",
];

const PROOF_COMMAND_MARKERS: &[&str] = &["host-dev", "host-loop", "inspect", "verify"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ContextPacket {
    schema_version: u32,
    generated_at_ms: u64,
    git: GitSection,
    base_branch: BaseBranchHint,
    tracking_branch: Option<String>,
    stacked_prs: Vec<StackedPrRow>,
    github_context_note: Option<String>,
    factory_ledger: Vec<String>,
    changed_files: ChangedFilesSection,
    untracked_warnings: Vec<String>,
    verification_summary: VerificationSummary,
    next_cut_notes: Option<String>,
    boundary_reminders: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GitSection {
    branch: String,
    head: String,
    head_short: String,
    head_oneline: String,
    dirty: bool,
    status_short: Vec<String>,
    diff_stat: String,
    staged_diff_stat: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BaseBranchHint {
    value: String,
    source: BaseBranchSource,
    detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BaseBranchSource {
    GithubPr,
    StackParent,
    RemoteDefault,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StackedPrRow {
    number: u64,
    title: String,
    head_ref: String,
    base_ref: String,
    url: String,
    state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ChangedFilesSection {
    worktree: Vec<ChangedFileRow>,
    staged: Vec<ChangedFileRow>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ChangedFileRow {
    status: String,
    path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct VerificationSummary {
    ledger_tail: Vec<String>,
    operator_notes: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct GhPrViewJson {
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(default, rename = "headRefName")]
    head_ref_name: String,
    number: u64,
    url: String,
    title: String,
    state: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GhPrListRow {
    number: u64,
    title: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    url: String,
    state: String,
}

pub(crate) fn context(args: ContextArgs) -> Result<()> {
    let root = project_root()?;
    let packet = capture_context_packet(&root, args.ledger_limit, args.notes)?;
    let target_dir = cargo_target_dir()?.join("context");
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;

    let json_path = target_dir.join("latest.json");
    let md_path = target_dir.join("latest.md");
    let json = serde_json::to_string_pretty(&packet).context("serialize context packet")?;
    let markdown = render_context_markdown(&packet);
    fs::write(&json_path, json).with_context(|| format!("write {}", json_path.display()))?;
    fs::write(&md_path, markdown).with_context(|| format!("write {}", md_path.display()))?;
    println!("context: wrote {}", json_path.display());
    println!("context: wrote {}", md_path.display());
    Ok(())
}

fn capture_context_packet(
    root: &Path,
    ledger_limit: usize,
    notes: Option<String>,
) -> Result<ContextPacket> {
    let branch =
        git_output(root, ["branch", "--show-current"]).unwrap_or_else(|_| "unknown".into());
    let head = git_output(root, ["rev-parse", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let head_short =
        git_output(root, ["rev-parse", "--short", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let head_oneline =
        git_output(root, ["log", "-1", "--oneline"]).unwrap_or_else(|_| "unknown".into());
    let status_short_raw =
        git_output(root, ["status", "--short"]).unwrap_or_else(|_| String::new());
    let status_short = parse_status_short_lines(&status_short_raw);
    let dirty = !status_short.is_empty();
    let diff_stat = git_output(root, ["diff", "--stat"]).unwrap_or_else(|_| String::new());
    let staged_diff_stat =
        git_output(root, ["diff", "--cached", "--stat"]).unwrap_or_else(|_| String::new());
    let worktree_name_status =
        git_output(root, ["diff", "--name-status"]).unwrap_or_else(|_| String::new());
    let staged_name_status =
        git_output(root, ["diff", "--cached", "--name-status"]).unwrap_or_else(|_| String::new());
    let changed_files = changed_files_from_name_status(&worktree_name_status, &staged_name_status);
    let untracked_warnings = untracked_from_status_short(&status_short);

    let tracking_branch = git_output(
        root,
        ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .ok();

    let (gh_pr, stacked_prs, github_context_note) = capture_github_context(root);
    let base_branch = infer_base_branch(&branch, gh_pr.as_ref(), || remote_default_branch(root));

    let factory_ledger = collect_ledger_lines(ledger_limit).unwrap_or_default();
    let ledger_tail = proof_lines_from_ledger(&factory_ledger);
    let verification_summary = VerificationSummary {
        ledger_tail,
        operator_notes: notes.clone(),
    };

    Ok(ContextPacket {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: now_ms(),
        git: GitSection {
            branch,
            head,
            head_short,
            head_oneline,
            dirty,
            status_short,
            diff_stat,
            staged_diff_stat,
        },
        base_branch,
        tracking_branch,
        stacked_prs,
        github_context_note,
        factory_ledger,
        changed_files,
        untracked_warnings,
        verification_summary,
        next_cut_notes: notes,
        boundary_reminders: BOUNDARY_REMINDERS
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
    })
}

fn capture_github_context(
    root: &Path,
) -> (Option<GhPrViewJson>, Vec<StackedPrRow>, Option<String>) {
    if !command_succeeds("gh", ["--version"]) {
        return (
            None,
            Vec::new(),
            Some("GitHub PR context: unavailable".to_owned()),
        );
    }

    let current = try_gh_pr_view(root);
    let mut stacked = try_gh_pr_list(root);
    if let Some(ref pr) = current {
        if !stacked.iter().any(|row| row.number == pr.number) {
            stacked.push(gh_pr_to_row(pr));
        }
        stacked.sort_by(|left, right| left.number.cmp(&right.number));
    }
    if current.is_none() && stacked.is_empty() {
        return (
            None,
            Vec::new(),
            Some("GitHub PR context: unavailable".to_owned()),
        );
    }
    (current, stacked, None)
}

fn gh_pr_to_row(pr: &GhPrViewJson) -> StackedPrRow {
    StackedPrRow {
        number: pr.number,
        title: pr.title.clone(),
        head_ref: pr.head_ref_name.clone(),
        base_ref: pr.base_ref_name.clone(),
        url: pr.url.clone(),
        state: pr.state.clone(),
    }
}

fn try_gh_pr_view(root: &Path) -> Option<GhPrViewJson> {
    let mut command = Command::new("gh");
    command.current_dir(root).args([
        "pr",
        "view",
        "--json",
        "baseRefName,headRefName,number,url,title,state",
    ]);
    let output = run_output(command).ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

fn try_gh_pr_list(root: &Path) -> Vec<StackedPrRow> {
    let mut command = Command::new("gh");
    command.current_dir(root).args([
        "pr",
        "list",
        "--json",
        "number,title,headRefName,baseRefName,url,state",
        "--limit",
        "20",
    ]);
    let Ok(output) = run_output(command) else {
        return Vec::new();
    };
    let rows: Vec<GhPrListRow> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let mut stacked: Vec<StackedPrRow> = rows
        .into_iter()
        .map(|row| StackedPrRow {
            number: row.number,
            title: row.title,
            head_ref: row.head_ref_name,
            base_ref: row.base_ref_name,
            url: row.url,
            state: row.state,
        })
        .collect();
    stacked.sort_by(|left, right| left.number.cmp(&right.number));
    stacked
}

pub(crate) fn stack_parent_for_branch(branch: &str) -> Option<&'static str> {
    FACTORY_STACK_PARENTS
        .iter()
        .find_map(|(child, parent)| (*child == branch).then_some(*parent))
}

pub(crate) fn infer_base_branch(
    branch: &str,
    gh_pr: Option<&GhPrViewJson>,
    remote_default: impl FnOnce() -> Option<String>,
) -> BaseBranchHint {
    if let Some(pr) = gh_pr {
        return BaseBranchHint {
            value: pr.base_ref_name.clone(),
            source: BaseBranchSource::GithubPr,
            detail: None,
        };
    }
    if let Some(parent) = stack_parent_for_branch(branch) {
        return BaseBranchHint {
            value: parent.to_owned(),
            source: BaseBranchSource::StackParent,
            detail: None,
        };
    }
    if let Some(default) = remote_default() {
        return BaseBranchHint {
            value: default,
            source: BaseBranchSource::RemoteDefault,
            detail: None,
        };
    }
    BaseBranchHint {
        value: "unknown".to_owned(),
        source: BaseBranchSource::Unknown,
        detail: Some("gh unavailable and no stack hint matched".to_owned()),
    }
}

fn remote_default_branch(root: &Path) -> Option<String> {
    let raw = git_output(root, ["symbolic-ref", "refs/remotes/origin/HEAD"]).ok()?;
    raw.strip_prefix("refs/remotes/origin/")
        .map(str::to_owned)
        .or_else(|| raw.strip_prefix("origin/").map(str::to_owned).or(Some(raw)))
}

pub(crate) fn parse_status_short_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

pub(crate) fn changed_files_from_name_status(worktree: &str, staged: &str) -> ChangedFilesSection {
    ChangedFilesSection {
        worktree: parse_name_status_lines(worktree),
        staged: parse_name_status_lines(staged),
    }
}

fn parse_name_status_lines(raw: &str) -> Vec<ChangedFileRow> {
    let mut rows = BTreeSet::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((status, path)) = split_name_status_line(line) else {
            continue;
        };
        rows.insert(ChangedFileRow {
            status: status.to_owned(),
            path: path.to_owned(),
        });
    }
    rows.into_iter().collect()
}

fn split_name_status_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.split_whitespace();
    let status = parts.next()?;
    let path = parts.next()?;
    Some((status, path))
}

pub(crate) fn untracked_from_status_short(status_short: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();
    for line in status_short {
        let Some(path) = line.strip_prefix("?? ") else {
            continue;
        };
        if path == "bpk-lib/crates/core/.batpak.lock" {
            warnings.push(format!("{path}: intentional local lock; do not commit"));
        } else if path.starts_with("bpk-lib/target/") || path.contains("/target/") {
            warnings.push(format!("{path}: build artifact; expected under target/"));
        } else {
            warnings.push(format!("{path}: untracked"));
        }
    }
    warnings.sort();
    warnings
}

fn proof_lines_from_ledger(lines: &[String]) -> Vec<String> {
    let mut hits: Vec<String> = lines
        .iter()
        .filter(|line| {
            PROOF_COMMAND_MARKERS
                .iter()
                .any(|marker| line.contains(marker))
        })
        .cloned()
        .collect();
    if hits.len() > 10 {
        hits.truncate(10);
    }
    hits
}

pub(crate) fn render_context_markdown(packet: &ContextPacket) -> String {
    let mut out = String::from("# BatPAK Context Packet (PCP-aligned handoff v0)\n\n");
    out.push_str(
        "Use this packet to hand work between agents or operators. \
         It captures git state, stacked-PR hints, factory-ledger tail, \
         and boundary reminders. It is not PCP-Core and does not validate schemas.\n\n",
    );

    out.push_str("## Git\n\n");
    out.push_str(&format!("- branch: `{}`\n", packet.git.branch));
    out.push_str(&format!(
        "- head: `{}` (`{}`)\n",
        packet.git.head_short, packet.git.head
    ));
    out.push_str(&format!("- oneline: `{}`\n", packet.git.head_oneline));
    out.push_str(&format!("- dirty: `{}`\n", packet.git.dirty));
    if let Some(tracking) = &packet.tracking_branch {
        out.push_str(&format!("- tracking branch: `{}`\n", tracking));
    }

    out.push_str("\n## Base branch\n\n");
    let source = base_branch_source_label(packet.base_branch.source);
    match &packet.base_branch.detail {
        Some(detail) => out.push_str(&format!(
            "Base branch: {} (source: {}; {})\n",
            packet.base_branch.value, source, detail
        )),
        None => out.push_str(&format!(
            "Base branch: {} (source: {})\n",
            packet.base_branch.value, source
        )),
    }

    if let Some(note) = &packet.github_context_note {
        out.push_str(&format!("\n{note}\n"));
    }

    out.push_str("\n## Stacked PRs (best-effort)\n\n");
    if packet.stacked_prs.is_empty() {
        out.push_str("- none captured\n");
    } else {
        for row in &packet.stacked_prs {
            out.push_str(&format!(
                "- #{} `{}` -> `{}` [{}]({})\n",
                row.number, row.head_ref, row.base_ref, row.state, row.url
            ));
        }
    }

    out.push_str("\n## Changed files (tracked)\n\n");
    if packet.changed_files.worktree.is_empty() && packet.changed_files.staged.is_empty() {
        out.push_str("- none\n");
    } else {
        if !packet.changed_files.staged.is_empty() {
            out.push_str("### Staged\n\n");
            for row in &packet.changed_files.staged {
                out.push_str(&format!("- `{}` {}\n", row.status, row.path));
            }
        }
        if !packet.changed_files.worktree.is_empty() {
            out.push_str("### Worktree\n\n");
            for row in &packet.changed_files.worktree {
                out.push_str(&format!("- `{}` {}\n", row.status, row.path));
            }
        }
    }

    out.push_str("\n## Untracked warnings\n\n");
    if packet.untracked_warnings.is_empty() {
        out.push_str("- none\n");
    } else {
        for warning in &packet.untracked_warnings {
            out.push_str(&format!("- {warning}\n"));
        }
    }

    out.push_str("\n## Verification summary\n\n");
    if packet.verification_summary.ledger_tail.is_empty() {
        out.push_str("- no recent proof commands in ledger tail\n");
    } else {
        for line in &packet.verification_summary.ledger_tail {
            out.push_str(&format!("- `{line}`\n"));
        }
    }
    if let Some(notes) = &packet.verification_summary.operator_notes {
        out.push_str(&format!("\nOperator notes: {notes}\n"));
    }

    out.push_str("\n## Factory ledger tail\n\n");
    if packet.factory_ledger.is_empty() {
        out.push_str("- empty (store absent or no events)\n");
    } else {
        for line in &packet.factory_ledger {
            out.push_str(&format!("- `{line}`\n"));
        }
    }

    if let Some(notes) = &packet.next_cut_notes {
        out.push_str(&format!("\n## Next cut notes\n\n{notes}\n"));
    }

    out.push_str("\n## Boundary reminders\n\n");
    for reminder in &packet.boundary_reminders {
        out.push_str(&format!("- {reminder}\n"));
    }

    out.push_str(&format!(
        "\n---\n\nGenerated at `{}` (schema version {}). JSON mirror: `target/context/latest.json`.\n",
        packet.generated_at_ms, packet.schema_version
    ));
    out
}

fn git_output<const N: usize>(root: &Path, args: [&str; N]) -> Result<String> {
    let mut command = Command::new("git");
    command.current_dir(root).args(args);
    let output = run_output(command)?;
    Ok(String::from_utf8(output.stdout)
        .context("git output utf8")?
        .trim()
        .to_owned())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn base_branch_source_label(source: BaseBranchSource) -> &'static str {
    match source {
        BaseBranchSource::GithubPr => "github_pr",
        BaseBranchSource::StackParent => "stack_parent",
        BaseBranchSource::RemoteDefault => "remote_default",
        BaseBranchSource::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh_fixture() -> GhPrViewJson {
        GhPrViewJson {
            base_ref_name: "factory/host-proof-verbs".to_owned(),
            head_ref_name: "factory/context-packets".to_owned(),
            number: 56,
            url: "https://github.com/example/pull/56".to_owned(),
            title: "Cut 7".to_owned(),
            state: "OPEN".to_owned(),
        }
    }

    #[test]
    fn stack_parent_map_resolves_factory_branches() {
        assert_eq!(
            stack_parent_for_branch("factory/context-packets"),
            Some("factory/host-proof-verbs")
        );
        assert_eq!(
            stack_parent_for_branch("factory/host-proof-verbs"),
            Some("factory/factory-ledger")
        );
        assert_eq!(stack_parent_for_branch("main"), None);
    }

    #[test]
    fn base_branch_inference_prefers_github_pr() {
        let hint = infer_base_branch("factory/context-packets", Some(&gh_fixture()), || {
            Some("main".to_owned())
        });
        assert_eq!(hint.source, BaseBranchSource::GithubPr);
        assert_eq!(hint.value, "factory/host-proof-verbs");
    }

    #[test]
    fn base_branch_falls_back_to_stack_parent() {
        let hint = infer_base_branch("factory/context-packets", None, || Some("main".to_owned()));
        assert_eq!(hint.source, BaseBranchSource::StackParent);
        assert_eq!(hint.value, "factory/host-proof-verbs");
    }

    #[test]
    fn base_branch_unknown_when_no_hints() {
        let hint = infer_base_branch("feature/unmapped", None, || None);
        assert_eq!(hint.source, BaseBranchSource::Unknown);
        assert_eq!(hint.value, "unknown");
        assert!(hint.detail.is_some());
    }

    #[test]
    fn base_branch_never_uses_tracking_as_pr_base() {
        let tracking = Some("origin/factory/context-packets".to_owned());
        let hint = infer_base_branch("factory/context-packets", None, || tracking.clone());
        assert_eq!(hint.source, BaseBranchSource::StackParent);
        assert_ne!(hint.value, tracking.unwrap());
    }

    #[test]
    fn render_markdown_includes_base_source() {
        let packet = ContextPacket {
            schema_version: 1,
            generated_at_ms: 1,
            git: GitSection {
                branch: "factory/context-packets".to_owned(),
                head: "abc".to_owned(),
                head_short: "abc".to_owned(),
                head_oneline: "abc msg".to_owned(),
                dirty: false,
                status_short: Vec::new(),
                diff_stat: String::new(),
                staged_diff_stat: String::new(),
            },
            base_branch: BaseBranchHint {
                value: "factory/host-proof-verbs".to_owned(),
                source: BaseBranchSource::StackParent,
                detail: None,
            },
            tracking_branch: Some("origin/factory/context-packets".to_owned()),
            stacked_prs: Vec::new(),
            github_context_note: None,
            factory_ledger: Vec::new(),
            changed_files: ChangedFilesSection {
                worktree: Vec::new(),
                staged: Vec::new(),
            },
            untracked_warnings: Vec::new(),
            verification_summary: VerificationSummary {
                ledger_tail: Vec::new(),
                operator_notes: None,
            },
            next_cut_notes: None,
            boundary_reminders: BOUNDARY_REMINDERS
                .iter()
                .map(|line| (*line).to_owned())
                .collect(),
        };
        let rendered = render_context_markdown(&packet);
        assert!(rendered.contains("source: stack_parent"));
        assert!(rendered.contains("tracking branch: `origin/factory/context-packets`"));
    }

    #[test]
    fn packet_json_roundtrips_schema_fields() {
        let packet = ContextPacket {
            schema_version: SCHEMA_VERSION,
            generated_at_ms: 42,
            git: GitSection {
                branch: "b".to_owned(),
                head: "full".to_owned(),
                head_short: "short".to_owned(),
                head_oneline: "line".to_owned(),
                dirty: true,
                status_short: vec![" M file".to_owned()],
                diff_stat: "1 file".to_owned(),
                staged_diff_stat: String::new(),
            },
            base_branch: BaseBranchHint {
                value: "main".to_owned(),
                source: BaseBranchSource::RemoteDefault,
                detail: None,
            },
            tracking_branch: None,
            stacked_prs: vec![StackedPrRow {
                number: 1,
                title: "t".to_owned(),
                head_ref: "h".to_owned(),
                base_ref: "main".to_owned(),
                url: "u".to_owned(),
                state: "OPEN".to_owned(),
            }],
            github_context_note: None,
            factory_ledger: vec!["line".to_owned()],
            changed_files: ChangedFilesSection {
                worktree: vec![ChangedFileRow {
                    status: "M".to_owned(),
                    path: "a.rs".to_owned(),
                }],
                staged: Vec::new(),
            },
            untracked_warnings: vec!["path: untracked".to_owned()],
            verification_summary: VerificationSummary {
                ledger_tail: vec!["inspect".to_owned()],
                operator_notes: Some("note".to_owned()),
            },
            next_cut_notes: Some("next".to_owned()),
            boundary_reminders: vec!["reminder".to_owned()],
        };
        let json = serde_json::to_string(&packet).expect("serialize");
        let decoded: ContextPacket = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, packet);
    }

    #[test]
    fn changed_files_from_diff_name_status() {
        let section = changed_files_from_name_status("M\ta.rs\n", "A\tb.rs\n");
        assert_eq!(section.worktree.len(), 1);
        assert_eq!(section.worktree[0].path, "a.rs");
        assert_eq!(section.staged.len(), 1);
        assert_eq!(section.staged[0].path, "b.rs");
    }

    #[test]
    fn untracked_from_status_short_parses_canonical_lines() {
        let status = parse_status_short_lines(
            "?? bpk-lib/crates/core/.batpak.lock\n?? bpk-lib/target/debug/foo\n",
        );
        let warnings = untracked_from_status_short(&status);
        assert!(warnings
            .iter()
            .any(|w| w.contains("intentional local lock")));
        assert!(warnings.iter().any(|w| w.contains("build artifact")));
    }
}
