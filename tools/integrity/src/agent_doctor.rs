use crate::agent_surface;
use crate::repo_surface::{ensure, files_with_extension, relative, tracked_repo_files};
use anyhow::Result;
use std::fs;
use std::path::Path;

struct Finding {
    id: &'static str,
    violates: &'static str,
    why: String,
    fix: &'static str,
    example: &'static str,
    warning_only: bool,
}

pub(crate) fn run(repo_root: &Path) -> Result<()> {
    let mut findings = Vec::new();
    collect_root_layout_findings(repo_root, &mut findings);
    collect_stale_path_findings(repo_root, &mut findings)?;
    collect_domain_vocab_findings(repo_root, &mut findings)?;
    collect_async_runtime_findings(repo_root, &mut findings)?;
    collect_template_findings(repo_root, &mut findings)?;

    if let Err(err) = agent_surface::check(repo_root) {
        findings.push(Finding {
            id: "BATPAK-E-AGENT-SURFACE",
            violates: "traceability/agent_surface.yaml must map intent to APIs, examples, tests, invariants, docs, and scaffolds",
            why: err.to_string(),
            fix: "Repair the referenced task or path, then run cargo xtask structural.",
            example: "traceability/agent_surface.yaml: append_typed_event",
            warning_only: false,
        });
    }

    if findings.is_empty() {
        println!("agent-doctor: ok");
        return Ok(());
    }

    for finding in &findings {
        println!("{}", finding.id);
        println!("violates: {}", finding.violates);
        println!("why: {}", finding.why);
        println!("fix: {}", finding.fix);
        println!("example: {}", finding.example);
        if finding.warning_only {
            println!("severity: warning");
        }
        println!();
    }

    let hard_count = findings
        .iter()
        .filter(|finding| !finding.warning_only)
        .count();
    ensure(
        hard_count == 0,
        format!("agent-doctor found {hard_count} blocking issue(s)"),
    )?;
    println!("agent-doctor: ok with warnings");
    Ok(())
}

fn collect_root_layout_findings(repo_root: &Path, out: &mut Vec<Finding>) {
    for legacy_dir in ["src", "tests", "benches", "examples", "fixtures"] {
        let path = repo_root.join(legacy_dir);
        if path.exists() {
            out.push(Finding {
                id: "BATPAK-E-ROOT-WRONG-SRC",
                violates: "runtime crate files live under crates/core after the topology compression",
                why: format!("root `{legacy_dir}/` exists and can trick agents into editing a dead path"),
                fix: "Move primary crate code under crates/core or delete the ownerless root directory.",
                example: "use crates/core/src/store/... instead of src/store/...",
                warning_only: false,
            });
        }
    }

    let tracked_plans: Vec<_> = tracked_repo_files(repo_root)
        .unwrap_or_default()
        .into_iter()
        .filter(|path| relative(repo_root, path).starts_with(".cursor/plans/"))
        .collect();
    if !tracked_plans.is_empty() {
        out.push(Finding {
            id: "BATPAK-E-AGENT-PLAN-TRACKED",
            violates: ".cursor/plans is local session state, not durable substrate source",
            why: format!(
                "{} tracked plan file(s) are still present under .cursor/plans",
                tracked_plans.len()
            ),
            fix: "Remove tracked plan files or promote durable content into docs/recipes/ or traceability/agent_surface.yaml.",
            example: ".cursor/rules may be tracked for repo-wide agent invariants; .cursor/plans is ignored session state.",
            warning_only: false,
        });
    }

    let local_plan_files = fs::read_dir(repo_root.join(".cursor/plans"))
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|entry| entry.file_type().map(|ty| ty.is_file()).unwrap_or(false))
        .count();
    if local_plan_files > 0 {
        out.push(Finding {
            id: "BATPAK-W-AGENT-SESSION-RESIDUE",
            violates: "session plans are not substrate source unless deliberately promoted",
            why: format!(".cursor/plans contains {local_plan_files} local file(s); keep it ignored or promote durable content into docs/ or traceability/"),
            fix: "Leave it if the tool requires it, otherwise remove it from tracking after preserving durable decisions.",
            example: "docs/recipes/ or traceability/agent_surface.yaml are durable owners; .cursor/plans is not.",
            warning_only: true,
        });
    }
}

fn collect_stale_path_findings(repo_root: &Path, out: &mut Vec<Finding>) -> Result<()> {
    for path in tracked_repo_files(repo_root)? {
        let rel = relative(repo_root, &path);
        if rel == "tools/shared/shared_checks.rs" {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for stale in [retired_path("adr"), retired_path("extraction")] {
            if content.contains(&stale) {
                out.push(Finding {
                    id: "BATPAK-E-STALE-DOC-PATH",
                    violates: "tracked docs and tools must point at live owner paths",
                    why: format!("{rel} still references retired path `{stale}`"),
                    fix: "Update the reference to docs/ADR-* or a current owner path; do not recreate retired extraction docs by accident.",
                    example: "docs/ADR-0019-canonical-encoding-contract.md",
                    warning_only: false,
                });
            }
        }
    }
    Ok(())
}

fn retired_path(name: &str) -> String {
    ["docs", name, ""].join("/")
}

fn collect_domain_vocab_findings(repo_root: &Path, out: &mut Vec<Finding>) -> Result<()> {
    let forbidden = ["downstream", "tenant", "customer", "saas"];
    for path in files_with_extension(&repo_root.join("crates/core/src"), "rs") {
        let content = fs::read_to_string(&path)?;
        for (line_index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("pub ") && !trimmed.starts_with("pub(crate) ") {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if let Some(term) = forbidden.iter().find(|term| lower.contains(**term)) {
                out.push(Finding {
                    id: "BATPAK-E-SURFACE-DOMAIN-VOCAB",
                    violates: "batpak public substrate APIs must stay domain-free",
                    why: format!(
                        "{}:{} public item contains `{term}`",
                        relative(repo_root, &path),
                        line_index + 1
                    ),
                    fix: "Rename the public surface to generic substrate vocabulary or move the domain concept above batpak.",
                    example: "reservation_ledger, state_transition, artifact_envelope",
                    warning_only: false,
                });
            }
        }
    }
    Ok(())
}

fn collect_async_runtime_findings(repo_root: &Path, out: &mut Vec<Finding>) -> Result<()> {
    for rel in ["Cargo.toml", "crates/core/Cargo.toml"] {
        let path = repo_root.join(rel);
        let content = fs::read_to_string(&path)?;
        if content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("tokio")
                || trimmed.starts_with("async-std")
                || trimmed.starts_with("smol")
        }) {
            out.push(Finding {
                id: "BATPAK-E-ASYNC-RUNTIME",
                violates: "production batpak must not introduce async runtime dependencies",
                why: format!("{rel} declares an async runtime dependency"),
                fix: "Keep runtime APIs sync-only or put async integration in a downstream adapter crate.",
                example: "ADR-0001 and INV-STORE-SYNC-ONLY",
                warning_only: false,
            });
        }
    }
    Ok(())
}

fn collect_template_findings(repo_root: &Path, out: &mut Vec<Finding>) -> Result<()> {
    for template in [
        "minimal-store",
        "typed-reactor",
        "audit-read-report",
        "projection-cache",
        "artifact-envelope",
        "registry-row",
        "backup-envelope",
        "state-transition",
        "reservation-ledger",
    ] {
        let root = repo_root.join("templates").join(template);
        for child in ["Cargo.toml", "src/lib.rs", "tests/basic.rs", "README.md"] {
            if !root.join(child).exists() {
                out.push(Finding {
                    id: "BATPAK-E-TEMPLATE-DRIFT",
                    violates: "agent templates must be executable cargo surfaces",
                    why: format!("templates/{template} is missing {child}"),
                    fix: "Add the missing file or remove the template from agent_surface.yaml and scaffold registry.",
                    example: "templates/minimal-store/tests/basic.rs",
                    warning_only: false,
                });
            }
        }
    }
    Ok(())
}
