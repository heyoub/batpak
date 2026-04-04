use crate::coordinate::Coordinate;
use crate::event::{Event, EventKind};

/// `EventSourced<P>`: backward-looking fold. Replay events to reconstruct state.
/// P is generic — NO serde_json dependency in the trait.
/// Store uses EventSourced<serde_json::Value>. [SPEC:src/event/sourcing.rs]
pub trait EventSourced<P>: Sized {
    /// Reconstructs state by folding over a slice of events; returns `None` if the slice is empty or invalid.
    fn from_events(events: &[Event<P>]) -> Option<Self>;
    /// Advances state by incorporating a single event.
    fn apply_event(&mut self, event: &Event<P>);
    /// Returns the event kinds this type cares about, used to filter store queries.
    /// The store uses this as a hard filter: only matching events are loaded from disk
    /// and passed to `from_events()`. Empty slice means "no filter — replay all events."
    fn relevant_event_kinds() -> &'static [EventKind];

    /// Schema version for projection cache isolation. Increment this when the
    /// serialized shape of this type changes in a breaking way. Default: 0.
    /// Different versions get separate cache keys — old cached projections
    /// are not served to new code.
    fn schema_version() -> u64 {
        0
    }

    /// Returns `true` if this type supports incremental application: loading a
    /// cached state at a watermark and calling `apply_event()` only for events
    /// newer than that watermark, instead of replaying from scratch.
    ///
    /// Opt-in — `false` by default. Only set to `true` if `from_events()` is a
    /// pure fold over `apply_event()` (i.e., the incremental result is identical
    /// to the full-replay result for any suffix of events).
    fn supports_incremental_apply() -> bool {
        false
    }

}

/// `Reactive<P>`: forward-looking counterpart. See event → maybe emit derived events.
/// Products compose: subscribe + react + append (7 lines of glue).
/// [SPEC:src/event/sourcing.rs]
///
/// # Manual Glue Pattern
/// ```no_run
/// # use batpak::prelude::*;
/// # use batpak::event::sourcing::Reactive;
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
    /// Inspects an incoming event and returns zero or more derived events to append.
    fn react(&self, event: &Event<P>) -> Vec<(Coordinate, EventKind, P)>;
}
