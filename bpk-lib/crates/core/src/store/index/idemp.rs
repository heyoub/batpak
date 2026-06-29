//! Durable idempotency key → receipt store (Phase 3, 0.8.3 schema-evolution).
//!
//! # Why this exists
//!
//! `AppendOptions::with_idempotency(key)` makes the key the `event_id` and the
//! writer returns the original receipt as a no-op on a duplicate. That dedup
//! was only durable inside an event's *retention window*: once `Retention`
//! compaction evicted the event, its `by_id` index entry vanished and a re-run
//! of the same key RE-APPENDED a duplicate. The bug: idempotency was not a true
//! durable correctness primitive.
//!
//! This module closes the gap with a dedicated sidecar that survives
//! compaction / retention / cold-start INDEPENDENT of event eviction:
//!
//! * an in-memory [`DashMap<u128, IdempEntry>`] capturing exactly the tuple
//!   needed to RECONSTRUCT the original `AppendReceipt` for a no-op return even
//!   when the underlying event has been evicted, and
//! * an on-disk artifact `index.idemp` (magic `FBATID`, version
//!   [`IDEMP_VERSION`], crc32fast CRC, written via `write_file_atomically`)
//!   restored UNCONDITIONALLY and early on open — it is an AUTHORITY the
//!   segment-scan index rebuild must NOT overwrite, and is NEVER reconstructed
//!   from a segment scan (segments may have evicted the events).
//!
//! # Growth bound — the window-priority hybrid
//!
//! A durable key store outlives event retention, so it grows unless bounded.
//! [`IdempotencyRetention`] picks the policy. The **window** is the inviolable
//! correctness guarantee; the **cap** is a soft bound + alarm that may only
//! ever evict keys ALREADY OUTSIDE the window. See [`IdempotencyStore::evict`]
//! for the verbatim eviction rule.
//!
//! justifies: INV-IDEMPOTENCY-DURABLE-WINDOW; this module is the durable
//! sidecar + window-priority hybrid that makes a within-window keyed retry an
//! unconditional no-op regardless of compaction, cold-start, or load.

use crate::store::platform::fs::{read as fs_read, write_file_atomically};
use crate::store::{EncodedBytes, ExtensionKey, StoreError};
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::path::Path;

use super::entry::{DiskPos, IndexEntry};
use crate::event::EventKind;

/// Magic bytes at the start of every `index.idemp` file.
pub(crate) const IDEMP_MAGIC: &[u8; 6] = b"FBATID";

/// On-disk format version stored in the `index.idemp` header.
/// v1: initial durable idempotency sidecar.
pub(crate) const IDEMP_VERSION: u16 = 1;

/// Final filename inside the data directory.
pub(crate) const IDEMP_FILENAME: &str = "index.idemp";

/// Header length: magic(6) + version(2) + crc(4).
const HEADER_LEN: usize = 6 + 2 + 4;

/// Default window guarantee: keep idempotency keys for the most recent
/// `DEFAULT_KEEP_SEQUENCES` committed global sequences. Generous on purpose —
/// it must comfortably outlive a realistic event-retention window so a retry
/// of a recently-committed key is always a no-op. Chosen consistently with the
/// store's generous segment/checkpoint defaults (256 MB segments, etc.).
pub(crate) const DEFAULT_KEEP_SEQUENCES: u64 = 16 * 1024 * 1024;

/// Default soft cap on total durable keys. Generous; the window always wins on
/// a residual pigeonhole (see [`IdempotencyStore::evict`]).
pub(crate) const DEFAULT_MAX_KEYS: u64 = 64 * 1024 * 1024;

/// Growth-bound policy for the durable idempotency store.
///
/// The window is the correctness guarantee; the cap is a soft bound + alarm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdempotencyRetention {
    /// Never evict. The store grows with the keyed-append rate forever. Only
    /// safe for bounded-lifetime stores or callers that compact externally.
    Unbounded,
    /// Keep keys whose `recorded_global_sequence` is within `keep_sequences`
    /// of the current frontier. Older keys are trimmed. This is the
    /// correctness guarantee with no soft cap.
    Window {
        /// How many global sequences back from the frontier to keep.
        keep_sequences: u64,
    },
    /// Window guarantee PLUS a soft cap and alarm. DEFAULT policy.
    ///
    /// The window is inviolable; the cap may only evict keys already OUTSIDE
    /// the window. If within-window keys alone exceed `max_keys` (a real
    /// key-rate spike), the window wins and the store temporarily exceeds
    /// `max_keys` (bounded by rate×window, not unbounded) with a loud
    /// diagnostic; [`OverflowPolicy`] then decides escalation.
    Hybrid {
        /// How many global sequences back from the frontier to keep.
        keep_sequences: u64,
        /// Soft cap on total durable keys.
        max_keys: u64,
    },
}

impl Default for IdempotencyRetention {
    fn default() -> Self {
        Self::Hybrid {
            keep_sequences: DEFAULT_KEEP_SEQUENCES,
            max_keys: DEFAULT_MAX_KEYS,
        }
    }
}

impl IdempotencyRetention {
    /// The window guarantee in global sequences, if any. `Unbounded` returns
    /// `None` (no aging).
    pub(crate) fn keep_sequences(self) -> Option<u64> {
        match self {
            Self::Unbounded => None,
            Self::Window { keep_sequences } | Self::Hybrid { keep_sequences, .. } => {
                Some(keep_sequences)
            }
        }
    }

    /// The soft cap on total keys, if any.
    pub(crate) fn max_keys(self) -> Option<u64> {
        match self {
            Self::Unbounded | Self::Window { .. } => None,
            Self::Hybrid { max_keys, .. } => Some(max_keys),
        }
    }
}

/// What to do when within-window keys alone exceed the soft cap (residual
/// pigeonhole). The window ALWAYS wins on correctness; this only decides
/// escalation behavior for the NEW keyed append that would push over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OverflowPolicy {
    /// Log a loud diagnostic and proceed. The store exceeds `max_keys`
    /// temporarily (bounded by rate×window). DEFAULT.
    #[default]
    Warn,
    /// Refuse the new keyed append with a clear error. Correctness over disk:
    /// the within-window keys already recorded are never evicted, so existing
    /// retries stay no-ops; only genuinely new keys are rejected.
    FailClosed,
    /// Signal backpressure / slow down. batpak's writer is single-threaded and
    /// exposes no clean append-time backpressure channel from this record
    /// path, so this is treated as `FailClosed` and noted in the diagnostic.
    Backpressure,
}

/// Minimal tuple captured per durable keyed append. Captures EXACTLY the fields
/// the writer's no-op path reads from an `IndexEntry` to reconstruct the
/// original `AppendReceipt` (see `store/write/writer/append.rs`):
/// `event_id`, `global_sequence`, `disk_pos`, `event_hash`, `prev_hash`,
/// `coord` (as entity+scope strings), `kind`, and `receipt_extensions`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct IdempEntry {
    /// The idempotency key (== the event_id of the original keyed append).
    pub(crate) key: u128,
    /// Original event id (identical to `key`, kept explicit for receipt build).
    pub(crate) event_id: u128,
    /// Global sequence assigned at the original commit.
    pub(crate) global_sequence: u64,
    /// Original frame location. Preserved verbatim even after the event is
    /// evicted — the reconstructed no-op receipt must always carry the ORIGINAL
    /// position so a within-window retry returns the original receipt unchanged.
    /// Eviction is tracked by the separate [`IdempEntry::event_evicted`] flag,
    /// never by mutating this field.
    pub(crate) disk_pos_segment: u64,
    /// Byte offset of the original frame.
    pub(crate) disk_pos_offset: u64,
    /// Byte length of the original frame.
    pub(crate) disk_pos_length: u32,
    /// Blake3 content hash of the original committed payload.
    pub(crate) content_hash: [u8; 32],
    /// Predecessor hash at the time of the original commit (needed to re-sign
    /// the reconstructed receipt identically).
    pub(crate) prev_hash: [u8; 32],
    /// Coordinate entity string.
    pub(crate) entity: String,
    /// Coordinate scope string.
    pub(crate) scope: String,
    /// Event kind discriminant.
    pub(crate) kind: EventKind,
    /// The global sequence at which this entry was RECORDED into the durable
    /// store. This is the value the window-priority rule ages against — it is
    /// the entry's position on the frontier timeline.
    pub(crate) recorded_global_sequence: u64,
    /// Whether this entry's underlying event frame has been evicted by
    /// retention compaction. Set by [`IdempotencyStore::mark_evicted`]; the
    /// `disk_pos_*` fields are NEVER mutated, so the reconstructed receipt keeps
    /// the original frame position.
    #[serde(default)]
    pub(crate) event_evicted: bool,
    /// Opaque receipt extensions committed with the original event.
    pub(crate) receipt_extensions: BTreeMap<ExtensionKey, EncodedBytes>,
}

impl IdempEntry {
    /// Capture the minimal reconstruction tuple from a freshly committed index
    /// entry plus the frontier sequence at record time.
    pub(crate) fn from_index_entry(entry: &IndexEntry, recorded_global_sequence: u64) -> Self {
        Self {
            key: entry.event_id,
            event_id: entry.event_id,
            global_sequence: entry.global_sequence,
            disk_pos_segment: entry.disk_pos.segment_id(),
            disk_pos_offset: entry.disk_pos.offset(),
            disk_pos_length: entry.disk_pos.length(),
            content_hash: entry.hash_chain.event_hash,
            prev_hash: entry.hash_chain.prev_hash,
            entity: entry.coord.entity().to_owned(),
            scope: entry.coord.scope().to_owned(),
            kind: entry.kind,
            recorded_global_sequence,
            event_evicted: false,
            receipt_extensions: entry.receipt_extensions.clone(),
        }
    }

    /// The recorded `disk_pos` as the typed [`DiskPos`] used by receipts.
    pub(crate) fn disk_pos(&self) -> DiskPos {
        DiskPos::new(
            self.disk_pos_segment,
            self.disk_pos_offset,
            self.disk_pos_length,
        )
    }

    /// Whether this entry's underlying event frame has been evicted (retention
    /// compaction). A no-op receipt for an evicted event still carries the
    /// original reconstruction tuple (including the original `disk_pos`); this
    /// flag lets callers distinguish "frame still live" from "deduplicated
    /// against an evicted event" without a disk probe. Non-test code reads the
    /// `event_evicted` field directly; this accessor is the test-facing reader.
    #[cfg(test)]
    pub(crate) fn is_event_evicted(&self) -> bool {
        self.event_evicted
    }
}

/// Outcome diagnostics from a single eviction pass. Used for tests and loud
/// logging; the store never silently changes a within-window answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EvictionReport {
    /// Keys trimmed because they aged out of the window.
    pub(crate) aged_out: u64,
    /// Keys trimmed by the cap from the OUT-OF-WINDOW region (free win).
    pub(crate) cap_trimmed_out_of_window: u64,
    /// True when within-window keys ALONE exceed the soft cap (residual
    /// pigeonhole). The window wins; the store temporarily exceeds `max_keys`.
    pub(crate) within_window_exceeds_cap: bool,
    /// Total keys remaining after the pass.
    pub(crate) remaining: u64,
}

/// Durable idempotency key → receipt-reconstruction store.
///
/// Lives on [`crate::store::index::StoreIndex`] so it is reachable both at
/// append time (the single writer thread holds `&StoreIndex`) and at
/// compaction / close time (lifecycle holds `store.index`). It is a separate
/// field from `by_id` and is therefore NOT cleared by
/// `replace_contents_from_fresh` — its survival across compaction is structural.
pub(crate) struct IdempotencyStore {
    map: DashMap<u128, IdempEntry>,
    retention: IdempotencyRetention,
    overflow: OverflowPolicy,
}

impl IdempotencyStore {
    /// Construct an empty store with the given policy.
    pub(crate) fn new(retention: IdempotencyRetention, overflow: OverflowPolicy) -> Self {
        Self {
            map: DashMap::new(),
            retention,
            overflow,
        }
    }

    /// Number of durable keys currently held.
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// Look up a key. Returns the reconstruction tuple on hit. This is the
    /// map-first authority consulted BEFORE `by_id` in the writer no-op check.
    pub(crate) fn get(&self, key: u128) -> Option<IdempEntry> {
        self.map.get(&key).map(|r| r.value().clone())
    }

    /// Whether a single NEW keyed append should be admitted under the soft cap.
    /// `frontier` lets us age out-of-window keys out before fail-closing, so a
    /// stale entry is trimmed rather than a fresh key refused. Re-recording a
    /// known key is always admitted (no growth).
    pub(crate) fn admit_new_key(&self, key: u128, frontier: u64) -> Result<(), StoreError> {
        if self.map.contains_key(&key) {
            return Ok(());
        }
        self.admit_unique_new_count(1, frontier)
    }

    /// Validate and admit a whole batch of candidate keys as a UNIT. Counts only
    /// keys NOT already present, rejects duplicate new keys WITHIN the batch
    /// (they would otherwise both pass a per-item check and derive duplicate
    /// event ids), and enforces `current + unique_new <= max_keys` atomically so
    /// a set of unique new keys can never collectively slip past a fail-closed
    /// cap. All-or-nothing. `frontier` drives the same age-out as
    /// [`IdempotencyStore::admit_new_key`].
    pub(crate) fn admit_new_keys(
        &self,
        keys: impl Iterator<Item = u128>,
        frontier: u64,
    ) -> Result<(), StoreError> {
        let mut seen_new: std::collections::HashSet<u128> = std::collections::HashSet::new();
        let mut unique_new: u64 = 0;
        for key in keys {
            // Already durable: re-recording is a no-op on growth.
            if self.map.contains_key(&key) {
                continue;
            }
            if !seen_new.insert(key) {
                return Err(StoreError::IdempotencyPartialBatch {
                    reason: "duplicate idempotency key in batch".into(),
                });
            }
            unique_new = unique_new.saturating_add(1);
        }
        self.admit_unique_new_count(unique_new, frontier)
    }

    /// Shared admission core: can `unique_new` genuinely-new keys be admitted
    /// without exceeding the soft cap? Ages out-of-window keys out first, then
    /// escalates per [`OverflowPolicy`]. `Unbounded`/`Window` (no cap) always
    /// admit.
    fn admit_unique_new_count(&self, unique_new: u64, frontier: u64) -> Result<(), StoreError> {
        let Some(max_keys) = self.retention.max_keys() else {
            return Ok(());
        };
        if unique_new == 0 {
            return Ok(());
        }
        let mut len = self.map.len() as u64;
        if len.saturating_add(unique_new) <= max_keys {
            return Ok(());
        }
        // Pressure: age out out-of-window keys before fail-closing so a stale
        // key is trimmed rather than a fresh one refused. justifies:
        // INV-IDEMPOTENCY-DURABLE-WINDOW
        self.evict(frontier);
        len = self.map.len() as u64;
        if len.saturating_add(unique_new) <= max_keys {
            return Ok(());
        }
        match self.overflow {
            OverflowPolicy::Warn => Ok(()),
            OverflowPolicy::FailClosed | OverflowPolicy::Backpressure => {
                let backpressure_note = matches!(self.overflow, OverflowPolicy::Backpressure);
                tracing::warn!(
                    target: "batpak::idemp",
                    len,
                    max_keys,
                    unique_new,
                    backpressure_note,
                    "durable idempotency store at soft cap; refusing new keyed append(s) (fail-closed)"
                );
                Err(StoreError::IdempotencyOverflowFailClosed { len, max_keys })
            }
        }
    }

    /// Record (or overwrite) a durable entry. Idempotent on the key.
    pub(crate) fn record(&self, entry: IdempEntry) {
        self.map.insert(entry.key, entry);
    }

    /// Mark durable entries whose underlying event frame is no longer live as
    /// EVICTED by setting the [`IdempEntry::event_evicted`] flag.
    /// `is_live(event_id)` reports whether the frame still exists in the live
    /// index. Run at the compaction tail so a subsequent no-op for a
    /// deduplicated-against-evicted key is honestly flagged. The whole
    /// reconstruction tuple — including `disk_pos_*` — is left immutable so the
    /// reconstructed receipt always carries the ORIGINAL frame position.
    pub(crate) fn mark_evicted(&self, is_live: impl Fn(u128) -> bool) {
        for mut entry in self.map.iter_mut() {
            if !entry.event_evicted && !is_live(entry.event_id) {
                entry.event_evicted = true;
            }
        }
    }

    /// Replace the whole map from a restored vector (cold-start authority).
    /// Existing contents are cleared first — this is the unconditional restore.
    pub(crate) fn restore(&self, entries: Vec<IdempEntry>) {
        self.map.clear();
        for entry in entries {
            self.map.insert(entry.key, entry);
        }
    }

    /// Snapshot all entries for persistence. Iteration is not linearizable but
    /// flushes always run from a quiesced path (close / compaction tail).
    pub(crate) fn snapshot(&self) -> Vec<IdempEntry> {
        self.map.iter().map(|r| r.value().clone()).collect()
    }

    /// THE window-priority hybrid eviction rule. See module docs and
    /// INV-IDEMPOTENCY-DURABLE-WINDOW.
    ///
    /// `frontier` is the current global-sequence frontier (next-to-assign).
    ///
    /// Invariant proved by `evict`: a key whose `recorded_global_sequence` is
    /// within `keep_sequences` of `frontier` is NEVER removed here — not by
    /// window-aging (it is inside the window) and not by the cap (the cap only
    /// touches the out-of-window region). The cap can never make a
    /// within-window retry re-append.
    pub(crate) fn evict(&self, frontier: u64) -> EvictionReport {
        let mut report = EvictionReport::default();

        // The window floor: an entry is INSIDE the window iff
        // recorded_global_sequence >= window_floor. With no keep_sequences
        // (Unbounded) everything is inside the window (floor 0) and nothing
        // ages out.
        let window_floor = match self.retention.keep_sequences() {
            None => {
                report.remaining = self.map.len() as u64;
                return report;
            }
            Some(keep) => frontier.saturating_sub(keep),
        };

        // ── Step 1: window-aging. Trim keys OLDER than the window. This is the
        // only place a key leaves the window; a within-window key is never a
        // candidate here because its recorded_global_sequence >= window_floor.
        let aged: Vec<u128> = self
            .map
            .iter()
            .filter(|r| r.value().recorded_global_sequence < window_floor)
            .map(|r| *r.key())
            .collect();
        for key in &aged {
            self.map.remove(key);
        }
        report.aged_out = aged.len() as u64;

        // ── Step 2: soft cap. The cap may ONLY evict keys ALREADY OUTSIDE the
        // window — pure free win on the out-of-window tail. It must NEVER cross
        // into within-window territory. After step 1 the only remaining
        // out-of-window keys are those with recorded_global_sequence ==
        // window_floor..(strictly < frontier-keep is already gone); in
        // practice step 1 removes everything strictly below window_floor, so
        // step 2 has nothing extra to trim unless window_floor itself sits on
        // the boundary. We keep the structure explicit so the invariant is
        // legible and future window definitions stay safe.
        if let Some(max_keys) = self.retention.max_keys() {
            let len = self.map.len() as u64;
            if len > max_keys {
                // Count within-window keys. If they ALONE exceed the cap, the
                // window wins (residual pigeonhole): we do NOT trim them.
                let within_window = self
                    .map
                    .iter()
                    .filter(|r| r.value().recorded_global_sequence >= window_floor)
                    .count() as u64;

                if within_window >= max_keys {
                    // Residual pigeonhole: window wins, store exceeds max_keys.
                    report.within_window_exceeds_cap = true;
                    tracing::warn!(
                        target: "batpak::idemp",
                        len,
                        max_keys,
                        within_window,
                        "durable idempotency store exceeds soft cap from within-window keys \
                         alone (key-rate spike); window wins, correctness preserved, store \
                         temporarily over cap (bounded by rate x window)"
                    );
                } else {
                    // Trim only the out-of-window surplus down toward the cap.
                    let trim_target = len.saturating_sub(max_keys);
                    let out_of_window: Vec<u128> = self
                        .map
                        .iter()
                        .filter(|r| r.value().recorded_global_sequence < window_floor)
                        .map(|r| *r.key())
                        .take(usize::try_from(trim_target).unwrap_or(usize::MAX))
                        .collect();
                    for key in &out_of_window {
                        self.map.remove(key);
                    }
                    report.cap_trimmed_out_of_window = out_of_window.len() as u64;
                }
            }
        }

        report.remaining = self.map.len() as u64;
        report
    }

    /// Persist the current map to `index.idemp` atomically. Runs from quiesced
    /// paths (close, compaction tail). Format:
    /// `magic(6) | version(2 le) | crc(4 le) | body(msgpack Vec<IdempEntry>)`.
    /// CRC covers the body only (same layout as the checkpoint footer).
    pub(crate) fn flush(&self, data_dir: &Path) -> Result<(), StoreError> {
        let entries = self.snapshot();
        let body = crate::encoding::to_bytes(&entries)
            .map_err(|error| StoreError::ser_msg(&format!("encode idemp store: {error}")))?;
        let crc = crc32fast::hash(&body);
        let final_path = data_dir.join(IDEMP_FILENAME);
        write_file_atomically(data_dir, &final_path, "idempotency-store", |file| {
            use std::io::Write;
            file.write_all(IDEMP_MAGIC).map_err(StoreError::Io)?;
            file.write_all(&IDEMP_VERSION.to_le_bytes())
                .map_err(StoreError::Io)?;
            file.write_all(&crc.to_le_bytes()).map_err(StoreError::Io)?;
            file.write_all(&body).map_err(StoreError::Io)?;
            Ok(())
        })?;
        tracing::debug!(
            target: "batpak::idemp",
            count = entries.len(),
            "flushed durable idempotency store"
        );
        Ok(())
    }
}

/// Result of attempting to read `index.idemp` from disk.
pub(crate) enum IdempLoad {
    /// File present and valid; the decoded entries.
    Loaded(Vec<IdempEntry>),
    /// File absent. First open, or store has no durable keys yet.
    Missing,
    /// File present but unreadable / wrong magic / bad CRC / decode failure.
    /// Treated as ABSENT (logged loudly) — the store is still correct, it just
    /// loses durable-dedup history, mirroring the checkpoint posture.
    Invalid {
        /// Human-readable reason for diagnostics.
        reason: String,
    },
}

/// Read and validate `index.idemp`.
///
/// * Missing file → [`IdempLoad::Missing`] (not an error).
/// * Wrong magic / short / bad CRC / decode failure → [`IdempLoad::Invalid`]
///   (logged loudly, treated as absent — never crashes cold-start).
/// * On-disk version NEWER than [`IDEMP_VERSION`] → hard
///   [`StoreError::IdempotencyFutureVersion`], mirroring the schema-evo
///   FutureVersion stance: a reader can never reconstruct a format it predates.
pub(crate) fn read_idemp_file(data_dir: &Path) -> Result<IdempLoad, StoreError> {
    let path = data_dir.join(IDEMP_FILENAME);
    let raw = match fs_read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(IdempLoad::Missing)
        }
        Err(error) => {
            tracing::warn!(
                target: "batpak::idemp",
                path = %path.display(),
                error = %error,
                "failed to read idempotency store file"
            );
            return Ok(IdempLoad::Invalid {
                reason: format!("read failed: {error}"),
            });
        }
    };

    if raw.len() < HEADER_LEN {
        tracing::warn!(
            target: "batpak::idemp",
            path = %path.display(),
            len = raw.len(),
            "idempotency store file too short for a valid header — ignoring"
        );
        return Ok(IdempLoad::Invalid {
            reason: format!("file too short: {} bytes", raw.len()),
        });
    }

    if &raw[..6] != IDEMP_MAGIC.as_ref() {
        tracing::warn!(
            target: "batpak::idemp",
            path = %path.display(),
            "idempotency store file has wrong magic bytes — ignoring"
        );
        return Ok(IdempLoad::Invalid {
            reason: "wrong magic bytes".to_owned(),
        });
    }

    let version = u16::from_le_bytes([raw[6], raw[7]]);
    if version > IDEMP_VERSION {
        // FutureVersion: hard error, mirror the schema-evolution stance.
        return Err(StoreError::IdempotencyFutureVersion {
            stored: version,
            current: IDEMP_VERSION,
        });
    }
    if version != IDEMP_VERSION {
        // The header is unauthenticated (the CRC covers the body only), so a
        // corrupted lower version with a CRC-valid body would otherwise load as
        // the current version. Degrade as absent (same posture as a bad CRC).
        tracing::warn!(
            target: "batpak::idemp",
            path = %path.display(),
            version,
            current = IDEMP_VERSION,
            "idempotency store file declares an unsupported version — ignoring"
        );
        return Ok(IdempLoad::Invalid {
            reason: format!("unsupported version: {version}"),
        });
    }

    let stored_crc = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let body = &raw[HEADER_LEN..];
    let computed_crc = crc32fast::hash(body);
    if stored_crc != computed_crc {
        tracing::warn!(
            target: "batpak::idemp",
            path = %path.display(),
            stored = stored_crc,
            computed = computed_crc,
            "idempotency store CRC mismatch — file is corrupt, ignoring"
        );
        return Ok(IdempLoad::Invalid {
            reason: format!("crc mismatch: stored {stored_crc}, computed {computed_crc}"),
        });
    }

    match crate::encoding::from_bytes::<Vec<IdempEntry>>(body) {
        Ok(entries) => Ok(IdempLoad::Loaded(entries)),
        Err(error) => {
            tracing::warn!(
                target: "batpak::idemp",
                path = %path.display(),
                error = %error,
                "idempotency store body failed to decode — ignoring"
            );
            Ok(IdempLoad::Invalid {
                reason: format!("decode failed: {error}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    // These unit tests prove the window-priority eviction rule directly on
    // synthetic entries — the correctness-critical "cake-and-eat-it" property
    // in isolation from the store machinery.
    use super::*;

    fn entry(key: u128, recorded_global_sequence: u64) -> IdempEntry {
        IdempEntry {
            key,
            event_id: key,
            global_sequence: recorded_global_sequence,
            disk_pos_segment: 0,
            disk_pos_offset: 0,
            disk_pos_length: 0,
            content_hash: [0u8; 32],
            prev_hash: [0u8; 32],
            entity: "e".to_owned(),
            scope: "s".to_owned(),
            kind: EventKind::custom(0xB, 1),
            recorded_global_sequence,
            event_evicted: false,
            receipt_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn cap_never_evicts_within_window_keys_even_under_residual_pigeonhole() {
        // Window keeps last 100 sequences; cap is only 3. Record 10 keys all
        // within the window (recorded at 90..100, frontier 100). The cap MUST
        // NOT evict any of them: within-window keys are inviolable.
        let store = IdempotencyStore::new(
            IdempotencyRetention::Hybrid {
                keep_sequences: 100,
                max_keys: 3,
            },
            OverflowPolicy::Warn,
        );
        for i in 0..10u128 {
            let seq = u64::try_from(i).expect("loop index 0..10 fits u64");
            store.record(entry(i, 90 + seq));
        }
        let report = store.evict(100);
        assert!(report.within_window_exceeds_cap, "residual pigeonhole");
        assert_eq!(report.aged_out, 0);
        assert_eq!(report.cap_trimmed_out_of_window, 0);
        assert_eq!(report.remaining, 10, "all within-window keys survive");
        assert!((0..10u128).all(|i| store.get(i).is_some()));
    }

    #[test]
    fn window_aging_trims_only_out_of_window_keys() {
        let store = IdempotencyStore::new(
            IdempotencyRetention::Window { keep_sequences: 10 },
            OverflowPolicy::Warn,
        );
        // Old keys at sequences 0..5, recent keys at 95..100; frontier 100, so
        // window floor is 90. Old keys age out; recent keys remain.
        for i in 0..5u128 {
            store.record(entry(
                i,
                u64::try_from(i).expect("loop index 0..5 fits u64"),
            ));
        }
        for i in 95..100u128 {
            store.record(entry(
                i,
                u64::try_from(i).expect("loop index 95..100 fits u64"),
            ));
        }
        let report = store.evict(100);
        assert_eq!(report.aged_out, 5, "five out-of-window keys aged out");
        assert_eq!(report.remaining, 5, "five within-window keys remain");
        for i in 0..5u128 {
            assert!(store.get(i).is_none(), "aged-out key {i} is gone");
        }
        assert!((95..100u128).all(|i| store.get(i).is_some()), "kept");
    }

    #[test]
    fn unbounded_never_evicts() {
        let store = IdempotencyStore::new(IdempotencyRetention::Unbounded, OverflowPolicy::Warn);
        for i in 0..50u128 {
            store.record(entry(
                i,
                u64::try_from(i).expect("loop index 0..50 fits u64"),
            ));
        }
        let report = store.evict(1_000_000);
        assert_eq!(report.aged_out, 0);
        assert_eq!(report.cap_trimmed_out_of_window, 0);
        assert_eq!(report.remaining, 50);
    }

    fn fail_closed(keep_sequences: u64, max_keys: u64) -> IdempotencyStore {
        IdempotencyStore::new(
            IdempotencyRetention::Hybrid {
                keep_sequences,
                max_keys,
            },
            OverflowPolicy::FailClosed,
        )
    }

    #[test]
    fn admit_new_key_fail_closed_refuses_only_new_keys_over_cap() {
        let store = fail_closed(1000, 2);
        store.record(entry(1, 1));
        store.record(entry(2, 2));
        // Re-recording an existing key is admitted; a new key over the cap is not
        // (both held keys are within window at frontier 2).
        assert!(store.admit_new_key(1, 2).is_ok());
        assert!(matches!(
            store.admit_new_key(99, 2),
            Err(StoreError::IdempotencyOverflowFailClosed { .. })
        ));
    }

    #[test]
    fn admit_new_key_ages_out_of_window_before_fail_closing() {
        // Both held keys are OUT of window at frontier 1000 (floor 990). A new
        // key must age them out first and be admitted, not refused.
        let store = fail_closed(10, 2);
        store.record(entry(1, 1));
        store.record(entry(2, 2));
        assert!(
            store.admit_new_key(99, 1000).is_ok(),
            "PROPERTY: out-of-window keys must age out before a fresh key is refused"
        );
        assert_eq!(store.len(), 0, "stale keys were aged out by admission");
    }

    #[test]
    fn admit_new_keys_validates_the_batch_as_a_unit() {
        // (1) Duplicate new keys within one batch are rejected (they would
        // otherwise derive duplicate event ids).
        let dup_store = fail_closed(1000, 1000);
        let dup_err = dup_store
            .admit_new_keys([7u128, 7u128].into_iter(), 0)
            .expect_err("PROPERTY: two identical new keys in a batch must be rejected");
        assert!(
            matches!(dup_err, StoreError::IdempotencyPartialBatch { .. }),
            "duplicate new key must surface IdempotencyPartialBatch, got {dup_err:?}"
        );

        // (2) Cap 3 with 2 within-window keys held: a batch of 2 unique NEW keys
        // reaches 4 > 3 and is rejected as a unit even though each alone passes a
        // per-item len check; a batch that fits (1 new => 3 == cap) is admitted.
        let store = fail_closed(1000, 3);
        store.record(entry(1, 1));
        store.record(entry(2, 2));
        let err = store
            .admit_new_keys([10u128, 11u128].into_iter(), 2)
            .expect_err("PROPERTY: a unique-new batch exceeding the cap must be rejected");
        assert!(
            matches!(err, StoreError::IdempotencyOverflowFailClosed { .. }),
            "over-cap unique batch must surface IdempotencyOverflowFailClosed, got {err:?}"
        );
        assert!(store.admit_new_keys([10u128].into_iter(), 2).is_ok());
    }

    #[test]
    fn mark_evicted_flags_only_dropped_events_and_preserves_tuple() {
        let store = IdempotencyStore::new(IdempotencyRetention::Unbounded, OverflowPolicy::Warn);
        let mut live = entry(1, 1);
        live.disk_pos_segment = 7;
        live.global_sequence = 1;
        let mut dropped = entry(2, 2);
        dropped.disk_pos_segment = 9;
        dropped.global_sequence = 2;
        dropped.content_hash = [0xAB; 32];
        store.record(live);
        store.record(dropped);

        // Event 1 stays live; event 2's frame was dropped.
        store.mark_evicted(|event_id| event_id == 1);

        let one = store.get(1).expect("event 1 was recorded and is live");
        assert!(!one.is_event_evicted() && !one.event_evicted);
        assert_eq!(one.disk_pos_segment, 7);

        let two = store.get(2).expect("event 2 was recorded");
        assert!(two.is_event_evicted() && two.event_evicted);
        // disk_pos is UNCHANGED after eviction: the reconstructed receipt must
        // carry the ORIGINAL frame position. The rest of the tuple is preserved.
        assert_eq!(two.disk_pos_segment, 9, "disk_pos unchanged after eviction");
        assert_eq!(two.global_sequence, 2, "sequence preserved");
        assert_eq!(two.content_hash, [0xAB; 32], "content hash preserved");
    }

    #[test]
    fn flush_then_read_roundtrips() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = IdempotencyStore::new(IdempotencyRetention::default(), OverflowPolicy::Warn);
        store.record(entry(7, 7));
        store.record(entry(8, 8));
        store
            .flush(dir.path())
            .expect("flush idempotency store to disk");
        let loaded = read_idemp_file(dir.path()).expect("read back the flushed idempotency file");
        assert!(matches!(&loaded, IdempLoad::Loaded(e) if e.len() == 2));
    }

    #[test]
    fn future_version_header_constants_are_stable() {
        // Format identity guard: magic + current version must not drift
        // silently. A future-version on-disk file (version > IDEMP_VERSION) is
        // exercised end-to-end as a hard error in the corruption-recovery
        // integration suite (read_idemp_file is crate-private).
        assert_eq!(IDEMP_MAGIC, b"FBATID");
        assert_eq!(IDEMP_VERSION, 1);
    }
}
