//! Deterministic evidence report for a store fork operation.

use crate::evidence::{content_hash, sort_findings};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Report body schema version for fork evidence.
pub const FORK_EVIDENCE_REPORT_SCHEMA_VERSION: u16 = 1;

/// Hash alias for fork evidence report bodies.
pub type ForkEvidenceHash = [u8; 32];

/// Copy strategy actually used for a forked artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ForkCopyStrategy {
    /// Filesystem copy-on-write clone.
    Reflink,
    /// Hardlink to an immutable sealed segment.
    Hardlink,
    /// Ordinary byte-for-byte file copy.
    DeepCopy,
}

/// Copy ladder preference for forked artifacts.
///
/// Selects how aggressively a fork shares storage with its source. Each rung
/// falls back to an ordinary deep copy when the preferred mechanism is not
/// available on the underlying filesystem.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyPreference {
    /// Try a filesystem reflink, then a hardlink, then a deep copy.
    #[default]
    ReflinkThenHardlink,
    /// Skip reflinks; try a hardlink, then a deep copy.
    HardlinkOnly,
    /// Always perform an ordinary byte-for-byte deep copy.
    DeepCopyOnly,
}

/// Caller options for [`crate::store::Store::fork_with_evidence`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct ForkOptions {
    /// Copy ladder preference for immutable sealed segments.
    pub copy_preference: CopyPreference,
    /// Exclude regenerable cold-start caches (`index.ckpt`, `index.fbati`).
    pub exclude_caches: bool,
}

impl Default for ForkOptions {
    fn default() -> Self {
        Self {
            copy_preference: CopyPreference::default(),
            exclude_caches: true,
        }
    }
}

/// Count of fork decisions and copy strategies observed during one fork.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkStrategyCounts {
    /// Number of files copied with reflink.
    pub reflink: usize,
    /// Number of files copied with hardlink.
    pub hardlink: usize,
    /// Number of files copied with ordinary deep copy.
    pub deep_copy: usize,
    /// Number of regenerable cache files excluded.
    pub cache_regenerable: usize,
    /// Number of store-shaped artifacts excluded.
    pub excluded: usize,
}

impl ForkStrategyCounts {
    pub(crate) fn record_copy(&mut self, strategy: ForkCopyStrategy) {
        match strategy {
            ForkCopyStrategy::Reflink => self.reflink += 1,
            ForkCopyStrategy::Hardlink => self.hardlink += 1,
            ForkCopyStrategy::DeepCopy => self.deep_copy += 1,
        }
    }

    pub(crate) fn record_cache_regenerable(&mut self) {
        self.cache_regenerable += 1;
    }

    pub(crate) fn record_excluded(&mut self) {
        self.excluded += 1;
    }
}

/// Deterministic structural findings for a fork attempt.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ForkFinding {
    /// Existing fork destination artifacts were cleared before copying.
    DestinationCleared {
        /// Number of destination artifacts removed.
        artifact_count: usize,
    },
    /// The private visibility fence was explicitly cancelled after copying.
    FenceTokenCancelled,
    /// A regenerable cache file was intentionally excluded.
    CacheRegenerableExcluded {
        /// Stable file name.
        file_name: String,
    },
    /// A store-shaped file was intentionally excluded.
    FileExcluded {
        /// Stable file name.
        file_name: String,
        /// Stable reason string.
        reason: String,
    },
    /// A source artifact was copied, with the concrete strategy recorded.
    FileCopied {
        /// Stable file name.
        file_name: String,
        /// Strategy actually used.
        strategy: ForkCopyStrategy,
    },
}

#[derive(Serialize)]
struct ForkStructuralFingerprint {
    schema_version: u16,
    fence_token: crate::store::SnapshotFenceTokenRef,
    source_watermark: crate::store::SnapshotWatermarkRef,
    active_segment_id: u64,
    shared_segment_ids_sorted: Vec<u64>,
    deep_copied_segment_ids_sorted: Vec<u64>,
    strategy_counts: ForkStrategyCounts,
    copied_visibility_ranges_present: bool,
    copied_pending_compaction_marker_present: bool,
    copied_idempotency_store_present: bool,
    destination_path_digest: ForkEvidenceHash,
}

fn fork_id_digest(
    fp: &ForkStructuralFingerprint,
) -> Result<ForkEvidenceHash, rmp_serde::encode::Error> {
    let bytes = crate::encoding::to_bytes(fp)?;
    Ok(content_hash(&bytes))
}

/// Deterministic report body for one store fork.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkReportBody {
    /// Report-body schema version.
    pub schema_version: u16,
    /// Stable digest over the structural fork core.
    pub fork_id: ForkEvidenceHash,
    /// Private visibility-fence token covering the copied segment set.
    pub fence_token: crate::store::SnapshotFenceTokenRef,
    /// Source segment watermark after draining the writer.
    pub source_watermark: crate::store::SnapshotWatermarkRef,
    /// Segment id classified as active at the fork boundary.
    pub active_segment_id: u64,
    /// Segment ids shared by reflink or hardlink, sorted ascending.
    pub shared_segment_ids_sorted: Vec<u64>,
    /// Segment ids deep-copied, sorted ascending.
    pub deep_copied_segment_ids_sorted: Vec<u64>,
    /// Counts of the strategies and exclusions observed.
    pub strategy_counts: ForkStrategyCounts,
    /// Whether `visibility_ranges.fbv` was copied.
    pub copied_visibility_ranges_present: bool,
    /// Whether the pending-compaction marker was copied.
    pub copied_pending_compaction_marker_present: bool,
    /// Whether the durable idempotency store (`index.idemp`) was copied.
    pub copied_idempotency_store_present: bool,
    /// Digest of the destination path bytes.
    pub destination_path_digest: ForkEvidenceHash,
    /// Structural findings sorted before `body_hash`.
    pub findings: Vec<ForkFinding>,
}

impl ForkReportBody {
    /// Full report-body digest, with findings sorted before encoding.
    ///
    /// # Errors
    /// MessagePack encoding failure from `rmp-serde`.
    pub fn body_hash(&self) -> Result<ForkEvidenceHash, rmp_serde::encode::Error> {
        fork_report_body_hash(self)
    }
}

/// Fork evidence report envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkReport {
    /// Deterministic report body.
    pub body: ForkReportBody,
    /// Canonical hash of `body`.
    pub body_hash: ForkEvidenceHash,
    /// Optional generation timestamp metadata outside deterministic identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside deterministic identity.
    pub batpak_version: Option<String>,
    /// Optional diagnostics outside deterministic identity.
    pub diagnostics: Vec<String>,
}

/// Canonical `body_hash` over a fork report body.
///
/// # Errors
/// MessagePack encoding failure from `rmp-serde`.
pub fn fork_report_body_hash(
    body: &ForkReportBody,
) -> Result<ForkEvidenceHash, rmp_serde::encode::Error> {
    let mut body = body.clone();
    sort_findings(&mut body.findings);
    let bytes = crate::encoding::to_bytes(&body)?;
    Ok(content_hash(&bytes))
}

pub(crate) fn destination_path_digest(dest: &Path) -> ForkEvidenceHash {
    content_hash(dest.as_os_str().as_encoded_bytes())
}

pub(crate) struct ForkReportInput {
    pub(crate) fence_token: u64,
    pub(crate) source_watermark_segment_id: u64,
    pub(crate) source_watermark_offset: u64,
    pub(crate) active_segment_id: u64,
    pub(crate) shared_segment_ids_sorted: Vec<u64>,
    pub(crate) deep_copied_segment_ids_sorted: Vec<u64>,
    pub(crate) strategy_counts: ForkStrategyCounts,
    pub(crate) copied_visibility_ranges_present: bool,
    pub(crate) copied_pending_compaction_marker_present: bool,
    pub(crate) copied_idempotency_store_present: bool,
    pub(crate) destination_path_digest: ForkEvidenceHash,
    pub(crate) findings: Vec<ForkFinding>,
}

pub(crate) fn fork_evidence_report(
    input: ForkReportInput,
) -> Result<ForkReport, rmp_serde::encode::Error> {
    let fence_token = crate::store::SnapshotFenceTokenRef {
        token: input.fence_token,
    };
    let source_watermark = crate::store::SnapshotWatermarkRef {
        segment_id: input.source_watermark_segment_id,
        offset: input.source_watermark_offset,
    };
    let mut shared_segment_ids_sorted = input.shared_segment_ids_sorted;
    shared_segment_ids_sorted.sort_unstable();
    let mut deep_copied_segment_ids_sorted = input.deep_copied_segment_ids_sorted;
    deep_copied_segment_ids_sorted.sort_unstable();
    let mut findings = input.findings;
    sort_findings(&mut findings);

    let fp = ForkStructuralFingerprint {
        schema_version: FORK_EVIDENCE_REPORT_SCHEMA_VERSION,
        fence_token,
        source_watermark,
        active_segment_id: input.active_segment_id,
        shared_segment_ids_sorted: shared_segment_ids_sorted.clone(),
        deep_copied_segment_ids_sorted: deep_copied_segment_ids_sorted.clone(),
        strategy_counts: input.strategy_counts,
        copied_visibility_ranges_present: input.copied_visibility_ranges_present,
        copied_pending_compaction_marker_present: input.copied_pending_compaction_marker_present,
        copied_idempotency_store_present: input.copied_idempotency_store_present,
        destination_path_digest: input.destination_path_digest,
    };
    let fork_id = fork_id_digest(&fp)?;
    let body = ForkReportBody {
        schema_version: FORK_EVIDENCE_REPORT_SCHEMA_VERSION,
        fork_id,
        fence_token,
        source_watermark,
        active_segment_id: input.active_segment_id,
        shared_segment_ids_sorted,
        deep_copied_segment_ids_sorted,
        strategy_counts: input.strategy_counts,
        copied_visibility_ranges_present: input.copied_visibility_ranges_present,
        copied_pending_compaction_marker_present: input.copied_pending_compaction_marker_present,
        copied_idempotency_store_present: input.copied_idempotency_store_present,
        destination_path_digest: input.destination_path_digest,
        findings,
    };
    let body_hash = fork_report_body_hash(&body)?;
    Ok(ForkReport {
        body,
        body_hash,
        generated_at_unix_ms: None,
        batpak_version: None,
        diagnostics: Vec::new(),
    })
}
