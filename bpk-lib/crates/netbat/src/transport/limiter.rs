//! Connection admission limiter shared by the request and subscription TCP
//! listeners.
//!
//! [`ConnectionLimit`] selects one of three admission policies; a listener
//! builds a [`Limiter`] from it and gates every accepted connection through
//! [`Limiter::admit`]. The `Concurrent` policy is a flume permit pool: a fixed
//! number of unit permits circulate, a connection acquires one before serving
//! and the [`ConnectionPermit`] RAII guard returns it on EVERY exit path —
//! normal return, error return, and the caught-panic path — because `Drop` runs
//! on scope exit and during unwinding alike.

use std::num::NonZeroUsize;
use std::time::Duration;

use super::tcp::ShutdownHandle;

/// Default concurrent connection cap. Matches the pre-0.9 `max_connections`
/// magnitude so a default listener admits the same volume, now as an in-flight
/// concurrency cap rather than a lifetime accept budget.
pub const DEFAULT_MAX_CONNECTIONS: usize = 1024;

const DEFAULT_CONCURRENT_CONNECTIONS: NonZeroUsize =
    match NonZeroUsize::new(DEFAULT_MAX_CONNECTIONS) {
        Some(value) => value,
        // DEFAULT_MAX_CONNECTIONS is a nonzero literal, so this arm is
        // unreachable; MIN keeps the const construction total without a panic.
        None => NonZeroUsize::MIN,
    };

/// How a listener caps the connections it serves.
///
/// Replaces the pre-0.9 lifetime-only `max_connections` budget with three
/// explicit policies. The [`Default`] is [`ConnectionLimit::Concurrent`] at the
/// pre-0.9 magnitude.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionLimit {
    /// Cap the number of connections served *concurrently*. A bounded permit
    /// pool of `n` slots gates admission: each connection acquires a permit
    /// before serving and releases it on disconnect (the panic path included),
    /// so a freed slot is immediately reusable by the next connection. The
    /// accept loop blocks for a free slot when the pool is empty. The default.
    Concurrent(NonZeroUsize),
    /// Cap the *lifetime* number of connections accepted: after `n` total
    /// accepts the listener stops accepting and returns. The pre-0.9
    /// `max_connections` behavior, now an explicit opt-in.
    Lifetime(NonZeroUsize),
    /// No connection gate. The listener accepts until shutdown.
    Unlimited,
}

impl Default for ConnectionLimit {
    fn default() -> Self {
        Self::Concurrent(DEFAULT_CONCURRENT_CONNECTIONS)
    }
}

/// RAII concurrency permit. Dropping it returns the slot to the pool — on the
/// normal return, the error return, AND the caught-panic path of a connection
/// worker, because `Drop` runs on every scope exit, unwinding included. The
/// non-pooled policies hand back a no-op permit (`release: None`).
pub(crate) struct ConnectionPermit {
    release: Option<flume::Sender<()>>,
}

impl ConnectionPermit {
    /// A permit that gates nothing (the `Lifetime` / `Unlimited` policies).
    fn noop() -> Self {
        Self { release: None }
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            // The pool holds exactly the permits in circulation, so returning
            // one never blocks; a `Disconnected` send (pool dropped) is moot.
            let _ = release.send(());
        }
    }
}

/// Bounded permit pool: `n` unit permits gate `n` concurrent connections.
#[derive(Clone)]
pub(crate) struct PermitPool {
    tx: flume::Sender<()>,
    rx: flume::Receiver<()>,
}

impl PermitPool {
    fn with_permits(count: NonZeroUsize) -> Self {
        let count = count.get();
        let (tx, rx) = flume::bounded(count);
        for _ in 0..count {
            // `bounded(count)` has exactly `count` slots, so these `count`
            // pre-fill sends fit without blocking; `rx` is held, so none fail.
            let _ = tx.send(());
        }
        Self { tx, rx }
    }

    /// Block (re-checking `shutdown` every `idle_sleep`) until a permit frees;
    /// return the RAII guard, or `None` if shutdown was requested first.
    fn acquire(&self, shutdown: &ShutdownHandle, idle_sleep: Duration) -> Option<ConnectionPermit> {
        loop {
            if shutdown.is_shutdown() {
                return None;
            }
            match self.rx.recv_timeout(idle_sleep) {
                Ok(()) => {
                    return Some(ConnectionPermit {
                        release: Some(self.tx.clone()),
                    });
                }
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

/// Admission policy built once per listener from a [`ConnectionLimit`].
pub(crate) enum Limiter {
    /// Bounded permit pool (`Concurrent`).
    Concurrent(PermitPool),
    /// Lifetime accept budget (`Lifetime`).
    Lifetime(NonZeroUsize),
    /// No gate (`Unlimited`).
    Unlimited,
}

/// Outcome of [`Limiter::admit`].
pub(crate) enum Admission {
    /// Serve the connection while holding this permit; it releases on drop.
    Permit(ConnectionPermit),
    /// Shutdown was requested while waiting for a concurrent slot; stop.
    Shutdown,
}

impl Limiter {
    pub(crate) fn from_limit(limit: ConnectionLimit) -> Self {
        match limit {
            ConnectionLimit::Concurrent(count) => Self::Concurrent(PermitPool::with_permits(count)),
            ConnectionLimit::Lifetime(count) => Self::Lifetime(count),
            ConnectionLimit::Unlimited => Self::Unlimited,
        }
    }

    /// Whether the accept loop may take another connection. Only the lifetime
    /// budget caps the *count* of accepts; the concurrent cap instead gates at
    /// [`Limiter::admit`] and the unlimited policy never stops on its own.
    pub(crate) fn accepting(&self, accepted: usize) -> bool {
        match self {
            Self::Lifetime(budget) => accepted < budget.get(),
            Self::Concurrent(_) | Self::Unlimited => true,
        }
    }

    /// Admit one connection, acquiring a concurrency permit (blocking for the
    /// pool, an instant no-op otherwise). Returns [`Admission::Shutdown`] only
    /// when shutdown is requested while a concurrent pool is empty.
    pub(crate) fn admit(&self, shutdown: &ShutdownHandle, idle_sleep: Duration) -> Admission {
        match self {
            Self::Concurrent(pool) => match pool.acquire(shutdown, idle_sleep) {
                Some(permit) => Admission::Permit(permit),
                None => Admission::Shutdown,
            },
            Self::Lifetime(_) | Self::Unlimited => Admission::Permit(ConnectionPermit::noop()),
        }
    }
}

/// Build the per-connection stats lane sized to the admission policy: bounded to
/// the cap for the capped policies (at most that many workers are ever alive to
/// send), unbounded for `Unlimited`. A finished worker therefore never blocks on
/// a full lane, so the listener's join phase cannot deadlock on stats delivery.
pub(crate) fn stats_lane<T>(limit: ConnectionLimit) -> (flume::Sender<T>, flume::Receiver<T>) {
    match limit {
        ConnectionLimit::Concurrent(count) | ConnectionLimit::Lifetime(count) => {
            flume::bounded(count.get())
        }
        ConnectionLimit::Unlimited => flume::unbounded(),
    }
}

#[cfg(test)]
mod tests {
    //! Deterministic unit tests for the admission limiter. The end-to-end
    //! permit-pool behavior over real sockets lives in
    //! `tests/connection_limit.rs`; these cover the pure policy logic and the
    //! pool's acquire/release/shutdown without TCP timing.

    use super::*;

    fn nz(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("nonzero test value")
    }

    #[test]
    fn default_connection_limit_is_concurrent_at_default_magnitude() {
        assert_eq!(
            ConnectionLimit::default(),
            ConnectionLimit::Concurrent(nz(DEFAULT_MAX_CONNECTIONS))
        );
        assert_eq!(DEFAULT_MAX_CONNECTIONS, 1024);
    }

    #[test]
    fn accepting_caps_only_the_lifetime_budget() {
        let lifetime = Limiter::from_limit(ConnectionLimit::Lifetime(nz(2)));
        assert!(lifetime.accepting(0));
        assert!(lifetime.accepting(1));
        assert!(!lifetime.accepting(2));
        assert!(!lifetime.accepting(3));

        let concurrent = Limiter::from_limit(ConnectionLimit::Concurrent(nz(1)));
        assert!(concurrent.accepting(0));
        assert!(concurrent.accepting(1_000_000));

        let unlimited = Limiter::from_limit(ConnectionLimit::Unlimited);
        assert!(unlimited.accepting(0));
        assert!(unlimited.accepting(1_000_000));
    }

    #[test]
    fn permit_pool_releases_slot_on_drop_for_reuse() {
        let pool = PermitPool::with_permits(nz(1));
        let shutdown = ShutdownHandle::new();
        let idle = Duration::from_millis(1);

        let first = pool
            .acquire(&shutdown, idle)
            .expect("the single permit is available");
        drop(first);
        // The dropped guard returned its slot, so a second acquire succeeds
        // without a fresh permit ever being minted.
        let second = pool.acquire(&shutdown, idle);
        assert!(second.is_some(), "a released slot must be reusable");
    }

    #[test]
    fn permit_pool_acquire_returns_none_once_shutdown_when_empty() {
        let pool = PermitPool::with_permits(nz(1));
        let shutdown = ShutdownHandle::new();
        let idle = Duration::from_millis(1);

        let _held = pool
            .acquire(&shutdown, idle)
            .expect("the single permit is available");
        // Pool is now empty. With shutdown requested, the next acquire must
        // give up instead of blocking forever.
        shutdown.shutdown();
        assert!(pool.acquire(&shutdown, idle).is_none());
    }

    #[test]
    fn admit_hands_a_noop_permit_for_lifetime_and_unlimited() {
        let shutdown = ShutdownHandle::new();
        let idle = Duration::from_millis(1);
        for limiter in [
            Limiter::from_limit(ConnectionLimit::Lifetime(nz(1))),
            Limiter::from_limit(ConnectionLimit::Unlimited),
        ] {
            assert!(matches!(
                limiter.admit(&shutdown, idle),
                Admission::Permit(_)
            ));
        }
    }

    #[test]
    fn stats_lane_is_bounded_for_caps_and_unbounded_for_unlimited() {
        let (tx, _rx) = stats_lane::<u8>(ConnectionLimit::Concurrent(nz(2)));
        assert_eq!(tx.capacity(), Some(2));
        let (tx, _rx) = stats_lane::<u8>(ConnectionLimit::Lifetime(nz(3)));
        assert_eq!(tx.capacity(), Some(3));
        let (tx, _rx) = stats_lane::<u8>(ConnectionLimit::Unlimited);
        assert_eq!(tx.capacity(), None);
    }
}
