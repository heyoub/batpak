//! Crypto-shred erasure surface (opt-in `payload-encryption`): the explicit,
//! granularity-agnostic `Store::shred_scope` "forget this scope" op.
//!
//! The read side (decrypt-on-read, the [`ReadDisposition::Shredded`] payoff)
//! lives in `read_api`; the encrypt-on-append seam + durability fence live under
//! `write::writer::encrypt`. This module holds ONLY the erasure trigger, split
//! out of `write_api` so neither file grows past its size ratchet.

use super::keyscope::ShredScope;
use super::{Open, Store, StoreError};

#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "payload-encryption"))
)]
impl Store<Open> {
    /// Crypto-shred a scope: destroy its payload key and flush the keyset durable,
    /// making every payload sealed under that scope permanently unrecoverable.
    ///
    /// This is the Shape-layer, granularity-agnostic "forget this scope" tool. It
    /// destroys the scope's KEY, never any event frame: the ciphertext and its
    /// hash-chain identity survive on disk — [`verify_chain`](Store::verify_chain)
    /// stays intact and the chain is unbroken — while the plaintext is gone.
    /// After a successful shred, a read of any payload in that scope returns
    /// [`ReadDisposition::Shredded`](crate::store::ReadDisposition::Shredded) /
    /// [`StoreError::PayloadShredded`], and a non-shredded sibling scope still
    /// decrypts normally.
    ///
    /// It NEVER over-shreds: erasure is EXACTLY this explicit op. Tombstone /
    /// retention compaction does not auto-destroy keys (see
    /// `store::lifecycle_compact`), so a coarse scope — the default
    /// [`KeyScopeGranularity::PerEntity`](crate::store::KeyScopeGranularity::PerEntity),
    /// where one key covers every payload of an entity — is only ever erased when
    /// the caller asks for that entity by selector.
    ///
    /// The `selector` must match the store's configured
    /// [`KeyScopeGranularity`](crate::store::KeyScopeGranularity): a
    /// [`ShredScope::Entity`] for `PerEntity`, a [`ShredScope::Kind`] for
    /// `PerCategory` / `PerTypeId`, a [`ShredScope::Event`] for `PerEvent`. A
    /// selector that cannot address the configured granularity is refused as a
    /// typed [`StoreError::ShredSelectorMismatch`] and destroys nothing.
    ///
    /// Returns `true` when a live key existed and was destroyed, `false` when the
    /// scope held no key (already shredded, or never minted); either way the
    /// keyset is flushed so the erasure is durable and idempotent.
    ///
    /// # Errors
    /// - [`StoreError::ShredSelectorMismatch`] when `selector` does not match the
    ///   configured granularity — refused before any key is touched.
    /// - [`StoreError::Configuration`] when the store was opened WITHOUT
    ///   `payload_encryption`: there is no keyset to shred.
    /// - The keyset flush error ([`StoreError::Io`] / [`StoreError::Serialization`])
    ///   when persisting the destruction fails. The in-memory key is already gone
    ///   (this process reads the scope as shredded), but the caller learns the
    ///   durable publish did not land — the old keyset on disk still holds the key,
    ///   so the durable erasure is not yet effective (the fail-SAFE direction: data
    ///   stays recoverable until a flush succeeds).
    pub fn shred_scope(&self, selector: ShredScope<'_>) -> Result<bool, StoreError> {
        let Some(key_store) = self.key_store.as_ref() else {
            return Err(StoreError::Configuration(
                "shred_scope requires a store opened with payload_encryption; no keyset is \
                 configured to shred"
                    .to_owned(),
            ));
        };
        let mut guard = key_store.lock();
        let granularity = guard.granularity();
        let scope = granularity.resolve_shred_scope(&selector).ok_or_else(|| {
            StoreError::ShredSelectorMismatch {
                granularity,
                selector: selector.label(),
            }
        })?;
        // Destroy in memory, THEN publish the shrunken keyset durable. Order
        // matters: the removal must be in the map before the flush serialises it.
        // A flush failure leaves memory ahead of disk (key gone here, still on
        // disk) — the fail-safe direction, since the payload stays recoverable
        // until the destruction is durably published.
        let destroyed = guard.destroy(&scope);
        guard.flush_with_fs(&self.config.data_dir, self.config.fs().as_ref())?;
        Ok(destroyed)
    }
}
