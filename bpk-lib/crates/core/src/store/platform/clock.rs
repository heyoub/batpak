use crate::store::StoreError;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// Runtime clock source for store timestamping, evidence, and deterministic tests.
///
/// Boundary: this seam owns every clock value that lands on disk, appears in a
/// public report or receipt, or participates in identity such as UUIDv7 wall
/// bits. Process-local wait deadlines still use `Instant`: cursor pull waits,
/// frontier waits, and writer idle backoff compute elapsed time for local
/// scheduling only, and never become durable bytes or receipt/report identity.
///
/// Production uses [`SystemClock`]. Tests and embeddings that need repeatable
/// store behavior can provide a custom implementation and install it with
/// [`crate::store::StoreConfig::with_clock`].
pub trait Clock: Send + Sync {
    /// Return microseconds since the Unix epoch.
    fn now_us(&self) -> i64;
    /// Return nanoseconds since the Unix epoch, saturating on overflow.
    fn now_wall_ns(&self) -> i64;
    /// Return process-local monotonic nanoseconds.
    fn now_mono_ns(&self) -> i64;
    /// Return the process-epoch marker for monotonic metadata.
    fn process_boot_ns(&self) -> u64;
}

/// Returns microseconds since Unix epoch, saturating to `i64::MAX` if the system
/// clock is beyond year ~292,277 (treat the max value as a clock-malfunction
/// signal). No panic; cache staleness checks downstream see a saturated value
/// and force a replay rather than poisoning the process.
fn system_now_us() -> i64 {
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

fn system_now_wall_ns_saturating() -> i64 {
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
pub(crate) struct MonotonicAnchor {
    anchor_instant: Instant,
    anchor_boot_ns: u64,
}

impl MonotonicAnchor {
    fn get() -> &'static Self {
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

    fn now_mono_ns(&self) -> i64 {
        let elapsed = self.anchor_instant.elapsed().as_nanos();
        i64::try_from(elapsed).unwrap_or(i64::MAX)
    }
}

/// Returns monotonic nanoseconds since the process-wide anchor. Guaranteed
/// non-decreasing within a single process; meaningless across processes
/// (use [`Clock::process_boot_ns`] to detect cross-process comparisons).
///
/// Saturates to `i64::MAX` if the process has been alive for more than
/// ~292 years.
#[derive(Clone)]
pub struct SystemClock {
    anchor: &'static MonotonicAnchor,
}

impl SystemClock {
    /// Create a production clock backed by system wall time and process-local monotonic time.
    pub fn new() -> Self {
        Self {
            anchor: MonotonicAnchor::get(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_us(&self) -> i64 {
        system_now_us()
    }

    fn now_wall_ns(&self) -> i64 {
        system_now_wall_ns_saturating()
    }

    fn now_mono_ns(&self) -> i64 {
        self.anchor.now_mono_ns()
    }

    fn process_boot_ns(&self) -> u64 {
        self.anchor.anchor_boot_ns
    }
}

struct FnClock {
    inner: Arc<dyn Fn() -> i64 + Send + Sync>,
    anchor: &'static MonotonicAnchor,
}

impl FnClock {
    fn new(inner: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            inner,
            anchor: MonotonicAnchor::get(),
        }
    }
}

impl Clock for FnClock {
    fn now_us(&self) -> i64 {
        (self.inner)()
    }

    fn now_wall_ns(&self) -> i64 {
        self.now_us().saturating_mul(1000)
    }

    fn now_mono_ns(&self) -> i64 {
        self.anchor.now_mono_ns()
    }

    fn process_boot_ns(&self) -> u64 {
        self.anchor.anchor_boot_ns
    }
}

pub(crate) fn clock_from_fn(inner: Arc<dyn Fn() -> i64 + Send + Sync>) -> Arc<dyn Clock> {
    Arc::new(FnClock::new(inner))
}

/// Non-decreasing wrapper around a clock source.
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
    inner: Arc<dyn Clock>,
    last: Arc<AtomicI64>,
}

impl MonotonicClock {
    /// Wrap a clock. The returned handle is cloneable
    /// and stores shared state (`AtomicI64`) in an `Arc`, so clones observe the
    /// same non-decreasing sequence.
    pub(crate) fn wrap(inner: Arc<dyn Clock>) -> Self {
        Self {
            inner,
            last: Arc::new(AtomicI64::new(i64::MIN)),
        }
    }

    /// Sample the wrapped clock and return a value that is never smaller than
    /// any value previously returned by this [`MonotonicClock`] (or any clone
    /// of it). A regression is logged at `error` level.
    pub(crate) fn now_us(&self) -> i64 {
        let raw = self.inner.now_us();
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

impl Clock for MonotonicClock {
    fn now_us(&self) -> i64 {
        MonotonicClock::now_us(self)
    }

    fn now_wall_ns(&self) -> i64 {
        self.inner.now_wall_ns()
    }

    fn now_mono_ns(&self) -> i64 {
        self.inner.now_mono_ns()
    }

    fn process_boot_ns(&self) -> u64 {
        self.inner.process_boot_ns()
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, FnClock};
    use std::sync::Arc;

    #[test]
    fn fn_clock_preserves_negative_wall_values_but_not_monotonic_time() {
        let clock = FnClock::new(Arc::new(|| -7));

        assert_eq!(
            clock.now_us(),
            -7,
            "PROPERTY: FnClock must expose malformed caller wall time for validation"
        );
        assert_eq!(
            clock.now_wall_ns(),
            -7_000,
            "PROPERTY: wall nanoseconds come from the caller wall clock, not the monotonic anchor"
        );
        assert!(
            clock.now_mono_ns() >= 0,
            "PROPERTY: process-local monotonic evidence must not echo a negative caller wall clock"
        );
    }
}
