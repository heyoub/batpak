use crate::coordinate::Coordinate;
use crate::event::{Event, EventKind};

/// `EventSourced<P>`: backward-looking fold. Replay events to reconstruct state.
/// P is generic — NO serde_json dependency in the trait.
/// Store uses EventSourced<serde_json::Value>. [SPEC:src/event/sourcing.rs]
pub trait EventSourced<P>: Sized {
    fn from_events(events: &[Event<P>]) -> Option<Self>;
    fn apply_event(&mut self, event: &Event<P>);
    fn relevant_event_kinds() -> &'static [EventKind];
}

/// `Reactive<P>`: forward-looking counterpart. See event → maybe emit derived events.
/// Products compose: subscribe + react + append (7 lines of glue).
/// [SPEC:src/event/sourcing.rs]
pub trait Reactive<P> {
    fn react(&self, event: &Event<P>) -> Vec<(Coordinate, EventKind, P)>;
}
