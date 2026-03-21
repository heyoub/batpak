use crate::store::StoreError;
use serde::{Deserialize, Serialize};

// ProjectionCache: trait for caching projected state.
// Three impls: NoCache (default), RedbCache (optional), LmdbCache (optional).
pub trait ProjectionCache: Send + Sync + 'static {
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError>;
    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError>;
    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError>;
    fn sync(&self) -> Result<(), StoreError>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheMeta {
    pub watermark: u64,
    pub cached_at_us: i64,
}

#[derive(Clone, Debug)]
pub enum Freshness {
    Consistent,
    BestEffort { max_stale_ms: u64 },
}

// NoCache: default. Every read replays from segments. No state.
pub struct NoCache;

impl ProjectionCache for NoCache {
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

// RedbCache: backed by redb embedded database.
#[cfg(feature = "redb")]
pub struct RedbCache {
    db: redb::Database,
}

#[cfg(feature = "redb")]
const CACHE_TABLE: redb::TableDefinition<&[u8], &[u8]> =
    redb::TableDefinition::new("projection_cache");

#[cfg(feature = "redb")]
impl RedbCache {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        let db = redb::Database::create(path.as_ref())
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(Self { db })
    }
}

#[cfg(feature = "redb")]
impl ProjectionCache for RedbCache {
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let table = txn
            .open_table(CACHE_TABLE)
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        match table.get(key) {
            Ok(Some(guard)) => {
                let bytes = guard.value().to_vec();
                // Last 16 bytes = CacheMeta (watermark u64 LE + cached_at_us i64 LE)
                if bytes.len() < 16 {
                    return Ok(None);
                }
                let (value, meta_bytes) = bytes.split_at(bytes.len() - 16);
                let watermark = u64::from_le_bytes(
                    meta_bytes[..8]
                        .try_into()
                        .expect("split guarantees 16 bytes"),
                );
                let cached_at_us = i64::from_le_bytes(
                    meta_bytes[8..16]
                        .try_into()
                        .expect("split guarantees 16 bytes"),
                );
                Ok(Some((
                    value.to_vec(),
                    CacheMeta {
                        watermark,
                        cached_at_us,
                    },
                )))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StoreError::CacheFailed(e.to_string())),
        }
    }

    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError> {
        // Append CacheMeta as last 16 bytes of value
        let mut buf = Vec::with_capacity(value.len() + 16);
        buf.extend_from_slice(value);
        buf.extend_from_slice(&meta.watermark.to_le_bytes());
        buf.extend_from_slice(&meta.cached_at_us.to_le_bytes());

        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        {
            let mut table = txn
                .open_table(CACHE_TABLE)
                .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
            table
                .insert(key, buf.as_slice())
                .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        }
        txn.commit()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(())
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError> {
        use redb::ReadableTable;
        // redb has no built-in delete_prefix. Iterate range + collect keys + delete.
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let mut count = 0u64;
        {
            let mut table = txn
                .open_table(CACHE_TABLE)
                .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
            // Range: prefix..prefix_end (increment last non-0xFF byte)
            let mut end = prefix.to_vec();
            end.push(0xFF);
            let keys: Vec<Vec<u8>> = table
                .range(prefix..end.as_slice())
                .map_err(|e| StoreError::CacheFailed(e.to_string()))?
                .filter_map(|r| match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!("cache iteration error (skipping row): {e}");
                        None
                    }
                })
                .map(|(k, _)| k.value().to_vec())
                .collect();
            for key in &keys {
                table
                    .remove(key.as_slice())
                    .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
                count += 1;
            }
        }
        txn.commit()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(count)
    }

    fn sync(&self) -> Result<(), StoreError> {
        Ok(()) // redb commits are durable by default
    }
}

// LmdbCache: backed by LMDB via heed.
#[cfg(feature = "lmdb")]
pub struct LmdbCache {
    env: heed::Env,
    db: heed::Database<heed::types::Bytes, heed::types::Bytes>,
}

#[cfg(feature = "lmdb")]
impl LmdbCache {
    pub fn open(path: impl AsRef<std::path::Path>, map_size: usize) -> Result<Self, StoreError> {
        std::fs::create_dir_all(path.as_ref()).map_err(StoreError::Io)?;
        // SAFETY: We guarantee this path is opened at most once per process.
        // The Store owns the LmdbCache exclusively.
        let env = unsafe {
            heed::EnvOpenOptions::new()
                .map_size(map_size)
                .max_dbs(1)
                .open(path.as_ref())
                .map_err(|e| StoreError::CacheFailed(e.to_string()))?
        };
        let mut wtxn = env
            .write_txn()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let db = env
            .create_database(&mut wtxn, Some("projection_cache"))
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        wtxn.commit()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(Self { env, db })
    }
}

#[cfg(feature = "lmdb")]
impl ProjectionCache for LmdbCache {
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        let txn = self
            .env
            .read_txn()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        match self
            .db
            .get(&txn, key)
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?
        {
            Some(bytes) if bytes.len() >= 16 => {
                let (value, meta_bytes) = bytes.split_at(bytes.len() - 16);
                let watermark = u64::from_le_bytes(
                    meta_bytes[..8]
                        .try_into()
                        .expect("split guarantees 16 bytes"),
                );
                let cached_at_us = i64::from_le_bytes(
                    meta_bytes[8..16]
                        .try_into()
                        .expect("split guarantees 16 bytes"),
                );
                Ok(Some((
                    value.to_vec(),
                    CacheMeta {
                        watermark,
                        cached_at_us,
                    },
                )))
            }
            _ => Ok(None),
        }
    }

    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError> {
        let mut buf = Vec::with_capacity(value.len() + 16);
        buf.extend_from_slice(value);
        buf.extend_from_slice(&meta.watermark.to_le_bytes());
        buf.extend_from_slice(&meta.cached_at_us.to_le_bytes());

        let mut txn = self
            .env
            .write_txn()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        self.db
            .put(&mut txn, key, &buf)
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        txn.commit()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(())
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError> {
        // heed does NOT have delete_prefix. Use prefix_iter_mut + del_current.
        let mut txn = self
            .env
            .write_txn()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let mut iter = self
            .db
            .prefix_iter_mut(&mut txn, prefix)
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let mut count = 0u64;
        while iter
            .next()
            .transpose()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?
            .is_some()
        {
            // SAFETY: We do not hold any references to the current entry;
            // the .is_some() check consumed the Option without binding.
            unsafe {
                iter.del_current()
                    .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
            }
            count += 1;
        }
        drop(iter);
        txn.commit()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(count)
    }

    fn sync(&self) -> Result<(), StoreError> {
        self.env
            .force_sync()
            .map_err(|e| StoreError::CacheFailed(e.to_string()))
    }
}
