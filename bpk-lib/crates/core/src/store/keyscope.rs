//! Per-scope symmetric key material for opt-in payload encryption (crypto-shred).
//!
//! This module is a *mechanism*, not a policy. A [`KeyStore`] holds one
//! 256-bit symmetric key per [`KeyScope`]; encrypting a payload under its
//! scope's key and later [`destroy`](KeyStore::destroy)-ing that key renders
//! the ciphertext permanently unrecoverable (crypto-shred). batpak only ever
//! observes "the key for scope X was created / used / destroyed" — never any
//! meaning attached to a scope. The scope granularity ([`KeyScopeGranularity`])
//! is a purely structural choice about which events share a key.
//!
//! The AEAD is XChaCha20-Poly1305 (a pure-Rust construction with a 192-bit
//! nonce and 128-bit authentication tag). Key and nonce bytes are drawn from
//! the OS CSPRNG; no non-cryptographic PRNG is ever used for key material.
//!
//! Stage B adds durable persistence + cold-start rehydration (see the
//! [`persist`] child module) but still does NOT wire into the append/read
//! payload paths — encrypt/decrypt is Stage C.
//!
//! # Durability ordering (a Stage C obligation, documented here at the source)
//!
//! [`persist`] gives Stage B a [`KeyStore::flush`]. Stage C, when it wires the
//! append path, MUST flush a freshly-minted key DURABLY **before** the data it
//! encrypts is acknowledged durable. If that order were inverted, a crash landing
//! between "append is durable" and "key is durable" would leave a durable
//! ciphertext whose key never reached disk — permanently unrecoverable *live*
//! data (a spontaneous, unintended crypto-shred). Stage B only provides the
//! `flush` primitive; the ordering fence is Stage C's to wire.

use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::id::{EntityIdType, EventId};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::collections::btree_map::{BTreeMap, Entry};
use std::fmt;
use zeroize::Zeroizing;

/// Byte length of a symmetric payload key (256-bit).
const KEY_LEN: usize = 32;
/// Byte length of an XChaCha20-Poly1305 nonce (192-bit).
const NONCE_LEN: usize = 24;

/// How coarsely payload keys are partitioned — i.e. which events share a key,
/// and therefore what a single [`destroy`](KeyStore::destroy) shreds.
///
/// Each variant is a neutral structural choice; batpak attaches no meaning to
/// the resulting partitions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyScopeGranularity {
    /// One key per entity: destroying it shreds every payload written for that
    /// entity, across all kinds. The default granularity.
    #[default]
    PerEntity,
    /// One key per event-kind category (the high 4 bits of an [`EventKind`]):
    /// destroying it shreds every payload whose kind falls in that category.
    PerCategory,
    /// One key per full event kind (category plus type id): destroying it
    /// shreds every payload of exactly that kind.
    PerTypeId,
    /// One key per individual event: destroying it shreds exactly that event's
    /// payload and nothing else — the finest granularity.
    PerEvent,
}

/// The opaque identity a payload key is filed under.
///
/// A `KeyScope` is derived deterministically and canonically from a
/// [`KeyScopeGranularity`] plus an event's coordinate, kind, and id via
/// [`scope_for`]. Its internal byte representation is private; callers treat it
/// only as an opaque, comparable, orderable handle.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyScope(Box<[u8]>);

impl fmt::Debug for KeyScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyScope(0x")?;
        for byte in self.0.iter() {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(")")
    }
}

// Stable scope-derivation discriminants: the first byte of every [`KeyScope`],
// so two granularities never collide and the wire byte never silently tracks a
// source-order change. Shared by [`scope_for`] (the write/read seams) and
// [`KeyScopeGranularity::resolve_shred_scope`] (the erasure selector) so the
// two can never drift out of byte-agreement.
const SCOPE_DISC_PER_ENTITY: u8 = 0x01;
const SCOPE_DISC_PER_CATEGORY: u8 = 0x02;
const SCOPE_DISC_PER_TYPE_ID: u8 = 0x03;
const SCOPE_DISC_PER_EVENT: u8 = 0x04;

fn scope_per_entity(entity: &str) -> KeyScope {
    let mut bytes = Vec::with_capacity(1 + entity.len());
    bytes.push(SCOPE_DISC_PER_ENTITY);
    bytes.extend_from_slice(entity.as_bytes());
    KeyScope(bytes.into_boxed_slice())
}

fn scope_per_category(category: u8) -> KeyScope {
    KeyScope(vec![SCOPE_DISC_PER_CATEGORY, category].into_boxed_slice())
}

fn scope_per_type_id(kind_raw: u16) -> KeyScope {
    let mut bytes = Vec::with_capacity(3);
    bytes.push(SCOPE_DISC_PER_TYPE_ID);
    bytes.extend_from_slice(&kind_raw.to_be_bytes());
    KeyScope(bytes.into_boxed_slice())
}

fn scope_per_event(event_id: u128) -> KeyScope {
    let mut bytes = Vec::with_capacity(17);
    bytes.push(SCOPE_DISC_PER_EVENT);
    bytes.extend_from_slice(&event_id.to_be_bytes());
    KeyScope(bytes.into_boxed_slice())
}

/// Derive the [`KeyScope`] an event's payload key is filed under.
///
/// Deterministic and canonical: the same inputs always yield byte-identical
/// scopes, and two granularities never collide (each derivation is prefixed
/// with a distinct discriminant). Only the field relevant to the chosen
/// granularity contributes to the identity.
#[must_use]
pub fn scope_for(
    granularity: KeyScopeGranularity,
    coordinate: &Coordinate,
    event_kind: EventKind,
    event_id: EventId,
) -> KeyScope {
    match granularity {
        KeyScopeGranularity::PerEntity => scope_per_entity(coordinate.entity()),
        KeyScopeGranularity::PerCategory => scope_per_category(event_kind.category()),
        KeyScopeGranularity::PerTypeId => scope_per_type_id(event_kind.as_raw_u16()),
        KeyScopeGranularity::PerEvent => scope_per_event(event_id.as_u128()),
    }
}

/// The selector that names WHICH scope's key an erasure destroys, matched to a
/// store's configured [`KeyScopeGranularity`].
///
/// Crypto-shred is per-SCOPE-KEY, and the scope partition depends on the
/// configured granularity, so the ergonomic selector differs per granularity: an
/// entity coordinate addresses a `PerEntity` scope, an [`EventKind`] addresses a
/// `PerCategory`/`PerTypeId` scope, and an [`EventId`] addresses a `PerEvent`
/// scope. A selector that cannot address the configured granularity is a typed
/// mismatch (it never silently reinterprets one granularity's selector as
/// another's) — see [`KeyScopeGranularity::resolve_shred_scope`].
#[derive(Clone, Copy, Debug)]
pub enum ShredScope<'a> {
    /// Erase the `PerEntity` scope keyed by a coordinate's entity id.
    Entity(&'a Coordinate),
    /// Erase the `PerCategory` (category nibble) or `PerTypeId` (full kind)
    /// scope keyed by an event kind.
    Kind(EventKind),
    /// Erase the `PerEvent` scope keyed by a single event id.
    Event(EventId),
}

impl ShredScope<'_> {
    /// A stable, non-secret label for the selector variant, used only to render
    /// the typed [`crate::store::StoreError::ShredSelectorMismatch`]. Never
    /// carries key material.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            ShredScope::Entity(_) => "Entity",
            ShredScope::Kind(_) => "Kind",
            ShredScope::Event(_) => "Event",
        }
    }
}

impl KeyScopeGranularity {
    /// Resolve a [`ShredScope`] selector into the [`KeyScope`] whose key an
    /// erasure would destroy — but ONLY when the selector addresses THIS
    /// granularity. A selector that cannot address this granularity returns
    /// `None` (the caller raises a typed mismatch error), so an entity selector
    /// can never be silently reinterpreted as a per-event scope, or vice versa.
    ///
    /// Reuses the same scope builders as [`scope_for`], so the resolved erasure
    /// scope is byte-identical to the scope a matching append sealed its payload
    /// under — the key the erasure removes is exactly the key those payloads
    /// were encrypted with.
    pub(crate) fn resolve_shred_scope(self, selector: &ShredScope<'_>) -> Option<KeyScope> {
        match (self, selector) {
            (KeyScopeGranularity::PerEntity, ShredScope::Entity(coordinate)) => {
                Some(scope_per_entity(coordinate.entity()))
            }
            (KeyScopeGranularity::PerCategory, ShredScope::Kind(kind)) => {
                Some(scope_per_category(kind.category()))
            }
            (KeyScopeGranularity::PerTypeId, ShredScope::Kind(kind)) => {
                Some(scope_per_type_id(kind.as_raw_u16()))
            }
            (KeyScopeGranularity::PerEvent, ShredScope::Event(event_id)) => {
                Some(scope_per_event(event_id.as_u128()))
            }
            _ => None,
        }
    }
}

impl KeyScope {
    /// Borrow the opaque scope bytes.
    ///
    /// Stage C stamps these into the event header ([`keyscope_id`]) so the read
    /// path can rebuild the exact scope a ciphertext's key is filed under. The
    /// bytes are non-secret (derived from coordinates/kinds/ids).
    ///
    /// [`keyscope_id`]: crate::event::PayloadEncryption::keyscope_id
    #[must_use]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Reconstruct a scope from its raw bytes (the read-path inverse of
    /// [`as_bytes`](Self::as_bytes)). The bytes are treated as opaque — no
    /// structural validation is performed, mirroring the fact that a scope is
    /// only ever compared for equality against the keyset's live entries.
    #[must_use]
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        KeyScope(bytes.into_boxed_slice())
    }
}

/// Canonical associated-data (AAD) binding a sealed payload to the stable
/// identity of the event it belongs to.
///
/// The AAD is authenticated (not encrypted) by the AEAD, so `open` only
/// succeeds when the ciphertext is presented under the SAME identity it was
/// sealed with. Binding `coordinate + kind + event_id` makes a ciphertext
/// non-relocatable: moving a `{nonce, ciphertext}` pair onto any other event
/// (different entity, scope, kind, or event id) changes the AAD and fails
/// authentication (tamper detected), so a ciphertext can never be replayed
/// against a different event.
///
/// The encoding is explicit and length-prefixed — NOT MessagePack — so the
/// write path (writer) and the read path (`read_api`) reconstruct byte-identical
/// AAD from fields that are all present in the frame header on read
/// (`event_id`, `event_kind`) plus the frame's entity/scope strings
/// (`coordinate`). `global_sequence` is deliberately NOT bound: it is assigned
/// by the writer and is absent from the frame header, so the read path could
/// not reconstruct it.
#[must_use]
pub(crate) fn payload_aad(
    coordinate: &Coordinate,
    event_kind: EventKind,
    event_id: EventId,
) -> Vec<u8> {
    // Version byte, then length-prefixed entity + scope, then kind, then id.
    let entity = coordinate.entity().as_bytes();
    let scope = coordinate.scope().as_bytes();
    let mut aad = Vec::with_capacity(1 + 4 + entity.len() + 4 + scope.len() + 2 + 16);
    aad.push(0x01);
    // Coordinate entity/scope are length-bounded well under u32::MAX at
    // construction, so the saturation never triggers; it keeps the length prefix
    // a fixed 4 bytes and identical on the write and read sides.
    let entity_len = u32::try_from(entity.len()).unwrap_or(u32::MAX);
    let scope_len = u32::try_from(scope.len()).unwrap_or(u32::MAX);
    aad.extend_from_slice(&entity_len.to_le_bytes());
    aad.extend_from_slice(entity);
    aad.extend_from_slice(&scope_len.to_le_bytes());
    aad.extend_from_slice(scope);
    aad.extend_from_slice(&event_kind.as_raw_u16().to_le_bytes());
    aad.extend_from_slice(&event_id.as_u128().to_be_bytes());
    aad
}

/// A failure from the key store or its AEAD primitives.
///
/// Deliberately opaque: an [`open`](PayloadKey::open) failure reveals only that
/// authentication failed, never why, so it cannot serve as a decryption oracle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStoreError {
    /// The OS CSPRNG failed to produce key material.
    Rng,
    /// AEAD cipher construction rejected the key length (defensive; a stored
    /// key is always exactly 256 bits).
    KeyInit,
    /// Authenticated encryption (sealing) failed.
    Seal,
    /// Authenticated decryption (opening) failed — wrong key, nonce, associated
    /// data, or a tampered ciphertext. No further detail is exposed.
    Open,
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Rng => "CSPRNG failed to produce key material",
            Self::KeyInit => "AEAD key initialization rejected the key length",
            Self::Seal => "authenticated encryption failed",
            Self::Open => "authenticated decryption failed",
        };
        f.write_str(message)
    }
}

impl std::error::Error for KeyStoreError {}

/// A 256-bit symmetric payload key.
///
/// The raw bytes are held in a [`Zeroizing`] buffer, so they are wiped from
/// memory when the key is dropped, and they never appear in any `Debug` output.
/// The only way to use a key is through [`seal`](Self::seal) /
/// [`open`](Self::open); the bytes are never exposed.
pub struct PayloadKey(Zeroizing<[u8; KEY_LEN]>);

impl fmt::Debug for PayloadKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never render key bytes — only an opaque marker.
        f.debug_struct("PayloadKey").finish_non_exhaustive()
    }
}

impl PayloadKey {
    /// Mint a fresh key from the OS CSPRNG.
    fn generate() -> Result<Self, KeyStoreError> {
        // Fill the secret in place inside the zeroizing buffer so no plaintext
        // key copy is ever left on the stack.
        let mut key: Zeroizing<[u8; KEY_LEN]> = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::fill(key.as_mut_slice()).map_err(|_| KeyStoreError::Rng)?;
        Ok(Self(key))
    }

    fn cipher(&self) -> Result<XChaCha20Poly1305, KeyStoreError> {
        XChaCha20Poly1305::new_from_slice(self.0.as_slice()).map_err(|_| KeyStoreError::KeyInit)
    }

    /// Seal `plaintext` under this key with a 24-byte `nonce`, binding `aad`
    /// (associated data authenticated but not encrypted). Returns the
    /// ciphertext with its appended authentication tag.
    ///
    /// The caller owns nonce uniqueness: a nonce must never repeat under the
    /// same key. XChaCha20-Poly1305's 192-bit nonce makes random nonces safe.
    ///
    /// # Errors
    /// Returns [`KeyStoreError::Seal`] if the AEAD encryption fails, or
    /// [`KeyStoreError::KeyInit`] if cipher construction rejects the key.
    pub fn seal(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, KeyStoreError> {
        let cipher = self.cipher()?;
        let nonce = XNonce::from_slice(nonce);
        cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| KeyStoreError::Seal)
    }

    /// Open `ciphertext` sealed under this key with the same `nonce` and `aad`.
    /// Returns the recovered plaintext.
    ///
    /// # Errors
    /// Returns [`KeyStoreError::Open`] if authentication fails (wrong key,
    /// nonce, associated data, or tampered ciphertext), or
    /// [`KeyStoreError::KeyInit`] if cipher construction rejects the key.
    pub fn open(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, KeyStoreError> {
        let cipher = self.cipher()?;
        let nonce = XNonce::from_slice(nonce);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| KeyStoreError::Open)
    }
}

/// An in-memory store of per-scope payload keys.
///
/// Keys are minted lazily on first use and destroyed on demand. Destroying a
/// scope's key is the crypto-shred primitive: it zeroizes and removes the key,
/// after which any payload sealed under that scope can never be opened again.
pub struct KeyStore {
    keys: BTreeMap<KeyScope, PayloadKey>,
    granularity: KeyScopeGranularity,
    /// `true` when the in-memory keyset has diverged from the last durable flush
    /// — set via [`mark_dirty`](Self::mark_dirty) (the writer calls it whenever an
    /// append mints a fresh scope key) or by [`destroy`](Self::destroy), and
    /// cleared ONLY by a successful [`flush`](Self::flush). The append durability
    /// fence flushes whenever this is set, so a mint whose fence-flush FAILED (the
    /// key is resident in memory but never reached disk) forces the NEXT ciphertext
    /// write to re-flush before it can ack, instead of trusting the resident key
    /// and skipping the fence — which would otherwise leave a durable ciphertext
    /// whose key is on disk nowhere (a silent, unintended crypto-shred of live
    /// data).
    dirty: bool,
}

impl KeyStore {
    /// Create an empty key store with the given scope granularity.
    #[must_use]
    pub fn new(granularity: KeyScopeGranularity) -> Self {
        Self {
            keys: BTreeMap::new(),
            granularity,
            dirty: false,
        }
    }

    /// Whether the in-memory keyset is ahead of the last durable flush (see the
    /// [`dirty`](Self#structfield.dirty) field). The durability fence flushes
    /// whenever this holds. Internal durability mechanism, not public surface.
    #[must_use]
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Flag the keyset dirty — the in-memory keys are ahead of the last durable
    /// flush. Idempotent; cleared only by a successful flush.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The scope granularity this store partitions keys by.
    #[must_use]
    pub fn granularity(&self) -> KeyScopeGranularity {
        self.granularity
    }

    /// Number of live payload keys currently held (observability; never exposes
    /// key material). A destroyed scope no longer counts.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// Return the key for `scope`, minting a fresh random key on first use.
    ///
    /// A second call for the same scope returns the same key until it is
    /// [`destroy`](Self::destroy)-ed.
    ///
    /// # Errors
    /// Returns [`KeyStoreError::Rng`] if the CSPRNG fails while minting a new key.
    pub fn get_or_create(&mut self, scope: &KeyScope) -> Result<&PayloadKey, KeyStoreError> {
        match self.keys.entry(scope.clone()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let key = PayloadKey::generate()?;
                Ok(entry.insert(key))
            }
        }
    }

    /// Return the key for `scope` if one currently exists, without minting.
    #[must_use]
    pub fn get(&self, scope: &KeyScope) -> Option<&PayloadKey> {
        self.keys.get(scope)
    }

    /// Destroy the key for `scope` (the crypto-shred primitive).
    ///
    /// Returns `true` if a key existed and was removed. The removed
    /// [`PayloadKey`] is dropped here, zeroizing its bytes; a subsequent
    /// [`get`](Self::get) returns `None`, and any ciphertext sealed under the
    /// old key is permanently unrecoverable.
    pub fn destroy(&mut self, scope: &KeyScope) -> bool {
        let removed = self.keys.remove(scope).is_some();
        if removed {
            // The in-memory keyset now differs from the last durable flush; the
            // erasure is not durable until the next successful flush persists it.
            self.dirty = true;
        }
        removed
    }
}

/// Durable keyset persistence + cold-start rehydration (Stage B).
pub mod persist;

#[cfg(test)]
mod tests;
