//! F2 platform matrix — fail-closed refusal proofs for scaffolding backends and
//! linux unimplemented cells. The family `support_matrix()` may advertise
//! `Enforced`, but an empty machine ceiling floors admission to `Unsupported`, so
//! `BoundaryPlanner::plan` refuses before any execution.

use bvisor::ScaffoldingBackend;
use bvisor::{
    Backend, BackendId, BackendRegistry, BoundaryPlanner, BoundarySpec, BudgetRequirements,
    Capability, Enforcement, EvidenceRequirements, FsAccess, FsConfinement, HostControl, PathSet,
    PlanError, RequirementKind, Workload,
};
use std::sync::Arc;

fn trivial_workload() -> Workload {
    Workload::Process {
        exe: "true".to_string(),
        args: Vec::new(),
    }
}

fn spec_for_kind(kind: RequirementKind) -> Result<BoundarySpec, String> {
    use bvisor::{
        CommitDurability, EnvPolicy, FdPolicy, KillGuarantee, KillTarget, NetDest, NetPolicy,
        PathView, SpawnPolicy, StdStreams,
    };

    let (capabilities, controls) = match kind {
        RequirementKind::Filesystem => (
            vec![Capability::Filesystem {
                access: FsAccess::Read,
                scope: PathSet::empty(),
                recursive: true,
                confinement: FsConfinement::DeclaredRootsOnly,
            }],
            vec![],
        ),
        RequirementKind::NetworkDenyAll => (
            vec![Capability::Network {
                policy: NetPolicy::DenyAll,
            }],
            vec![],
        ),
        RequirementKind::NetworkAllowList => (
            vec![Capability::Network {
                policy: NetPolicy::AllowList(vec![NetDest {
                    host: "example".to_string(),
                    port: 443,
                }]),
            }],
            vec![],
        ),
        RequirementKind::ChildSpawnDenyNewTasks => (
            vec![Capability::ChildSpawn {
                policy: SpawnPolicy::DenyNewTasks,
            }],
            vec![],
        ),
        RequirementKind::ChildSpawnAllowThreads => (
            vec![Capability::ChildSpawn {
                policy: SpawnPolicy::AllowThreadsWithinBoundary,
            }],
            vec![],
        ),
        RequirementKind::ChildSpawnAllowDescendants => (
            vec![Capability::ChildSpawn {
                policy: SpawnPolicy::AllowDescendantsWithinBoundary,
            }],
            vec![],
        ),
        RequirementKind::Environment => (
            vec![Capability::Environment {
                policy: EnvPolicy::Exact(Vec::new()),
            }],
            vec![],
        ),
        RequirementKind::InheritedFdsNone => (
            vec![Capability::InheritedFds {
                policy: FdPolicy::None,
            }],
            vec![],
        ),
        RequirementKind::InheritedFdsOnly => (
            vec![Capability::InheritedFds {
                policy: FdPolicy::Only(vec![3]),
            }],
            vec![],
        ),
        RequirementKind::LaunchWorkload => (vec![], vec![HostControl::LaunchWorkload]),
        RequirementKind::CaptureStreams => (
            vec![],
            vec![HostControl::CaptureStreams {
                streams: StdStreams::capture_out_err(),
            }],
        ),
        RequirementKind::TempRoot => (
            vec![],
            vec![HostControl::TempRoot {
                visibility: PathView::PrivateToBoundary,
            }],
        ),
        RequirementKind::ExposePath => (
            vec![],
            vec![HostControl::ExposePath {
                source: String::new(),
                dest: String::new(),
                access: FsAccess::Read,
                view: PathView::PrivateToBoundary,
            }],
        ),
        RequirementKind::CommitArtifact => (
            vec![],
            vec![HostControl::CommitArtifact {
                durability: CommitDurability::Atomic,
            }],
        ),
        RequirementKind::DiscardArtifact => (vec![], vec![HostControl::DiscardArtifact]),
        RequirementKind::Kill => (
            vec![],
            vec![HostControl::Kill {
                target: KillTarget::RunTree,
                guarantee: KillGuarantee::Atomic,
            }],
        ),
        RequirementKind::ListOutputs => (vec![], vec![HostControl::ListOutputs]),
        _ => {
            return Err(format!("PROPERTY: spec_for_kind does not cover {kind:?}"));
        }
    };

    Ok(BoundarySpec {
        workload: trivial_workload(),
        capabilities,
        controls,
        budgets: BudgetRequirements::deny_all(),
        evidence: EvidenceRequirements::default(),
    })
}

fn registry_with(backend: Arc<dyn Backend>) -> BackendRegistry {
    let mut registry = BackendRegistry::new();
    registry.register(backend);
    registry
}

fn assert_plan_refuses(
    planner: &BoundaryPlanner<'_>,
    spec: &BoundarySpec,
    backend_id: &BackendId,
    kind: RequirementKind,
) -> Result<(), String> {
    match planner.plan(spec, backend_id) {
        Ok(_) => Err(format!(
            "PROPERTY: {kind:?} must fail closed on {backend_id:?} (empty ceiling), but plan admitted"
        )),
        Err(PlanError::Unsupported { backend, .. }) if backend == *backend_id => Ok(()),
        Err(err) => Err(format!(
            "PROPERTY: {kind:?} on {backend_id:?} must refuse with Unsupported, got {err:?}"
        )),
    }
}

fn scaffolding_refuses_representative_enforced_kinds(
    backend: &Arc<dyn Backend>,
    kinds: &[RequirementKind],
) -> Result<(), String> {
    let id = backend.id();
    let registry = registry_with(Arc::clone(backend));
    let planner = BoundaryPlanner::new(&registry);
    for &kind in kinds {
        assert!(
            backend.support().best_case_for(kind).enforcement == Enforcement::Enforced,
            "PROPERTY: {kind:?} must be Enforced in the family matrix for this refusal proof"
        );
        assert_plan_refuses(&planner, &spec_for_kind(kind)?, &id, kind)?;
    }
    Ok(())
}

const WINDOWS_REPRESENTATIVE: &[RequirementKind] = &[
    RequirementKind::Filesystem,
    RequirementKind::LaunchWorkload,
    RequirementKind::Environment,
];

const MACOS_REPRESENTATIVE: &[RequirementKind] = &[
    RequirementKind::LaunchWorkload,
    RequirementKind::Environment,
    RequirementKind::TempRoot,
];

const WASM_REPRESENTATIVE: &[RequirementKind] = &[
    RequirementKind::Filesystem,
    RequirementKind::LaunchWorkload,
    RequirementKind::NetworkDenyAll,
];

#[test]
fn windows_scaffolding_refuses_representative_enforced_kinds() -> Result<(), String> {
    let backend = Arc::new(ScaffoldingBackend::windows()) as Arc<dyn Backend>;
    scaffolding_refuses_representative_enforced_kinds(&backend, WINDOWS_REPRESENTATIVE)
}

#[test]
fn macos_scaffolding_refuses_representative_enforced_kinds() -> Result<(), String> {
    let backend = Arc::new(ScaffoldingBackend::macos()) as Arc<dyn Backend>;
    scaffolding_refuses_representative_enforced_kinds(&backend, MACOS_REPRESENTATIVE)
}

#[test]
fn wasm_scaffolding_refuses_representative_enforced_kinds() -> Result<(), String> {
    let backend = Arc::new(ScaffoldingBackend::wasm()) as Arc<dyn Backend>;
    scaffolding_refuses_representative_enforced_kinds(&backend, WASM_REPRESENTATIVE)
}
