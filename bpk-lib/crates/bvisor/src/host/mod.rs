//! `bvisor::host` — mount the boundary contract as ONE hostbat module.
//!
//! This is bvisor's host wiring (kernel plan §11), feature-gated behind `host`.
//! There is **no `bvisor-host` crate**: bvisor provides exactly one mountable
//! [`HostModule`]; `hostbat` supplies the generic shell that composes it over a
//! `syncbat::Core`. Meaning stays here; the host stays generic.
//!
//! The module exposes one operation, `bvisor.boundary.run`, that realizes the
//! grammar `request → admit → execute → seal → persist`:
//! - **admit** — the [`AdmissionGuard`] runs the fail-closed [`BoundaryPlanner`].
//!   Planning IS admission (composition law 1: no effect without admission); a
//!   spec that does not plan is denied before the handler — and thus before any
//!   backend effect — runs.
//! - **execute + seal** — the handler runs the admitted plan through the
//!   [`BoundaryRunner`], which seals a [`BoundaryReport`].
//! - **persist** — the handler appends the sealed report as a durable 0xE
//!   [`BoundaryReportEvent`]. Per §10.4 the runner seals and the *host* persists;
//!   bvisor supplies that boundary-specific persistence because `hostbat` cannot
//!   know the 0xE projection.
//!
//! Identity note: the admitted plan's `plan_id` binds the admitted material today
//! via the imperative reference. Folding `H_A`/`H_L` (the validated admission
//! circuit + lowering schedule) into durable identity is the Track-A cloud seam
//! and does not change this wiring.

use std::sync::Arc;

use batpak::coordinate::Coordinate;
use batpak::store::{Open, Store};
use hostbat::{GuardDescriptor, HostError, HostModule};
use syncbat::{
    AdmissionDecision, AdmissionGuard, Ctx, EffectClass, Handler, HandlerError, HandlerResult,
    OperationDescriptor,
};

use crate::contract::events::BoundaryReportEvent;
use crate::{
    BackendId, BackendRegistry, BoundaryPlan, BoundaryPlanner, BoundaryReport, BoundaryRunner,
    BoundarySpec, PlanError,
};

/// Stable module id for the boundary module.
pub const BOUNDARY_MODULE_ID: &str = "bvisor.boundary";
/// The single boundary operation: run an admitted spec to a sealed report.
pub const BOUNDARY_RUN_OP: &str = "bvisor.boundary.run";
/// Stable guard-policy code attested in the module manifest.
pub const BOUNDARY_GUARD_CODE: &str = "bvisor.boundary.admission.v1";
/// Receipt-extension namespace the boundary module owns.
pub const BOUNDARY_RECEIPT_NAMESPACE: &str = "bvisor";

const SPEC_SCHEMA_REF: &str = "bvisor.boundary.spec.v1";
const REPORT_SCHEMA_REF: &str = "bvisor.boundary.report.v1";
const REPORT_RECEIPT_KIND: &str = "bvisor.boundary.report.v1";

/// Shared admission/execution context: the backends to plan against, the chosen
/// backend, and the store the sealed 0xE report is persisted into.
struct BoundaryContext {
    registry: Arc<BackendRegistry>,
    backend: BackendId,
    store: Arc<Store<Open>>,
    coordinate: Coordinate,
}

impl BoundaryContext {
    /// Plan `spec` against the bound backend — the one admission computation,
    /// shared by the guard (admit/deny) and the handler (re-derive to execute).
    /// Deterministic: the same spec + backend + probed profile yield the same
    /// plan, so re-deriving in the handler cannot diverge from the guard.
    fn plan(&self, spec: &BoundarySpec) -> Result<BoundaryPlan, PlanError> {
        BoundaryPlanner::new(&self.registry).plan(spec, &self.backend)
    }
}

fn decode_spec(input: &[u8]) -> Result<BoundarySpec, String> {
    batpak::canonical::from_bytes(input).map_err(|error| error.to_string())
}

fn encode_report(report: &BoundaryReport) -> Result<Vec<u8>, String> {
    batpak::canonical::to_bytes(report).map_err(|error| error.to_string())
}

/// Stable kebab-case denial class for each [`PlanError`]. Exhaustive: `PlanError`
/// is defined in this crate, so its `#[non_exhaustive]` does not force a wildcard
/// here — a new variant is a compile error until it is given a code.
fn plan_error_code(error: &PlanError) -> &'static str {
    match error {
        PlanError::Unsupported { .. } => "unsupported",
        PlanError::WorkloadIncompatible { .. } => "workload-incompatible",
        PlanError::ProfileInsufficient { .. } => "profile-insufficient",
        PlanError::BudgetInvalid { .. } => "budget-invalid",
        PlanError::EvidenceUnsatisfiable { .. } => "evidence-unsatisfiable",
        PlanError::BudgetRefused { .. } => "budget-refused",
        PlanError::UnknownBackend { .. } => "unknown-backend",
        PlanError::ShadowDivergence { .. } => "shadow-divergence",
    }
}

/// The admission guard: planning IS admission. A spec that fails to plan is
/// denied — the handler never runs, so no backend effect occurs.
struct BoundaryGuard {
    cx: Arc<BoundaryContext>,
}

impl AdmissionGuard for BoundaryGuard {
    fn admit(
        &self,
        _descriptor: &OperationDescriptor,
        input: &[u8],
        _cx: &mut Ctx<'_>,
    ) -> AdmissionDecision {
        let spec = match decode_spec(input) {
            Ok(spec) => spec,
            Err(detail) => return AdmissionDecision::deny("decode-error", detail),
        };
        match self.cx.plan(&spec) {
            Ok(_plan) => AdmissionDecision::Admit,
            Err(error) => AdmissionDecision::deny(plan_error_code(&error), error.to_string()),
        }
    }
}

/// The boundary handler: reached ONLY after admission. It re-derives the plan,
/// runs it to a sealed report, persists the 0xE report event, then returns the
/// sealed report bytes.
struct BoundaryRunHandler {
    cx: Arc<BoundaryContext>,
}

impl Handler for BoundaryRunHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        let spec = decode_spec(input).map_err(HandlerError::invalid_input)?;
        let plan = self
            .cx
            .plan(&spec)
            .map_err(|error| HandlerError::failed(error.to_string()))?;
        let report = BoundaryRunner::new(&self.cx.registry)
            .run(&plan)
            .map_err(|error| HandlerError::failed(error.to_string()))?;

        // PERSIST: the host appends the sealed report as one durable 0xE event;
        // replay reconstructs the boundary's terminal evidence from it.
        let event = BoundaryReportEvent {
            report: report.clone(),
        };
        let _receipt = self
            .cx
            .store
            .append_typed(&self.cx.coordinate, &event)
            .map_err(|error| HandlerError::failed(format!("persist boundary report: {error}")))?;

        encode_report(&report).map_err(HandlerError::failed)
    }
}

/// How to mount the boundary as a host module.
pub struct BoundaryModuleConfig {
    /// The backends available to plan and run against.
    pub registry: Arc<BackendRegistry>,
    /// The backend the boundary is planned + run against.
    pub backend: BackendId,
    /// The store the sealed 0xE report is persisted into.
    pub store: Arc<Store<Open>>,
    /// The coordinate the report event is appended under.
    pub coordinate: Coordinate,
}

/// Build the bvisor boundary as a content-identified [`HostModule`]: one
/// `bvisor.boundary.run` operation, guarded by the planner and handled by the
/// runner + persist step. Mount it on a [`hostbat::HostBuilder`] to get a
/// runnable host.
///
/// # Errors
/// [`HostError`] if the module fails internal coherence (unreachable for this
/// fixed shape) or manifest sealing fails.
pub fn boundary_module(config: BoundaryModuleConfig) -> Result<HostModule, HostError> {
    let cx = Arc::new(BoundaryContext {
        registry: config.registry,
        backend: config.backend,
        store: config.store,
        coordinate: config.coordinate,
    });
    let descriptor = OperationDescriptor::new(
        BOUNDARY_RUN_OP,
        EffectClass::Emit,
        SPEC_SCHEMA_REF,
        REPORT_SCHEMA_REF,
        REPORT_RECEIPT_KIND,
    );
    HostModule::builder(BOUNDARY_MODULE_ID, 1)
        .operation(
            descriptor,
            BoundaryRunHandler {
                cx: Arc::clone(&cx),
            },
        )?
        .guard(
            GuardDescriptor::new(BOUNDARY_GUARD_CODE),
            BoundaryGuard { cx },
        )?
        .receipt_namespace(BOUNDARY_RECEIPT_NAMESPACE)?
        .build()
}

/// End-to-end proof of the full path `request → admit → execute → seal → persist`
/// over a real `hostbat::Host` and a real batpak `Store`. Needs the honest
/// `SimBackend` (the positive reference), so the module is gated on
/// `dangerous-test-hooks` in addition to `host`. The H_A/H_L identity binding is
/// the Track-A cloud seam and does not gate this path.
#[cfg(all(test, feature = "dangerous-test-hooks"))]
mod e2e_tests {
    use super::{boundary_module, BoundaryModuleConfig, BOUNDARY_RUN_OP};
    use crate::contract::budget::{BudgetRequirements, MinGuarantee};
    use crate::contract::host_control::HostControl;
    use crate::contract::plan::{EvidenceRequirements, Workload};
    use crate::sim::backend::{LieMode, OneShotLiar, SimBackend};
    use crate::{BackendId, BackendRegistry, BoundaryReport, BoundarySpec, InertBackend};
    use batpak::coordinate::Coordinate;
    use batpak::store::{Open, Store, StoreConfig};
    use hostbat::{Host, HostBuilder};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// A coherent budgeted launch the honest Sim admits and the Inert floor
    /// refuses — the same shape the sim supervisor tests plan.
    fn valid_spec() -> BoundarySpec {
        BoundarySpec {
            workload: Workload::Process {
                exe: "true".to_owned(),
                args: Vec::new(),
            },
            capabilities: Vec::new(),
            controls: vec![HostControl::LaunchWorkload],
            budgets: BudgetRequirements::uniform(64, MinGuarantee::Mediated),
            evidence: EvidenceRequirements::default(),
        }
    }

    fn open_store(dir: &TempDir) -> Arc<Store<Open>> {
        Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open store"))
    }

    fn report_coordinate() -> Coordinate {
        Coordinate::new("bvisor:boundary", "bvisor:report").expect("coordinate")
    }

    fn host_over(registry: BackendRegistry, backend: BackendId, store: Arc<Store<Open>>) -> Host {
        let module = boundary_module(BoundaryModuleConfig {
            registry: Arc::new(registry),
            backend,
            store,
            coordinate: report_coordinate(),
        })
        .expect("boundary module builds");
        HostBuilder::new()
            .mount(module)
            .expect("mount")
            .build()
            .expect("build host")
    }

    #[test]
    fn honest_sim_admits_runs_seals_and_persists() {
        let mut registry = BackendRegistry::new();
        registry.register(Arc::new(SimBackend::new(Box::new(OneShotLiar::new(
            LieMode::Honest,
        )))));
        let backend = BackendId::new(SimBackend::ID);
        let dir = TempDir::new().expect("tempdir");
        let store = open_store(&dir);
        let before = store.stats().event_count;
        let mut host = host_over(registry, backend, Arc::clone(&store));

        let spec_bytes = batpak::canonical::to_bytes(&valid_spec()).expect("encode spec");
        let result = host
            .invoke(BOUNDARY_RUN_OP, spec_bytes)
            .expect("an honest-sim spec is admitted, run, and sealed");

        let report: BoundaryReport =
            batpak::canonical::from_bytes(result.output()).expect("the output is a sealed report");
        assert_ne!(
            report.body_hash.0, [0u8; 32],
            "the returned report is sealed (body_hash computed over the body)",
        );
        assert_eq!(
            store.stats().event_count,
            before + 1,
            "the sealed report persisted as exactly one durable 0xE event",
        );
    }

    #[test]
    fn inert_backend_denies_at_admission_with_no_effect_and_no_persist() {
        let mut registry = BackendRegistry::new();
        registry.register(Arc::new(InertBackend::new()));
        let backend = BackendId::new(InertBackend::ID);
        let dir = TempDir::new().expect("tempdir");
        let store = open_store(&dir);
        let before = store.stats().event_count;
        let mut host = host_over(registry, backend, Arc::clone(&store));

        let spec_bytes = batpak::canonical::to_bytes(&valid_spec()).expect("encode spec");
        let result = host.invoke(BOUNDARY_RUN_OP, spec_bytes);
        assert!(
            result.is_err(),
            "the all-Unsupported Inert floor refuses every budgeted spec → the guard \
             denies before the handler (and any backend effect) runs",
        );
        assert_eq!(
            store.stats().event_count,
            before,
            "a denied boundary persists nothing — no effect without admission",
        );
    }
}
