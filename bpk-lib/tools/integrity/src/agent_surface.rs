use crate::repo_surface::{ensure, load_yaml, relative, resolve_repo_or_core_path};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const REQUIRED_TASKS: &[&str] = &[
    "append_typed_event",
    "read_region",
    "read_with_evidence",
    "build_projection",
    "watch_projection",
    "cursor_replay",
    "lossy_subscription",
    "schema_snapshot",
    "chain_walk_evidence",
    "subscriber_frontier_evidence",
    "projection_run_evidence",
    "artifact_envelope",
    "attested_registry",
    "backup_envelope",
    "state_transition",
    "reservation_ledger",
    "platform_evidence",
];

const SCAFFOLD_PATTERNS: &[&str] = &[
    "typed-store",
    "reactor",
    "evidence-read",
    "projection-cache",
    "artifact-envelope",
    "registry-row",
    "backup-envelope",
    "state-transition",
    "reservation-ledger",
];

#[derive(Debug, Deserialize)]
struct AgentSurface {
    tasks: BTreeMap<String, AgentTask>,
}

#[derive(Debug, Deserialize)]
struct AgentTask {
    problem: String,
    #[serde(default, rename = "use")]
    use_: Vec<String>,
    #[serde(default)]
    avoid: Vec<String>,
    #[serde(default)]
    examples: Vec<String>,
    #[serde(default)]
    tests: Vec<String>,
    #[serde(default)]
    invariants: Vec<String>,
    #[serde(default)]
    docs: Vec<String>,
    #[serde(default)]
    scaffold: Vec<String>,
}

pub(crate) fn run(repo_root: &Path) -> Result<()> {
    check(repo_root)?;
    outln!("agent-surface-check: ok");
    Ok(())
}

pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let path = repo_root.join("traceability/agent_surface.yaml");
    let surface: AgentSurface = load_yaml(&path)?;
    for task in REQUIRED_TASKS {
        ensure(
            surface.tasks.contains_key(*task),
            format!("BATPAK-E-AGENT-SURFACE-MISSING-TASK: traceability/agent_surface.yaml is missing required task `{task}`"),
        )?;
    }
    for (name, task) in &surface.tasks {
        validate_task(repo_root, name, task)
            .with_context(|| format!("validate agent surface task `{name}`"))?;
    }
    Ok(())
}

fn validate_task(repo_root: &Path, name: &str, task: &AgentTask) -> Result<()> {
    ensure(
        !task.problem.trim().is_empty(),
        format!("BATPAK-E-AGENT-SURFACE-EMPTY: task `{name}` has empty problem"),
    )?;
    ensure_non_empty(name, "use", &task.use_)?;
    ensure_non_empty(name, "avoid", &task.avoid)?;
    ensure_non_empty(name, "examples", &task.examples)?;
    ensure_non_empty(name, "tests", &task.tests)?;
    ensure_non_empty(name, "invariants", &task.invariants)?;
    ensure_non_empty(name, "docs", &task.docs)?;

    for rel in task
        .examples
        .iter()
        .chain(task.tests.iter())
        .chain(task.docs.iter())
    {
        ensure_path_exists(repo_root, name, rel)?;
    }
    for pattern in &task.scaffold {
        ensure(
            SCAFFOLD_PATTERNS.contains(&pattern.as_str()),
            format!("BATPAK-E-AGENT-SURFACE-SCAFFOLD: task `{name}` references unknown scaffold `{pattern}`"),
        )?;
    }
    Ok(())
}

fn ensure_non_empty(name: &str, field: &str, values: &[String]) -> Result<()> {
    ensure(
        !values.is_empty() && values.iter().all(|value| !value.trim().is_empty()),
        format!(
            "BATPAK-E-AGENT-SURFACE-EMPTY: task `{name}` must have non-empty `{field}` entries"
        ),
    )
}

fn ensure_path_exists(repo_root: &Path, task: &str, rel: &str) -> Result<()> {
    let path = resolve_repo_or_core_path(repo_root, rel);
    ensure(
        path.exists(),
        format!(
            "BATPAK-E-AGENT-SURFACE-PATH: task `{task}` references missing path `{rel}` (resolved as `{}`)",
            relative(repo_root, &path)
        ),
    )
}
