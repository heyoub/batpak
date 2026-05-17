use crate::coordinate::Region;
use crate::store::delivery::canal::{Canal, CanalBatch, CanalClosed};
use crate::store::write::fanout::{notification_matches_region, Notification};
use flume::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::Duration;

/// Subscription: push-based per-subscriber flume channel. Lossy.
/// If subscriber is slow, bounded channel fills. Writer's retain() prunes.
/// For ordered pull delivery with optional durable checkpoints, use Cursor
/// instead.
pub struct Subscription {
    rx: Receiver<Notification>,
    region: Region,
}

impl Subscription {
    pub(crate) fn new(rx: Receiver<Notification>, region: Region) -> Self {
        Self { rx, region }
    }

    /// Blocking receive. Filters by region. Returns None if channel closed.
    ///
    /// The filter loop is unbounded: if the region filter matches only a
    /// rare event kind but the underlying stream is high-throughput,
    /// `recv()` may loop internally for many notifications before
    /// returning a match. Callers who need timeout semantics should
    /// use [`Subscription::filtered_receiver`] and drive the returned
    /// channel with `recv_deadline`; the filter is applied at the
    /// writer push point so no filter loop is required on the consumer
    /// side.
    pub fn recv(&self) -> Option<Notification> {
        loop {
            match self.rx.recv() {
                Ok(notif) => {
                    // Filter: only return events matching our region.
                    // [FILE:src/coordinate/mod.rs — Region::matches_event]
                    if notification_matches_region(&self.region, &notif) {
                        return Some(notif);
                    }
                    // Didn't match — keep receiving
                }
                Err(_) => return None, // channel closed
            }
        }
    }

    /// F8: region-preserving receiver for async / deadline-driven
    /// consumers. Returns a `flume::Receiver<Notification>` whose
    /// contents are pre-filtered at the writer push point — no
    /// out-of-region notification is ever placed into the channel. Use
    /// this in preference to the raw [`Subscription::receiver`] shim:
    /// `sub.filtered_receiver().recv_async().await`.
    ///
    /// The returned receiver is borrowed (`&Receiver<_>`); the
    /// subscription owns the channel lifetime. The underlying channel
    /// was registered with this subscription's region when the
    /// subscription was created, so the filter contract is established
    /// at the writer side — this accessor simply re-exposes that
    /// already-filtered channel.
    ///
    /// ASYNC NOTE: use this for async event consumption. For Store
    /// methods (append, get, query), use spawn_blocking instead — two
    /// different patterns.
    pub fn filtered_receiver(&self) -> &Receiver<Notification> {
        &self.rx
    }

    /// F8: legacy raw-receiver accessor. Retained under
    /// `#[doc(hidden)]` so existing async consumers keep compiling, but
    /// new callers should use [`Subscription::filtered_receiver`] — the
    /// semantics are identical (both receivers are pre-filtered at the
    /// writer push point since F8), and the name advertises the
    /// contract.
    #[doc(hidden)]
    pub fn receiver(&self) -> &Receiver<Notification> {
        &self.rx
    }

    /// Create a composable ops wrapper for chainable filter/map/take.
    pub fn ops(self) -> SubscriptionOps {
        SubscriptionOps {
            sub: self,
            filters: Vec::new(),
            map_fn: None,
            limit: None,
            count: 0,
        }
    }
}

impl Canal for Subscription {
    type Item = Notification;
    type Error = CanalClosed;

    fn pull_batch(
        &mut self,
        max: usize,
        deadline: Duration,
    ) -> Result<CanalBatch<Self::Item>, Self::Error> {
        if max == 0 {
            return Ok(CanalBatch::Empty);
        }
        let first = match self.rx.recv_timeout(deadline) {
            Ok(notification) => notification,
            Err(RecvTimeoutError::Timeout) => return Ok(CanalBatch::Empty),
            Err(RecvTimeoutError::Disconnected) => return Err(CanalClosed),
        };
        if max == 1 {
            return Ok(CanalBatch::One(first));
        }

        let mut rest = Vec::new();
        while rest.len() + 1 < max {
            match self.rx.try_recv() {
                Ok(notification) => rest.push(notification),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(CanalClosed),
            }
        }
        if rest.is_empty() {
            Ok(CanalBatch::One(first))
        } else {
            let mut items = Vec::with_capacity(rest.len() + 1);
            items.push(first);
            items.extend(rest);
            Ok(CanalBatch::Many(items))
        }
    }
}

/// SubscriptionOps: composable stream wrapper over Subscription.
/// Chains filter/take operations. No tokio, no async — just closures in recv loop.
type NotifFilter = Box<dyn Fn(&Notification) -> bool + Send>;
type NotifMapper = Box<dyn Fn(&Notification) -> Option<Notification> + Send>;

/// Composable wrapper around a `Subscription` supporting chainable filter, map, and take operations.
pub struct SubscriptionOps {
    sub: Subscription,
    filters: Vec<NotifFilter>,
    map_fn: Option<NotifMapper>,
    limit: Option<usize>,
    count: usize,
}

impl SubscriptionOps {
    /// Add a filter predicate. Only notifications passing all filters are returned.
    pub fn filter<F: Fn(&Notification) -> bool + Send + 'static>(mut self, f: F) -> Self {
        self.filters.push(Box::new(f));
        self
    }

    /// Transform notifications. The mapper returns Some(notification) to pass through,
    /// or None to skip. Chainable with filter/take.
    pub fn map<F: Fn(&Notification) -> Option<Notification> + Send + 'static>(
        mut self,
        f: F,
    ) -> Self {
        self.map_fn = Some(Box::new(f));
        self
    }

    /// Limit the number of notifications returned before stopping.
    pub fn take(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Fold the lossy subscription stream into a derived live state.
    ///
    /// This is best suited to dashboards and approximate live views. Because
    /// the underlying subscription is lossy, a slow subscriber may skip
    /// notifications and therefore skip state transitions. Use `Cursor` when
    /// the fold must observe every event.
    pub fn scan<S, F>(self, initial: S, f: F) -> ScanSubscriptionOps<S, F>
    where
        S: Clone + Send + 'static,
        F: FnMut(&mut S, &Notification) -> Option<S> + Send + 'static,
    {
        ScanSubscriptionOps {
            ops: self,
            state: initial,
            fold: f,
        }
    }

    /// Blocking receive with all filters applied. Returns None when channel closes or limit reached.
    pub fn recv(&mut self) -> Option<Notification> {
        if let Some(limit) = self.limit {
            if self.count >= limit {
                return None;
            }
        }
        loop {
            let notif = self.sub.recv()?;
            if self.filters.iter().all(|f| f(&notif)) {
                let result = if let Some(ref map_fn) = self.map_fn {
                    map_fn(&notif)
                } else {
                    Some(notif)
                };
                if let Some(n) = result {
                    self.count += 1;
                    return Some(n);
                }
            }
        }
    }
}

/// Stateful lossy subscription fold.
pub struct ScanSubscriptionOps<S, F> {
    ops: SubscriptionOps,
    state: S,
    fold: F,
}

impl<S, F> ScanSubscriptionOps<S, F>
where
    S: Clone + Send + 'static,
    F: FnMut(&mut S, &Notification) -> Option<S> + Send + 'static,
{
    /// Receive the next folded state value.
    pub fn recv(&mut self) -> Option<S> {
        loop {
            let notif = self.ops.recv()?;
            if let Some(next) = (self.fold)(&mut self.state, &notif) {
                self.state = next.clone();
                return Some(next);
            }
        }
    }
}
