//! Startup reconciliation — FIRST-CLASS, not prose.
//!
//! What the NEXT `open()` concludes about a boundary whose plan was sealed but
//! whose report was not (host crash). DISTINCT from the run-time
//! [`crate::Outcome`].

use serde::{Deserialize, Serialize};

/// A reconciliation verdict for one in-flight boundary on `open()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RecoveryClassification {
    /// A terminal report was sealed; the boundary completed.
    Completed,
    /// Plan sealed, no report, no committed artifacts; rolled back + swept.
    RolledBack,
    /// Torn / contradictory 0xE state; a typed refusal, never silent repair.
    CanonicalRefusal,
}

/// One orphan (proc / fd / dir) swept during a `RolledBack` reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QuarantineRecord {
    /// Stable kind tag, e.g. `"process"`, `"fd"`, `"dir"`.
    pub kind: String,
    /// Stable identifier of the swept resource (audit evidence).
    pub reference: String,
}
