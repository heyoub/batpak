//! Deterministic evidence report for a single projection run.

use crate::event::EventSourced;
use crate::store::projection::flow::{
    project_outcome, ProjectionCacheObservation, ProjectionObservedFreshness, ReplayInput,
};
use crate::store::{Freshness, HlcPoint, Open, Store, StoreError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Report-body schema version for projection run evidence.
pub const PROJECTION_RUN_REPORT_SCHEMA_VERSION: u16 = 1;

/// Fixed-width hash used by projection run evidence.
pub type ProjectionRunHash = [u8; 32];

/// Source reference included in a projection run report.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionSourceRef {
    /// Entity-scoped source reference.
    Entity {
        /// Entity identifier.
        entity: String,
    },
    /// Event kind admitted by the projection fold.
    RelevantKind {
        /// Event kind category.
        category: u8,
        /// Event kind type identifier.
        type_id: u16,
    },
}

/// Replay boundary mode for the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunReplayMode {
    /// Current visible replay boundary.
    Current,
}

/// Requested freshness policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunRequestedFreshness {
    /// Force current replay.
    Consistent,
    /// Allow stale reads within the provided age bound.
    MaybeStale {
        /// Maximum stale age in milliseconds.
        max_stale_ms: u64,
    },
}

/// Observed freshness status for the run result.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunFreshnessStatus {
    /// Output reflects current replay boundary.
    Fresh,
    /// Output came from stale-allowed cache semantics.
    StaleAllowed,
    /// Freshness does not apply for this run.
    NotApplicable,
    /// Freshness could not be acquired because the run failed.
    Unavailable {
        /// Availability reason.
        reason: String,
    },
}

/// Cache status observed during the run.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunCacheStatus {
    /// Cache hit path served or seeded the run.
    Hit,
    /// Cache miss path required replay.
    Miss,
    /// Cache path was bypassed.
    Bypassed,
    /// Cache observation was unavailable with deterministic reason.
    Unavailable {
        /// Availability reason.
        reason: String,
    },
}

/// Optional checkpoint reference availability for this run path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunCheckpointRef {
    /// Checkpoint does not apply to this run path.
    NotApplicable,
}

/// Output hash availability for the run.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunOutputHash {
    /// Canonical output hash is known.
    Known(ProjectionRunHash),
    /// Output hash does not apply (for example empty projection state).
    NotApplicable,
    /// Output hash is unavailable with deterministic reason.
    Unavailable {
        /// Availability reason.
        reason: String,
    },
}

/// Kind of projection input boundary recorded by this report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunFrontierKind {
    /// Replay/cache watermark selected from the visible index. Durable and
    /// process-wide applied watermarks remain available through
    /// [`crate::store::FrontierView`]; they are not the projection input
    /// boundary consumed by this run.
    Visible,
}

/// Input frontier boundary observed by the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectionRunInputFrontier {
    /// Frontier kind used as run boundary.
    pub kind: ProjectionRunFrontierKind,
    /// HLC wall milliseconds at boundary.
    pub wall_ms: u64,
    /// Global sequence at boundary.
    pub global_sequence: u64,
}

/// Structural findings emitted by projection run evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionRunFinding {
    /// Observed freshness was unavailable.
    ObservedFreshnessUnavailable,
    /// Input frontier could not be determined.
    InputFrontierUnknown,
    /// Output hash was unavailable.
    OutputHashUnavailable,
    /// Cache status was unavailable.
    CacheStatusUnavailable,
    /// Partial visibility does not apply to this run path.
    PartialVisibilityNotApplicable,
    /// Projection run failed.
    ProjectionFailed,
    /// Run served stale-allowed output.
    StaleUsed,
}

/// Deterministic report body for one projection run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionRunReportBody {
    /// Report-body schema version.
    pub schema_version: u16,
    /// Stable projection identifier.
    pub projection_id: String,
    /// Projection source references.
    pub source_refs: Vec<ProjectionSourceRef>,
    /// Replay mode used for this run.
    pub replay_mode: ProjectionRunReplayMode,
    /// Requested freshness policy.
    pub requested_freshness: ProjectionRunRequestedFreshness,
    /// Observed freshness status.
    pub observed_freshness: ProjectionRunFreshnessStatus,
    /// Input frontier boundary if known.
    pub input_frontier: Option<ProjectionRunInputFrontier>,
    /// Output hash availability.
    pub output_hash: ProjectionRunOutputHash,
    /// Cache status.
    pub cache_status: ProjectionRunCacheStatus,
    /// Checkpoint reference availability.
    pub checkpoint_ref: ProjectionRunCheckpointRef,
    /// Deterministic structural findings.
    pub findings: Vec<ProjectionRunFinding>,
}

/// Projection run evidence report envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionRunEvidenceReport {
    /// Deterministic report body.
    pub body: ProjectionRunReportBody,
    /// Canonical hash of `body`.
    pub body_hash: ProjectionRunHash,
    /// Optional generation timestamp metadata outside deterministic identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside deterministic identity.
    pub batpak_version: Option<String>,
    /// Optional diagnostics outside deterministic identity.
    pub diagnostics: Vec<String>,
}

/// Error returned when projection run evidence generation fails.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProjectionRunReportError {
    /// Canonical report-body encoding failed.
    BodyEncoding {
        /// Human-readable encoding error.
        message: String,
    },
    /// Projection execution failed; includes a deterministic report.
    ProjectionFailed {
        /// Underlying store error.
        source: StoreError,
        /// Deterministic report produced for the failed run.
        report: Box<ProjectionRunEvidenceReport>,
    },
}

impl std::fmt::Display for ProjectionRunReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyEncoding { message } => {
                write!(f, "projection run report body encoding failed: {message}")
            }
            Self::ProjectionFailed { source, .. } => {
                write!(f, "projection run failed: {source}")
            }
        }
    }
}

impl std::error::Error for ProjectionRunReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BodyEncoding { .. } => None,
            Self::ProjectionFailed { source, .. } => Some(source),
        }
    }
}

impl<State> Store<State> {
    /// Run a projection and return both materialized state and deterministic
    /// projection run evidence.
    ///
    /// # Errors
    /// Returns [`ProjectionRunReportError::BodyEncoding`] when deterministic
    /// report-body encoding fails, or [`ProjectionRunReportError::ProjectionFailed`]
    /// when the projection run fails.
    pub fn project_run_evidence<T>(
        &self,
        entity: &str,
        freshness: &Freshness,
    ) -> Result<(Option<T>, ProjectionRunEvidenceReport), ProjectionRunReportError>
    where
        T: crate::event::EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: crate::store::projection::flow::ReplayInput,
    {
        let projection_id =
            crate::store::projection::registry::ProjectionRegistry::id_for_type::<T>(entity);
        let mut source_refs = Vec::new();
        source_refs.push(ProjectionSourceRef::Entity {
            entity: entity.to_owned(),
        });
        for kind in T::relevant_event_kinds() {
            source_refs.push(ProjectionSourceRef::RelevantKind {
                category: kind.category(),
                type_id: kind.type_id(),
            });
        }
        source_refs.sort();

        let requested_freshness = map_requested_freshness(freshness);
        let replay_mode = ProjectionRunReplayMode::Current;

        let run_result = project_outcome::<T, State>(self, entity, freshness);
        match run_result {
            Ok(outcome) => {
                let observed_freshness = map_observed_freshness(outcome.observed_freshness());
                let cache_status = map_cache_status(outcome.cache_status());
                let input_frontier = outcome.input_frontier().map(map_input_frontier);
                let state = outcome.into_state();
                let output_hash = output_hash_for_state(state.as_ref());
                let checkpoint_ref = ProjectionRunCheckpointRef::NotApplicable;

                let mut findings = Vec::new();
                append_common_findings(
                    &mut findings,
                    &observed_freshness,
                    input_frontier,
                    &output_hash,
                    &cache_status,
                );
                crate::evidence::sort_findings(&mut findings);

                let report = build_report(
                    ProjectionRunReportBody {
                        schema_version: PROJECTION_RUN_REPORT_SCHEMA_VERSION,
                        projection_id,
                        source_refs,
                        replay_mode,
                        requested_freshness,
                        observed_freshness,
                        input_frontier,
                        output_hash,
                        cache_status,
                        checkpoint_ref,
                        findings,
                    },
                    Vec::new(),
                )?;
                Ok((state, report))
            }
            Err(error) => {
                let observed_freshness = ProjectionRunFreshnessStatus::Unavailable {
                    reason: "projection_failed".to_owned(),
                };
                let cache_status = ProjectionRunCacheStatus::Unavailable {
                    reason: "projection_failed".to_owned(),
                };
                let input_frontier = None;
                let output_hash = ProjectionRunOutputHash::Unavailable {
                    reason: "projection_failed".to_owned(),
                };
                let checkpoint_ref = ProjectionRunCheckpointRef::NotApplicable;
                let mut findings = vec![ProjectionRunFinding::ProjectionFailed];
                append_common_findings(
                    &mut findings,
                    &observed_freshness,
                    input_frontier,
                    &output_hash,
                    &cache_status,
                );
                crate::evidence::sort_findings(&mut findings);

                let report = build_report(
                    ProjectionRunReportBody {
                        schema_version: PROJECTION_RUN_REPORT_SCHEMA_VERSION,
                        projection_id,
                        source_refs,
                        replay_mode,
                        requested_freshness,
                        observed_freshness,
                        input_frontier,
                        output_hash,
                        cache_status,
                        checkpoint_ref,
                        findings,
                    },
                    vec![error.to_string()],
                )?;
                Err(ProjectionRunReportError::ProjectionFailed {
                    source: error,
                    report: Box::new(report),
                })
            }
        }
    }
}

fn map_input_frontier(frontier: HlcPoint) -> ProjectionRunInputFrontier {
    ProjectionRunInputFrontier {
        kind: ProjectionRunFrontierKind::Visible,
        wall_ms: frontier.wall_ms,
        global_sequence: frontier.global_sequence,
    }
}

fn map_requested_freshness(freshness: &Freshness) -> ProjectionRunRequestedFreshness {
    match freshness {
        Freshness::Consistent => ProjectionRunRequestedFreshness::Consistent,
        Freshness::MaybeStale { max_stale_ms } => ProjectionRunRequestedFreshness::MaybeStale {
            max_stale_ms: *max_stale_ms,
        },
    }
}

fn map_observed_freshness(value: ProjectionObservedFreshness) -> ProjectionRunFreshnessStatus {
    match value {
        ProjectionObservedFreshness::Fresh => ProjectionRunFreshnessStatus::Fresh,
        ProjectionObservedFreshness::StaleAllowed => ProjectionRunFreshnessStatus::StaleAllowed,
        ProjectionObservedFreshness::NotApplicable => ProjectionRunFreshnessStatus::NotApplicable,
    }
}

fn map_cache_status(value: ProjectionCacheObservation) -> ProjectionRunCacheStatus {
    match value {
        ProjectionCacheObservation::Hit => ProjectionRunCacheStatus::Hit,
        ProjectionCacheObservation::Miss => ProjectionRunCacheStatus::Miss,
        ProjectionCacheObservation::Bypassed => ProjectionRunCacheStatus::Bypassed,
        ProjectionCacheObservation::Unavailable { reason } => {
            ProjectionRunCacheStatus::Unavailable {
                reason: reason.to_owned(),
            }
        }
    }
}

fn output_hash_for_state<T: serde::Serialize>(state: Option<&T>) -> ProjectionRunOutputHash {
    let Some(value) = state else {
        return ProjectionRunOutputHash::NotApplicable;
    };
    match crate::canonical::to_bytes(value) {
        Ok(bytes) => ProjectionRunOutputHash::Known(crate::evidence::content_hash(&bytes)),
        Err(error) => ProjectionRunOutputHash::Unavailable {
            reason: error.to_string(),
        },
    }
}

fn append_common_findings(
    findings: &mut Vec<ProjectionRunFinding>,
    observed_freshness: &ProjectionRunFreshnessStatus,
    input_frontier: Option<ProjectionRunInputFrontier>,
    output_hash: &ProjectionRunOutputHash,
    cache_status: &ProjectionRunCacheStatus,
) {
    if matches!(
        observed_freshness,
        ProjectionRunFreshnessStatus::Unavailable { .. }
    ) {
        findings.push(ProjectionRunFinding::ObservedFreshnessUnavailable);
    }
    if observed_freshness == &ProjectionRunFreshnessStatus::StaleAllowed {
        findings.push(ProjectionRunFinding::StaleUsed);
    }
    if input_frontier.is_none()
        && observed_freshness != &ProjectionRunFreshnessStatus::NotApplicable
    {
        findings.push(ProjectionRunFinding::InputFrontierUnknown);
    }
    if matches!(output_hash, ProjectionRunOutputHash::Unavailable { .. }) {
        findings.push(ProjectionRunFinding::OutputHashUnavailable);
    }
    if matches!(cache_status, ProjectionRunCacheStatus::Unavailable { .. }) {
        findings.push(ProjectionRunFinding::CacheStatusUnavailable);
    }
    // Projection runs return an in-memory folded value; partial state is not
    // exposed from this path.
    findings.push(ProjectionRunFinding::PartialVisibilityNotApplicable);
}

fn build_report(
    body: ProjectionRunReportBody,
    diagnostics: Vec<String>,
) -> Result<ProjectionRunEvidenceReport, ProjectionRunReportError> {
    let body_hash = report_body_hash(&body)?;
    Ok(ProjectionRunEvidenceReport {
        body,
        body_hash,
        generated_at_unix_ms: None,
        batpak_version: None,
        diagnostics,
    })
}

fn report_body_hash(
    body: &ProjectionRunReportBody,
) -> Result<ProjectionRunHash, ProjectionRunReportError> {
    crate::evidence::report_body_hash(body, |message| ProjectionRunReportError::BodyEncoding {
        message,
    })
}

/// Type-erased runner that produces projection-run evidence for one registered
/// projection type, hiding the domain `EventSourced` type behind the
/// domain-neutral [`ProjectionRunEvidenceReport`].
type ProjectionEvidenceRunner<State> = Box<
    dyn Fn(
            &Store<State>,
            &str,
            &Freshness,
        ) -> Result<ProjectionRunEvidenceReport, ProjectionRunReportError>
        + Send
        + Sync,
>;

/// Embedder-populated dispatch from a domain-neutral projection id to a
/// type-erased [`Store::project_run_evidence`] runner.
///
/// `Store::project_run_evidence::<T>` is generic over a domain projection type
/// because running a projection *is* executing that type's fold. A
/// domain-neutral wire host (for example `hbat`) therefore cannot reconstruct
/// `T` from a request string on its own. The embedder registers each projection
/// once (`registry.register::<MyProjection>("my.projection")`); the registry
/// stores a monomorphized closure that discards the folded `T` state and yields
/// only the report. The public surface stays domain-neutral: keys are opaque
/// strings, values are [`ProjectionRunEvidenceReport`], and the domain type
/// appears solely as a generic parameter at registration time.
pub struct ProjectionEvidenceRegistry<State = Open> {
    runners: BTreeMap<String, ProjectionEvidenceRunner<State>>,
}

impl<State> ProjectionEvidenceRegistry<State> {
    /// Create an empty registry. A host with no registered projections answers
    /// every projection id with [`ProjectionEvidenceRegistry::run`] returning
    /// `None`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            runners: BTreeMap::new(),
        }
    }

    /// Register projection type `T` under a stable, domain-neutral `projection`
    /// id. Re-registering the same id replaces the prior runner.
    ///
    /// Callers satisfy the `T::Input: ReplayInput` bound structurally and need
    /// not name `ReplayInput`, which stays crate-private.
    pub fn register<T>(&mut self, projection: impl Into<String>)
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: ReplayInput,
    {
        self.runners.insert(
            projection.into(),
            Box::new(|store: &Store<State>, entity: &str, freshness: &Freshness| {
                store
                    .project_run_evidence::<T>(entity, freshness)
                    .map(|(_state, report)| report)
            }),
        );
    }

    /// Returns `true` when `projection` has a registered runner.
    #[must_use]
    pub fn contains(&self, projection: &str) -> bool {
        self.runners.contains_key(projection)
    }

    /// Run projection-run evidence for `projection` against `store`/`entity`.
    ///
    /// Returns `None` when no runner is registered under `projection` (the
    /// caller maps this to a domain-neutral "unknown projection" response);
    /// otherwise returns the runner's report result.
    pub fn run(
        &self,
        projection: &str,
        store: &Store<State>,
        entity: &str,
        freshness: &Freshness,
    ) -> Option<Result<ProjectionRunEvidenceReport, ProjectionRunReportError>> {
        self.runners
            .get(projection)
            .map(|runner| runner(store, entity, freshness))
    }

    /// Iterate the registered projection ids in sorted order.
    pub fn projection_ids(&self) -> impl Iterator<Item = &str> {
        self.runners.keys().map(String::as_str)
    }
}

impl<State> Default for ProjectionEvidenceRegistry<State> {
    fn default() -> Self {
        Self::new()
    }
}
