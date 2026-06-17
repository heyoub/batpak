//! Push-based subscriber fanout.
//!
//! Lossy push — slow subscribers are dropped, not retained. A subscriber
//! whose channel fills is treated as disconnected and removed from the
//! sender list; the writer thread must never be paced by a single slow
//! consumer. Callers who need every event must use `Cursor` (pull) or
//! `Subscription`-on-top-of-Cursor, not this fanout.

use crate::coordinate::{Coordinate, DagPosition, Region};
use crate::event::{EventKind, StoredEvent};
use flume::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;

/// Generic push-based notification fanout via bounded flume channels.
pub(crate) struct FanoutList<T: Clone + RegionFanoutItem> {
    senders: Mutex<Vec<FanoutSender<T>>>,
}

struct FanoutSender<T: Clone> {
    tx: Sender<T>,
    region: Region,
}

/// F8: region-filtered fanout for [`Notification`] subscribers.
///
/// Each subscriber is stored alongside the [`Region`] that defined it at
/// `subscribe` time. The broadcast path tests the predicate BEFORE pushing
/// onto the subscriber's channel, so a raw receiver handed out here only
/// ever sees in-region notifications. That closes the drift where
/// `Subscription::recv()` filtered on the consume side while
/// `Subscription::receiver()` exposed the unfiltered channel — async
/// consumers could observe unrelated events.
pub(crate) struct FilteredSubscriberList {
    senders: Mutex<Vec<FilteredSender>>,
}

struct FilteredSender {
    tx: Sender<Notification>,
    region: Region,
}

/// Private richer event envelope used by internal reactor consumers so they do
/// not need to re-read the just-committed event from disk.
#[derive(Clone, Debug)]
pub(crate) struct CommittedEventEnvelope {
    pub notification: Notification,
    pub stored: StoredEvent<serde_json::Value>,
}

/// Subscriber-facing list for push-based notifications. F8 replaces the
/// raw `FanoutList<Notification>` so per-subscriber region filtering is
/// applied at the writer push point rather than the consumer side.
pub(crate) type SubscriberList = FilteredSubscriberList;
pub(crate) type ReactorSubscriberList = FanoutList<CommittedEventEnvelope>;

pub(crate) trait RegionFanoutItem {
    fn matches_region(&self, region: &Region) -> bool;
}

/// Notification: lightweight event summary pushed to subscribers.
/// Must derive `Clone` because the writer fanout uses `try_send` broadcast loops.
#[derive(Clone, Debug)]
pub struct Notification {
    /// Unique ID of the event that was appended.
    pub event_id: u128,
    /// Correlation ID linking this event to a causal chain.
    pub correlation_id: u128,
    /// ID of the event that caused this one; `None` for root-cause events.
    pub causation_id: Option<u128>,
    /// Entity and scope coordinates for the event.
    pub coord: Coordinate,
    /// Event kind (type discriminant).
    pub kind: EventKind,
    /// Global sequence number assigned to this event at commit time.
    pub sequence: u64,
    /// Committed DAG position for the event.
    pub position: DagPosition,
}

pub(crate) fn notification_matches_region(region: &Region, value: &Notification) -> bool {
    region.matches_event(value.coord.entity(), value.coord.scope(), value.kind)
}

impl RegionFanoutItem for CommittedEventEnvelope {
    fn matches_region(&self, region: &Region) -> bool {
        notification_matches_region(region, &self.notification)
    }
}

impl<T: Clone + RegionFanoutItem> FanoutList<T> {
    pub(crate) fn new() -> Self {
        Self {
            senders: Mutex::new(Vec::new()),
        }
    }

    /// Subscribe with a [`Region`] filter applied before pushing to the receiver.
    pub(crate) fn subscribe_with_region(&self, capacity: usize, region: Region) -> Receiver<T> {
        let (tx, rx) = flume::bounded(capacity);
        self.senders.lock().push(FanoutSender { tx, region });
        rx
    }

    pub(crate) fn has_subscribers(&self) -> bool {
        !self.senders.lock().is_empty()
    }

    /// Broadcast: try_send to all, prune on Full OR Disconnected.
    /// NEVER use blocking send() — one slow subscriber must not block the writer.
    ///
    /// A `Full` subscriber is a slow subscriber. Retaining it would let the
    /// sender list grow without bound and keep a dead channel alive across
    /// every broadcast; we drop it immediately and treat the channel as
    /// disconnected for our purposes. Subscribers that need guaranteed
    /// delivery must use `Cursor` (pull), not this push fanout.
    ///
    /// [DEP:flume::Sender::try_send] → Result<(), TrySendError<T>>
    /// [DEP:flume::TrySendError::Full] / [DEP:flume::TrySendError::Disconnected]
    pub(crate) fn broadcast(&self, value: &T) {
        let mut guard = self.senders.lock();
        let subscribers_before = guard.len();
        guard.retain(|sub| {
            if !value.matches_region(&sub.region) {
                // Out of region: no push, but still prune a dropped receiver so
                // subscribers to quiet regions don't leak senders forever and
                // impose O(dead) work on every commit (audit R4).
                return !sub.tx.is_disconnected();
            }
            match sub.tx.try_send(value.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => false,
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
        tracing::trace!(
            target: "batpak::fanout",
            subscribers_before,
            subscribers_after = guard.len(),
            pruned = subscribers_before.saturating_sub(guard.len()),
            "reactor fanout try_send pass",
        );
    }
}

impl FilteredSubscriberList {
    pub(crate) fn new() -> Self {
        Self {
            senders: Mutex::new(Vec::new()),
        }
    }

    /// Subscribe with a [`Region`] filter applied at the writer push point.
    /// Returns the receiver half of a bounded channel that only ever sees
    /// notifications whose `(entity, scope, kind)` match the region.
    ///
    /// F8: this is the preferred subscribe path. Region filtering runs at
    /// the broadcast-push site so the raw receiver only ever contains
    /// in-region notifications; async consumers that poll the receiver
    /// directly cannot observe unrelated events.
    pub(crate) fn subscribe_with_region(
        &self,
        capacity: usize,
        region: Region,
    ) -> Receiver<Notification> {
        let (tx, rx) = flume::bounded(capacity);
        self.senders.lock().push(FilteredSender { tx, region });
        rx
    }

    /// F8: apply the per-subscriber region filter before `try_send`. A
    /// non-matching notification is not pushed at all; the subscriber
    /// stays in the list (it is NOT considered "Full"). Matching
    /// notifications follow the same `Full`/`Disconnected`-prune rule as
    /// the raw `FanoutList::broadcast`.
    pub(crate) fn broadcast(&self, value: &Notification) {
        let mut guard = self.senders.lock();
        let subscribers_before = guard.len();
        guard.retain(|sub| {
            match notification_matches_region(&sub.region, value) {
                // Out of region: no push, but prune a dropped receiver so quiet-
                // region subscribers don't leak senders forever (audit R4).
                false => !sub.tx.is_disconnected(),
                true => match sub.tx.try_send(value.clone()) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_)) => false,
                    Err(TrySendError::Disconnected(_)) => false,
                },
            }
        });
        tracing::trace!(
            target: "batpak::fanout",
            subscribers_before,
            subscribers_after = guard.len(),
            pruned = subscribers_before.saturating_sub(guard.len()),
            "subscription fanout try_send pass",
        );
    }
}

#[cfg(test)]
mod fanout_subscriber_tests {
    use super::{CommittedEventEnvelope, FanoutList, FilteredSubscriberList, Notification};
    use crate::coordinate::{Coordinate, DagPosition, Region};
    use crate::event::EventKind;

    #[test]
    fn has_subscribers_tracks_subscription_state() {
        // Pins `has_subscribers`: hardcoding it to `true` would make the writer
        // broadcast into an empty subscriber list on every commit.
        let fanout: FanoutList<CommittedEventEnvelope> = FanoutList::new();
        assert!(
            !fanout.has_subscribers(),
            "a freshly constructed fanout has no subscribers"
        );

        let _rx = fanout.subscribe_with_region(1, Region::all());
        assert!(
            fanout.has_subscribers(),
            "after a subscribe the fanout must report subscribers"
        );
    }

    fn notification(scope: &str) -> Notification {
        Notification {
            event_id: 1,
            correlation_id: 1,
            causation_id: None,
            coord: Coordinate::new("entity", scope).expect("coordinate"),
            kind: EventKind::DATA,
            sequence: 1,
            position: DagPosition::root(),
        }
    }

    #[test]
    fn dropped_out_of_region_subscriber_is_pruned_not_leaked() {
        // R4: a subscriber whose region never matches incoming events used to be
        // retained forever, even after its receiver was dropped — an unbounded
        // sender leak plus O(dead) work on every commit. Now an out-of-region
        // broadcast still prunes a disconnected sender.
        let list = FilteredSubscriberList::new();
        let rx = list.subscribe_with_region(4, Region::scope("alpha"));
        assert_eq!(list.senders.lock().len(), 1);

        drop(rx); // receiver gone -> channel disconnected

        // Broadcast targets scope "beta": out of the subscriber's region.
        list.broadcast(&notification("beta"));
        assert_eq!(
            list.senders.lock().len(),
            0,
            "a dropped out-of-region subscriber must be pruned (audit R4)"
        );
    }

    #[test]
    fn live_out_of_region_subscriber_is_retained() {
        // Dual direction: a still-connected subscriber must NOT be pruned just
        // because a broadcast was out of its region.
        let list = FilteredSubscriberList::new();
        let _rx = list.subscribe_with_region(4, Region::scope("alpha"));

        list.broadcast(&notification("beta")); // out of region, receiver alive
        assert_eq!(
            list.senders.lock().len(),
            1,
            "a live out-of-region subscriber must be retained"
        );
    }
}
