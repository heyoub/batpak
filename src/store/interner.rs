//! String interner for compact index storage.
//!
//! Maps arbitrary strings to dense [`InternId`] values (1-based `u32`) so that
//! entity names, scope names, and event-kind strings can be stored as 4-byte
//! integers rather than heap-allocated strings in hot index paths.
//!
//! # Design
//!
//! * Slot 0 is a permanent sentinel (`""`).  Valid interned IDs start at 1.
//! * The forward map (`String → InternId`) is held behind a [`parking_lot::RwLock`]
//!   so that many concurrent readers pay only a shared-lock cost.
//! * The reverse table (`InternId → Arc<str>`) is a `Vec` behind a second
//!   [`parking_lot::RwLock`]; index position equals the numeric ID value.
//! * [`AtomicU32`] drives ID allocation.  The write path does a single CAS after
//!   verifying the string is absent under the read lock — a brief write lock is
//!   taken only when a genuinely new string is observed.
//! * [`parking_lot::RwLock`] is used throughout (already a transitive dependency
//!   via `dashmap`).  It never poisons on panic, which is important for a
//!   store that can survive writer-thread restarts.
//!
//! # Thread-safety contract
//!
//! Multiple threads may call [`StringInterner::resolve`] at any time.
//! [`StringInterner::intern`] is safe to call concurrently; contention only
//! occurs on the write lock when a new string is first seen.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// ── Sentinel ────────────────────────────────────────────────────────────────

/// Numeric value used as the sentinel slot.  Slot 0 holds the empty string and
/// is never returned as the result of a genuine [`StringInterner::intern`] call.
const SENTINEL_ID: u32 = 0;

/// The string stored at slot 0.
const SENTINEL_STR: &str = "";

// ── InternId ─────────────────────────────────────────────────────────────────

/// A compact, copy-cheap identifier for an interned string.
///
/// The inner `u32` is a 1-based index into the interner's reverse table.
/// `InternId(0)` is the sentinel and is **never** returned by
/// [`StringInterner::intern`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct InternId(pub(crate) u32);

impl InternId {
    /// Returns the raw numeric value of this ID.
    #[inline]
    pub(crate) fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns the sentinel ID (slot 0, empty string). Used as a placeholder
    /// when the interner is not available (e.g., in test constructors).
    #[cfg(test)]
    #[inline]
    pub(crate) fn sentinel() -> Self {
        Self(SENTINEL_ID)
    }

    /// Returns `true` if this is the sentinel ID (slot 0, empty string).
    #[cfg(test)]
    #[inline]
    pub(crate) fn is_sentinel(self) -> bool {
        self.0 == SENTINEL_ID
    }
}

// ── StringInterner ────────────────────────────────────────────────────────────

/// A thread-safe string interner that maps strings to compact [`InternId`] values.
///
/// Slot 0 is permanently reserved for the sentinel string `""`.  All strings
/// passed to [`intern`](StringInterner::intern) receive IDs starting at 1.
///
/// # Snapshot / restore
///
/// Call [`to_snapshot`](StringInterner::to_snapshot) to serialise the interner's
/// contents to a `Vec<String>` for checkpoint files, and
/// [`from_snapshot`](StringInterner::from_snapshot) to restore it.  The sentinel
/// is *not* included in the snapshot; it is re-inserted automatically.
pub(crate) struct StringInterner {
    /// Forward map: interned string → [`InternId`].
    ///
    /// Uses `Arc<str>` as the key so the key allocation is shared with the
    /// reverse table — no double allocation per entry.
    forward: RwLock<HashMap<Arc<str>, InternId>>,

    /// Reverse table: slot index == `InternId` value → string.
    ///
    /// `reverse[0]` is always the sentinel.  All other slots are 1-based.
    reverse: RwLock<Vec<Arc<str>>>,

    /// Next ID to assign.  Always >= 1 after construction.
    next_id: AtomicU32,
}

impl StringInterner {
    /// Create a new, empty interner with the sentinel pre-installed at slot 0.
    pub(crate) fn new() -> Self {
        let sentinel: Arc<str> = Arc::from(SENTINEL_STR);
        let mut forward_map = HashMap::new();
        forward_map.insert(Arc::clone(&sentinel), InternId(SENTINEL_ID));

        Self {
            forward: RwLock::new(forward_map),
            reverse: RwLock::new(vec![sentinel]),
            next_id: AtomicU32::new(1),
        }
    }

    fn install_snapshot_iter<I>(&self, strings: I)
    where
        I: IntoIterator<Item = Arc<str>>,
    {
        let sentinel: Arc<str> = Arc::from(SENTINEL_STR);
        let mut forward_map = HashMap::new();
        let mut reverse = vec![Arc::clone(&sentinel)];
        forward_map.insert(sentinel, InternId(SENTINEL_ID));

        for (idx, string) in strings.into_iter().enumerate() {
            let raw = u32::try_from(idx + 1).expect("interner snapshot exceeds u32 slots");
            forward_map.insert(Arc::clone(&string), InternId(raw));
            reverse.push(string);
        }

        *self.forward.write() = forward_map;
        *self.reverse.write() = reverse;
        self.next_id.store(
            u32::try_from(self.reverse.read().len()).expect("interner size exceeds u32 slots"),
            Ordering::Release,
        );
    }

    /// Return the [`InternId`] for `s`, creating a new one if `s` has not been
    /// seen before.
    ///
    /// # Fast path
    /// A shared (read) lock is taken on the forward map.  If `s` is already
    /// interned the lock is released immediately and no allocation occurs.
    ///
    /// # Slow path
    /// When `s` is absent, the read lock is dropped, an exclusive write lock is
    /// acquired, and the string is inserted.  A second lookup under the write lock
    /// guards against a concurrent `intern` that may have raced and inserted the
    /// same string between the read-unlock and write-lock.
    pub(crate) fn intern(&self, s: &str) -> InternId {
        // ── Fast path: already interned ──────────────────────────────────────
        {
            let fwd = self.forward.read();
            if let Some(&id) = fwd.get(s) {
                return id;
            }
        }

        // ── Slow path: first time seeing this string ─────────────────────────
        let mut fwd = self.forward.write();

        // Re-check under write lock — another thread may have inserted between
        // the two lock acquisitions (classic double-checked pattern).
        if let Some(&id) = fwd.get(s) {
            return id;
        }

        // Allocate a new ID.  `fetch_add` is AcqRel so the increment is visible
        // to any thread that subsequently observes the forward map entry.
        let raw = self.next_id.fetch_add(1, Ordering::AcqRel);
        let id = InternId(raw);

        let arc: Arc<str> = Arc::from(s);
        fwd.insert(Arc::clone(&arc), id);

        // Append to reverse table under its own write lock.
        // We take `reverse` *inside* the `forward` write lock to preserve the
        // invariant that every ID visible in the forward map has a corresponding
        // slot in the reverse table.
        {
            let mut rev = self.reverse.write();
            // The Vec should always be exactly `raw` elements long at this point
            // because IDs are issued monotonically and we hold the forward write
            // lock the entire time.
            debug_assert_eq!(
                u32::try_from(rev.len()).unwrap_or(u32::MAX),
                raw,
                "reverse table length mismatch: expected {raw}, got {}",
                rev.len()
            );
            rev.push(arc);
        }

        id
    }

    /// Resolve an [`InternId`] back to the original string.
    ///
    /// Returns `None` if `id` is out of range (i.e. was never issued by this
    /// interner instance).  Returns `Some(Arc::from(""))` for the sentinel ID 0.
    ///
    /// The reverse table is accessed under a brief shared read lock; the returned
    /// [`Arc<str>`] extends the string's lifetime beyond the lock.
    #[cfg(test)]
    pub(crate) fn resolve(&self, id: InternId) -> Option<Arc<str>> {
        let rev = self.reverse.read();
        rev.get(id.0 as usize).map(Arc::clone)
    }

    /// Return the number of interned strings, **excluding** the sentinel.
    ///
    /// A freshly constructed interner returns `0`.
    pub(crate) fn len(&self) -> usize {
        // next_id starts at 1 and is incremented once per new string, so
        // `next_id - 1` is the count of user-visible entries.
        // Saturating subtraction prevents underflow if somehow next_id is 0
        // (which the constructor prohibits, but be defensive).
        (self.next_id.load(Ordering::Acquire)).saturating_sub(1) as usize
    }

    /// Serialise all interned strings (excluding the sentinel) into an ordered
    /// `Vec<String>` suitable for writing to a checkpoint file.
    ///
    /// The `i`-th element of the returned `Vec` corresponds to `InternId(i + 1)`.
    /// Passing the result to [`from_snapshot`](StringInterner::from_snapshot)
    /// produces an interner with identical ID assignments.
    pub(crate) fn to_snapshot(&self) -> Vec<String> {
        let rev = self.reverse.read();
        // Skip slot 0 (sentinel); map the rest to owned `String`.
        rev.iter()
            .skip(1)
            .map(|arc| arc.as_ref().to_owned())
            .collect()
    }

    /// Restore an interner from a snapshot produced by
    /// [`to_snapshot`](StringInterner::to_snapshot).
    ///
    /// The sentinel is automatically re-installed at slot 0.  Each element of
    /// `strings` is assigned the next available ID starting at 1, preserving
    /// the original ID assignments.
    #[cfg(test)]
    pub(crate) fn from_snapshot(strings: Vec<String>) -> Self {
        let interner = Self::new();

        for s in strings {
            // `intern` handles sentinel-avoidance and double-insertion protection
            // automatically; duplicates in the snapshot are silently deduplicated.
            let _ = interner.intern(&s);
        }

        interner
    }

    /// Reset the interner to its empty state while preserving the sentinel slot.
    pub(crate) fn reset(&self) {
        self.install_snapshot_iter(std::iter::empty());
    }

    /// Replace the current contents with a snapshot that includes the sentinel
    /// at slot 0, matching the cold-start artifact formats.
    pub(crate) fn replace_from_full_snapshot(&self, strings: &[String]) {
        let iter = strings.iter().skip(1).map(|s| Arc::<str>::from(s.as_str()));
        self.install_snapshot_iter(iter);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_is_slot_zero() {
        let interner = StringInterner::new();
        let resolved = interner
            .resolve(InternId(0))
            .expect("sentinel must always resolve");
        assert_eq!(resolved.as_ref(), "");
    }

    #[test]
    fn sentinel_is_sentinel() {
        assert!(InternId::sentinel().is_sentinel());
        let interner = StringInterner::new();
        let real_id = interner.intern("hello");
        assert!(!real_id.is_sentinel());
    }

    #[test]
    fn intern_returns_stable_id() {
        let interner = StringInterner::new();
        let id1 = interner.intern("entity:1");
        let id2 = interner.intern("entity:1");
        assert_eq!(id1, id2);
    }

    #[test]
    fn distinct_strings_get_distinct_ids() {
        let interner = StringInterner::new();
        let a = interner.intern("entity:1");
        let b = interner.intern("entity:2");
        assert_ne!(a, b);
    }

    #[test]
    fn ids_are_one_based() {
        let interner = StringInterner::new();
        let id = interner.intern("first");
        assert_eq!(id.as_u32(), 1);
    }

    #[test]
    fn resolve_roundtrips() {
        let interner = StringInterner::new();
        let id = interner.intern("scope:orders");
        let resolved = interner.resolve(id).expect("must resolve a valid id");
        assert_eq!(resolved.as_ref(), "scope:orders");
    }

    #[test]
    fn resolve_out_of_range_returns_none() {
        let interner = StringInterner::new();
        assert!(interner.resolve(InternId(999)).is_none());
    }

    #[test]
    fn len_excludes_sentinel() {
        let interner = StringInterner::new();
        assert_eq!(interner.len(), 0);
        let _ = interner.intern("a");
        assert_eq!(interner.len(), 1);
        let _ = interner.intern("a"); // duplicate
        assert_eq!(interner.len(), 1);
        let _ = interner.intern("b");
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn snapshot_roundtrip_preserves_ids() {
        let original = StringInterner::new();
        let id_a = original.intern("entity:apple");
        let id_b = original.intern("entity:banana");

        let snapshot = original.to_snapshot();
        let restored = StringInterner::from_snapshot(snapshot);

        assert_eq!(restored.intern("entity:apple"), id_a);
        assert_eq!(restored.intern("entity:banana"), id_b);
    }

    #[test]
    fn snapshot_excludes_sentinel() {
        let interner = StringInterner::new();
        let _ = interner.intern("x");
        let snap = interner.to_snapshot();
        // Sentinel ("") must not appear in the snapshot.
        assert!(!snap.iter().any(|s| s.is_empty()));
        assert_eq!(snap.len(), 1);
    }

    #[test]
    fn concurrent_intern_is_consistent() {
        use std::sync::Arc;
        use std::thread;

        let interner = Arc::new(StringInterner::new());
        let n_threads = 8_usize;
        let n_strings = 50_usize;

        let handles: Vec<_> = (0..n_threads)
            .map(|t| {
                let interner = Arc::clone(&interner);
                thread::Builder::new()
                    .name(format!("test-intern-{t}"))
                    .spawn(move || {
                        (0..n_strings)
                            .map(|i| {
                                let s = format!("string:{i}");
                                interner.intern(&s)
                            })
                            .collect::<Vec<_>>()
                    })
                    .expect("thread spawn must succeed in tests")
            })
            .collect();

        let all_results: Vec<Vec<InternId>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread must not panic"))
            .collect();

        // All threads must agree on the same ID for the same string.
        for i in 0..n_strings {
            let s = format!("string:{i}");
            let expected_id = interner.intern(&s);
            for thread_results in &all_results {
                assert_eq!(thread_results[i], expected_id, "mismatch for {s}");
            }
        }

        // No gaps or duplicates in the reverse table.
        assert_eq!(interner.len(), n_strings);
    }
}
