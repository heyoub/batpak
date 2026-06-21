//! The [`Backend`] trait: à la StoreFs/RealFs, it OBSERVES — it does NOT seal
//! or persist.
//!
//! The trait does ZERO BatPak writes and contains ZERO OS code. A backend
//! probes the machine into a RAW snapshot, derives a TYPED profile
//! deterministically, classifies requirements against that profile, and
//! executes an admitted plan into an UNSEALED report body. Sealing belongs to
//! [`crate::BoundaryRunner`]; persistence belongs to the host.

use crate::contract::capability::Enforcement;
use crate::contract::ids::BackendId;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement};
use crate::contract::report::BoundaryReportBody;
use crate::contract::support::{BackendProfile, BackendProfileSnapshot, SupportMatrix};

/// A platform boundary backend. OBSERVES only; never seals or writes BatPak.
pub trait Backend: Send + Sync {
    /// The backend's stable family id.
    fn id(&self) -> BackendId;

    /// The static family truth table.
    fn support(&self) -> &SupportMatrix;

    /// RAW probe of THIS machine. Audit/replay evidence; never admitted from
    /// directly.
    fn probe(&self) -> BackendProfileSnapshot;

    /// Derive the TYPED planning profile DETERMINISTICALLY from a raw snapshot,
    /// so replay re-derives identical admission decisions.
    fn profile(&self, snap: &BackendProfileSnapshot) -> BackendProfile;

    /// Classify a requirement against the TYPED profile (no string parsing at
    /// admission).
    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> Enforcement;

    /// Lower an admitted plan and EXECUTE it, returning the OBSERVED facts as an
    /// UNSEALED body. The backend does NOT canonicalize, hash, or touch BatPak.
    ///
    /// There is no ordinary error return: every CONTROLLED terminal is encoded
    /// in [`BoundaryReportBody::outcome`]. A host crash is NOT a controlled
    /// terminal — that path is handled by startup reconciliation.
    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody;
}
