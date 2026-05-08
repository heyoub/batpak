//! Deterministic, opt-in read-walk evidence over store query paths.
//!
//! This report captures what a read observed without appending by default.

use crate::coordinate::{KindFilter, Region};
use crate::store::{Freshness, HlcPoint, IndexEntry, Store};
use serde::{Deserialize, Serialize};

/// Report-body schema version for read walk evidence.
pub const READ_WALK_REPORT_SCHEMA_VERSION: u16 = 1;

/// Fixed-width hash used by read walk evidence.
pub type ReadWalkHash = [u8; 32];

/// Request for an opt-in read walk evidence report.
#[derive(Clone, Debug)]
pub struct ReadWalkRequest {
    /// Region selector used by the read.
    pub region: Region,
    /// Optional output limit applied to the matched sequence.
    pub limit: Option<usize>,
    /// Include deterministic proof refs for returned entries.
    pub include_proof_refs: bool,
    /// Caller-declared freshness intent. The v1 read walk path always samples
    /// current visible index state; this field records intent, not a cache
    /// policy applied by the query engine.
    pub freshness_intent: Freshness,
}

impl ReadWalkRequest {
    /// Build a request for the full visible region without proof refs.
    #[must_use]
    pub fn full(region: Region) -> Self {
        Self {
            region,
            limit: None,
            include_proof_refs: false,
            freshness_intent: Freshness::Consistent,
        }
    }
}

/// Stable source reference describing the read selector.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReadWalkSourceRef {
    /// Entity namespace prefix selector.
    EntityPrefix {
        /// Namespace prefix.
        prefix: String,
    },
    /// Scope selector.
    Scope {
        /// Scope string.
        scope: String,
    },
    /// Exact event kind selector.
    FactExact {
        /// Event kind category.
        category: u8,
        /// Event kind type identifier.
        type_id: u16,
    },
    /// Event kind category selector.
    FactCategory {
        /// Event kind category.
        category: u8,
    },
    /// Clock-range selector.
    ClockRange {
        /// Inclusive start.
        start_clock: u32,
        /// Inclusive end.
        end_clock: u32,
    },
}

/// Replay mode for read walk evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReadWalkReplayMode {
    /// Current visible frontier only.
    Current,
}

/// Caller-declared freshness intent captured in read walk evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReadWalkFreshnessIntent {
    /// Caller requested current visible state.
    Consistent,
    /// Caller would tolerate stale output, although v1 read walks still sample
    /// current visible index state.
    MaybeStale {
        /// Maximum stale age in milliseconds.
        max_stale_ms: u64,
    },
}

/// Frontier kind used by read walk evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReadWalkFrontierKind {
    /// Visible frontier.
    Visible,
}

/// Input frontier captured for the read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReadWalkInputFrontier {
    /// Frontier kind.
    pub kind: ReadWalkFrontierKind,
    /// HLC wall-clock milliseconds.
    pub wall_ms: u64,
    /// Global sequence.
    pub global_sequence: u64,
}

/// Precision of dropped count observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReadWalkDroppedCount {
    /// Dropped count is known exactly.
    Known(u64),
    /// No drop path applies.
    NotApplicable,
}

/// Proof reference for a returned read entry.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReadWalkProofRef {
    /// Event ID.
    pub event_id: u128,
    /// Global sequence.
    pub global_sequence: u64,
    /// Stored event hash.
    pub event_hash: ReadWalkHash,
}

/// Proof refs availability state for read walk evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadWalkProofRefs {
    /// Deterministic refs for returned entries.
    Known(Vec<ReadWalkProofRef>),
    /// Caller did not request proof refs.
    NotApplicable,
}

/// Structural findings produced by read walk evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReadWalkFinding {
    /// Input frontier could not be determined.
    InputFrontierUnknown,
    /// Output was limited and dropped additional matched results.
    LimitedResults {
        /// Number of matched entries dropped by the limit.
        dropped_count: u64,
    },
    /// Query hit did not map to backing index entry.
    MissingBackingEntry {
        /// Missing event ID.
        event_id: u128,
    },
}

/// Deterministic report body for one read walk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadWalkReportBody {
    /// Report-body schema version.
    pub schema_version: u16,
    /// Source refs extracted from the region selector.
    pub source_refs: Vec<ReadWalkSourceRef>,
    /// Read replay mode.
    pub replay_mode: ReadWalkReplayMode,
    /// Caller-declared freshness intent.
    pub freshness_intent: ReadWalkFreshnessIntent,
    /// Input frontier observed by the read.
    pub input_frontier: Option<ReadWalkInputFrontier>,
    /// Optional requested output limit.
    pub requested_limit: Option<u64>,
    /// Number of matched entries before limit/drop application.
    pub matched_count: u64,
    /// Number of returned entries.
    pub returned_count: u64,
    /// Number of dropped entries due to limit when known.
    pub dropped_limited_count: ReadWalkDroppedCount,
    /// Proof refs availability.
    pub proof_refs: ReadWalkProofRefs,
    /// Deterministic structural findings.
    pub findings: Vec<ReadWalkFinding>,
}

/// Read walk evidence report envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadWalkEvidenceReport {
    /// Deterministic report body.
    pub body: ReadWalkReportBody,
    /// Canonical hash of `body`.
    pub body_hash: ReadWalkHash,
    /// Optional generation timestamp metadata outside deterministic identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside deterministic identity.
    pub batpak_version: Option<String>,
    /// Optional diagnostics outside deterministic identity.
    pub diagnostics: Vec<String>,
}

/// Error returned when read walk evidence report generation fails.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadWalkReportError {
    /// Canonical report-body encoding failed.
    BodyEncoding {
        /// Human-readable encoding error.
        message: String,
    },
}

impl std::fmt::Display for ReadWalkReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyEncoding { message } => {
                write!(f, "read walk report body encoding failed: {message}")
            }
        }
    }
}

impl std::error::Error for ReadWalkReportError {}

impl<State> Store<State> {
    /// Perform a region query and return deterministic, opt-in read evidence.
    ///
    /// This method does not append evidence automatically.
    ///
    /// # Errors
    /// Returns [`ReadWalkReportError::BodyEncoding`] when canonical encoding of
    /// the deterministic report body fails.
    pub fn query_with_read_walk_evidence(
        &self,
        request: &ReadWalkRequest,
    ) -> Result<(Vec<IndexEntry>, ReadWalkEvidenceReport), ReadWalkReportError> {
        let (hits, visible_upper_bound) = self
            .index
            .query_hits_with_visible_upper_bound(&request.region);

        let matched_count = hits.len() as u64;
        let requested_limit = request.limit.map(|value| value as u64);
        let mut selected_hits = hits;
        let dropped_limited_count = if let Some(limit) = request.limit {
            if selected_hits.len() > limit {
                let dropped = (selected_hits.len() - limit) as u64;
                selected_hits.truncate(limit);
                ReadWalkDroppedCount::Known(dropped)
            } else {
                ReadWalkDroppedCount::NotApplicable
            }
        } else {
            ReadWalkDroppedCount::NotApplicable
        };

        let mut findings = Vec::new();
        if let ReadWalkDroppedCount::Known(dropped_count) = dropped_limited_count {
            findings.push(ReadWalkFinding::LimitedResults { dropped_count });
        }

        let mut entries = Vec::with_capacity(selected_hits.len());
        for hit in &selected_hits {
            match self
                .index
                .upgrade_hit_with_visible_upper_bound(*hit, visible_upper_bound)
            {
                Some(entry) => entries.push(entry),
                None => findings.push(ReadWalkFinding::MissingBackingEntry {
                    event_id: hit.event_id,
                }),
            }
        }

        let proof_refs = if request.include_proof_refs {
            ReadWalkProofRefs::Known(
                entries
                    .iter()
                    .map(|entry| ReadWalkProofRef {
                        event_id: entry.event_id,
                        global_sequence: entry.global_sequence,
                        event_hash: entry.hash_chain.event_hash,
                    })
                    .collect(),
            )
        } else {
            ReadWalkProofRefs::NotApplicable
        };

        let observed_visible_sequence = visible_upper_bound.saturating_sub(1);
        let input_frontier = if visible_upper_bound == 0 {
            Some(ReadWalkInputFrontier {
                kind: ReadWalkFrontierKind::Visible,
                wall_ms: HlcPoint::ORIGIN.wall_ms,
                global_sequence: HlcPoint::ORIGIN.global_sequence,
            })
        } else {
            self.index
                .hlc_for_global_sequence(observed_visible_sequence)
                .map(|point| ReadWalkInputFrontier {
                    kind: ReadWalkFrontierKind::Visible,
                    wall_ms: point.wall_ms,
                    global_sequence: point.global_sequence,
                })
        };
        if input_frontier.is_none() {
            findings.push(ReadWalkFinding::InputFrontierUnknown);
        }

        crate::evidence::sort_findings(&mut findings);
        let body = ReadWalkReportBody {
            schema_version: READ_WALK_REPORT_SCHEMA_VERSION,
            source_refs: source_refs_from_region(&request.region),
            replay_mode: ReadWalkReplayMode::Current,
            freshness_intent: map_freshness_intent(&request.freshness_intent),
            input_frontier,
            requested_limit,
            matched_count,
            returned_count: entries.len() as u64,
            dropped_limited_count,
            proof_refs,
            findings,
        };
        let body_hash = report_body_hash(&body)?;
        let report = ReadWalkEvidenceReport {
            body,
            body_hash,
            generated_at_unix_ms: None,
            batpak_version: None,
            diagnostics: Vec::new(),
        };
        Ok((entries, report))
    }
}

fn source_refs_from_region(region: &Region) -> Vec<ReadWalkSourceRef> {
    let mut refs = Vec::new();
    if let Some(prefix) = region.entity_prefix() {
        refs.push(ReadWalkSourceRef::EntityPrefix {
            prefix: prefix.to_owned(),
        });
    }
    if let Some(scope) = region.scope_value() {
        refs.push(ReadWalkSourceRef::Scope {
            scope: scope.to_owned(),
        });
    }
    if let Some(fact) = region.fact() {
        match fact {
            KindFilter::Exact(kind) => refs.push(ReadWalkSourceRef::FactExact {
                category: kind.category(),
                type_id: kind.type_id(),
            }),
            KindFilter::Category(category) => refs.push(ReadWalkSourceRef::FactCategory {
                category: *category,
            }),
            KindFilter::Any => {}
        }
    }
    if let Some((start_clock, end_clock)) = region.clock_range() {
        refs.push(ReadWalkSourceRef::ClockRange {
            start_clock,
            end_clock,
        });
    }
    refs.sort();
    refs
}

fn map_freshness_intent(freshness: &Freshness) -> ReadWalkFreshnessIntent {
    match freshness {
        Freshness::Consistent => ReadWalkFreshnessIntent::Consistent,
        Freshness::MaybeStale { max_stale_ms } => ReadWalkFreshnessIntent::MaybeStale {
            max_stale_ms: *max_stale_ms,
        },
    }
}

fn report_body_hash(body: &ReadWalkReportBody) -> Result<ReadWalkHash, ReadWalkReportError> {
    crate::evidence::report_body_hash(body, |message| ReadWalkReportError::BodyEncoding {
        message,
    })
}
