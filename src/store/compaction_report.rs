//! Deterministic structural evidence for a compaction run (no retention/legal semantics).
//!
//! Built from segment identity and [`crate::store::segment::CompactionResult`].
// justifies: INV-ALLOW-IS-DESIGN; compaction report `body_hash` is encode-only like evidence reports; `tests/lane_a_fullsend_substrate.rs`
#![allow(clippy::missing_errors_doc)]
use crate::evidence::{content_hash, sort_findings};
use crate::store::append::{CompactionConfig, CompactionStrategy};
use crate::store::segment::{CompactionOutcome, CompactionResult};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Report body schema version for compaction evidence.
pub const COMPACTION_REPORT_SCHEMA_VERSION: u16 = 1;

/// Shape of the configured strategy (predicates are intentionally not captured).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CompactionStrategyShape {
    /// Plain merge.
    Merge,
    /// Retention-style filter present (predicate opaque).
    Retention,
    /// Tombstone rewrite path (predicate opaque).
    Tombstone,
}

/// Structural finding in a compaction report (sorted before [`CompactionReportBody::body_hash`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CompactionReportFinding {
    /// Output segment file hash unavailable on the evidence path.
    OutputSegmentHashUnavailable {
        /// Deterministic reason.
        reason: String,
    },
}

/// Evidence body for one compaction attempt: structural identities only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReportBody {
    /// Schema version for this report shape.
    pub schema_version: u16,
    /// [`CompactionStrategyShape`] for this run.
    pub strategy_shape: CompactionStrategyShape,
    /// `CompactionConfig::min_segments` threshold used for the attempt.
    pub min_segments_threshold: usize,
    /// Active (append) segment id at decision time.
    pub active_segment_id: u64,
    /// Count of sealed segments observed at decision time.
    pub sealed_segment_count: usize,
    /// Sealed segment ids participating in the structural view (sorted).
    pub source_segment_ids_sorted: Vec<u64>,
    /// Merged / output sealed id when materialization ran (`None` if skipped early).
    pub merged_segment_id: Option<u64>,
    /// Raw bytes hash of the merged `.fbat` after seal (`None` if not performed or unavailable).
    pub output_segment_bytes_hash: Option<[u8; 32]>,
    /// Outcome of the engine run.
    pub outcome: CompactionOutcome,
    /// Echo of [`CompactionResult::segments_removed`].
    pub segments_removed: usize,
    /// Echo of [`CompactionResult::bytes_reclaimed`].
    pub bytes_reclaimed: u64,
    /// Structural findings (sorted before hashing).
    pub findings: Vec<CompactionReportFinding>,
}

impl CompactionReportBody {
    /// Deterministic body digest (MessagePack; findings sorted for canonical order).
    pub fn body_hash(&self) -> Result<[u8; 32], rmp_serde::encode::Error> {
        let mut body = self.clone();
        sort_findings(&mut body.findings);
        let bytes = crate::encoding::to_bytes(&body)?;
        Ok(content_hash(&bytes))
    }
}

/// Map strategy to its structural shape (predicates ignored).
pub fn compaction_strategy_shape(strategy: &CompactionStrategy) -> CompactionStrategyShape {
    match strategy {
        CompactionStrategy::Merge => CompactionStrategyShape::Merge,
        CompactionStrategy::Retention(_) => CompactionStrategyShape::Retention,
        CompactionStrategy::Tombstone(_) => CompactionStrategyShape::Tombstone,
    }
}

/// Build evidence for early skip (`sealed.len() < min_segments`).
pub fn report_skipped(
    config: &CompactionConfig,
    active_segment_id: u64,
    sealed: &[(u64, std::path::PathBuf)],
) -> CompactionReportBody {
    let mut source_segment_ids_sorted: Vec<u64> = sealed.iter().map(|(id, _)| *id).collect();
    source_segment_ids_sorted.sort();
    CompactionReportBody {
        schema_version: COMPACTION_REPORT_SCHEMA_VERSION,
        strategy_shape: compaction_strategy_shape(&config.strategy),
        min_segments_threshold: config.min_segments,
        active_segment_id,
        sealed_segment_count: sealed.len(),
        source_segment_ids_sorted,
        merged_segment_id: None,
        output_segment_bytes_hash: None,
        outcome: CompactionOutcome::Skipped,
        segments_removed: 0,
        bytes_reclaimed: 0,
        findings: Vec::new(),
    }
}

/// Evidence for a completed attempt: pairs the engine [`CompactionResult`] with structural ids.
pub fn report_for_run(
    config: &CompactionConfig,
    active_segment_id: u64,
    sealed: &[(u64, std::path::PathBuf)],
    merged_segment_id: Option<u64>,
    result: &CompactionResult,
    merged_segment_path_for_hash: Option<&Path>,
) -> CompactionReportBody {
    let mut source_segment_ids_sorted: Vec<u64> = sealed.iter().map(|(id, _)| *id).collect();
    source_segment_ids_sorted.sort();

    let mut findings = Vec::new();
    let output_segment_bytes_hash = match (&result.outcome, merged_segment_path_for_hash) {
        (CompactionOutcome::Performed, Some(path)) => match std::fs::read(path) {
            Ok(bytes) => Some(content_hash(&bytes)),
            Err(e) => {
                findings.push(CompactionReportFinding::OutputSegmentHashUnavailable {
                    reason: format!("read merged segment for evidence hash: {e}"),
                });
                None
            }
        },
        _ => None,
    };

    sort_findings(&mut findings);

    CompactionReportBody {
        schema_version: COMPACTION_REPORT_SCHEMA_VERSION,
        strategy_shape: compaction_strategy_shape(&config.strategy),
        min_segments_threshold: config.min_segments,
        active_segment_id,
        sealed_segment_count: sealed.len(),
        source_segment_ids_sorted,
        merged_segment_id,
        output_segment_bytes_hash,
        outcome: result.outcome.clone(),
        segments_removed: result.segments_removed,
        bytes_reclaimed: result.bytes_reclaimed,
        findings,
    }
}
