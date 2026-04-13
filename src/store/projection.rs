use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tempfile::NamedTempFile;

/// Describes optional capabilities supported by a cache backend.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheCapabilities {
    /// Whether this backend supports `prefetch()` hints for pre-warming.
    pub supports_prefetch: bool,
    /// Whether this backend is a no-op (e.g. `NoCache`). When true, the
    /// projection flow skips the external-cache probe entirely.
    pub is_noop: bool,
}

impl CacheCapabilities {
    /// Return capabilities with no optional features enabled.
    pub const fn none() -> Self {
        Self {
            supports_prefetch: false,
            is_noop: false,
        }
    }

    /// Return capabilities indicating support for prefetch hints.
    pub const fn prefetch_hints() -> Self {
        Self {
            supports_prefetch: true,
            is_noop: false,
        }
    }
}

/// Trait for caching projected state. Two impls: `NoCache` (default), `NativeCache`.
pub trait ProjectionCache: Send + Sync + 'static {
    /// Return the capabilities advertised by this cache backend.
    fn capabilities(&self) -> CacheCapabilities;
    /// Retrieve a cached value and its metadata by key. Returns `None` on a cache miss.
    ///
    /// # Errors
    /// Returns `StoreError::CacheFailed` if the underlying cache backend fails.
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError>;
    /// Store a value with associated metadata under the given key.
    ///
    /// # Errors
    /// Returns `StoreError::CacheFailed` if the underlying cache backend fails.
    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError>;
    /// Delete all entries whose keys start with the given prefix. Returns the number of entries removed.
    ///
    /// # Errors
    /// Returns `StoreError::CacheFailed` if the underlying cache backend fails.
    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError>;
    /// Flush any pending writes to durable storage.
    ///
    /// # Errors
    /// Returns `StoreError::CacheFailed` if flushing the cache backend fails.
    fn sync(&self) -> Result<(), StoreError>;

    /// Hint that this key is likely to be requested soon. Implementations may
    /// pre-warm internal caches or pre-compute values. Default: no-op.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the prefetch operation fails.
    fn prefetch(&self, _key: &[u8], _predicted_meta: CacheMeta) -> Result<(), StoreError> {
        Ok(()) // default: no-op (NoCache, lazy impls)
    }
}

/// Metadata stored alongside each cached projection value.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheMeta {
    /// Global sequence watermark at the time the value was cached.
    pub watermark: u64,
    /// Wall-clock timestamp (microseconds since epoch) when the value was cached.
    pub cached_at_us: i64,
}

/// Byte layout: value bytes followed by 16 bytes of metadata (watermark u64 LE + cached_at_us i64 LE).
const CACHE_META_SIZE: usize = 16;

impl CacheMeta {
    /// Encode value + metadata into a single byte buffer for cache storage.
    pub(crate) fn encode_with_value(&self, value: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(value.len() + CACHE_META_SIZE);
        buf.extend_from_slice(value);
        buf.extend_from_slice(&self.watermark.to_le_bytes());
        buf.extend_from_slice(&self.cached_at_us.to_le_bytes());
        buf
    }

    /// Decode value + metadata from a cache-stored byte buffer.
    pub(crate) fn decode_from_bytes(bytes: &[u8]) -> Result<(Vec<u8>, Self), StoreError> {
        if bytes.len() < CACHE_META_SIZE {
            return Err(StoreError::cache_msg("corrupt cache metadata: too short"));
        }
        let (value, meta_bytes) = bytes.split_at(bytes.len() - CACHE_META_SIZE);
        let watermark = u64::from_le_bytes(
            meta_bytes[..8]
                .try_into()
                .map_err(|_| StoreError::cache_msg("corrupt cache metadata"))?,
        );
        let cached_at_us = i64::from_le_bytes(
            meta_bytes[8..16]
                .try_into()
                .map_err(|_| StoreError::cache_msg("corrupt cache metadata"))?,
        );
        Ok((
            value.to_vec(),
            Self {
                watermark,
                cached_at_us,
            },
        ))
    }
}

/// Controls how stale a cached projection may be when returned by `project()`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Freshness {
    /// Always replay from the current head; never return a stale cached value.
    Consistent,
    /// Return a cached value if it is no older than `max_stale_ms` milliseconds.
    MaybeStale {
        /// Maximum age in milliseconds a cached value may have before forcing a replay.
        max_stale_ms: u64,
    },
}

/// No-op cache backend. Every `project()` call replays events from segments; nothing is stored.
pub struct NoCache;

impl ProjectionCache for NoCache {
    fn capabilities(&self) -> CacheCapabilities {
        CacheCapabilities {
            is_noop: true,
            ..CacheCapabilities::none()
        }
    }

    fn get(&self, _key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        Ok(None) // always miss — forces replay
    }

    fn put(&self, _key: &[u8], _value: &[u8], _meta: CacheMeta) -> Result<(), StoreError> {
        Ok(()) // no-op
    }

    fn delete_prefix(&self, _prefix: &[u8]) -> Result<u64, StoreError> {
        Ok(0) // nothing to delete
    }

    fn sync(&self) -> Result<(), StoreError> {
        Ok(()) // nothing to sync
    }
}

/// Built-in file-backed projection cache. Always available (no feature flag).
///
/// Each cache entry is stored as a single file under a sharded directory
/// layout: `<root>/<hex_prefix_2chars>/<full_hex_key>.bin`. Writes use
/// the same atomic temp-file-then-rename pattern as `checkpoint.rs`.
///
/// **Performance note:** NativeCache is correctness-first. It issues a
/// filesystem `open()` + `read()` per cache hit, which is slower than
/// an in-process B+tree. The trade-off is acceptable because cache misses
/// cost full event replay (milliseconds), which dwarfs even a 10x slower
/// cache hit (microseconds).
pub struct NativeCache {
    root: PathBuf,
}

impl NativeCache {
    /// Open (or create) a native file-backed projection cache at the given path.
    ///
    /// # Errors
    /// Returns `StoreError::CacheFailed` if the root directory cannot be created.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        let root = path.as_ref().to_path_buf();
        reject_symlink_leaf(&root)?;
        std::fs::create_dir_all(&root).map_err(StoreError::cache_error)?;
        Ok(Self { root })
    }

    /// Compute the file path for a cache key: `<root>/<shard>/<hex_key>.bin`
    fn key_path(&self, key: &[u8]) -> (PathBuf, PathBuf) {
        let hex = to_hex(key);
        let shard = if hex.len() >= 2 { &hex[..2] } else { "00" };
        let shard_dir = self.root.join(shard);
        let file_path = shard_dir.join(format!("{hex}.bin"));
        (shard_dir, file_path)
    }
}

impl ProjectionCache for NativeCache {
    fn capabilities(&self) -> CacheCapabilities {
        CacheCapabilities::none()
    }

    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        let (_shard, path) = self.key_path(key);
        match std::fs::read(&path) {
            Ok(bytes) => match CacheMeta::decode_from_bytes(&bytes) {
                Ok((value, meta)) => Ok(Some((value, meta))),
                Err(_) => {
                    // Corrupt cache file — self-heal by deleting it.
                    tracing::warn!("corrupt cache file, deleting: {}", path.display());
                    let _ = std::fs::remove_file(&path);
                    Ok(None)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            // Real IO errors (permissions, bad mount, etc.) surface as CacheFailed
            // per the trait contract. Silent degradation would hide real problems.
            Err(e) => Err(StoreError::CacheFailed(Box::new(e))),
        }
    }

    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError> {
        let (shard_dir, final_path) = self.key_path(key);

        // Ensure shard directory exists (lazy creation).
        reject_symlink_leaf(&shard_dir)?;
        std::fs::create_dir_all(&shard_dir).map_err(StoreError::cache_error)?;
        reject_symlink_leaf(&final_path)?;

        let buf = meta.encode_with_value(value);

        // Atomic write: temp file → rename. **Intentionally no fsync.**
        //
        // The projection cache is rebuildable from segments — losing a cache
        // file on power loss is recoverable by replaying events. Atomicity
        // (no torn reads) comes from `std::fs::rename`, which is atomic on
        // POSIX and on Windows since Rust 1.57. We do NOT need durability.
        //
        // Skipping the per-write `sync_all()` and directory fsync removes
        // ~600 µs of latency per cache write, which previously dwarfed the
        // savings from incremental projection apply. There is no public path
        // to force per-cache durability for `NativeCache` (`sync()` on this
        // backend is a no-op by design); a power-loss-recoverable cache is
        // an explicit non-goal of this backend, since the segment log is the
        // source of truth and a missing cache entry simply triggers a
        // replay-and-rewrite on the next `project()` call.
        let write_result = (|| -> Result<(), StoreError> {
            let tmp = NamedTempFile::new_in(&shard_dir).map_err(StoreError::cache_error)?;
            {
                use std::io::Write;
                let mut f = std::io::BufWriter::new(tmp.as_file());
                f.write_all(&buf).map_err(StoreError::cache_error)?;
                f.into_inner()
                    .map_err(|e| StoreError::CacheFailed(Box::new(e.into_error())))?;
            }
            tmp.persist(&final_path)
                .map_err(|e| StoreError::CacheFailed(Box::new(e.error)))?;
            Ok(())
        })();
        write_result
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError> {
        let hex_prefix = to_hex(prefix);
        let mut count = 0u64;

        // Read all shard directories.
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(StoreError::CacheFailed(Box::new(e))),
        };

        for dir_entry in entries.filter_map(|e| e.ok()) {
            let shard_path = dir_entry.path();
            if !shard_path.is_dir() {
                continue;
            }

            // Optimization: if hex_prefix is >= 2 chars, skip non-matching shards.
            if hex_prefix.len() >= 2 {
                if let Some(shard_name) = shard_path.file_name().and_then(|n| n.to_str()) {
                    if !hex_prefix.starts_with(shard_name)
                        && !shard_name.starts_with(&hex_prefix[..2])
                    {
                        continue;
                    }
                }
            }

            let shard_entries = match std::fs::read_dir(&shard_path) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for file_entry in shard_entries.filter_map(|e| e.ok()) {
                let file_name = file_entry.file_name();
                let name = match file_name.to_str() {
                    Some(n) if n.ends_with(".bin") => &n[..n.len() - 4],
                    _ => continue,
                };
                if name.starts_with(&hex_prefix) && std::fs::remove_file(file_entry.path()).is_ok()
                {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    fn sync(&self) -> Result<(), StoreError> {
        // Intentional no-op. The `ProjectionCache::sync` trait method is the
        // contract surface for backends whose `put` is buffered or
        // non-durable; `NativeCache::put` is already a `tempfile + rename`
        // sequence (atomic but not fsynced) and the cache is treated as a
        // rebuildable derivative of the segment log. There is no in-process
        // buffer to flush. A future custom backend (e.g. one that buffers
        // writes in memory or talks to a remote KV) MUST implement `sync`
        // properly; `NativeCache` exists in the no-op camp by design, and
        // there is no public `Store` API path to invoke it.
        Ok(())
    }
}

fn reject_symlink_leaf(path: &std::path::Path) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(StoreError::CacheFailed(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "refusing to use cache path through symlink {}",
                    path.display()
                ),
            ))))
        }
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Encode bytes as lowercase hex string.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_meta_encode_decode_roundtrip() {
        let meta = CacheMeta {
            watermark: 42,
            cached_at_us: 1_700_000_000_000,
        };
        let value = b"hello world";
        let encoded = meta.encode_with_value(value);
        let (decoded_value, decoded_meta) =
            CacheMeta::decode_from_bytes(&encoded).expect("decode should succeed");
        assert_eq!(decoded_value, value);
        assert_eq!(decoded_meta.watermark, 42);
        assert_eq!(decoded_meta.cached_at_us, 1_700_000_000_000);
    }

    #[test]
    fn cache_meta_decode_rejects_short_buffer() {
        let short = [0u8; 8];
        let result = CacheMeta::decode_from_bytes(&short);
        assert!(result.is_err());
    }

    #[test]
    fn cache_meta_roundtrip_empty_value() {
        let meta = CacheMeta {
            watermark: 0,
            cached_at_us: 0,
        };
        let encoded = meta.encode_with_value(b"");
        let (decoded_value, decoded_meta) =
            CacheMeta::decode_from_bytes(&encoded).expect("decode should succeed");
        assert!(decoded_value.is_empty());
        assert_eq!(decoded_meta.watermark, 0);
        assert_eq!(decoded_meta.cached_at_us, 0);
    }
}
