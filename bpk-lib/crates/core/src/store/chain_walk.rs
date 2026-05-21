//! Deterministic Batpak Substrate Closure structural chain-walk evidence over stored event
//! material.
//!
//! This surface reports linear chain continuity findings without inferring
//! downstream semantics.

use crate::store::index::IndexEntry;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Current report-body schema version for chain walk evidence reports.
pub const CHAIN_WALK_REPORT_SCHEMA_VERSION: u16 = 1;

/// Fixed-width hash used in chain walk evidence.
pub type ChainWalkHash = [u8; 32];

/// Chain walk mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainWalkMode {
    /// Follow one linear parent chain from the start reference.
    Linear,
}

/// Start reference for chain walk evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainWalkStartRef {
    /// Start from a stored event ID.
    EventId(u128),
    /// Start from a stored event ID plus expected chain hash from a receipt.
    Receipt {
        /// Event ID from receipt material.
        event_id: u128,
        /// Expected chain hash for the start event.
        content_hash: ChainWalkHash,
    },
}

/// Request for a deterministic linear chain-walk evidence report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainWalkRequest {
    /// Start reference.
    pub start: ChainWalkStartRef,
    /// Optional stop event ID.
    pub end_event_id: Option<u128>,
    /// Maximum number of checked entries.
    pub limit: usize,
    /// Walk mode.
    pub mode: ChainWalkMode,
}

impl ChainWalkRequest {
    /// Create a linear chain-walk request with no explicit end bound.
    #[must_use]
    pub fn linear(start: ChainWalkStartRef, limit: usize) -> Self {
        Self {
            start,
            end_event_id: None,
            limit,
            mode: ChainWalkMode::Linear,
        }
    }
}

/// Deterministic structural findings from a chain walk.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChainWalkFinding {
    /// Request limit cannot check even the start entry.
    InvalidLimit {
        /// Requested walk limit.
        limit: usize,
    },
    /// Start event was not found.
    MissingStart {
        /// Missing start event ID.
        event_id: u128,
    },
    /// A non-start event disappeared from the in-memory index during the walk.
    MissingChainEntry {
        /// Missing event ID.
        event_id: u128,
    },
    /// Start receipt hash did not match stored chain hash.
    StartHashMismatch {
        /// Start event ID.
        event_id: u128,
        /// Expected hash from receipt material.
        expected: ChainWalkHash,
        /// Observed hash from stored chain material.
        observed: ChainWalkHash,
    },
    /// Stored content hash did not match the chain hash at this entry.
    EntryHashMismatch {
        /// Entry event ID.
        event_id: u128,
        /// Expected chain hash from index material.
        expected: ChainWalkHash,
        /// Observed hash from stored payload bytes.
        observed: ChainWalkHash,
    },
    /// Expected parent hash was not present in the same entity chain.
    MissingParentLink {
        /// Child event where the missing parent edge was encountered.
        child_event_id: u128,
        /// Parent hash required by the child chain edge.
        expected_parent_hash: ChainWalkHash,
    },
    /// More than one prior entry in the same entity stream matched the required parent hash.
    ParentHashAmbiguous {
        /// Child event where the ambiguous parent edge was encountered.
        child_event_id: u128,
        /// Parent hash required by the child chain edge.
        expected_parent_hash: ChainWalkHash,
        /// Nearest prior matching event selected for the walk.
        selected_parent_event_id: u128,
        /// Number of prior matching entries.
        matching_parent_count: u64,
    },
    /// Parent entry was found but sequence ordering regressed.
    OrderingRegression {
        /// Child event ID.
        child_event_id: u128,
        /// Parent event ID.
        parent_event_id: u128,
        /// Child global sequence.
        child_sequence: u64,
        /// Parent global sequence.
        parent_sequence: u64,
    },
    /// Walk revisited the same event ID.
    CycleDetected {
        /// Revisited event ID.
        event_id: u128,
    },
    /// Walk could not read a persisted event entry.
    StoppedEarlyReadFailure {
        /// Event ID at which the read failed.
        event_id: u128,
        /// Reader error message.
        reason: String,
    },
    /// Walk hit the request limit while the chain still had a parent edge.
    TruncatedByLimit {
        /// Requested walk limit.
        limit: usize,
        /// Parent hash that would have been followed next.
        next_parent_hash: ChainWalkHash,
    },
    /// Explicit end event was not reached.
    EndNotReached {
        /// End event ID requested by caller.
        expected_end_event_id: u128,
    },
}

/// Deterministic report body for chain walk evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainWalkReportBody {
    /// Report-body schema version.
    pub schema_version: u16,
    /// Walk mode used for this report.
    pub mode: ChainWalkMode,
    /// Number of checked entries.
    pub checked_count: u64,
    /// First checked event reference.
    pub first_ref: Option<u128>,
    /// Last checked event reference.
    pub last_ref: Option<u128>,
    /// Deterministic digest over checked refs and chain hashes.
    pub walk_digest: ChainWalkHash,
    /// Deterministic findings in sorted structural order.
    pub findings: Vec<ChainWalkFinding>,
}

/// Chain walk structural evidence report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainWalkEvidenceReport {
    /// Deterministic report body.
    pub body: ChainWalkReportBody,
    /// Canonical hash of `body` bytes.
    pub body_hash: ChainWalkHash,
    /// Optional generation timestamp metadata outside body identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside body identity.
    pub batpak_version: Option<String>,
    /// Optional diagnostics outside deterministic body identity.
    pub diagnostics: Vec<String>,
}

/// Error returned when a chain walk evidence report cannot be built.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainWalkReportError {
    /// Canonical report-body encoding failed.
    BodyEncoding {
        /// Human-readable encoding error.
        message: String,
    },
}

impl std::fmt::Display for ChainWalkReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyEncoding { message } => {
                write!(f, "chain walk report body encoding failed: {message}")
            }
        }
    }
}

impl std::error::Error for ChainWalkReportError {}

impl<State> Store<State> {
    /// Build a deterministic structural chain-walk evidence report.
    ///
    /// This operation validates continuity and hash linkage over stored entries.
    /// It does not infer application-level causation semantics.
    ///
    /// # Errors
    /// Returns [`ChainWalkReportError::BodyEncoding`] when canonical encoding of
    /// the deterministic report body fails.
    pub fn chain_walk_evidence(
        &self,
        request: &ChainWalkRequest,
    ) -> Result<ChainWalkEvidenceReport, ChainWalkReportError> {
        let (start_event_id, expected_start_hash) = match request.start {
            ChainWalkStartRef::EventId(event_id) => (event_id, None),
            ChainWalkStartRef::Receipt {
                event_id,
                content_hash,
            } => (event_id, Some(content_hash)),
        };

        if request.limit == 0 {
            return build_report(
                request.mode,
                &[],
                vec![ChainWalkFinding::InvalidLimit { limit: 0 }],
            );
        }

        let start_entry = match self.index.get_by_id(start_event_id) {
            Some(entry) => entry,
            None => {
                return build_report(
                    request.mode,
                    &[],
                    vec![ChainWalkFinding::MissingStart {
                        event_id: start_event_id,
                    }],
                );
            }
        };
        let entity_stream = self.index.stream(start_entry.coord.entity());

        let mut findings = Vec::new();
        let mut checked = Vec::new();
        let mut visited = HashSet::new();
        let mut cursor = Some(start_event_id);
        let mut reached_end = false;
        let mut pending_parent_hash_for_limit: Option<ChainWalkHash> = None;

        while checked.len() < request.limit {
            let Some(current_id) = cursor.take() else {
                break;
            };

            if !visited.insert(current_id) {
                findings.push(ChainWalkFinding::CycleDetected {
                    event_id: current_id,
                });
                break;
            }

            let Some(entry) = self.index.get_by_id(current_id) else {
                findings.push(ChainWalkFinding::MissingChainEntry {
                    event_id: current_id,
                });
                break;
            };

            let stored = match self.reader.read_entry_raw(&entry.disk_pos) {
                Ok(stored) => stored,
                Err(error) => {
                    findings.push(ChainWalkFinding::StoppedEarlyReadFailure {
                        event_id: current_id,
                        reason: error.to_string(),
                    });
                    break;
                }
            };

            if checked.is_empty() {
                if let Some(expected_hash) = expected_start_hash {
                    if expected_hash != entry.hash_chain.event_hash {
                        findings.push(ChainWalkFinding::StartHashMismatch {
                            event_id: current_id,
                            expected: expected_hash,
                            observed: entry.hash_chain.event_hash,
                        });
                    }
                }
            }

            let computed_hash = observed_content_hash(&stored);
            if computed_hash != entry.hash_chain.event_hash {
                findings.push(ChainWalkFinding::EntryHashMismatch {
                    event_id: current_id,
                    expected: entry.hash_chain.event_hash,
                    observed: computed_hash,
                });
                checked.push((current_id, entry.hash_chain.event_hash));
                break;
            }

            checked.push((current_id, entry.hash_chain.event_hash));

            if request.end_event_id == Some(current_id) {
                reached_end = true;
                break;
            }

            if entry.hash_chain.prev_hash == [0_u8; 32] {
                break;
            }

            pending_parent_hash_for_limit = Some(entry.hash_chain.prev_hash);
            let Some(parent_id) = resolve_parent_event_id_by_hash(
                &entity_stream,
                entry.hash_chain.prev_hash,
                entry.global_sequence,
            ) else {
                findings.push(ChainWalkFinding::MissingParentLink {
                    child_event_id: current_id,
                    expected_parent_hash: entry.hash_chain.prev_hash,
                });
                break;
            };
            if parent_id.matching_parent_count > 1 {
                findings.push(ChainWalkFinding::ParentHashAmbiguous {
                    child_event_id: current_id,
                    expected_parent_hash: entry.hash_chain.prev_hash,
                    selected_parent_event_id: parent_id.event_id,
                    matching_parent_count: parent_id.matching_parent_count,
                });
            }

            if let Some(parent_entry) = self.index.get_by_id(parent_id.event_id) {
                if parent_entry.global_sequence >= entry.global_sequence {
                    findings.push(ChainWalkFinding::OrderingRegression {
                        child_event_id: current_id,
                        parent_event_id: parent_id.event_id,
                        child_sequence: entry.global_sequence,
                        parent_sequence: parent_entry.global_sequence,
                    });
                    break;
                }
            }
            cursor = Some(parent_id.event_id);
        }

        if findings.is_empty() && checked.len() == request.limit {
            if let Some(next_parent_hash) = pending_parent_hash_for_limit {
                findings.push(ChainWalkFinding::TruncatedByLimit {
                    limit: request.limit,
                    next_parent_hash,
                });
            }
        }

        if findings.is_empty() && !reached_end {
            if let Some(expected_end_event_id) = request.end_event_id {
                if checked.last().map(|(event_id, _)| *event_id) != Some(expected_end_event_id) {
                    findings.push(ChainWalkFinding::EndNotReached {
                        expected_end_event_id,
                    });
                }
            }
        }

        build_report(request.mode, &checked, findings)
    }
}

struct ParentHashResolution {
    event_id: u128,
    matching_parent_count: u64,
}

fn resolve_parent_event_id_by_hash(
    entity_stream: &[IndexEntry],
    parent_hash: ChainWalkHash,
    child_sequence: u64,
) -> Option<ParentHashResolution> {
    let mut matching_parent_count = 0u64;
    let mut selected: Option<&IndexEntry> = None;

    for candidate in entity_stream.iter().filter(|candidate| {
        candidate.hash_chain.event_hash == parent_hash && candidate.global_sequence < child_sequence
    }) {
        matching_parent_count = matching_parent_count.saturating_add(1);
        if selected
            .map(|current| candidate.global_sequence > current.global_sequence)
            .unwrap_or(true)
        {
            selected = Some(candidate);
        }
    }

    let selected = selected?;
    Some(ParentHashResolution {
        event_id: selected.event_id,
        matching_parent_count,
    })
}

fn build_report(
    mode: ChainWalkMode,
    checked: &[(u128, ChainWalkHash)],
    mut findings: Vec<ChainWalkFinding>,
) -> Result<ChainWalkEvidenceReport, ChainWalkReportError> {
    crate::evidence::sort_findings(&mut findings);
    let checked_count = checked.len() as u64;
    let first_ref = checked.first().map(|(event_id, _)| *event_id);
    let last_ref = checked.last().map(|(event_id, _)| *event_id);
    let walk_digest = walk_digest(checked)?;
    let body = ChainWalkReportBody {
        schema_version: CHAIN_WALK_REPORT_SCHEMA_VERSION,
        mode,
        checked_count,
        first_ref,
        last_ref,
        walk_digest,
        findings,
    };
    let body_hash = report_body_hash(&body)?;
    Ok(ChainWalkEvidenceReport {
        body,
        body_hash,
        generated_at_unix_ms: None,
        batpak_version: None,
        diagnostics: Vec::new(),
    })
}

fn walk_digest(checked: &[(u128, ChainWalkHash)]) -> Result<ChainWalkHash, ChainWalkReportError> {
    let bytes = crate::canonical::to_bytes(checked).map_err(|error| {
        ChainWalkReportError::BodyEncoding {
            message: error.to_string(),
        }
    })?;
    Ok(crate::evidence::content_hash(&bytes))
}

fn report_body_hash(body: &ChainWalkReportBody) -> Result<ChainWalkHash, ChainWalkReportError> {
    crate::evidence::report_body_hash(body, |message| ChainWalkReportError::BodyEncoding {
        message,
    })
}

fn observed_content_hash(stored: &crate::event::StoredEvent<Vec<u8>>) -> ChainWalkHash {
    crate::event::hash::compute_hash(&stored.event.payload)
}

#[cfg(test)]
mod tests {
    use super::{build_report, ChainWalkFinding, ChainWalkMode};
    use std::error::Error;

    #[test]
    fn chain_walk_report_sorts_findings_structurally() -> Result<(), Box<dyn Error>> {
        let report = build_report(
            ChainWalkMode::Linear,
            &[],
            vec![
                ChainWalkFinding::MissingStart { event_id: 9 },
                ChainWalkFinding::MissingStart { event_id: 3 },
            ],
        )?;

        assert_eq!(
            report.body.findings,
            vec![
                ChainWalkFinding::MissingStart { event_id: 3 },
                ChainWalkFinding::MissingStart { event_id: 9 },
            ],
            "PROPERTY: chain walk findings must be sorted in deterministic structural order"
        );
        Ok(())
    }
}
