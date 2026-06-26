use super::*;
use crate::coordinate::Coordinate;
use crate::event::{Event, EventKind, EventSourced, JsonValueInput};
use crate::store::{Freshness, Store, StoreConfig};
use std::sync::Mutex;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

static FUSION_TEST_LOCK: Mutex<()> = Mutex::new(());

const LEFT_KIND: EventKind = EventKind::custom(0xF, 41);
const RIGHT_KIND: EventKind = EventKind::custom(0xF, 42);
const OVERLAP_KIND: EventKind = EventKind::custom(0xF, 43);
const NOISE_KIND: EventKind = EventKind::custom(0xF, 44);

macro_rules! single_entity_state_contract {
    ($key_space:literal) => {
        const STATE_CONTRACT: crate::event::ProjectionStateContract =
            crate::event::ProjectionStateContract::single_entity($key_space);

        fn state_extent(&self) -> crate::event::StateExtent {
            crate::event::StateExtent::single_entity()
        }
    };
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct LeftCount {
    count: usize,
}

impl EventSourced for LeftCount {
    type Input = JsonValueInput;
    single_entity_state_contract!("fusion-left-count");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[LEFT_KIND]
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct RightTotal {
    total: u64,
}

impl EventSourced for RightTotal {
    type Input = JsonValueInput;
    single_entity_state_contract!("fusion-right-total");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        let total = events
            .iter()
            .filter_map(|event| event.payload.get("n"))
            .filter_map(serde_json::Value::as_u64)
            .sum::<u64>();
        (total > 0).then_some(Self { total })
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        self.total += event
            .payload
            .get("n")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[RIGHT_KIND]
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct OverlapCount {
    count: usize,
}

impl EventSourced for OverlapCount {
    type Input = JsonValueInput;
    single_entity_state_contract!("fusion-overlap-count");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[OVERLAP_KIND]
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct OverlapTotal {
    total: u64,
}

impl EventSourced for OverlapTotal {
    type Input = JsonValueInput;
    single_entity_state_contract!("fusion-overlap-total");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        let total = events
            .iter()
            .filter_map(|event| event.payload.get("n"))
            .filter_map(serde_json::Value::as_u64)
            .sum::<u64>();
        (total > 0).then_some(Self { total })
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        self.total += event
            .payload
            .get("n")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[OVERLAP_KIND]
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct MatchAllCount {
    count: usize,
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct NoiseCount {
    count: usize,
}

impl EventSourced for NoiseCount {
    type Input = JsonValueInput;
    single_entity_state_contract!("fusion-noise-count");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[NOISE_KIND]
    }
}

impl EventSourced for MatchAllCount {
    type Input = JsonValueInput;
    single_entity_state_contract!("fusion-match-all-count");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[]
    }
}

#[test]
fn fused_direct_replay_reads_shared_stream_once() -> TestResult {
    let _lock = match FUSION_TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fused-once", "scope:fused")?;
    let _ = store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 1 }))?;
    let _ = store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 2 }))?;
    let _ = store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 3 }))?;

    reset_fused_replay_batch_reads();
    let fused: (Option<LeftCount>, Option<RightTotal>) =
        store.project_fused2("entity:fused-once")?;

    assert_eq!(fused.0, Some(LeftCount { count: 1 }));
    assert_eq!(fused.1, Some(RightTotal { total: 5 }));
    assert_eq!(
        fused_replay_batch_reads(),
        1,
        "PROPERTY: fused projection must batch-read the shared replay stream once"
    );
    store.close()?;
    Ok(())
}

#[test]
fn fused_result_matches_separate_consistent_projections() -> TestResult {
    let _lock = match FUSION_TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fused-equiv", "scope:fused")?;
    let _ = store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 1 }))?;
    let _ = store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 2 }))?;
    let _ = store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 3 }))?;

    let separate_left: Option<LeftCount> =
        store.project("entity:fused-equiv", &Freshness::Consistent)?;
    let separate_right: Option<RightTotal> =
        store.project("entity:fused-equiv", &Freshness::Consistent)?;
    let fused: (Option<LeftCount>, Option<RightTotal>) =
        store.project_fused2("entity:fused-equiv")?;

    assert_eq!(fused, (separate_left, separate_right));
    store.close()?;
    Ok(())
}

#[test]
fn fused_overlapping_kinds_match_separate_projections_and_batch_read_once() -> TestResult {
    let _lock = match FUSION_TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fused-overlap", "scope:fused")?;
    let _ = store.append(&coord, OVERLAP_KIND, &serde_json::json!({ "n": 2 }))?;
    let _ = store.append(&coord, NOISE_KIND, &serde_json::json!({ "n": 100 }))?;
    let _ = store.append(&coord, OVERLAP_KIND, &serde_json::json!({ "n": 5 }))?;

    let separate_count: Option<OverlapCount> =
        store.project("entity:fused-overlap", &Freshness::Consistent)?;
    let separate_total: Option<OverlapTotal> =
        store.project("entity:fused-overlap", &Freshness::Consistent)?;
    reset_fused_replay_batch_reads();
    let fused: (Option<OverlapCount>, Option<OverlapTotal>) =
        store.project_fused2("entity:fused-overlap")?;

    assert_eq!(
        fused,
        (separate_count, separate_total),
        "PROPERTY: overlapping-kind fused replay must equal separate projections"
    );
    assert_eq!(
        fused_replay_batch_reads(),
        1,
        "PROPERTY: overlapping-kind fusion must still batch-read the shared replay stream once"
    );
    store.close()?;
    Ok(())
}

#[test]
fn fused_empty_relevant_kind_projection_matches_all_events() -> TestResult {
    let _lock = match FUSION_TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fused-match-all", "scope:fused")?;
    let _ = store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 1 }))?;
    let _ = store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 2 }))?;
    let _ = store.append(&coord, NOISE_KIND, &serde_json::json!({ "n": 3 }))?;

    reset_fused_replay_batch_reads();
    let fused: (Option<MatchAllCount>, Option<RightTotal>) =
        store.project_fused2("entity:fused-match-all")?;

    assert_eq!(
        fused,
        (
            Some(MatchAllCount { count: 3 }),
            Some(RightTotal { total: 2 })
        ),
        "PROPERTY: an empty relevant_event_kinds slice means match all events in fused replay"
    );
    assert_eq!(
        fused_replay_batch_reads(),
        1,
        "PROPERTY: match-all fusion must batch-read the shared replay stream once"
    );
    store.close()?;
    Ok(())
}

#[test]
fn fused_three_projection_tuple_batch_reads_once() -> TestResult {
    let _lock = match FUSION_TEST_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fused-three", "scope:fused")?;
    let _ = store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 1 }))?;
    let _ = store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 2 }))?;
    let _ = store.append(&coord, NOISE_KIND, &serde_json::json!({ "n": 3 }))?;

    let separate_left: Option<LeftCount> =
        store.project("entity:fused-three", &Freshness::Consistent)?;
    let separate_right: Option<RightTotal> =
        store.project("entity:fused-three", &Freshness::Consistent)?;
    let separate_noise: Option<NoiseCount> =
        store.project("entity:fused-three", &Freshness::Consistent)?;
    reset_fused_replay_batch_reads();
    let fused: (Option<LeftCount>, Option<RightTotal>, Option<NoiseCount>) =
        store.project_fused3("entity:fused-three")?;

    assert_eq!(
        fused,
        (separate_left, separate_right, separate_noise),
        "PROPERTY: fused 3-projection tuple fold must equal separate consistent folds"
    );
    assert_eq!(
        fused_replay_batch_reads(),
        1,
        "PROPERTY: fused 3-projection tuple must batch-read the shared replay stream once"
    );
    store.close()?;
    Ok(())
}
