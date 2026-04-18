//! Push-based subscriber fanout.
//!
//! Lossy push — slow subscribers are dropped, not retained. A subscriber
//! whose channel fills is treated as disconnected and removed from the
//! sender list; the writer thread must never be paced by a single slow
//! consumer. Callers who need every event must use `Cursor` (pull) or
//! `Subscription`-on-top-of-Cursor, not this fanout.

use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{EventKind, StoredEvent};
use flume::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;

/// Generic push-based notification fanout via bounded flume channels.
pub(crate) struct FanoutList<T: Clone> {
    senders: Mutex<Vec<Sender<T>>>,
}

/// Private richer event envelope used by internal reactor consumers so they do
/// not need to re-read the just-committed event from disk.
#[derive(Clone, Debug)]
pub(crate) struct CommittedEventEnvelope {
    pub notification: Notification,
    pub stored: StoredEvent<serde_json::Value>,
}

pub(crate) type SubscriberList = FanoutList<Notification>;
pub(crate) type ReactorSubscriberList = FanoutList<CommittedEventEnvelope>;

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

impl<T: Clone> FanoutList<T> {
    pub(crate) fn new() -> Self {
        Self {
            senders: Mutex::new(Vec::new()),
        }
    }

    /// Subscribe: create a new bounded channel, store the sender, return the receiver.
    pub(crate) fn subscribe(&self, capacity: usize) -> Receiver<T> {
        let (tx, rx) = flume::bounded(capacity);
        self.senders.lock().push(tx);
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
        guard.retain(|tx| match tx.try_send(value.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}
