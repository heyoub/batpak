use crate::coordinate::Region;
use crate::store::write::fanout::Notification;
use flume::Receiver;

/// Subscription: push-based per-subscriber flume channel. Lossy.
/// If subscriber is slow, bounded channel fills. Writer's retain() prunes.
/// For guaranteed delivery, use Cursor instead.
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
    /// drive the underlying receiver directly — a `crossbeam_channel::
    /// select!` pattern or a polling loop with `receiver().recv_deadline`
    /// around [`Subscription::receiver`] gives per-call deadlines while
    /// keeping the region filter on the caller side.
    pub fn recv(&self) -> Option<Notification> {
        loop {
            match self.rx.recv() {
                Ok(notif) => {
                    // Filter: only return events matching our region.
                    // [FILE:src/coordinate/mod.rs — Region::matches_event]
                    if self.region.matches_event(
                        notif.coord.entity(),
                        notif.coord.scope(),
                        notif.kind,
                    ) {
                        return Some(notif);
                    }
                    // Didn't match — keep receiving
                }
                Err(_) => return None, // channel closed
            }
        }
    }

    /// Expose the raw receiver for async usage.
    /// Caller uses: sub.receiver().recv_async().await
    /// \[DEP:flume::Receiver::recv_async\] → `RecvFut<'_, T>`: Future
    /// ASYNC NOTE: This is for async event consumption. For Store methods
    /// (append, get, query), use spawn_blocking instead. Two different patterns.
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
