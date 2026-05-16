//! Deterministic Batpak Substrate Closure structural evidence for a compaction attempt.
//!
//! Built from segment identity and [`crate::store::segment::CompactionResult`].

use crate::evidence::{content_hash, sort_findings};
use crate::store::append::{CompactionConfig, CompactionStrategy};
use crate::store::segment::{CompactionOutcome, CompactionResult};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Report body schema version for compaction evidence.
pub const COMPACTION_REPORT_SCHEMA_VERSION: u16 = 1;

/// Strategy shape participating in compaction evidence (predicate bodies intentionally omitted).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CompactionStrategyShape {
    /// Plain merge path.
    Merge,
    /// Retention-filter path (`RetentionPredicate` opaque).
    Retention,
    /// Tombstone-rewrite path (`RetentionPredicate` opaque).
    Tombstone,
}

/// Structural compaction finding (deterministically sorted for [`CompactionReportBody::body_hash`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CompactionReportFinding {
    /// Engine rolled back disk state before swap; correlates with [`CompactionOutcome::Failed`].
    PreSwapRollback {
        /// Mirrors the engine failure reason text.
        reason: String,
    },
    /// Evidence path could not hash merged segment bytes while outcome is [`CompactionOutcome::Performed`].
    OutputSegmentHashUnavailable {
        /// Deterministic IO/encoding reason.
        reason: String,
    },
}

#[derive(Serialize)]
struct CompactionStructuralFingerprint {
    schema_version: u16,
    strategy_shape: CompactionStrategyShape,
    min_segments_threshold: usize,
    active_segment_id: u64,
    sealed_segment_count: usize,
    source_segment_ids_sorted: Vec<u64>,
    merged_segment_id: Option<u64>,
    outcome: CompactionOutcome,
    segments_removed: usize,
    bytes_reclaimed: u64,
}

fn compaction_id_digest(
    fp: &CompactionStructuralFingerprint,
) -> Result<[u8; 32], rmp_serde::encode::Error> {
    let bytes = crate::encoding::to_bytes(fp)?;
    Ok(content_hash(&bytes))
}

fn segment_id_bounds(ids: &[u64]) -> (Option<u64>, Option<u64>) {
    match (ids.first(), ids.last()) {
        (Some(lo), Some(hi)) => (Some(*lo), Some(*hi)),
        _ => (None, None),
    }
}

/// Evidence body for a single compaction decision (structural only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionReportBody {
    /// Schema version for this compaction evidence shape.
    pub schema_version: u16,
    /// Stable digest over the MessagePack-serialized structural compaction core (findings/output hash excluded).
    pub compaction_id: [u8; 32],
    /// Inclusive bounds over [`CompactionReportBody::source_segment_ids_sorted`] when present.
    pub input_segment_id_low: Option<u64>,
    /// Inclusive high bound paired with [`CompactionReportBody::input_segment_id_low`].
    pub input_segment_id_high: Option<u64>,
    /// Shape of [`CompactionConfig::strategy`] for evidence (predicates omitted).
    pub strategy_shape: CompactionStrategyShape,
    /// [`CompactionConfig::min_segments`] threshold at evidence time.
    pub min_segments_threshold: usize,
    /// Active tail segment id at evidence time.
    pub active_segment_id: u64,
    /// Count of sealed segments considered as compaction sources.
    pub sealed_segment_count: usize,
    /// Source segment ids sorted ascending (not directory iteration order).
    pub source_segment_ids_sorted: Vec<u64>,
    /// Merged output segment id when materialization started, if any.
    pub merged_segment_id: Option<u64>,
    /// Content hash of merged segment file bytes when outcome is performed and readable.
    pub output_segment_bytes_hash: Option<[u8; 32]>,
    /// Engine outcome for this attempt.
    pub outcome: CompactionOutcome,
    /// Count of sealed segment files removed after a performed compaction.
    pub segments_removed: usize,
    /// Sum of removed sealed file sizes (best-effort metadata), if measured.
    pub bytes_reclaimed: u64,
    /// Structural findings (canonical order for [`CompactionReportBody::body_hash`]).
    pub findings: Vec<CompactionReportFinding>,
}

impl CompactionReportBody {
    /// Full report body digest (findings sorted; includes `compaction_id` and output hash columns).
    ///
    /// # Errors
    /// MessagePack encoding failure from `rmp-serde`.
    pub fn body_hash(&self) -> Result<[u8; 32], rmp_serde::encode::Error> {
        let mut body = self.clone();
        sort_findings(&mut body.findings);
        let bytes = crate::encoding::to_bytes(&body)?;
        Ok(content_hash(&bytes))
    }
}

/// Map live compaction strategy to its structural evidence shape.
pub fn compaction_strategy_shape(strategy: &CompactionStrategy) -> CompactionStrategyShape {
    match strategy {
        CompactionStrategy::Merge => CompactionStrategyShape::Merge,
        CompactionStrategy::Retention(_) => CompactionStrategyShape::Retention,
        CompactionStrategy::Tombstone(_) => CompactionStrategyShape::Tombstone,
    }
}

/// Evidence for compaction skip (`sealed.len() < min_segments`).
///
/// # Errors
/// MessagePack encoding failure while computing [`CompactionReportBody::compaction_id`].
pub fn report_skipped(
    config: &CompactionConfig,
    active_segment_id: u64,
    sealed: &[(u64, std::path::PathBuf)],
) -> Result<CompactionReportBody, rmp_serde::encode::Error> {
    let mut source_segment_ids_sorted: Vec<u64> = sealed.iter().map(|(id, _)| *id).collect();
    source_segment_ids_sorted.sort();

    let (input_segment_id_low, input_segment_id_high) =
        segment_id_bounds(&source_segment_ids_sorted);

    let outcome = CompactionOutcome::Skipped;
    let fp = CompactionStructuralFingerprint {
        schema_version: COMPACTION_REPORT_SCHEMA_VERSION,
        strategy_shape: compaction_strategy_shape(&config.strategy),
        min_segments_threshold: config.min_segments,
        active_segment_id,
        sealed_segment_count: sealed.len(),
        source_segment_ids_sorted: source_segment_ids_sorted.clone(),
        merged_segment_id: None,
        outcome: outcome.clone(),
        segments_removed: 0,
        bytes_reclaimed: 0,
    };
    let compaction_id = compaction_id_digest(&fp)?;

    Ok(CompactionReportBody {
        schema_version: COMPACTION_REPORT_SCHEMA_VERSION,
        compaction_id,
        input_segment_id_low,
        input_segment_id_high,
        strategy_shape: fp.strategy_shape,
        min_segments_threshold: fp.min_segments_threshold,
        active_segment_id,
        sealed_segment_count: sealed.len(),
        source_segment_ids_sorted,
        merged_segment_id: None,
        output_segment_bytes_hash: None,
        outcome,
        segments_removed: 0,
        bytes_reclaimed: 0,
        findings: Vec::new(),
    })
}

fn push_failed_finding(findings: &mut Vec<CompactionReportFinding>, outcome: &CompactionOutcome) {
    if let CompactionOutcome::Failed { reason } = outcome {
        findings.push(CompactionReportFinding::PreSwapRollback {
            reason: reason.clone(),
        });
    }
}

/// Evidence tying engine [`CompactionResult`] to deterministic structural refs.
///
/// # Errors
/// MessagePack encoding failure while computing [`CompactionReportBody::compaction_id`].
pub fn report_for_run(
    config: &CompactionConfig,
    active_segment_id: u64,
    sealed: &[(u64, std::path::PathBuf)],
    merged_segment_id: Option<u64>,
    result: &CompactionResult,
    merged_segment_path_for_hash: Option<&Path>,
) -> Result<CompactionReportBody, rmp_serde::encode::Error> {
    let mut source_segment_ids_sorted: Vec<u64> = sealed.iter().map(|(id, _)| *id).collect();
    source_segment_ids_sorted.sort();

    let (input_segment_id_low, input_segment_id_high) =
        segment_id_bounds(&source_segment_ids_sorted);

    let mut findings = Vec::new();
    push_failed_finding(&mut findings, &result.outcome);

    let output_segment_bytes_hash = match (&result.outcome, merged_segment_path_for_hash) {
        (CompactionOutcome::Performed, Some(path)) => match std::fs::read(path) {
            Ok(bytes) => Some(content_hash(&bytes)),
            Err(err) => {
                findings.push(CompactionReportFinding::OutputSegmentHashUnavailable {
                    reason: format!("read merged segment for evidence hash: {err}"),
                });
                None
            }
        },
        (CompactionOutcome::Performed, None) => {
            findings.push(CompactionReportFinding::OutputSegmentHashUnavailable {
                reason: "merged segment path unavailable for evidence hash".into(),
            });
            None
        }
        _ => None,
    };

    let fp = CompactionStructuralFingerprint {
        schema_version: COMPACTION_REPORT_SCHEMA_VERSION,
        strategy_shape: compaction_strategy_shape(&config.strategy),
        min_segments_threshold: config.min_segments,
        active_segment_id,
        sealed_segment_count: sealed.len(),
        source_segment_ids_sorted: source_segment_ids_sorted.clone(),
        merged_segment_id,
        outcome: result.outcome.clone(),
        segments_removed: result.segments_removed,
        bytes_reclaimed: result.bytes_reclaimed,
    };
    let compaction_id = compaction_id_digest(&fp)?;

    sort_findings(&mut findings);

    Ok(CompactionReportBody {
        schema_version: COMPACTION_REPORT_SCHEMA_VERSION,
        compaction_id,
        input_segment_id_low,
        input_segment_id_high,
        strategy_shape: fp.strategy_shape,
        min_segments_threshold: fp.min_segments_threshold,
        active_segment_id,
        sealed_segment_count: sealed.len(),
        source_segment_ids_sorted,
        merged_segment_id,
        output_segment_bytes_hash,
        outcome: result.outcome.clone(),
        segments_removed: result.segments_removed,
        bytes_reclaimed: result.bytes_reclaimed,
        findings,
    })
}
