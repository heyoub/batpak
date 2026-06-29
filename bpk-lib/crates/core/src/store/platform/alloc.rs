//! Test-only global-allocator shims for allocation accounting and allocation
//! fault injection.
//!
//! Two independent modes live here, each behind its own off-by-default Cargo
//! feature so a normal build never compiles either one:
//!
//! - `CountingAlloc` (`alloc-count`): wraps `System` and records the number
//!   of `alloc`/`realloc`/`dealloc` calls in process-wide atomics. Used to gate
//!   a hot path against an allocation budget (GAUNT-PERF-5a).
//! - `FailingAlloc` (`fault-alloc`): wraps `System` and returns null (an
//!   allocation failure) starting at the Kth allocation. Used to exercise OOM
//!   handling paths deterministically.
//!
//! # Process-wide caveat
//!
//! A `#[global_allocator]` is a single process-wide static. Installing either
//! shim affects EVERY allocation in the test binary, including the harness
//! itself. Therefore each must be used from a DEDICATED single-test binary
//! (its own `tests/<name>.rs`), never alongside unrelated tests in the same
//! binary, or the counters/fault state will be polluted by foreign
//! allocations. The provided `CountingAlloc::scope` guard narrows a count to
//! the closure body, but the binary must still be dedicated to keep
//! cross-thread noise out.

#[cfg(any(feature = "alloc-count", feature = "fault-alloc"))]
use std::alloc::{GlobalAlloc, Layout, System};

// ---------------------------------------------------------------------------
// Allocation counting (`alloc-count`).
// ---------------------------------------------------------------------------

/// A `#[global_allocator]`-compatible wrapper around [`System`] that counts
/// allocation activity in process-wide atomics.
///
/// Install in a dedicated test binary:
///
/// ```rust,ignore
/// use batpak::store::platform::alloc::CountingAlloc;
///
/// #[global_allocator]
/// static ALLOC: CountingAlloc = CountingAlloc::new();
/// ```
#[cfg(feature = "alloc-count")]
pub struct CountingAlloc {
    inner: System,
}

#[cfg(feature = "alloc-count")]
mod counters {
    use std::sync::atomic::AtomicU64;

    pub(super) static ALLOCS: AtomicU64 = AtomicU64::new(0);
    pub(super) static REALLOCS: AtomicU64 = AtomicU64::new(0);
    pub(super) static DEALLOCS: AtomicU64 = AtomicU64::new(0);
}

/// A snapshot of the global allocation counters.
#[cfg(feature = "alloc-count")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocSnapshot {
    /// Cumulative `alloc` calls (includes the alloc half of `alloc_zeroed`).
    pub allocs: u64,
    /// Cumulative `realloc` calls.
    pub reallocs: u64,
    /// Cumulative `dealloc` calls.
    pub deallocs: u64,
}

#[cfg(feature = "alloc-count")]
impl AllocSnapshot {
    /// Allocation calls performed between `self` (earlier) and `later`.
    #[must_use]
    pub fn delta_allocs(self, later: AllocSnapshot) -> u64 {
        later.allocs.saturating_sub(self.allocs)
    }

    /// Reallocation calls performed between `self` (earlier) and `later`.
    #[must_use]
    pub fn delta_reallocs(self, later: AllocSnapshot) -> u64 {
        later.reallocs.saturating_sub(self.reallocs)
    }

    /// Total allocating calls (alloc + realloc) between `self` and `later`.
    #[must_use]
    pub fn delta_allocating(self, later: AllocSnapshot) -> u64 {
        self.delta_allocs(later)
            .saturating_add(self.delta_reallocs(later))
    }
}

#[cfg(feature = "alloc-count")]
impl CountingAlloc {
    /// Construct the counting allocator. `const` so it can initialize a
    /// `static` for `#[global_allocator]`.
    #[must_use]
    pub const fn new() -> Self {
        Self { inner: System }
    }

    /// Read the current global allocation counters.
    #[must_use]
    pub fn snapshot() -> AllocSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        AllocSnapshot {
            allocs: counters::ALLOCS.load(Relaxed),
            reallocs: counters::REALLOCS.load(Relaxed),
            deallocs: counters::DEALLOCS.load(Relaxed),
        }
    }

    /// Run `body`, returning its result plus the allocation snapshot delta
    /// observed across the call.
    ///
    /// NOTE: this only narrows the time window. Because the allocator is
    /// process-wide, concurrent threads still contribute to the counters; run
    /// allocation-budget assertions single-threaded in a dedicated binary.
    pub fn scope<R>(body: impl FnOnce() -> R) -> (R, AllocSnapshot) {
        let before = Self::snapshot();
        let result = body();
        let after = Self::snapshot();
        let delta = AllocSnapshot {
            allocs: before.delta_allocs(after),
            reallocs: before.delta_reallocs(after),
            deallocs: after.deallocs.saturating_sub(before.deallocs),
        };
        (result, delta)
    }
}

#[cfg(feature = "alloc-count")]
impl Default for CountingAlloc {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: every method forwards to `System`, the standard system allocator,
// which already satisfies the `GlobalAlloc` contract. We only add atomic
// counter increments around the forwarded calls, which neither aliases nor
// invalidates the returned pointers. justifies: test-only accounting shim;
// the unsafe impl simply re-exposes `System`'s already-sound behavior.
#[cfg(feature = "alloc-count")]
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        counters::ALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        counters::DEALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: `ptr`/`layout` come from a matching prior `alloc`.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        counters::ALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        counters::REALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: `ptr`/`layout` come from a matching prior `alloc`; `new_size`
        // is forwarded unchanged.
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}

// ---------------------------------------------------------------------------
// Allocation fault injection (`fault-alloc`).
// ---------------------------------------------------------------------------

/// A `#[global_allocator]`-compatible wrapper around [`System`] that fails
/// (returns null) starting at the Kth allocation.
///
/// The trip point is process-wide atomic state seeded via
/// [`FailingAlloc::fail_after`]. Until armed, every allocation succeeds. Once
/// armed, the Kth and every subsequent `alloc`/`alloc_zeroed`/`realloc`
/// returns null, modeling an out-of-memory condition. `dealloc` always
/// forwards normally.
///
/// Like [`CountingAlloc`], this must live in a dedicated single-test binary.
#[cfg(feature = "fault-alloc")]
pub struct FailingAlloc {
    inner: System,
}

#[cfg(feature = "fault-alloc")]
mod fail_state {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    /// Whether the failure trip is armed.
    pub(super) static ARMED: AtomicBool = AtomicBool::new(false);
    /// Count of allocations observed since arming.
    pub(super) static SEEN: AtomicU64 = AtomicU64::new(0);
    /// The 1-based allocation index at (and after) which allocation fails.
    pub(super) static FAIL_AT: AtomicU64 = AtomicU64::new(u64::MAX);
}

#[cfg(feature = "fault-alloc")]
impl FailingAlloc {
    /// Construct the failing allocator. `const` so it can initialize a
    /// `static` for `#[global_allocator]`.
    #[must_use]
    pub const fn new() -> Self {
        Self { inner: System }
    }

    /// Arm the allocator to start failing at the `k`th allocation (1-based)
    /// after this call. Resets the observed-allocation counter.
    pub fn fail_after(k: u64) {
        use std::sync::atomic::Ordering::SeqCst;
        fail_state::SEEN.store(0, SeqCst);
        fail_state::FAIL_AT.store(k, SeqCst);
        fail_state::ARMED.store(true, SeqCst);
    }

    /// Disarm the allocator so all allocations succeed again.
    pub fn disarm() {
        use std::sync::atomic::Ordering::SeqCst;
        fail_state::ARMED.store(false, SeqCst);
        fail_state::FAIL_AT.store(u64::MAX, SeqCst);
    }

    /// Returns true if the next allocation should be failed, advancing the
    /// observed-allocation counter when armed.
    fn should_fail(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        if !fail_state::ARMED.load(SeqCst) {
            return false;
        }
        let seen = fail_state::SEEN.fetch_add(1, SeqCst).saturating_add(1);
        seen >= fail_state::FAIL_AT.load(SeqCst)
    }
}

#[cfg(feature = "fault-alloc")]
impl Default for FailingAlloc {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: each method either returns null (a permitted `GlobalAlloc` failure
// signal) or forwards unchanged to `System`, which is already a sound
// `GlobalAlloc`. We never hand out a pointer we did not get from `System`, and
// `dealloc` always forwards. justifies: test-only OOM-injection shim.
#[cfg(feature = "fault-alloc")]
unsafe impl GlobalAlloc for FailingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.should_fail() {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` come from a matching prior successful `alloc`.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if self.should_fail() {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.should_fail() {
            return std::ptr::null_mut();
        }
        // SAFETY: `ptr`/`layout` come from a matching prior successful `alloc`.
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}
