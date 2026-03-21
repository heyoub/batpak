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
    /// [DEP:flume::Receiver::recv_async] → RecvFut<'_, T>: Future
    /// ASYNC NOTE: This is for async event consumption. For Store methods
    /// (append, get, query), use spawn_blocking instead. Two different patterns.
    /// [SPEC:src/store/subscription.rs — ASYNC NOTE]
    pub fn receiver(&self) -> &Receiver<Notification> {
        &self.rx
    }
}
