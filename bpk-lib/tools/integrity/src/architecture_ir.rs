use crate::repo_surface::{ensure, load_yaml, relative, resolve_repo_or_core_path};
use crate::source_cache::SourceCache;
use crate::store_pub_fn_coverage;
use anyhow::{bail, Context, Result};
use cargo_metadata::{Metadata, MetadataCommand};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Serialize)]
struct ArchitectureIr {
    schema_version: u32,
    generated_by: &'static str,
    packages: Vec<PackageIr>,
    traceability: TraceabilityIr,
    gates: Vec<GateIr>,
    platform_allowlist: PlatformAllowlistIr,
    benches: BenchIr,
    store_pub_fns: Vec<StorePubFnIr>,
    invariant_gate_links: Vec<InvariantGateLinkIr>,
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

#[derive(Debug, Serialize)]
struct PlatformAllowlistIr {
    source: String,
    direct_fs_needles: Vec<String>,
    allowed_direct_fs_contacts: Vec<PlatformContactIr>,
}

#[derive(Debug, Serialize)]
struct PlatformContactIr {
    path: String,
    needle: String,
    allowed_count: usize,
}

#[derive(Debug, Serialize)]
struct BenchIr {
    cargo_targets: Vec<BenchTargetIr>,
    xtask_surfaces: Vec<BenchSurfaceIr>,
    family_targets: Vec<FamilyBenchIr>,
}

#[derive(Debug, Serialize)]
struct BenchTargetIr {
    package: String,
    name: String,
    src_path: String,
}

#[derive(Debug, Serialize)]
struct BenchSurfaceIr {
    surface: String,
    targets: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FamilyBenchIr {
    package: String,
    targets: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StorePubFnIr {
    name: String,
    covered: bool,
    allowlisted: bool,
}

#[derive(Debug, Serialize)]
struct InvariantGateLinkIr {
    id: String,
    direct_test_artifact: bool,
    testing_ledger_entry: bool,
    waiver: bool,
    gate_links: Vec<&'static str>,
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

#[derive(Debug, Deserialize)]
struct WaiverNameRecord {
    name: String,
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

    let traceability = load_traceability(repo_root)?;
    let invariant_gate_links = load_invariant_gate_links(repo_root, &traceability)?;
    let mut source_cache = SourceCache::new(repo_root);

    Ok(ArchitectureIr {
        schema_version: SCHEMA_VERSION,
        generated_by: "batpak-integrity architecture-ir",
        packages,
        traceability,
        gates: gate_catalog(),
        platform_allowlist: load_platform_allowlist(repo_root)?,
        benches: load_benches(repo_root, &metadata)?,
        store_pub_fns: load_store_pub_fns(repo_root, &mut source_cache)?,
        invariant_gate_links,
    })
}

fn load_store_pub_fns(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<Vec<StorePubFnIr>> {
    Ok(store_pub_fn_coverage::inventory(repo_root, source_cache)?
        .into_iter()
        .map(|entry| StorePubFnIr {
            name: entry.name,
            covered: entry.covered,
            allowlisted: entry.allowlisted,
        })
        .collect())
}

fn load_platform_allowlist(repo_root: &Path) -> Result<PlatformAllowlistIr> {
    let source_path = repo_root.join("tools/integrity/src/architecture_lints/platform_boundary.rs");
    let source = fs::read_to_string(&source_path)
        .with_context(|| format!("read {}", source_path.display()))?;
    Ok(PlatformAllowlistIr {
        source: relative(repo_root, &source_path),
        direct_fs_needles: parse_string_list_const(&source, "DIRECT_FS_NEEDLES")?,
        allowed_direct_fs_contacts: parse_platform_contacts(&source)?,
    })
}

fn load_benches(repo_root: &Path, metadata: &Metadata) -> Result<BenchIr> {
    let mut cargo_targets = metadata
        .packages
        .iter()
        .flat_map(|package| {
            package
                .targets
                .iter()
                .filter(|target| target.kind.iter().any(|kind| kind.to_string() == "bench"))
                .map(|target| BenchTargetIr {
                    package: package.name.to_string(),
                    name: target.name.to_string(),
                    src_path: relative(repo_root, target.src_path.as_std_path()),
                })
        })
        .collect::<Vec<_>>();
    cargo_targets.sort_by(|a, b| a.package.cmp(&b.package).then_with(|| a.name.cmp(&b.name)));

    let bench_source_path = repo_root.join("tools/xtask/src/bench.rs");
    let bench_source = fs::read_to_string(&bench_source_path)
        .with_context(|| format!("read {}", bench_source_path.display()))?;
    Ok(BenchIr {
        cargo_targets,
        xtask_surfaces: vec![
            BenchSurfaceIr {
                surface: "neutral".to_owned(),
                targets: parse_bench_surface_targets(&bench_source, "Neutral")?,
            },
            BenchSurfaceIr {
                surface: "native".to_owned(),
                targets: parse_bench_surface_targets(&bench_source, "Native")?,
            },
        ],
        family_targets: parse_family_bench_targets(&bench_source)?,
    })
}

fn load_invariant_gate_links(
    repo_root: &Path,
    traceability: &TraceabilityIr,
) -> Result<Vec<InvariantGateLinkIr>> {
    let artifact_map = traceability
        .artifacts
        .iter()
        .map(|artifact| (artifact.id.as_str(), artifact))
        .collect::<BTreeMap<_, _>>();
    let ledger_citations = ledger_invariant_citations(repo_root)?;
    let waiver_names = load_invariant_waiver_names(repo_root)?;

    Ok(traceability
        .invariants
        .iter()
        .map(|invariant| {
            let direct_test_artifact = invariant.artifacts.iter().any(|artifact_id| {
                artifact_map
                    .get(artifact_id.as_str())
                    .is_some_and(|artifact| {
                        artifact
                            .paths
                            .iter()
                            .any(|path| is_test_artifact(repo_root, path))
                    })
            });
            let testing_ledger_entry = ledger_citations.contains(&invariant.id);
            let waiver = waives_invariant(&waiver_names, &invariant.id);
            let mut gate_links = vec!["traceability-check", "structural-check:invariant-bridge"];
            if direct_test_artifact {
                gate_links.push("structural-check:doctrine-header-anchors");
            }
            InvariantGateLinkIr {
                id: invariant.id.clone(),
                direct_test_artifact,
                testing_ledger_entry,
                waiver,
                gate_links,
            }
        })
        .collect())
}

fn ledger_invariant_citations(repo_root: &Path) -> Result<BTreeSet<String>> {
    let mut citations = BTreeSet::new();
    for entry in crate::harness_lints::load_ledger_citations(repo_root)? {
        citations.extend(entry.invariants);
    }
    Ok(citations)
}

fn load_invariant_waiver_names(repo_root: &Path) -> Result<BTreeSet<String>> {
    let path = repo_root
        .join("traceability")
        .join("invariant_citation_waivers.yaml");
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
    let records: Vec<WaiverNameRecord> =
        load_yaml(&path).with_context(|| format!("parse {}", path.display()))?;
    Ok(records.into_iter().map(|record| record.name).collect())
}

fn waives_invariant(waiver_names: &BTreeSet<String>, invariant: &str) -> bool {
    let prefix = format!("{invariant}:");
    waiver_names
        .iter()
        .any(|name| name == invariant || name.starts_with(&prefix))
}

fn is_test_artifact(repo_root: &Path, path: &str) -> bool {
    if path.starts_with("crates/core/tests") {
        return true;
    }
    let resolved = resolve_repo_or_core_path(repo_root, path);
    resolved
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "rs")
        && path.contains("/tests/")
}

fn parse_string_list_const(source: &str, const_name: &str) -> Result<Vec<String>> {
    let body = const_array_body(source, const_name)?;
    parse_string_literals(body)
}

fn parse_platform_contacts(source: &str) -> Result<Vec<PlatformContactIr>> {
    let body = const_array_body(source, "ALLOWED_DIRECT_FS_CONTACTS")?;
    let tuple_re = Regex::new(r#"\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*([0-9]+)\s*,?\s*\)"#)?;
    let mut contacts = Vec::new();
    for capture in tuple_re.captures_iter(body) {
        let allowed_count = capture[3]
            .parse::<usize>()
            .with_context(|| format!("parse platform allowlist count `{}`", &capture[3]))?;
        contacts.push(PlatformContactIr {
            path: capture[1].to_owned(),
            needle: capture[2].to_owned(),
            allowed_count,
        });
    }
    ensure(
        !contacts.is_empty(),
        "architecture-ir: platform allowlist projection is empty",
    )?;
    Ok(contacts)
}

fn parse_bench_surface_targets(source: &str, surface: &str) -> Result<Vec<String>> {
    let marker = format!("BenchSurface::{surface} => &[");
    let body = bracketed_body_after_marker(source, &marker)
        .with_context(|| format!("architecture-ir: missing bench surface {surface}"))?;
    parse_string_literals(body)
}

fn parse_family_bench_targets(source: &str) -> Result<Vec<FamilyBenchIr>> {
    let body = const_array_body(source, "FAMILY_BENCH_TARGETS")?;
    let tuple_re = Regex::new(r#"\(\s*"([^"]+)"\s*,\s*&\[(.*?)\]\s*\)"#)?;
    let mut targets = Vec::new();
    for capture in tuple_re.captures_iter(body) {
        targets.push(FamilyBenchIr {
            package: capture[1].to_owned(),
            targets: parse_string_literals(&capture[2])?,
        });
    }
    ensure(
        !targets.is_empty(),
        "architecture-ir: family bench projection is empty",
    )?;
    Ok(targets)
}

fn const_array_body<'a>(source: &'a str, const_name: &str) -> Result<&'a str> {
    let marker = format!("const {const_name}:");
    let start = source
        .find(&marker)
        .with_context(|| format!("architecture-ir: missing const {const_name}"))?;
    let after_marker = &source[start..];
    let body_start = after_marker
        .find("&[")
        .with_context(|| format!("architecture-ir: missing array body for const {const_name}"))?
        + 2;
    let after_body = &after_marker[body_start..];
    let body_end = after_body.find("];").with_context(|| {
        format!("architecture-ir: missing array terminator for const {const_name}")
    })?;
    Ok(&after_body[..body_end])
}

fn bracketed_body_after_marker<'a>(source: &'a str, marker: &str) -> Option<&'a str> {
    let start = source.find(marker)?;
    let body_start = start + marker.len();
    let after_body = &source[body_start..];
    let body_end = after_body.find("]")?;
    Some(&after_body[..body_end])
}

fn parse_string_literals(source: &str) -> Result<Vec<String>> {
    let string_re = Regex::new(r#""([^"]+)""#)?;
    Ok(string_re
        .captures_iter(source)
        .map(|capture| capture[1].to_owned())
        .collect())
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

#[cfg(test)]
mod tests {
    use super::{
        parse_bench_surface_targets, parse_family_bench_targets, parse_platform_contacts,
        parse_string_list_const,
    };
    use anyhow::Result;

    #[test]
    fn platform_projection_reads_needles_and_allowlist_rows() -> Result<()> {
        let source = r#"
const DIRECT_FS_NEEDLES: &[&str] = &[
    "std::fs::read_dir",
    "File::open",
];
const ALLOWED_DIRECT_FS_CONTACTS: &[(&str, &str, usize)] = &[
    (
        "crates/core/src/store/lifecycle.rs",
        "std::fs::read_dir",
        3,
    ),
    ("crates/core/src/store/open.rs", "std::fs::write(", 2),
];
"#;

        let needles = parse_string_list_const(source, "DIRECT_FS_NEEDLES")?;
        assert_eq!(needles, vec!["std::fs::read_dir", "File::open"]);

        let contacts = parse_platform_contacts(source)?;
        assert_eq!(contacts.len(), 2);
        assert_eq!(contacts[0].path, "crates/core/src/store/lifecycle.rs");
        assert_eq!(contacts[0].needle, "std::fs::read_dir");
        assert_eq!(contacts[0].allowed_count, 3);
        assert_eq!(contacts[1].path, "crates/core/src/store/open.rs");
        assert_eq!(contacts[1].needle, "std::fs::write(");
        assert_eq!(contacts[1].allowed_count, 2);
        Ok(())
    }

    #[test]
    fn bench_projection_reads_surface_and_family_targets() -> Result<()> {
        let source = r#"
pub(crate) fn bench_targets(surface: BenchSurface) -> &'static [&'static str] {
    match surface {
        BenchSurface::Neutral => &[
            "cold_start",
            "write_throughput",
        ],
        BenchSurface::Native => &[
            "recovery_lanes",
            "writer_coordinate_churn",
        ],
    }
}

const FAMILY_BENCH_TARGETS: &[(&str, &[&str])] = &[
    ("syncbat", &["dispatch"]),
    ("hbat", &["live_operations"]),
];
"#;

        assert_eq!(
            parse_bench_surface_targets(source, "Neutral")?,
            vec!["cold_start", "write_throughput"]
        );
        assert_eq!(
            parse_bench_surface_targets(source, "Native")?,
            vec!["recovery_lanes", "writer_coordinate_churn"]
        );

        let family = parse_family_bench_targets(source)?;
        assert_eq!(family.len(), 2);
        assert_eq!(family[0].package, "syncbat");
        assert_eq!(family[0].targets, vec!["dispatch"]);
        assert_eq!(family[1].package, "hbat");
        assert_eq!(family[1].targets, vec!["live_operations"]);
        Ok(())
    }
}
