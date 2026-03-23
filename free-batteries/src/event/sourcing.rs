use crate::coordinate::Coordinate;
use crate::event::{Event, EventKind};

/// EventSourced<P>: backward-looking fold. Replay events to reconstruct state.
/// P is generic — NO serde_json dependency in the trait.
/// Store uses EventSourced<serde_json::Value>. [SPEC:src/event/sourcing.rs]
pub trait EventSourced<P>: Sized {
    fn from_events(events: &[Event<P>]) -> Option<Self>;
    fn apply_event(&mut self, event: &Event<P>);
    fn relevant_event_kinds() -> &'static [EventKind];
}

/// Reactive<P>: forward-looking counterpart. See event → maybe emit derived events.
/// Products compose: subscribe + react + append (7 lines of glue).
/// [SPEC:src/event/sourcing.rs]
///
/// # Manual Glue Pattern
/// ```no_run
/// # use free_batteries::prelude::*;
/// # use free_batteries::event::sourcing::Reactive;
/// # struct MyReactor;
/// # impl Reactive<serde_json::Value> for MyReactor {
/// #     fn react(&self, _event: &Event<serde_json::Value>) -> Vec<(Coordinate, EventKind, serde_json::Value)> { vec![] }
/// # }
/// # fn example(store: &Store, reactor: &MyReactor) {
/// let region = Region::entity("order:*");
/// let sub = store.subscribe(&region);
/// while let Some(notif) = sub.recv() {
///     let stored = store.get(notif.event_id).unwrap();
///     for (coord, kind, payload) in reactor.react(&stored.event) {
///         store.append_reaction(&coord, kind, &payload, notif.correlation_id, notif.event_id).unwrap();
///     }
/// }
/// # }
/// ```
///
/// For convenience, use [`Store::react_loop`](crate::store::Store::react_loop) which
/// spawns a thread running this pattern automatically.
pub trait Reactive<P> {
    fn react(&self, event: &Event<P>) -> Vec<(Coordinate, EventKind, P)>;
}
