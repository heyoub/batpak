use crate::store::StoreError;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Returns microseconds since Unix epoch, saturating to `i64::MAX` if the system
/// clock is beyond year ~292,277 (treat the max value as a clock-malfunction
/// signal). No panic; cache staleness checks downstream see a saturated value
/// and force a replay rather than poisoning the process.
pub(crate) fn now_us() -> i64 {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    i64::try_from(micros).unwrap_or(i64::MAX)
}

/// Convert a public clock reading to persisted wall-clock milliseconds.
///
/// Custom clocks must report microseconds since Unix epoch as a non-negative
/// `i64`. Negative values are rejected as invalid caller input rather than
/// panicking in append/batch hot paths.
pub(crate) fn wall_ms_from_timestamp_us(timestamp_us: i64) -> Result<u64, StoreError> {
    if timestamp_us < 0 {
        return Err(StoreError::InvalidClock {
            timestamp_us,
            reason: "timestamp_us must be >= 0 microseconds since Unix epoch".into(),
        });
    }
    Ok((timestamp_us / 1000).cast_unsigned())
}

pub(crate) fn now_wall_ns_saturating() -> i64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    i64::try_from(nanos).unwrap_or(i64::MAX)
}

/// Process-wide monotonic anchor. Captured on first call; subsequent calls read
/// the elapsed nanoseconds from `Instant::now()` relative to this anchor.
///
/// The anchor couples two facts:
///   1. `anchor_instant`: the `Instant` captured at first call.
///   2. `anchor_boot_ns`: a u64 marker that identifies *this* process's
///      monotonic epoch. Any cached monotonic value persisted to disk and then
///      read back by a different process MUST compare its `process_boot_ns`
///      against this value — mismatch means the monotonic value belongs to a
///      different process's clock and cannot be trusted.
struct MonotonicAnchor {
    anchor_instant: Instant,
    anchor_boot_ns: u64,
}

impl MonotonicAnchor {
    fn get() -> &'static Self {
        use std::sync::OnceLock;
        static ANCHOR: OnceLock<MonotonicAnchor> = OnceLock::new();
        ANCHOR.get_or_init(|| {
            // The boot marker is the wall-clock time at anchor creation, encoded
            // as nanoseconds since Unix epoch and saturated to u64. Two processes
            // booting in the same nanosecond on the same machine would collide,
            // which is acceptable (they would both re-project on mismatch anyway).
            let wall_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let anchor_boot_ns = u64::try_from(wall_ns).unwrap_or(u64::MAX);
            MonotonicAnchor {
                anchor_instant: Instant::now(),
                anchor_boot_ns,
            }
        })
    }
}

/// Returns monotonic nanoseconds since the process-wide anchor. Guaranteed
/// non-decreasing within a single process; meaningless across processes
/// (use [`process_boot_ns`] to detect cross-process comparisons).
///
/// Saturates to `i64::MAX` if the process has been alive for more than
/// ~292 years.
pub(crate) fn now_mono_ns() -> i64 {
    let anchor = MonotonicAnchor::get();
    let elapsed = anchor.anchor_instant.elapsed().as_nanos();
    i64::try_from(elapsed).unwrap_or(i64::MAX)
}

/// Returns this process's monotonic epoch marker. Two processes never share
/// this value (except in the vanishingly unlikely case of same-nanosecond
/// boot); a monotonic value read from disk whose `process_boot_ns` does not
/// match the current one belongs to a prior process and cannot be compared
/// against [`now_mono_ns`].
pub(crate) fn process_boot_ns() -> u64 {
    MonotonicAnchor::get().anchor_boot_ns
}

/// Non-decreasing wrapper around a user-supplied `Fn() -> i64` clock.
///
/// A user clock that regresses (e.g. NTP jump, manual reset) would poison age
/// comparisons — a slot cached at `now=1000` and read at `now=500` would look
/// like it's `-500` µs old, and a naive check can misclassify it. This wrapper
/// clamps each observed value to `max(last, new)`: once we see a value, we
/// never return anything smaller. Regressions emit `tracing::error!` with the
/// previous and new values and return the previous value — the user's clock
/// is broken, but the store keeps running.
#[derive(Clone)]
pub(crate) struct MonotonicClock {
    inner: Arc<dyn Fn() -> i64 + Send + Sync>,
    last: Arc<AtomicI64>,
}

impl MonotonicClock {
    /// Wrap a user-supplied clock function. The returned handle is cloneable
    /// and stores shared state (`AtomicI64`) in an `Arc`, so clones observe the
    /// same non-decreasing sequence.
    pub(crate) fn wrap(inner: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            inner,
            last: Arc::new(AtomicI64::new(i64::MIN)),
        }
    }

    /// Sample the wrapped clock and return a value that is never smaller than
    /// any value previously returned by this [`MonotonicClock`] (or any clone
    /// of it). A regression is logged at `error` level.
    pub(crate) fn now_us(&self) -> i64 {
        let raw = (self.inner)();
        // Compare-and-swap loop: install `raw` if it's newer than `last`,
        // otherwise report a regression and keep the old value.
        loop {
            let prev = self.last.load(Ordering::Acquire);
            if raw >= prev {
                match self
                    .last
                    .compare_exchange(prev, raw, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => return raw,
                    Err(_) => continue, // another thread stored a newer value; retry
                }
            } else {
                tracing::error!("user clock regressed: prev={} new={}", prev, raw);
                return prev;
            }
        }
    }
}
