use super::ReplayInput;
use crate::event::{Event, EventKind, EventSourced, ProjectionInput};
use crate::store::index::{projection_kind_matches, ProjectionReplayItem};
use crate::store::{HlcPoint, ProjectionFusion3, Store, StoreError};
use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static FUSED_REPLAY_BATCH_READS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn reset_fused_replay_batch_reads() {
    FUSED_REPLAY_BATCH_READS.store(0, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn fused_replay_batch_reads() -> usize {
    FUSED_REPLAY_BATCH_READS.load(Ordering::SeqCst)
}

#[cfg(test)]
fn observe_fused_replay_batch_read() {
    FUSED_REPLAY_BATCH_READS.fetch_add(1, Ordering::SeqCst);
}

#[cfg(not(test))]
fn observe_fused_replay_batch_read() {}

pub(crate) fn project_fused2<Left, Right, State>(
    store: &Store<State>,
    entity: &str,
) -> Result<(Option<Left>, Option<Right>), StoreError>
where
    Left: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Right: EventSourced<Input = Left::Input>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + 'static,
    Left::Input: ReplayInput,
{
    let relevant_kinds = fused_relevant_kinds::<Left, Right>();
    let Some(plan) = store
        .index
        .projection_replay_plan(entity, relevant_kinds.as_slice())
    else {
        return Ok((None, None));
    };

    let positions: Vec<&crate::store::index::DiskPos> =
        plan.items.iter().map(|item| &item.disk_pos).collect();
    observe_fused_replay_batch_read();
    let events = Left::Input::read_batch(&store.reader, &positions)?;
    let (left_events, left_lanes) = filtered_projection_events::<Left, _>(&events, &plan.items);
    let (right_events, right_lanes) = filtered_projection_events::<Right, _>(&events, &plan.items);

    let left = Left::from_events(left_events.as_slice());
    let right = Right::from_events(right_events.as_slice());
    notify_projection_applied_lanes::<Left, State>(store, entity, &left_lanes);
    notify_projection_applied_lanes::<Right, State>(store, entity, &right_lanes);

    Ok((left, right))
}

pub(crate) fn project_fused3<First, Second, Third, State>(
    store: &Store<State>,
    entity: &str,
) -> Result<ProjectionFusion3<First, Second, Third>, StoreError>
where
    First: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Second: EventSourced<Input = First::Input>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + 'static,
    Third: EventSourced<Input = First::Input>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + 'static,
    First::Input: ReplayInput,
{
    let relevant_kinds = fused_relevant_kinds3::<First, Second, Third>();
    let Some(plan) = store
        .index
        .projection_replay_plan(entity, relevant_kinds.as_slice())
    else {
        return Ok((None, None, None));
    };

    let positions: Vec<&crate::store::index::DiskPos> =
        plan.items.iter().map(|item| &item.disk_pos).collect();
    observe_fused_replay_batch_read();
    let events = First::Input::read_batch(&store.reader, &positions)?;
    let (first_events, first_lanes) = filtered_projection_events::<First, _>(&events, &plan.items);
    let (second_events, second_lanes) =
        filtered_projection_events::<Second, _>(&events, &plan.items);
    let (third_events, third_lanes) = filtered_projection_events::<Third, _>(&events, &plan.items);

    let first = First::from_events(first_events.as_slice());
    let second = Second::from_events(second_events.as_slice());
    let third = Third::from_events(third_events.as_slice());
    notify_projection_applied_lanes::<First, State>(store, entity, &first_lanes);
    notify_projection_applied_lanes::<Second, State>(store, entity, &second_lanes);
    notify_projection_applied_lanes::<Third, State>(store, entity, &third_lanes);

    Ok((first, second, third))
}

fn fused_relevant_kinds<Left, Right>() -> Vec<EventKind>
where
    Left: EventSourced,
    Right: EventSourced,
{
    let left = Left::relevant_event_kinds();
    let right = Right::relevant_event_kinds();
    collect_relevant_kinds(&[left, right])
}

fn fused_relevant_kinds3<First, Second, Third>() -> Vec<EventKind>
where
    First: EventSourced,
    Second: EventSourced,
    Third: EventSourced,
{
    collect_relevant_kinds(&[
        First::relevant_event_kinds(),
        Second::relevant_event_kinds(),
        Third::relevant_event_kinds(),
    ])
}

fn collect_relevant_kinds(slices: &[&[EventKind]]) -> Vec<EventKind> {
    if slices.iter().any(|slice| slice.is_empty()) {
        return Vec::new();
    }

    let capacity = slices
        .iter()
        .fold(0usize, |total, slice| total.saturating_add(slice.len()));
    let mut kinds = Vec::with_capacity(capacity);
    for slice in slices {
        for kind in slice.iter().copied() {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    kinds
}

fn filtered_projection_events<T, I>(
    events: &[Event<I::Payload>],
    items: &[ProjectionReplayItem],
) -> (Vec<Event<I::Payload>>, BTreeMap<u32, HlcPoint>)
where
    T: EventSourced<Input = I>,
    I: ProjectionInput,
{
    let mut filtered = Vec::new();
    let mut lanes = BTreeMap::<u32, HlcPoint>::new();
    for (event, item) in events.iter().zip(items) {
        if projection_kind_matches(T::relevant_event_kinds(), event.event_kind()) {
            filtered.push(event.clone());
            lanes
                .entry(item.lane)
                .and_modify(|current| *current = (*current).max_by_sequence(item.point))
                .or_insert(item.point);
        }
    }
    (filtered, lanes)
}

fn notify_projection_applied_lanes<T, State>(
    store: &Store<State>,
    entity: &str,
    lanes: &BTreeMap<u32, HlcPoint>,
) where
    T: 'static,
{
    let projection_id =
        crate::store::projection::registry::ProjectionRegistry::id_for_type::<T>(entity);
    for (lane, point) in lanes {
        store
            .projection_registry
            .notify_applied_on_lane(projection_id.clone(), *lane, *point);
    }
}
