use crate::coordinate::Region;
use crate::store::writer::Notification;
use flume::Receiver;

/// Subscription: push-based per-subscriber flume channel. Lossy.
/// If subscriber is slow, bounded channel fills. Writer's retain() prunes.
/// For guaranteed delivery, use Cursor instead.
/// [SPEC:src/store/subscription.rs]
pub struct Subscription {
    rx: Receiver<Notification>,
    region: Region,
}

impl Subscription {
    pub(crate) fn new(rx: Receiver<Notification>, region: Region) -> Self {
        Self { rx, region }
    }

    /// Blocking receive. Filters by region. Returns None if channel closed.
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
    /// [SPEC:src/store/subscription.rs — ASYNC NOTE]
    pub fn receiver(&self) -> &Receiver<Notification> {
        &self.rx
    }

    /// Create a composable ops wrapper for chainable filter/map/take.
    /// [CROSS-POLLINATION:czap/wire.ts — fluent stream composition]
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
/// [CROSS-POLLINATION:czap/wire.ts — Wire<T,E> fluent operators]
type NotifFilter = Box<dyn Fn(&Notification) -> bool + Send>;
type NotifMapper = Box<dyn Fn(&Notification) -> Option<Notification> + Send>;

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

    /// Limit the number of notifications returned before stopping.
    pub fn take(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
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
