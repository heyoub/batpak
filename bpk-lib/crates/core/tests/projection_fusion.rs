//! Projection fusion behavior proofs.
//!
//! PROVES: INV-PROJECTION-FUSION-EQUIVALENT. Two projections over one entity
//! can be folded from one shared replay stream while each projection still sees
//! only its declared event-kind slice.
//! CATCHES: fused replay dropping one projection's kind, leaking unrelated
//! kinds into a projection fold, or diverging from separate consistent
//! projections.
//! SEEDED: interleaved event kinds on one entity plus an unrelated noise kind.

mod support;
use std::time::Duration;
use support::prelude::*;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const LEFT_KIND: EventKind = EventKind::custom(0xF, 51);
const RIGHT_KIND: EventKind = EventKind::custom(0xF, 52);
const NOISE_KIND: EventKind = EventKind::custom(0xF, 53);

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct LeftCount {
    count: usize,
}

impl EventSourced for LeftCount {
    type Input = JsonValueInput;

    fn from_events(events: &[ProjectionEvent<Self>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &ProjectionEvent<Self>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[LEFT_KIND]
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct LeftNone;

impl EventSourced for LeftNone {
    type Input = JsonValueInput;

    fn from_events(_events: &[ProjectionEvent<Self>]) -> Option<Self> {
        None
    }

    fn apply_event(&mut self, _event: &ProjectionEvent<Self>) {}

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

    fn from_events(events: &[ProjectionEvent<Self>]) -> Option<Self> {
        let total = events
            .iter()
            .filter_map(|event| event.payload.get("n"))
            .filter_map(serde_json::Value::as_u64)
            .sum::<u64>();
        (total > 0).then_some(Self { total })
    }

    fn apply_event(&mut self, event: &ProjectionEvent<Self>) {
        if let Some(n) = event.payload.get("n").and_then(serde_json::Value::as_u64) {
            self.total += n;
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[RIGHT_KIND]
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct NoiseCount {
    count: usize,
}

impl EventSourced for NoiseCount {
    type Input = JsonValueInput;

    fn from_events(events: &[ProjectionEvent<Self>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &ProjectionEvent<Self>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[NOISE_KIND]
    }
}

#[test]
fn fused_projection_matches_separate_consistent_projection() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fusion", "scope:fusion")?;
    store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 1 }))?;
    store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 2 }))?;
    store.append(&coord, NOISE_KIND, &serde_json::json!({ "n": 100 }))?;
    store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 3 }))?;
    store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 4 }))?;

    let separate_left: Option<LeftCount> =
        store.project("entity:fusion", &Freshness::Consistent)?;
    let separate_right: Option<RightTotal> =
        store.project("entity:fusion", &Freshness::Consistent)?;
    let fused: (Option<LeftCount>, Option<RightTotal>) = store.project_fused2("entity:fusion")?;

    assert_eq!(
        fused,
        (separate_left, separate_right),
        "PROPERTY: fused tuple fold must equal the pair of separate consistent folds"
    );
    assert_eq!(fused.0, Some(LeftCount { count: 2 }));
    assert_eq!(fused.1, Some(RightTotal { total: 6 }));
    store.close()?;
    Ok(())
}

#[test]
fn fused_three_projection_matches_separate_consistent_projection() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fusion-three", "scope:fusion")?;
    store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 1 }))?;
    store.append(&coord, RIGHT_KIND, &serde_json::json!({ "n": 2 }))?;
    store.append(&coord, NOISE_KIND, &serde_json::json!({ "n": 3 }))?;
    store.append(&coord, LEFT_KIND, &serde_json::json!({ "n": 4 }))?;

    let separate_left: Option<LeftCount> =
        store.project("entity:fusion-three", &Freshness::Consistent)?;
    let separate_right: Option<RightTotal> =
        store.project("entity:fusion-three", &Freshness::Consistent)?;
    let separate_noise: Option<NoiseCount> =
        store.project("entity:fusion-three", &Freshness::Consistent)?;
    let fused: batpak::store::ProjectionFusion3<LeftCount, RightTotal, NoiseCount> =
        store.project_fused3("entity:fusion-three")?;

    assert_eq!(
        fused,
        (separate_left, separate_right, separate_noise),
        "PROPERTY: public fused 3-projection tuple fold must equal separate consistent folds"
    );
    assert_eq!(fused.0, Some(LeftCount { count: 2 }));
    assert_eq!(fused.1, Some(RightTotal { total: 2 }));
    assert_eq!(fused.2, Some(NoiseCount { count: 1 }));
    store.close()?;
    Ok(())
}

#[test]
fn fused_projection_returns_empty_pair_for_missing_entity() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let fused: (Option<LeftCount>, Option<RightTotal>) = store.project_fused2("entity:missing")?;

    assert_eq!(fused, (None, None));
    store.close()?;
    Ok(())
}

#[test]
fn fused_projection_marks_consumed_none_projection_inputs_applied() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:fusion-none", "scope:fusion")?;
    store.append_with_options(
        &coord,
        LEFT_KIND,
        &serde_json::json!({ "n": 1 }),
        AppendOptions::new().with_position_hint(AppendPositionHint::branch_root(1, 0)),
    )?;
    let entry = store
        .latest_lane("entity:fusion-none", 1)
        .ok_or_else(|| std::io::Error::other("expected lane-1 fusion entry"))?;
    let point = HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    };

    let fused: (Option<LeftNone>, Option<RightTotal>) =
        store.project_fused2("entity:fusion-none")?;

    assert_eq!(
        fused,
        (None, None),
        "PROPERTY: the left projection may consume inputs and still return None"
    );
    store.wait_for_applied_lane(1, point, Duration::ZERO)?;
    store.close()?;
    Ok(())
}
