//! [`GroundTruth`] — the harness-owned shadow oracle.
//!
//! THE MONSTER NEVER GRADES ITSELF. This shadow records what ACTUALLY happened
//! during a simulated boundary run — bytes served, sockets opened, live PIDs,
//! paths written, the committed set, fd reachability — INDEPENDENTLY of the
//! backend's self-reported [`BoundaryReport`]. The oracle ([`GroundTruth::diff`])
//! then compares the two. This is the same separation batpak's recovery matrix
//! enforces by REOPENING a real store and classifying independently
//! (`crates/core/src/store/sim/recovery_matrix.rs`): the thing under test does
//! not get to certify its own safety.
//!
//! How it stays independent (the key to the whole grid):
//! - The [`SimBackend`](crate::__sim::SimBackend) writes the REAL effects it
//!   performed into the GroundTruth (a `&mut` handle the harness owns and reads),
//!   and SEPARATELY constructs a [`BoundaryReportBody`] that may LIE about them.
//! - The GroundTruth is never derived FROM the report, and the report is never
//!   derived FROM the GroundTruth. The diff is between two independently produced
//!   records. A lie is exactly a divergence the diff surfaces.

use crate::contract::plan::BoundaryRequirement;
use crate::contract::report::{BoundaryReportBody, Outcome};
use std::collections::BTreeSet;
use std::fmt;

/// The catalogue of lies the monster can tell, each mapped to the Gn that
/// catches it. A [`SimBackend`](crate::__sim::SimBackend) in a given
/// [`LieMode`](crate::__sim::LieMode) injects one of these; the oracle's job is
/// to surface it as a [`GroundTruthDiff`] mismatch.
///
/// INVERSION RULE (from the recovery matrix): a backend may DENY MORE than asked
/// (fail-closed is always legal — denying an attempt that GroundTruth never
/// recorded is fine), but it may NEVER REPORT LESS DANGER THAN OCCURRED.
/// `DropOrphanFromReport`, `DropDeniedAttempt`, and `AutoCommitButReportFalse`
/// are "resurrect hidden danger" lies, illegal in EVERY mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lie {
    /// G1 — claimed read denied/enforced, but actually served secret bytes.
    ClaimEnforcedButAllowRead,
    /// G2 — claimed net denied/enforced, but actually opened a socket.
    ClaimEnforcedButAllowNet,
    /// G3 — a write landed OUTSIDE the quarantine root.
    WriteEscapesQuarantine,
    /// G4 — spawned a child despite a `ChildSpawn::Deny` policy.
    SpawnDespiteDeny,
    /// G5 — left an orphan PID live but omitted it from the report.
    DropOrphanFromReport,
    /// G6 — an inherited host fd leaked into a proxy/child.
    ProxyInheritedFd,
    /// G7 — auto-promoted an artifact out of quarantine but hid it from the report.
    AutoCommitButReportFalse,
    /// G8 — skipped sealing (a terminal outcome with no honest body).
    SkipSealing,
    /// G9 — a denied attempt actually occurred but was dropped from the report.
    DropDeniedAttempt,
    /// G10 — misreported enforcement DEPTH (claimed deeper than performed).
    MisreportEnforcementDepth,
    /// G11 — crashed mid-boundary (no terminal report).
    CrashMidBoundary,
}

impl Lie {
    /// The proof-grid gate id (`"G1"`..`"G13"`) that catches this lie.
    #[must_use]
    pub fn gate(self) -> &'static str {
        match self {
            Lie::ClaimEnforcedButAllowRead => "G1",
            Lie::ClaimEnforcedButAllowNet => "G2",
            Lie::WriteEscapesQuarantine => "G3",
            Lie::SpawnDespiteDeny => "G4",
            Lie::DropOrphanFromReport => "G5",
            Lie::ProxyInheritedFd => "G6",
            Lie::AutoCommitButReportFalse => "G7",
            Lie::SkipSealing => "G8",
            Lie::DropDeniedAttempt => "G9",
            Lie::MisreportEnforcementDepth => "G10",
            Lie::CrashMidBoundary => "G11",
        }
    }

    /// Whether this lie hides danger that GroundTruth recorded as having
    /// occurred (vs merely failing closed). Hiding danger is illegal in EVERY
    /// mode; failing closed is always legal.
    #[must_use]
    pub fn hides_danger(self) -> bool {
        // Every lie in the catalogue hides danger the GroundTruth saw — that is
        // what makes it a lie rather than a legal fail-closed denial. Listed
        // exhaustively (no wildcard) so a future variant must declare itself.
        match self {
            Lie::ClaimEnforcedButAllowRead
            | Lie::ClaimEnforcedButAllowNet
            | Lie::WriteEscapesQuarantine
            | Lie::SpawnDespiteDeny
            | Lie::DropOrphanFromReport
            | Lie::ProxyInheritedFd
            | Lie::AutoCommitButReportFalse
            | Lie::SkipSealing
            | Lie::DropDeniedAttempt
            | Lie::MisreportEnforcementDepth
            | Lie::CrashMidBoundary => true,
        }
    }
}

/// The harness-owned record of what ACTUALLY happened during a simulated run.
///
/// Each field is the INDEPENDENT truth; the backend's report is compared against
/// it, never the other way round. Sorted collections so the record is canonical
/// and replay-stable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroundTruth {
    /// Secret byte-spans actually served to the workload (e.g. `"secret.key"`).
    served_secret_bytes: BTreeSet<String>,
    /// Network destinations a socket was actually opened to.
    opened_sockets: BTreeSet<String>,
    /// PIDs still live when the boundary terminated (orphans if not torn down).
    live_pids: BTreeSet<u64>,
    /// Paths actually written OUTSIDE the declared quarantine root.
    writes_outside_quarantine: BTreeSet<String>,
    /// Children actually spawned by the workload.
    spawned_children: BTreeSet<u64>,
    /// Host fds that actually reached a child/proxy.
    reachable_inherited_fds: BTreeSet<u32>,
    /// Artifacts the backend actually committed out of quarantine.
    committed_artifacts: BTreeSet<String>,
    /// Attempts the boundary actually blocked (and so MUST appear in `denied`).
    actually_denied: BTreeSet<String>,
    /// Whether the run actually reached a terminal (vs crashed mid-boundary).
    reached_terminal: bool,
    /// The deepest enforcement actually performed, as an honest mechanism tag.
    actual_enforcement_depth: Option<String>,
}

impl GroundTruth {
    /// A fresh, empty truth record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the backend actually served secret bytes to the workload.
    pub fn served_secret(&mut self, label: impl Into<String>) {
        self.served_secret_bytes.insert(label.into());
    }

    /// Record that the backend actually opened a socket to `dest`.
    pub fn opened_socket(&mut self, dest: impl Into<String>) {
        self.opened_sockets.insert(dest.into());
    }

    /// Record a still-live PID at boundary termination.
    pub fn live_pid(&mut self, pid: u64) {
        self.live_pids.insert(pid);
    }

    /// Record a write that landed outside the quarantine root.
    pub fn wrote_outside_quarantine(&mut self, path: impl Into<String>) {
        self.writes_outside_quarantine.insert(path.into());
    }

    /// Record a child the workload actually spawned.
    pub fn spawned_child(&mut self, pid: u64) {
        self.spawned_children.insert(pid);
    }

    /// Record a host fd that actually reached a child/proxy.
    pub fn inherited_fd_reachable(&mut self, fd: u32) {
        self.reachable_inherited_fds.insert(fd);
    }

    /// Record an artifact the backend actually committed.
    pub fn committed_artifact(&mut self, name: impl Into<String>) {
        self.committed_artifacts.insert(name.into());
    }

    /// Record an attempt the boundary actually blocked.
    pub fn denied_attempt(&mut self, label: impl Into<String>) {
        self.actually_denied.insert(label.into());
    }

    /// Record that the run reached a terminal (a report MUST be sealed).
    pub fn reached_terminal(&mut self) {
        self.reached_terminal = true;
    }

    /// Record the deepest enforcement actually performed.
    pub fn enforcement_depth(&mut self, mechanism: impl Into<String>) {
        self.actual_enforcement_depth = Some(mechanism.into());
    }

    /// The independent oracle: diff this GroundTruth against the backend's
    /// self-reported body (and, where sealing matters, whether a report was
    /// sealed at all). Returns the set of lies the report tried to hide.
    ///
    /// `sealed` is whether the runner produced a sealed [`crate::BoundaryReport`]
    /// at all — the [`Lie::SkipSealing`] / [`Lie::CrashMidBoundary`] axis, which
    /// is invisible inside the body itself.
    #[must_use]
    pub fn diff(&self, report: Option<&BoundaryReportBody>, sealed: bool) -> GroundTruthDiff {
        let mut caught = BTreeSet::new();

        // G8/G11: a run that reached a terminal MUST seal a report. No body, or
        // an unsealed terminal, hides the most danger of all.
        if self.reached_terminal && (!sealed || report.is_none()) {
            caught.insert(Lie::SkipSealing);
        }
        if !self.reached_terminal && sealed && report.is_some() {
            // Claimed a terminal report for a run that crashed mid-boundary.
            caught.insert(Lie::CrashMidBoundary);
        }

        if let Some(body) = report {
            self.diff_body(body, &mut caught);
        }

        GroundTruthDiff { caught }
    }

    /// Diff the body fields (everything except the seal-existence axis).
    fn diff_body(&self, body: &BoundaryReportBody, caught: &mut BTreeSet<Lie>) {
        // G1: served secret bytes but the report shows no honest observation of
        // having served them (the report claims enforcement / silence).
        if !self.served_secret_bytes.is_empty() && !observed_any(body, "secret_served") {
            caught.insert(Lie::ClaimEnforcedButAllowRead);
        }
        // G2: opened a socket but the report does not observe it.
        if !self.opened_sockets.is_empty() && !observed_any(body, "socket_opened") {
            caught.insert(Lie::ClaimEnforcedButAllowNet);
        }
        // G3: wrote outside quarantine but the report does not observe it.
        if !self.writes_outside_quarantine.is_empty()
            && !observed_any(body, "write_outside_quarantine")
        {
            caught.insert(Lie::WriteEscapesQuarantine);
        }
        // G4: spawned a child but the report does not observe the spawn.
        if !self.spawned_children.is_empty() && !observed_any(body, "child_spawned") {
            caught.insert(Lie::SpawnDespiteDeny);
        }
        // G5: a PID is still live but no orphan observation was reported.
        if !self.live_pids.is_empty() && !observed_any(body, "orphan_pid") {
            caught.insert(Lie::DropOrphanFromReport);
        }
        // G6: an inherited fd reached a child but no leak was reported.
        if !self.reachable_inherited_fds.is_empty() && !observed_any(body, "fd_leaked") {
            caught.insert(Lie::ProxyInheritedFd);
        }
        // G7: the backend auto-promoted bytes OUT of quarantine (committed an
        // artifact it has no authority to commit — committal is a host
        // disposition, never a backend act) but the report does not OBSERVE it.
        // Promoting bytes and hiding it leaves the artifact ungoverned: no
        // disposition ceremony ever sees it. Same observed-fact shape as G1–G6.
        if !self.committed_artifacts.is_empty() && !observed_any(body, "artifact_committed") {
            caught.insert(Lie::AutoCommitButReportFalse);
        }
        // G9: an attempt was actually denied but is absent from `denied`.
        for label in &self.actually_denied {
            let present = body.denied.iter().any(|d| denied_label_matches(d, label));
            if !present {
                caught.insert(Lie::DropDeniedAttempt);
            }
        }
        // G10: reported enforcement depth deeper than the mechanism actually
        // performed. GroundTruth holds the honest mechanism; the report's
        // admitted mechanisms are the claim.
        if let Some(actual) = &self.actual_enforcement_depth {
            let claims_deeper = body
                .admitted
                .iter()
                .any(|a| mechanism_is_deeper(&a.mechanism, actual));
            if claims_deeper {
                caught.insert(Lie::MisreportEnforcementDepth);
            }
        }
        // The terminal-outcome axis: a `Completed`/`Denied` outcome for a run
        // that GroundTruth says never reached a terminal is a crash-hidden lie.
        if !self.reached_terminal && matches!(body.outcome, Outcome::Completed | Outcome::Denied) {
            caught.insert(Lie::CrashMidBoundary);
        }
    }
}

/// Whether the body observed any fact with the given `kind` tag.
fn observed_any(body: &BoundaryReportBody, kind: &str) -> bool {
    body.observed.iter().any(|f| f.kind == kind)
}

/// Whether a reported denied attempt's detail names the given truth label.
fn denied_label_matches(denied: &crate::contract::report::DeniedAttempt, label: &str) -> bool {
    denied.detail.contains(label) || requirement_names(&denied.requirement).contains(label)
}

/// A stable string naming a requirement, for matching denied labels.
fn requirement_names(req: &BoundaryRequirement) -> String {
    format!("{req:?}")
}

/// Whether `claimed` asserts a strictly deeper guarantee than `actual` (the
/// honest mechanism). `"none/no-confinement"` is the shallowest; any non-none
/// mechanism is deeper than it. A claim deeper than the actual is a lie.
fn mechanism_is_deeper(claimed: &str, actual: &str) -> bool {
    let claimed_none = claimed.contains("none") || claimed.contains("no-confinement");
    let actual_none = actual.contains("none") || actual.contains("no-confinement");
    // Claimed a real mechanism while actually doing nothing → deeper than truth.
    !claimed_none && actual_none
}

/// The oracle's verdict: the set of lies the diff caught. Empty = the report was
/// honest (the GREEN posture for an honest backend).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroundTruthDiff {
    caught: BTreeSet<Lie>,
}

impl GroundTruthDiff {
    /// Whether the diff caught the specific lie `gn` expects.
    #[must_use]
    pub fn caught(&self, lie: Lie) -> bool {
        self.caught.contains(&lie)
    }

    /// Whether the diff caught NO lie (the report was honest).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.caught.is_empty()
    }

    /// The full set of caught lies, sorted.
    #[must_use]
    pub fn caught_lies(&self) -> Vec<Lie> {
        self.caught.iter().copied().collect()
    }
}

impl fmt::Display for GroundTruthDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.caught.is_empty() {
            return write!(f, "clean (no lie caught)");
        }
        write!(f, "caught: ")?;
        for (i, lie) in self.caught.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}={lie:?}", lie.gate())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ground_truth_against_empty_report_is_clean() {
        let gt = GroundTruth::new();
        let diff = gt.diff(None, true);
        assert!(diff.is_clean(), "no truth, no report → no lie: {diff}");
    }

    #[test]
    fn lie_to_gate_mapping_is_total_and_distinct() {
        let lies = [
            Lie::ClaimEnforcedButAllowRead,
            Lie::ClaimEnforcedButAllowNet,
            Lie::WriteEscapesQuarantine,
            Lie::SpawnDespiteDeny,
            Lie::DropOrphanFromReport,
            Lie::ProxyInheritedFd,
            Lie::AutoCommitButReportFalse,
            Lie::SkipSealing,
            Lie::DropDeniedAttempt,
            Lie::MisreportEnforcementDepth,
            Lie::CrashMidBoundary,
        ];
        let gates: BTreeSet<&str> = lies.iter().map(|l| l.gate()).collect();
        assert_eq!(gates.len(), lies.len(), "each lie maps to a distinct gate");
        for lie in lies {
            assert!(lie.hides_danger(), "every catalogued lie hides danger");
        }
    }
}
