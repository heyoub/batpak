use crate::store::index::IndexEntry;
use crate::store::platform::spawn::SimJoin;
use crate::store::write::fanout::Notification;
use crate::store::StoreError;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// One pulled canal batch without forcing one allocation for one-item canals.
#[derive(Debug)]
pub enum CanalBatch<I> {
    /// No matching item was available before the deadline.
    Empty,
    /// Exactly one item was available.
    One(I),
    /// More than one item was available.
    Many(Vec<I>),
}

impl<I> CanalBatch<I> {
    /// Returns true when this batch contains no item.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// Minimal event reference yielded by a [`Canal`].
pub trait CanalItem {
    /// Event id to fetch from the store replay lane.
    fn event_id(&self) -> crate::id::EventId;
}

impl CanalItem for IndexEntry {
    fn event_id(&self) -> crate::id::EventId {
        self.event_id()
    }
}

impl CanalItem for Notification {
    fn event_id(&self) -> crate::id::EventId {
        self.event_id
    }
}

/// Terminal canal closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanalClosed;

impl std::fmt::Display for CanalClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "canal closed")
    }
}

impl std::error::Error for CanalClosed {}

/// Common consumption surface over shipped delivery primitives.
///
/// Implementors keep their own ordering, backpressure, durability, restart,
/// checkpoint, and witness contracts. `Canal` standardises only "produce the
/// next batch the caller should inspect".
pub trait Canal: Send {
    /// Per-item unit yielded by this canal.
    type Item: CanalItem + Send;
    /// Error returned by a terminal or failed pull.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Pull up to `max` items, blocking for at most `deadline` when no item is
    /// immediately available.
    ///
    /// An empty batch means timeout/idle. An error means the canal cannot
    /// produce more items and the caller should stop.
    ///
    /// # Errors
    /// Returns the implementation's terminal error when the canal is closed or
    /// can no longer produce items.
    fn pull_batch(
        &mut self,
        max: usize,
        deadline: Duration,
    ) -> Result<CanalBatch<Self::Item>, Self::Error>;
}

/// Lifecycle for a running canal-backed worker.
pub trait CanalHandle: Send {
    /// Signal stop without blocking.
    fn stop(&self);
    /// Wait passively for worker exit.
    ///
    /// # Errors
    /// Returns a store error when the worker panicked or stashed a terminal
    /// store-level failure before exiting.
    fn join(self: Box<Self>) -> Result<(), StoreError>;
    /// Signal stop, then wait for worker exit.
    ///
    /// # Errors
    /// Returns the same failures as [`join`](Self::join).
    fn stop_and_join(self: Box<Self>) -> Result<(), StoreError>;
}

/// Handle for lossy subscription-backed workers.
pub(crate) struct SubscriptionWorkerHandle {
    stop: Arc<AtomicBool>,
    join: Option<Box<dyn SimJoin>>,
    error_slot: Arc<Mutex<Option<StoreError>>>,
}

impl SubscriptionWorkerHandle {
    pub(crate) fn new(
        stop: Arc<AtomicBool>,
        join: Box<dyn SimJoin>,
        error_slot: Arc<Mutex<Option<StoreError>>>,
    ) -> Self {
        Self {
            stop,
            join: Some(join),
            error_slot,
        }
    }

    fn finish_join(&mut self) -> Result<(), StoreError> {
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| StoreError::WriterCrashed)?;
        }
        let mut guard = self.error_slot.lock();
        guard.take().map_or(Ok(()), Err)
    }
}

impl CanalHandle for SubscriptionWorkerHandle {
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn join(mut self: Box<Self>) -> Result<(), StoreError> {
        self.finish_join()
    }

    fn stop_and_join(mut self: Box<Self>) -> Result<(), StoreError> {
        self.stop();
        self.finish_join()
    }
}

impl Drop for SubscriptionWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Delivery canal used by typed reactor runners.
///
/// This is intentionally a selector over existing primitives, not a new owner
/// of delivery semantics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReactorCanal {
    /// Ordered pull replay through [`Cursor`](crate::store::Cursor).
    ///
    /// This is the default typed-reactor canal. It is at-least-once within the
    /// process and can become durable at-least-once when the reactor carries a
    /// checkpoint id.
    #[default]
    CursorGuaranteed,
    /// Lossy push observation through [`Subscription`](crate::store::Subscription).
    ///
    /// This keeps writer isolation and does not checkpoint, restart, or provide
    /// an [`AtLeastOnce`](crate::store::AtLeastOnce) witness. Use it only for
    /// live views that may skip work under backpressure.
    LossySubscription,
}
