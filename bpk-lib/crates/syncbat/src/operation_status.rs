//! Durable operation-status facts and event-sourced view state.

use batpak::event::sourcing::{
    EventSourced, ProjectionEvent, ProjectionStateContract, RawMsgpackInput, StateExtent,
};
use batpak::event::{EventKind, EventPayload};
use serde::{Deserialize, Serialize};

/// Batpak custom event kind for syncbat operation-status facts.
pub const SYNCBAT_OPERATION_STATUS_EVENT_KIND: EventKind = EventKind::custom(0xC, 0x5B9);

/// Terminal or in-flight lifecycle phase carried by a status fact.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatusLifecycle {
    /// Checkout passed admission and the handler is running or has started.
    #[default]
    Started,
    /// Handler completed successfully.
    Completed,
    /// Handler returned a failure outcome.
    Failed,
    /// Admission or runtime policy denied the checkout.
    Denied,
}

/// One durable operation-status fact appended for an operation entity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationStatusFactV1 {
    /// Fact schema version.
    pub schema_version: u32,
    /// Operation name this fact describes.
    pub operation: String,
    /// Lifecycle phase recorded by this fact.
    pub lifecycle: OperationStatusLifecycle,
    /// Receipt kind from the operation descriptor, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_kind: Option<String>,
    /// Stable outcome or denial class for terminal facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Human-readable detail for terminal facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Optional hash of checkout input bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<[u8; 32]>,
    /// Optional hash of checkout output bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<[u8; 32]>,
}

impl OperationStatusFactV1 {
    /// Current fact schema version.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Build a started fact for one checkout attempt.
    #[must_use]
    pub fn started(operation: impl Into<String>, receipt_kind: impl Into<String>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            operation: operation.into(),
            lifecycle: OperationStatusLifecycle::Started,
            receipt_kind: Some(receipt_kind.into()),
            code: None,
            message: None,
            input_hash: None,
            output_hash: None,
        }
    }

    /// Build a terminal fact from a checkout outcome.
    #[must_use]
    pub fn terminal(
        operation: impl Into<String>,
        lifecycle: OperationStatusLifecycle,
        receipt_kind: impl Into<String>,
        code: Option<String>,
        message: Option<String>,
        input_hash: Option<[u8; 32]>,
        output_hash: Option<[u8; 32]>,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            operation: operation.into(),
            lifecycle,
            receipt_kind: Some(receipt_kind.into()),
            code,
            message,
            input_hash,
            output_hash,
        }
    }
}

impl EventPayload for OperationStatusFactV1 {
    const KIND: EventKind = SYNCBAT_OPERATION_STATUS_EVENT_KIND;
}

/// Event-sourced aggregate view over [`OperationStatusFactV1`] events.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationStatusView {
    /// View schema version.
    pub schema_version: u32,
    /// Operation name bound to this entity stream.
    pub operation: String,
    /// Latest lifecycle phase observed in the fact stream.
    pub lifecycle: OperationStatusLifecycle,
    /// Total checkout attempts observed (started + denied-without-start).
    pub attempts_seen: u64,
    /// Count of started checkouts.
    pub started_count: u64,
    /// Count of completed checkouts.
    pub completed_count: u64,
    /// Count of failed checkouts.
    pub failed_count: u64,
    /// Count of denied checkouts.
    pub denied_count: u64,
    /// Last recorded phase label (`started`, `completed`, `failed`, `denied`).
    pub last_phase: Option<String>,
    /// Receipt kind from the latest terminal fact.
    pub last_receipt_kind: Option<String>,
    /// Stable class from the latest terminal fact.
    pub last_code: Option<String>,
    /// Message from the latest terminal fact.
    pub last_message: Option<String>,
    /// Input hash from the latest terminal fact.
    pub last_input_hash: Option<[u8; 32]>,
    /// Output hash from the latest terminal fact.
    pub last_output_hash: Option<[u8; 32]>,
}

impl OperationStatusView {
    /// Current view schema version.
    pub const SCHEMA_VERSION: u32 = 1;
}

impl EventSourced for OperationStatusView {
    type Input = RawMsgpackInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("syncbat-operation-status-view");

    fn from_events(events: &[ProjectionEvent<Self>]) -> Option<Self> {
        let mut view = None;
        for event in events {
            let fact = decode_fact(event)?;
            match &mut view {
                None => view = Some(Self::apply_fact(None, &fact)),
                Some(current) => Self::apply_fact_to(current, &fact),
            }
        }
        view
    }

    fn apply_event(&mut self, event: &ProjectionEvent<Self>) {
        if let Some(fact) = decode_fact(event) {
            Self::apply_fact_to(self, &fact);
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[SYNCBAT_OPERATION_STATUS_EVENT_KIND]
    }

    fn schema_version() -> u64 {
        u64::from(Self::SCHEMA_VERSION)
    }

    fn supports_incremental_apply() -> bool {
        true
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

impl OperationStatusView {
    fn apply_fact(existing: Option<Self>, fact: &OperationStatusFactV1) -> Self {
        let mut view = existing.unwrap_or_else(|| Self {
            schema_version: Self::SCHEMA_VERSION,
            operation: fact.operation.clone(),
            lifecycle: OperationStatusLifecycle::Started,
            attempts_seen: 0,
            started_count: 0,
            completed_count: 0,
            failed_count: 0,
            denied_count: 0,
            last_phase: None,
            last_receipt_kind: None,
            last_code: None,
            last_message: None,
            last_input_hash: None,
            last_output_hash: None,
        });
        Self::apply_fact_to(&mut view, fact);
        view
    }

    fn apply_fact_to(view: &mut Self, fact: &OperationStatusFactV1) {
        view.operation = fact.operation.clone();
        view.lifecycle = fact.lifecycle.clone();
        match fact.lifecycle {
            OperationStatusLifecycle::Started => {
                view.attempts_seen = view.attempts_seen.saturating_add(1);
                view.started_count = view.started_count.saturating_add(1);
                view.last_phase = Some("started".to_owned());
            }
            OperationStatusLifecycle::Completed => {
                view.completed_count = view.completed_count.saturating_add(1);
                view.last_phase = Some("completed".to_owned());
                update_terminal_fields(view, fact);
            }
            OperationStatusLifecycle::Failed => {
                view.failed_count = view.failed_count.saturating_add(1);
                view.last_phase = Some("failed".to_owned());
                update_terminal_fields(view, fact);
            }
            OperationStatusLifecycle::Denied => {
                if !has_open_attempt(view) {
                    view.attempts_seen = view.attempts_seen.saturating_add(1);
                }
                view.denied_count = view.denied_count.saturating_add(1);
                view.last_phase = Some("denied".to_owned());
                update_terminal_fields(view, fact);
            }
        }
    }
}

fn has_open_attempt(view: &OperationStatusView) -> bool {
    let terminal_count = view
        .completed_count
        .saturating_add(view.failed_count)
        .saturating_add(view.denied_count);
    view.started_count > terminal_count
}

fn update_terminal_fields(view: &mut OperationStatusView, fact: &OperationStatusFactV1) {
    view.last_receipt_kind = fact.receipt_kind.clone();
    view.last_code = fact.code.clone();
    view.last_message = fact.message.clone();
    view.last_input_hash = fact.input_hash;
    view.last_output_hash = fact.output_hash;
}

fn decode_fact(event: &ProjectionEvent<OperationStatusView>) -> Option<OperationStatusFactV1> {
    if event.header.event_kind != SYNCBAT_OPERATION_STATUS_EVENT_KIND {
        return None;
    }
    batpak::canonical::from_bytes(event.payload.as_slice()).ok()
}
