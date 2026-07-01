//! Durable keyset persistence + cold-start rehydration for the crypto-shred
//! [`KeyStore`] (Stage B).
//!
//! # On-disk layout — a SINGLE atomically-rewritten keyset file
//!
//! The whole keyset (every `scope → 32-byte key`, plus the scope granularity)
//! lives in ONE file, [`KEYSET_FILENAME`], rewritten in full on each
//! [`KeyStore::flush`] through the crash-safe
//! [`write_file_atomically_with_fs`] seam. Format:
//! `magic(6) | version(2 le) | crc(4 le) | body(msgpack)` — the same header
//! shape as the durable idempotency store, so the CRC covers the body only.
//!
//! Why single-file over per-scope key files:
//!   * **Crash safety is trivially correct.** There is exactly ONE atomic
//!     publish point (the temp-file rename), so a torn flush leaves the keyset
//!     either the OLD complete version or the NEW complete version — never a
//!     half-updated mix. Per-scope files (destroy = `unlink`, mint = new file)
//!     would spread a multi-key change across many independent renames/unlinks
//!     with no single atomic point; making THAT crash-safe would need a manifest
//!     or journal to avoid a partially-applied keyset after a mid-batch crash.
//!   * It mirrors the established durable-authority pattern (signing registry /
//!     idempotency store / cold-start artifacts) exactly.
//!
//! **Tradeoff flagged:** single-file flush is `O(keys)` — every flush rewrites
//! the entire keyset, so at a fine granularity ([`KeyScopeGranularity::PerEvent`]
//! over millions of events) a flush re-serialises the whole set. Per-scope files
//! would be `O(1)` per mint/destroy but multiply inode count and forfeit the
//! single atomic-consistency point (needing a journal for crash safety). Because
//! crypto-shred correctness hinges on an always-consistent keyset and Stage B
//! flushes from quiesced points (not per mint), single-file is the right default;
//! a future log-structured/journaled keyset can lift the amplification if a
//! fine-granularity deployment ever needs it.
//!
//! # Threat model — keys at rest (documented, not silently assumed)
//!
//! The keyset lives inside the store's own data directory, so the keys sit next
//! to the ciphertext they protect. What crypto-shred DOES buy: once a scope's key
//! is destroyed AND that destruction is flushed, the payloads sealed under it are
//! unrecoverable even to an operator with full disk access — deletion becomes
//! cryptographically effective rather than a best-effort overwrite. What it does
//! NOT buy: it does not protect a disk image captured *before* the shred (the key
//! was still present then), and co-locating keys with data means a stolen data
//! directory yields both. Hardening the keyset location — a separate volume, an
//! OS keyring, or an external KMS holding the keyset out of the data dir — is a
//! deployment / future-stage concern, deliberately out of Stage B's mechanism.
//!
//! # Fail-closed on a corrupt keyset
//!
//! Unlike the idempotency store (which degrades a corrupt sidecar to "absent"),
//! an unreadable keyset is a HARD [`StoreError::KeysetCorrupt`]. A silently-empty
//! key store would re-mint every scope's key from scratch, rendering all prior
//! ciphertext unrecoverable — an accidental total shred. Only a genuinely ABSENT
//! file (first open, or a store that has never flushed) rehydrates to an empty
//! store; every other outcome fails the open so the operator can restore the real
//! keyset from backup.

use super::{KeyScope, KeyScopeGranularity, KeyStore, PayloadKey, KEY_LEN};
use crate::store::file_classification::KEYSET_FILENAME;
use crate::store::platform::fs::{read as fs_read, write_file_atomically_with_fs, RealFs, StoreFs};
use crate::store::StoreError;
use std::collections::BTreeMap;
use std::path::Path;
use zeroize::{Zeroize, Zeroizing};

/// Magic bytes at the start of every keyset file.
pub(crate) const KEYSET_MAGIC: &[u8; 6] = b"FBATKS";

/// On-disk format version stored in the keyset header.
/// v1: initial single-file crypto-shred keyset.
pub(crate) const KEYSET_VERSION: u16 = 1;

/// Header length: magic(6) + version(2) + crc(4).
const HEADER_LEN: usize = 6 + 2 + 4;

// Stable on-disk discriminants for the scope granularity. Kept explicit (not the
// in-memory enum ordinal) so the wire byte never silently tracks a source-order
// change, and so an unknown byte can fail closed on load.
const DISC_PER_ENTITY: u8 = 1;
const DISC_PER_CATEGORY: u8 = 2;
const DISC_PER_TYPE_ID: u8 = 3;
const DISC_PER_EVENT: u8 = 4;

fn granularity_to_disc(granularity: KeyScopeGranularity) -> u8 {
    match granularity {
        KeyScopeGranularity::PerEntity => DISC_PER_ENTITY,
        KeyScopeGranularity::PerCategory => DISC_PER_CATEGORY,
        KeyScopeGranularity::PerTypeId => DISC_PER_TYPE_ID,
        KeyScopeGranularity::PerEvent => DISC_PER_EVENT,
    }
}

fn granularity_from_disc(disc: u8) -> Option<KeyScopeGranularity> {
    match disc {
        DISC_PER_ENTITY => Some(KeyScopeGranularity::PerEntity),
        DISC_PER_CATEGORY => Some(KeyScopeGranularity::PerCategory),
        DISC_PER_TYPE_ID => Some(KeyScopeGranularity::PerTypeId),
        DISC_PER_EVENT => Some(KeyScopeGranularity::PerEvent),
        _ => None,
    }
}

/// The full keyset as serialised. `granularity` is the stable discriminant of the
/// scope granularity; `entries` is the per-scope key material.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeysetWire {
    granularity: u8,
    entries: Vec<KeysetEntryWire>,
}

/// One `scope → key` pair on the wire. `scope` bytes are non-secret (derived from
/// coordinates/kinds/ids); `key` is the sensitive 256-bit material and is wiped
/// from every transient copy the moment it has been encoded / rehydrated.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeysetEntryWire {
    scope: Vec<u8>,
    key: [u8; KEY_LEN],
}

fn corrupt(reason: String) -> StoreError {
    StoreError::KeysetCorrupt { reason }
}

impl KeyStore {
    /// Persist the whole keyset to [`KEYSET_FILENAME`] in `dir`, crash-safely.
    ///
    /// The entire keyset is rewritten and published through the atomic
    /// temp-file-then-rename seam, so a torn flush leaves the on-disk keyset
    /// either the OLD complete version or the NEW one — never partially written.
    /// The transient serialized body carries raw key material and is held in a
    /// [`Zeroizing`] buffer wiped on drop; the per-entry plaintext key copies are
    /// wiped explicitly the instant they are encoded.
    ///
    /// Persist the whole keyset crash-safely to [`KEYSET_FILENAME`] in `dir`,
    /// through the production filesystem backend.
    ///
    /// A thin wrapper over [`KeyStore::flush_with_fs`] pinned to [`RealFs`]; the
    /// `fs`-taking seam stays `pub(crate)` because [`StoreFs`] is crate-private.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] if the atomic write fails, or
    /// [`StoreError::Serialization`] if the keyset cannot be encoded.
    pub fn flush(&mut self, dir: &Path) -> Result<(), StoreError> {
        self.flush_with_fs(dir, &RealFs)
    }

    /// [`KeyStore::flush`] routed through the supplied [`StoreFs`] backend so a
    /// fault-injecting filesystem can tear the atomic publish. The whole keyset is
    /// rewritten and published through the atomic temp-file-then-rename seam, so a
    /// torn flush leaves the on-disk keyset either the OLD complete version or the
    /// NEW one — never partially written. The transient serialized body carries
    /// raw key material and is held in a [`Zeroizing`] buffer wiped on drop; the
    /// per-entry plaintext key copies are wiped explicitly once encoded.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] if the atomic write fails, or
    /// [`StoreError::Serialization`] if the keyset cannot be encoded.
    pub(crate) fn flush_with_fs(&mut self, dir: &Path, fs: &dyn StoreFs) -> Result<(), StoreError> {
        let mut wire = KeysetWire {
            granularity: granularity_to_disc(self.granularity),
            entries: Vec::with_capacity(self.keys.len()),
        };
        for (scope, key) in &self.keys {
            wire.entries.push(KeysetEntryWire {
                scope: scope.0.to_vec(),
                // Copy the 32 bytes out of their Zeroizing home into a transient
                // buffer we wipe immediately after encoding (see below).
                key: *key.0,
            });
        }

        // The encoded body carries every raw key — keep it in a Zeroizing buffer
        // so it is wiped on drop even on an early-return error path.
        let body = Zeroizing::new(
            crate::encoding::to_bytes(&wire)
                .map_err(|error| StoreError::ser_msg(&format!("encode keyset: {error}")))?,
        );
        // Wipe the transient plaintext key copies now they are inside `body`.
        for entry in &mut wire.entries {
            entry.key.zeroize();
        }

        let crc = crc32fast::hash(&body);
        let final_path = dir.join(KEYSET_FILENAME);
        write_file_atomically_with_fs(
            dir,
            &final_path,
            "crypto-shred-keyset",
            |file| {
                use std::io::Write;
                file.write_all(KEYSET_MAGIC).map_err(StoreError::Io)?;
                file.write_all(&KEYSET_VERSION.to_le_bytes())
                    .map_err(StoreError::Io)?;
                file.write_all(&crc.to_le_bytes()).map_err(StoreError::Io)?;
                file.write_all(&body).map_err(StoreError::Io)?;
                Ok(())
            },
            fs,
        )?;
        // The whole keyset is now durable on disk — the in-memory keys match the
        // last flush, so clear the fence's dirty signal. Only reached on a
        // successful publish; a torn/failed flush leaves `dirty` set so the next
        // ciphertext write re-flushes before it can ack.
        self.dirty = false;
        tracing::debug!(
            target: "batpak::keyscope",
            count = self.keys.len(),
            "flushed crypto-shred keyset"
        );
        Ok(())
    }

    /// Cold-start rehydration: load the keyset from `dir` into a fresh
    /// [`KeyStore`] partitioned by `granularity`.
    ///
    /// An ABSENT file (first open, or a store that never flushed) yields an empty
    /// store. Any OTHER problem — wrong magic, short/truncated header, CRC
    /// mismatch, unsupported version, a decode failure, or a persisted granularity
    /// that disagrees with `granularity` — is a hard [`StoreError::KeysetCorrupt`]
    /// (fail closed): silently starting empty would crypto-shred every payload the
    /// real keyset protects. Rehydrated keys land in [`Zeroizing`] storage and
    /// every transient key buffer is wiped before this returns.
    ///
    /// Cold-start rehydration through the production filesystem backend: load the
    /// keyset from `dir` into a fresh [`KeyStore`] partitioned by `granularity`.
    ///
    /// A thin wrapper over [`KeyStore::load_with_fs`] pinned to [`RealFs`].
    ///
    /// # Errors
    /// Returns [`StoreError::KeysetCorrupt`] on any unreadable/undecodable/
    /// granularity-mismatched keyset, or [`StoreError::Io`] on a non-absent read
    /// failure.
    pub fn load(dir: &Path, granularity: KeyScopeGranularity) -> Result<Self, StoreError> {
        Self::load_with_fs(dir, &RealFs, granularity)
    }

    /// [`KeyStore::load`] routed through the supplied [`StoreFs`] backend.
    ///
    /// An ABSENT file (first open, or a store that never flushed) yields an empty
    /// store. Any OTHER problem — wrong magic, short/truncated header, CRC
    /// mismatch, unsupported version, a decode failure, or a persisted granularity
    /// that disagrees with `granularity` — is a hard [`StoreError::KeysetCorrupt`]
    /// (fail closed): silently starting empty would crypto-shred every payload the
    /// real keyset protects. Rehydrated keys land in [`Zeroizing`] storage and
    /// every transient key buffer is wiped before this returns.
    ///
    /// # Errors
    /// Returns [`StoreError::KeysetCorrupt`] on any unreadable/undecodable/
    /// granularity-mismatched keyset, or [`StoreError::Io`] on a non-absent read
    /// failure.
    pub(crate) fn load_with_fs(
        dir: &Path,
        fs: &dyn StoreFs,
        granularity: KeyScopeGranularity,
    ) -> Result<Self, StoreError> {
        let path = dir.join(KEYSET_FILENAME);
        // Defence for keys at rest: never follow a symlink where the keyset sits,
        // mirroring the write path's leaf guard.
        fs.reject_symlink_leaf(&path, "crypto-shred-keyset")?;
        let raw = match fs_read(&path) {
            // The file carries raw keys — read into a Zeroizing buffer.
            Ok(bytes) => Zeroizing::new(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::new(granularity));
            }
            Err(error) => return Err(StoreError::Io(error)),
        };
        decode_keyset(&raw, granularity)
    }
}

/// Validate the header and return the body slice, or a typed corruption error.
fn validate_header_and_body(raw: &[u8]) -> Result<&[u8], StoreError> {
    if raw.len() < HEADER_LEN {
        return Err(corrupt(format!("file too short: {} bytes", raw.len())));
    }
    if &raw[..6] != KEYSET_MAGIC.as_ref() {
        return Err(corrupt("wrong magic bytes".to_owned()));
    }
    let version = u16::from_le_bytes([raw[6], raw[7]]);
    if version != KEYSET_VERSION {
        return Err(corrupt(format!(
            "unsupported keyset version {version}; this binary reads and writes version \
             {KEYSET_VERSION}"
        )));
    }
    let stored_crc = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let body = &raw[HEADER_LEN..];
    let computed_crc = crc32fast::hash(body);
    if stored_crc != computed_crc {
        return Err(corrupt(format!(
            "crc mismatch: stored {stored_crc:#010x}, computed {computed_crc:#010x}"
        )));
    }
    Ok(body)
}

/// Decode a validated keyset body into a [`KeyStore`], failing closed on any
/// corruption or a granularity mismatch. Every transient key buffer is wiped.
fn decode_keyset(raw: &[u8], configured: KeyScopeGranularity) -> Result<KeyStore, StoreError> {
    let body = validate_header_and_body(raw)?;
    let mut wire: KeysetWire = crate::encoding::from_bytes(body)
        .map_err(|error| corrupt(format!("decode keyset body: {error}")))?;

    let result = rehydrate(&wire, configured);
    // Wipe the transient plaintext key copies regardless of success/failure.
    for entry in &mut wire.entries {
        entry.key.zeroize();
    }
    result
}

/// Cross-check the granularity and move each key into [`Zeroizing`] storage.
fn rehydrate(wire: &KeysetWire, configured: KeyScopeGranularity) -> Result<KeyStore, StoreError> {
    let persisted = granularity_from_disc(wire.granularity).ok_or_else(|| {
        corrupt(format!(
            "unknown key-scope granularity discriminant {}",
            wire.granularity
        ))
    })?;
    // A granularity mismatch means every derived scope would differ, so none of
    // the loaded keys would ever be found again — an effective silent shred.
    if persisted != configured {
        return Err(corrupt(format!(
            "configured key-scope granularity {configured:?} does not match persisted keyset \
             granularity {persisted:?}"
        )));
    }
    let mut keys = BTreeMap::new();
    for entry in &wire.entries {
        let scope = KeyScope(entry.scope.clone().into_boxed_slice());
        // Copy the key into its Zeroizing home; the wire copy is wiped by the
        // caller once every entry has been consumed.
        let key = PayloadKey(Zeroizing::new(entry.key));
        keys.insert(scope, key);
    }
    Ok(KeyStore {
        keys,
        granularity: configured,
        // Freshly rehydrated from disk — the in-memory keyset matches the durable
        // one, so it starts clean; the first mint/destroy flags it dirty.
        dirty: false,
    })
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "dangerous-test-hooks"))]
mod crash_tests;
