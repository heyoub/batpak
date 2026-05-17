//! Deterministic Batpak Substrate Closure subscriber frontier observations.
//!
//! This module reports structural delivery/frontier observations for lossy
//! subscribers and cursor-backed pull paths without imposing policy semantics.

use crate::store::Store;
use serde::{Deserialize, Serialize};

/// Report-body schema version for subscriber frontier evidence.
pub const SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION: u16 = 1;

/// Fixed-width hash used by subscriber frontier reports.
pub type SubscriberFrontierHash = [u8; 32];

/// Subscriber source lane being observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriberFrontierSource {
    /// Push-based lossy subscription.
    LossyPush,
    /// Cursor-backed pull path.
    CursorBacked,
}

/// Coarse delivery state for subscriber frontier observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriberDeliveryState {
    /// Subscriber is actively consuming.
    Active,
    /// Subscriber is behind available frontier.
    Lagging,
    /// Subscriber was dropped from a lossy path.
    Dropped,
    /// Delivery path disconnected.
    Disconnected,
    /// Caller could not determine the delivery state.
    Unknown,
}

/// Precision class for observed delivery loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LossPrecision {
    /// Exact dropped ranges are available.
    ExactRange,
    /// Loss can be localized only after a consumed frontier.
    LossAfterFrontier,
    /// Subscriber was dropped from the lossy fanout path.
    SubscriberDropped,
    /// Caller could not determine loss precision.
    Unknown,
}

/// Request input for subscriber frontier observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriberFrontierRequest {
    /// Observed source lane.
    pub source: SubscriberFrontierSource,
    /// Last consumed global sequence if known.
    pub consumed_frontier_sequence: Option<u64>,
    /// Delivery state observed by caller.
    pub delivery_state: SubscriberDeliveryState,
    /// Loss precision observed by caller.
    pub loss_precision: LossPrecision,
    /// Exact dropped ranges when `loss_precision == ExactRange`.
    pub exact_dropped_ranges: Vec<(u64, u64)>,
}

impl SubscriberFrontierRequest {
    /// Build a lossy push observation request.
    #[must_use]
    pub fn lossy_push(
        consumed_frontier_sequence: Option<u64>,
        delivery_state: SubscriberDeliveryState,
        loss_precision: LossPrecision,
    ) -> Self {
        Self {
            source: SubscriberFrontierSource::LossyPush,
            consumed_frontier_sequence,
            delivery_state,
            loss_precision,
            exact_dropped_ranges: Vec::new(),
        }
    }

    /// Build a cursor-backed observation request.
    #[must_use]
    pub fn cursor_backed(
        consumed_frontier_sequence: Option<u64>,
        delivery_state: SubscriberDeliveryState,
        loss_precision: LossPrecision,
    ) -> Self {
        Self {
            source: SubscriberFrontierSource::CursorBacked,
            consumed_frontier_sequence,
            delivery_state,
            loss_precision,
            exact_dropped_ranges: Vec::new(),
        }
    }
}

/// Deterministic structural findings from subscriber frontier observation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SubscriberFrontierFinding {
    /// Consumed frontier was not provided.
    ConsumedFrontierUnknown,
    /// Delivery state was unknown.
    DeliveryStateUnknown,
    /// Subscriber lag is observed in event-sequence units.
    LagObserved {
        /// Number of events between available and consumed frontiers.
        lag_events: u64,
    },
    /// Caller-reported consumed frontier is ahead of the available frontier.
    ConsumedFrontierAheadOfAvailable {
        /// Caller-reported consumed sequence.
        consumed_sequence: u64,
        /// Available sequence for the observed source lane.
        available_sequence: u64,
    },
    /// Delivery path reported dropped subscriber state.
    DeliveryDropped,
    /// Delivery path reported disconnection.
    DeliveryDisconnected,
    /// Loss is observed with the given precision class.
    LossObserved {
        /// Precision class for loss observations.
        precision: LossPrecision,
    },
    /// Exact dropped range when precision is available.
    ExactDroppedRange {
        /// Inclusive range start.
        start_sequence: u64,
        /// Exclusive range end.
        end_sequence: u64,
    },
}

/// Deterministic report body for subscriber frontier evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriberFrontierReportBody {
    /// Report-body schema version.
    pub schema_version: u16,
    /// Observed source lane.
    pub source: SubscriberFrontierSource,
    /// Last consumed frontier sequence.
    pub consumed_frontier_sequence: Option<u64>,
    /// Available frontier sequence relevant to the source lane.
    pub available_frontier_sequence: u64,
    /// Lag in events if consumed frontier is known.
    pub lag_events: Option<u64>,
    /// Coarse delivery state.
    pub delivery_state: SubscriberDeliveryState,
    /// Loss precision class.
    pub loss_precision: LossPrecision,
    /// Deterministic structural findings.
    pub findings: Vec<SubscriberFrontierFinding>,
}

/// Subscriber frontier evidence report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriberFrontierEvidenceReport {
    /// Deterministic report body.
    pub body: SubscriberFrontierReportBody,
    /// Canonical hash of `body`.
    pub body_hash: SubscriberFrontierHash,
    /// Optional generation timestamp metadata outside deterministic identity.
    pub generated_at_unix_ms: Option<u64>,
    /// Optional producer version metadata outside deterministic identity.
    pub batpak_version: Option<String>,
    /// Optional diagnostics outside deterministic identity.
    pub diagnostics: Vec<String>,
}

/// Error returned when subscriber frontier report generation fails.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubscriberFrontierReportError {
    /// Canonical report-body encoding failed.
    BodyEncoding {
        /// Human-readable encoding failure.
        message: String,
    },
}

impl std::fmt::Display for SubscriberFrontierReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyEncoding { message } => {
                write!(
                    f,
                    "subscriber frontier report body encoding failed: {message}"
                )
            }
        }
    }
}

impl std::error::Error for SubscriberFrontierReportError {}

impl<State> Store<State> {
    /// Build a deterministic subscriber frontier evidence report.
    ///
    /// # Errors
    /// Returns [`SubscriberFrontierReportError::BodyEncoding`] when canonical
    /// encoding of the deterministic report body fails.
    pub fn subscriber_frontier_observation(
        &self,
        request: &SubscriberFrontierRequest,
    ) -> Result<SubscriberFrontierEvidenceReport, SubscriberFrontierReportError> {
        let frontier = self.frontier();
        let available_frontier_sequence = match request.source {
            SubscriberFrontierSource::LossyPush => frontier.emitted_hlc.global_sequence,
            SubscriberFrontierSource::CursorBacked => frontier.visible_hlc.global_sequence,
        };

        let lag_events = request
            .consumed_frontier_sequence
            .map(|consumed| available_frontier_sequence.saturating_sub(consumed));

        let mut findings = Vec::new();
        if request.consumed_frontier_sequence.is_none() {
            findings.push(SubscriberFrontierFinding::ConsumedFrontierUnknown);
        }
        if request.delivery_state == SubscriberDeliveryState::Unknown {
            findings.push(SubscriberFrontierFinding::DeliveryStateUnknown);
        }
        if let Some(lag) = lag_events {
            if lag > 0 {
                findings.push(SubscriberFrontierFinding::LagObserved { lag_events: lag });
            }
        }
        if let Some(consumed_sequence) = request.consumed_frontier_sequence {
            if consumed_sequence > available_frontier_sequence {
                findings.push(
                    SubscriberFrontierFinding::ConsumedFrontierAheadOfAvailable {
                        consumed_sequence,
                        available_sequence: available_frontier_sequence,
                    },
                );
            }
        }

        match request.delivery_state {
            SubscriberDeliveryState::Dropped => {
                findings.push(SubscriberFrontierFinding::DeliveryDropped)
            }
            SubscriberDeliveryState::Disconnected => {
                findings.push(SubscriberFrontierFinding::DeliveryDisconnected)
            }
            SubscriberDeliveryState::Active
            | SubscriberDeliveryState::Lagging
            | SubscriberDeliveryState::Unknown => {}
        }

        if request.loss_precision != LossPrecision::Unknown {
            findings.push(SubscriberFrontierFinding::LossObserved {
                precision: request.loss_precision,
            });
        }
        if request.loss_precision == LossPrecision::ExactRange {
            let mut ranges = request.exact_dropped_ranges.clone();
            ranges.sort_unstable();
            for (start_sequence, end_sequence) in ranges {
                findings.push(SubscriberFrontierFinding::ExactDroppedRange {
                    start_sequence,
                    end_sequence,
                });
            }
        }

        crate::evidence::sort_findings(&mut findings);

        let body = SubscriberFrontierReportBody {
            schema_version: SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION,
            source: request.source,
            consumed_frontier_sequence: request.consumed_frontier_sequence,
            available_frontier_sequence,
            lag_events,
            delivery_state: request.delivery_state,
            loss_precision: request.loss_precision,
            findings,
        };
        let body_hash = report_body_hash(&body)?;
        Ok(SubscriberFrontierEvidenceReport {
            body,
            body_hash,
            generated_at_unix_ms: None,
            batpak_version: None,
            diagnostics: Vec::new(),
        })
    }
}

fn report_body_hash(
    body: &SubscriberFrontierReportBody,
) -> Result<SubscriberFrontierHash, SubscriberFrontierReportError> {
    crate::evidence::report_body_hash(body, |message| {
        SubscriberFrontierReportError::BodyEncoding { message }
    })
}

#[cfg(test)]
mod tests {
    use super::{LossPrecision, SubscriberFrontierFinding};

    #[test]
    fn subscriber_frontier_findings_are_sorted_structurally() {
        let mut findings = vec![
            SubscriberFrontierFinding::ExactDroppedRange {
                start_sequence: 20,
                end_sequence: 30,
            },
            SubscriberFrontierFinding::ExactDroppedRange {
                start_sequence: 10,
                end_sequence: 15,
            },
            SubscriberFrontierFinding::LossObserved {
                precision: LossPrecision::Unknown,
            },
        ];
        crate::evidence::sort_findings(&mut findings);

        assert_eq!(
            findings,
            vec![
                SubscriberFrontierFinding::LossObserved {
                    precision: LossPrecision::Unknown,
                },
                SubscriberFrontierFinding::ExactDroppedRange {
                    start_sequence: 10,
                    end_sequence: 15,
                },
                SubscriberFrontierFinding::ExactDroppedRange {
                    start_sequence: 20,
                    end_sequence: 30,
                },
            ],
            "PROPERTY: subscriber frontier findings must be sorted in deterministic structural order"
        );
    }
}
