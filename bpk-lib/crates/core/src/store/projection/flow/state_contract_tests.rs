use crate::event::{Event, EventKind, EventSourced};
use crate::store::{Freshness, Store, StoreConfig, StoreError};
use std::error::Error;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct UnspecifiedProjection;

impl EventSourced for UnspecifiedProjection {
    type Input = crate::event::JsonValueInput;

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        std::hint::black_box(event.event_kind());
    }

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self)
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 71)];
        &KINDS
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OverBoundProjection {
    count: u64,
}

impl EventSourced for OverBoundProjection {
    type Input = crate::event::JsonValueInput;
    const STATE_CONTRACT: crate::event::ProjectionStateContract =
        crate::event::ProjectionStateContract::bounded(
            "projection-flow-over-bound",
            1,
            "single event retained",
            "projection cache overwrite",
            "projection cache",
        );

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        std::hint::black_box(event.event_kind());
        self.count += 1;
    }

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: u64::try_from(events.len()).expect("test corpus fits u64"),
        })
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 72)];
        &KINDS
    }

    fn state_extent(&self) -> crate::event::StateExtent {
        crate::event::StateExtent::cardinality(
            self.count,
            crate::event::StateExtentCost::ConstantTime,
        )
    }
}

#[test]
fn unspecified_projection_contract_is_rejected() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = crate::coordinate::Coordinate::new("entity:unspecified-contract", "scope:test")?;
    let _receipt = store.append(
        &coord,
        EventKind::custom(0xF, 71),
        &serde_json::json!({"n": 1}),
    )?;

    let err = store
        .project::<UnspecifiedProjection>("entity:unspecified-contract", &Freshness::Consistent)
        .expect_err("unspecified projection contract must fail closed");
    assert!(
        matches!(err, StoreError::ProjectionStateContractUnspecified { .. }),
        "PROPERTY: unspecified projection contract must fail with the exact state-contract error, got {err:?}"
    );
    Ok(())
}

#[test]
fn projection_exceeding_declared_state_bound_is_rejected() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = crate::coordinate::Coordinate::new("entity:over-bound", "scope:test")?;
    for n in [1_u8, 2] {
        let _receipt = store.append(
            &coord,
            EventKind::custom(0xF, 72),
            &serde_json::json!({ "n": n }),
        )?;
    }

    let err = store
        .project::<OverBoundProjection>("entity:over-bound", &Freshness::Consistent)
        .expect_err("projection above declared state bound must fail closed");
    assert!(
        matches!(err, StoreError::ProjectionStateBoundExceeded { .. }),
        "PROPERTY: over-bound projection must fail with the exact state-bound error, got {err:?}"
    );
    Ok(())
}
