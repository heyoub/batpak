//! [`HostControl`] — HOST-PROVISIONED lifecycle, NOT guest authority.
//!
//! What the HOST/runner does AROUND the boundary; never grantable to the
//! workload. Exposed (later) as the namespaced syncbat op surface. Each carries
//! a guarantee-shaped QUALITY grade the backend reports honestly. The mechanism
//! (cgroup.kill / Job Object / pgid / rename-same-fs / bind-mount / preopen) is
//! the backend's choice, recorded in [`crate::AdmittedRequirement::mechanism`].
//! [`crate::Enforcement::Unsupported`] is the backend's ANSWER, never requested.

use crate::contract::capability::FsAccess;
use serde::{Deserialize, Serialize};

/// Host-provisioned lifecycle controls applied around the boundary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HostControl {
    /// The initial spawn of the workload (the `boundary.run` op).
    LaunchWorkload,
    /// The host wires the workload's standard streams.
    CaptureStreams {
        /// Which standard streams the host captures.
        streams: StdStreams,
    },
    /// Temp storage provisioned privately for this boundary.
    TempRoot {
        /// The visibility GUARANTEE required for the temp root.
        visibility: PathView,
    },
    /// Expose a host path into the boundary (guarantee-shaped MountPath).
    ExposePath {
        /// Source path on the host, as a portable string.
        source: String,
        /// Destination path inside the boundary, as a portable string.
        dest: String,
        /// Access grant for the exposed path.
        access: FsAccess,
        /// The visibility GUARANTEE required for the exposed path.
        view: PathView,
    },
    /// Commit a produced artifact out of the boundary's quarantine.
    CommitArtifact {
        /// The durability GUARANTEE required for the commit.
        durability: CommitDurability,
    },
    /// Discard the boundary's quarantined artifacts.
    DiscardArtifact,
    /// Tear down the boundary's run tree.
    Kill {
        /// What the kill targets.
        target: KillTarget,
        /// The kill GUARANTEE required.
        guarantee: KillGuarantee,
    },
    /// List the outputs the boundary produced.
    ListOutputs,
}

/// GUARANTEE: a path view private to this boundary (not a mechanism).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PathView {
    /// The path is visible only inside this boundary.
    PrivateToBoundary,
}

/// GUARANTEE: how durable a committed artifact is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CommitDurability {
    /// Atomic, all-or-nothing publish (e.g. rename-same-fs).
    Atomic,
    /// Durable through a crash (e.g. copy + fsync).
    Durable,
    /// Best-effort durability only.
    BestEffortDurable,
}

/// GUARANTEE: what a kill targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum KillTarget {
    /// The entire run tree of the boundary.
    RunTree,
}

/// GUARANTEE: how complete a kill is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum KillGuarantee {
    /// Atomic subtree teardown with no escape window.
    Atomic,
    /// Best-effort teardown is acceptable (some escape window allowed).
    BestEffortAllowed,
}

/// Which standard streams the host captures around the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StdStreams {
    /// Capture standard output.
    pub stdout: bool,
    /// Capture standard error.
    pub stderr: bool,
    /// Provide a standard input stream.
    pub stdin: bool,
}

impl StdStreams {
    /// Capture stdout and stderr, no stdin — the common observe posture.
    #[must_use]
    pub fn capture_out_err() -> Self {
        Self {
            stdout: true,
            stderr: true,
            stdin: false,
        }
    }
}
