use crate::repo_surface::{ensure, load_yaml, relative};
use anyhow::{bail, Context, Result};
use cargo_metadata::MetadataCommand;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
struct ArchitectureIr {
    schema_version: u32,
    generated_by: &'static str,
    packages: Vec<PackageIr>,
    traceability: TraceabilityIr,
    gates: Vec<GateIr>,
}

#[derive(Debug, Serialize)]
struct PackageIr {
    name: String,
    version: String,
    manifest_path: String,
    publishable: bool,
    targets: Vec<TargetIr>,
}

#[derive(Debug, Serialize)]
struct TargetIr {
    name: String,
    kind: Vec<String>,
    src_path: String,
}

#[derive(Debug, Serialize)]
struct TraceabilityIr {
    requirements: Vec<TraceRecordIr>,
    invariants: Vec<TraceRecordIr>,
    flows: Vec<TraceRecordIr>,
    observations: Vec<ObservationIr>,
    artifacts: Vec<ArtifactIr>,
}

#[derive(Debug, Serialize)]
struct TraceRecordIr {
    id: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ObservationIr {
    id: String,
    status: String,
    evidence: Vec<String>,
    artifacts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ArtifactIr {
    id: String,
    kind: String,
    paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GateIr {
    id: &'static str,
    command: &'static str,
    bucket: &'static str,
}

#[derive(Debug, Deserialize)]
struct RequirementRecord {
    id: String,
    #[serde(default)]
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct InvariantRecord {
    id: String,
    #[serde(default)]
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FlowRecord {
    id: String,
    #[serde(default)]
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ObservationRecord {
    id: String,
    status: String,
    evidence: Vec<String>,
    #[serde(default)]
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ArtifactRecord {
    id: String,
    kind: String,
    paths: Vec<String>,
}

pub(crate) fn run(repo_root: &Path, out: Option<PathBuf>, check: bool) -> Result<()> {
    let ir = build(repo_root)?;
    let rendered = format!("{}\n", serde_json::to_string_pretty(&ir)?);

    let Some(out) = out else {
        print!("{rendered}");
        return Ok(());
    };

    if check {
        let existing =
            fs::read_to_string(&out).with_context(|| format!("read {}", out.display()))?;
        ensure(
            existing == rendered,
            format!(
                "architecture-ir: {} is stale; rerun `cargo xtask architecture-ir --out {}`",
                out.display(),
                out.display()
            ),
        )?;
        println!("architecture-ir: {} is current", out.display());
        return Ok(());
    }

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&out, rendered).with_context(|| format!("write {}", out.display()))?;
    println!("architecture-ir: wrote {}", out.display());
    Ok(())
}

fn build(repo_root: &Path) -> Result<ArchitectureIr> {
    let mut metadata_command = MetadataCommand::new();
    metadata_command.current_dir(repo_root);
    metadata_command.no_deps();
    let metadata = metadata_command.exec().context("cargo metadata")?;
    let workspace_members = metadata.workspace_members.iter().collect::<BTreeSet<_>>();

    let mut packages = metadata
        .packages
        .iter()
        .filter(|package| workspace_members.contains(&package.id))
        .map(|package| PackageIr {
            name: package.name.to_string(),
            version: package.version.to_string(),
            manifest_path: relative(repo_root, package.manifest_path.as_std_path()),
            publishable: package
                .publish
                .as_ref()
                .is_none_or(|registries| !registries.is_empty()),
            targets: package
                .targets
                .iter()
                .map(|target| TargetIr {
                    name: target.name.to_string(),
                    kind: target.kind.iter().map(|kind| kind.to_string()).collect(),
                    src_path: relative(repo_root, target.src_path.as_std_path()),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    packages.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ArchitectureIr {
        schema_version: SCHEMA_VERSION,
        generated_by: "batpak-integrity architecture-ir",
        packages,
        traceability: load_traceability(repo_root)?,
        gates: gate_catalog(),
    })
}

fn load_traceability(repo_root: &Path) -> Result<TraceabilityIr> {
    let trace_dir = repo_root.join("traceability");
    let requirements: Vec<RequirementRecord> =
        load_yaml(&trace_dir.join("requirements.yaml")).context("requirements")?;
    let invariants: Vec<InvariantRecord> =
        load_yaml(&trace_dir.join("invariants.yaml")).context("invariants")?;
    let flows: Vec<FlowRecord> = load_yaml(&trace_dir.join("flows.yaml")).context("flows")?;
    let observations: Vec<ObservationRecord> =
        load_yaml(&trace_dir.join("observations.yaml")).context("observations")?;
    let artifacts: Vec<ArtifactRecord> =
        load_yaml(&trace_dir.join("artifacts.yaml")).context("artifacts")?;

    if requirements.is_empty()
        || invariants.is_empty()
        || flows.is_empty()
        || observations.is_empty()
        || artifacts.is_empty()
    {
        bail!("architecture-ir: traceability inputs must not be empty");
    }

    Ok(TraceabilityIr {
        requirements: requirements
            .into_iter()
            .map(|record| TraceRecordIr {
                id: record.id,
                artifacts: record.artifacts,
            })
            .collect(),
        invariants: invariants
            .into_iter()
            .map(|record| TraceRecordIr {
                id: record.id,
                artifacts: record.artifacts,
            })
            .collect(),
        flows: flows
            .into_iter()
            .map(|record| TraceRecordIr {
                id: record.id,
                artifacts: record.artifacts,
            })
            .collect(),
        observations: observations
            .into_iter()
            .map(|record| ObservationIr {
                id: record.id,
                status: record.status,
                evidence: record.evidence,
                artifacts: record.artifacts,
            })
            .collect(),
        artifacts: artifacts
            .into_iter()
            .map(|record| ArtifactIr {
                id: record.id,
                kind: record.kind,
                paths: record.paths,
            })
            .collect(),
    })
}

fn gate_catalog() -> Vec<GateIr> {
    vec![
        GateIr {
            id: "traceability-check",
            command: "cargo xtask traceability",
            bucket: "documents-as-views",
        },
        GateIr {
            id: "structural-check",
            command: "cargo xtask structural",
            bucket: "structural-constraints",
        },
        GateIr {
            id: "evidence-audit",
            command: "cargo xtask evidence-audit",
            bucket: "emitted-evidence",
        },
        GateIr {
            id: "agent-doctor",
            command: "cargo xtask agent-doctor",
            bucket: "agent-repair-surface",
        },
        GateIr {
            id: "mutants-smoke",
            command: "cargo xtask mutants smoke",
            bucket: "harness-lattice",
        },
        GateIr {
            id: "bench-surfaces",
            command: "cargo xtask bench --surface neutral|native",
            bucket: "performance-seams",
        },
    ]
}
