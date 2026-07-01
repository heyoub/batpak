use super::*;
use crate::id::EntityIdType;
use crate::id::EventId;
use crate::store::index::IndexEntry;
use std::collections::BTreeMap;

/// Report from a full store hash-chain verification ([`Store::verify_chain`]).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChainVerificationReport {
    /// Number of committed, visible events whose content hash was recomputed.
    pub events_checked: usize,
    /// Events whose recomputed blake3 content hash did NOT match the stored
    /// `event_hash` — the content no longer matches its claimed identity.
    pub content_hash_mismatches: Vec<EventId>,
    /// Non-genesis events whose `prev_hash` references no verified event in the
    /// store (a dangling chain link).
    pub dangling_links: Vec<EventId>,
}

impl ChainVerificationReport {
    /// True when every checked event's content hash matched and every link
    /// referenced a verified event.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.content_hash_mismatches.is_empty() && self.dangling_links.is_empty()
    }
}

/// Read disposition for a single event under opt-in `payload-encryption`.
///
/// Returned by [`Store::get_shreddable`] so a caller can distinguish a payload
/// it can still read from one whose key has been crypto-shredded WITHOUT
/// catching an error — the event is present in the chain either way.
#[cfg(feature = "payload-encryption")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "payload-encryption"))
)]
#[derive(Clone, Debug)]
pub enum ReadDisposition {
    /// The payload is readable: a plaintext event, or an encrypted event whose
    /// key is present and whose ciphertext authenticated.
    ///
    /// Boxed so this readable variant does not inflate the size of the rare
    /// [`Shredded`](Self::Shredded) variant.
    Present(Box<StoredEvent<serde_json::Value>>),
    /// The event is present in the chain (its `event_hash` still verifies) but
    /// its payload key has been destroyed — the plaintext is permanently
    /// unrecoverable. NOT corruption, and never the raw ciphertext.
    Shredded,
}

/// Outcome of decrypting an encrypted payload's ciphertext under the live keyset.
///
/// Distinguishes recovered plaintext from a crypto-shredded payload (key
/// destroyed) WITHOUT raising an error, so each key-aware reader — single-event
/// read, projection replay, content compaction — can pick its OWN shredded
/// semantics rather than all of them funnelling through one error. The
/// crate-internal counterpart to the public [`ReadDisposition`], returning the
/// raw plaintext BYTES so the caller decodes them into whatever payload shape it
/// needs (a `serde_json::Value`, or the raw MessagePack of a raw-lane replay).
#[cfg(feature = "payload-encryption")]
pub(crate) enum PayloadPlaintext {
    /// Recovered plaintext bytes, ready to decode into the caller's payload type.
    Plaintext(Vec<u8>),
    /// The scope key was destroyed — the plaintext is permanently unrecoverable.
    Shredded,
}

/// Key-aware disposition of a single event's payload for LIVE DELIVERY under
/// opt-in `payload-encryption`, carrying the DECRYPTED raw MessagePack bytes.
///
/// Returned by [`Store::read_delivery_payload`] so a delivery consumer (a reactor
/// dispatch, a syncbat subscription envelope builder) receives PLAINTEXT bytes
/// rather than the ciphertext that Stage C wrote to disk — decrypted at the core
/// boundary so the keyset never crosses to the consumer. A crypto-shredded event
/// reports [`Shredded`](Self::Shredded) so the consumer emits a LOUD, observable
/// skip instead of silently shipping (or misdecoding) ciphertext.
#[cfg(feature = "payload-encryption")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "payload-encryption"))
)]
#[derive(Clone, Debug)]
pub enum DeliveryPayload {
    /// A readable delivered event: `stored.event.payload` is the PLAINTEXT
    /// MessagePack bytes (decrypted, or — for a plaintext / system-carve-out
    /// event, or a store with no keyset — the stored bytes unchanged, so the
    /// delivered envelope is byte-identical to a non-encryption build).
    ///
    /// Boxed so this common variant does not inflate the rare
    /// [`Shredded`](Self::Shredded) variant.
    Readable(Box<StoredEvent<Vec<u8>>>),
    /// The event is present in the chain (its `event_hash` still verifies) but its
    /// payload key has been destroyed — the plaintext is permanently
    /// unrecoverable. The consumer must NOT deliver the ciphertext; it emits a
    /// loud, observable skip and advances its cursor past the event.
    Shredded {
        /// Id of the event whose payload key has been shredded.
        event_id: EventId,
    },
}

impl<State: crate::store::StoreState> Store<State> {
    /// READ: get a single event by ID.
    ///
    /// With opt-in `payload-encryption` configured, an encrypted event is
    /// transparently decrypted under the store's keyset. If the event's key has
    /// been crypto-shredded, `get` returns [`StoreError::PayloadShredded`] — a
    /// typed, explicitly-non-corruption signal; use
    /// [`get_shreddable`](Self::get_shreddable) to receive that case as a
    /// [`ReadDisposition::Shredded`] value instead of an error.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading from disk fails.
    pub fn get(&self, event_id: EventId) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let raw = event_id.as_u128();
        let entry = self
            .index
            .get_by_id(raw)
            .ok_or(StoreError::NotFound(event_id))?;
        // Encryption-enabled store: route through the key-aware read so an
        // encrypted payload is decrypted (or reported shredded) rather than
        // MessagePack-decoded as ciphertext. When `payload_encryption` is not
        // configured (`key_store` is `None`) this branch is skipped entirely and
        // the read is byte-for-byte the pre-Stage-C path.
        #[cfg(feature = "payload-encryption")]
        if self.key_store.is_some() {
            return match self.read_maybe_encrypted(&entry.disk_pos)? {
                ReadDisposition::Present(stored) => Ok(*stored),
                ReadDisposition::Shredded => Err(StoreError::PayloadShredded { event_id }),
            };
        }
        self.reader.read_entry(&entry.disk_pos)
    }

    /// READ: get a single event by ID, reporting a crypto-shredded payload as a
    /// [`ReadDisposition`] instead of an error (opt-in `payload-encryption`).
    ///
    /// Behaves like [`get`](Self::get) for readable events (plaintext or
    /// decryptable), but a destroyed key yields [`ReadDisposition::Shredded`]
    /// rather than [`StoreError::PayloadShredded`]. A ciphertext whose key is
    /// present but which fails authentication still surfaces as
    /// [`StoreError::PayloadDecryptFailed`] (tamper is an error, not a shred).
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists, or an
    /// I/O / decode / authentication error while reading the event.
    #[cfg(feature = "payload-encryption")]
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "payload-encryption"))
    )]
    pub fn get_shreddable(&self, event_id: EventId) -> Result<ReadDisposition, StoreError> {
        let raw = event_id.as_u128();
        let entry = self
            .index
            .get_by_id(raw)
            .ok_or(StoreError::NotFound(event_id))?;
        if self.key_store.is_none() {
            // Encryption not configured: every payload is plaintext-present.
            return Ok(ReadDisposition::Present(Box::new(
                self.reader.read_entry(&entry.disk_pos)?,
            )));
        }
        self.read_maybe_encrypted(&entry.disk_pos)
    }

    /// Key-aware single-event read: decode a plaintext event, decrypt an
    /// encrypted one under the live keyset, or report [`ReadDisposition::Shredded`]
    /// when the key is absent. Shared by [`get`](Self::get) and
    /// [`get_shreddable`](Self::get_shreddable).
    ///
    /// Reads the RAW frame (ciphertext + header), so it never routes ciphertext
    /// through the Value decode seam, and takes the event's identity from that
    /// header. `verify_chain`/`read_raw` are untouched: they still see the stored
    /// bytes.
    #[cfg(feature = "payload-encryption")]
    fn read_maybe_encrypted(
        &self,
        pos: &crate::store::index::DiskPos,
    ) -> Result<ReadDisposition, StoreError> {
        let raw = self.reader.read_entry_raw(pos)?;
        let coordinate = raw.coordinate;
        let header = raw.event.header;
        let hash_chain = raw.event.hash_chain;
        let payload_bytes = raw.event.payload;

        // Cloned so the header can be moved into the returned `Event` below
        // without the `meta` borrow outliving it.
        let Some(meta) = header.payload_encryption.clone() else {
            // Plaintext event in an encryption-enabled store (e.g. written before
            // encryption was configured): decode exactly as the plaintext read
            // would, so the returned Value is identical.
            let value = crate::encoding::from_bytes::<serde_json::Value>(&payload_bytes)
                .map_err(|error| StoreError::Serialization(Box::new(error)))?;
            return Ok(ReadDisposition::Present(Box::new(StoredEvent {
                coordinate,
                event: crate::event::Event {
                    header,
                    payload: value,
                    hash_chain,
                },
            })));
        };

        // Decrypt via the shared Stage C primitive: a destroyed key reports
        // `Shredded` (present in the chain, plaintext gone — NOT corruption, NOT
        // the ciphertext); a present key that fails to authenticate is a tamper
        // error, never a shred.
        match self.open_encrypted_payload_bytes(
            &coordinate,
            header.event_kind,
            header.event_id,
            &meta,
            &payload_bytes,
        )? {
            PayloadPlaintext::Shredded => Ok(ReadDisposition::Shredded),
            PayloadPlaintext::Plaintext(plaintext) => {
                let value = crate::encoding::from_bytes::<serde_json::Value>(&plaintext)
                    .map_err(|error| StoreError::Serialization(Box::new(error)))?;
                Ok(ReadDisposition::Present(Box::new(StoredEvent {
                    coordinate,
                    event: crate::event::Event {
                        header,
                        payload: value,
                        hash_chain,
                    },
                })))
            }
        }
    }

    /// Decrypt an encrypted payload's ciphertext to plaintext BYTES under the
    /// live keyset, or report [`PayloadPlaintext::Shredded`] when the scope key
    /// has been destroyed. The single, shared Stage C decrypt primitive.
    ///
    /// Rebuilds the [`KeyScope`](crate::store::keyscope::KeyScope) the ciphertext's
    /// key is filed under and the AAD that binds it to THIS event's stable
    /// identity, looks the key up in the live keyset, and AEAD-opens the
    /// ciphertext. Reused by the key-aware single-event read
    /// ([`read_maybe_encrypted`](Self::read_maybe_encrypted)), key-aware
    /// projection replay, and key-aware content compaction so all three decrypt
    /// through ONE path — never reinventing decryption or misdecoding ciphertext.
    ///
    /// # Errors
    /// [`StoreError::PayloadDecryptFailed`] if the key is present but the
    /// ciphertext/nonce/bound identity fails to authenticate (tamper), or an
    /// internal [`StoreError::Serialization`] if reached with no keyset (an
    /// invariant break — callers gate on `key_store.is_some()`).
    #[cfg(feature = "payload-encryption")]
    pub(crate) fn open_encrypted_payload_bytes(
        &self,
        coordinate: &crate::coordinate::Coordinate,
        event_kind: EventKind,
        event_id: EventId,
        meta: &crate::event::PayloadEncryption,
        ciphertext: &[u8],
    ) -> Result<PayloadPlaintext, StoreError> {
        let scope = crate::store::keyscope::KeyScope::from_bytes(meta.keyscope_id.clone());
        let aad = crate::store::keyscope::payload_aad(coordinate, event_kind, event_id);
        // `key_store` is `Some` on every path that reaches here (every caller gates
        // on it), so its absence is an internal invariant break, surfaced — never
        // silently treated as shredded.
        let key_store = self.key_store.as_ref().ok_or_else(|| {
            StoreError::ser_msg("open_encrypted_payload_bytes reached with no keyset configured")
        })?;
        let guard = key_store.lock();
        let Some(key) = guard.get(&scope) else {
            return Ok(PayloadPlaintext::Shredded);
        };
        let plaintext = key
            .open(&meta.nonce, &aad, ciphertext)
            .map_err(|_| StoreError::PayloadDecryptFailed { event_id })?;
        drop(guard);
        Ok(PayloadPlaintext::Plaintext(plaintext))
    }

    /// DELIVER: key-aware read of one event's payload as raw MessagePack bytes,
    /// decrypted at the core boundary (opt-in `payload-encryption`).
    ///
    /// Live delivery — a reactor's cursor dispatch, a syncbat subscription
    /// envelope — builds a delivered envelope from a STORED event. Stage C made an
    /// encrypted event's on-disk payload ciphertext, so a delivery path that
    /// shipped the raw stored bytes would ship ciphertext: undecryptable
    /// downstream, silent data loss. This decrypts here, on the core `Store` that
    /// owns the keyset, so the keys never cross to a delivery consumer:
    ///
    /// * A readable event (plaintext, a system-carve-out event, or an encrypted
    ///   event whose key is present) yields [`DeliveryPayload::Readable`] carrying
    ///   the PLAINTEXT MessagePack bytes. For a plaintext event, or a store with no
    ///   keyset, the bytes are the stored bytes unchanged — byte-identical to
    ///   [`read_raw`](Self::read_raw).
    /// * A crypto-shredded event yields [`DeliveryPayload::Shredded`] so the
    ///   consumer emits a loud, observable skip rather than the ciphertext.
    ///
    /// Reuses the shared Stage C decrypt primitive
    /// ([`open_encrypted_payload_bytes`](Self::open_encrypted_payload_bytes)); a
    /// present key that fails to authenticate is still a tamper error
    /// ([`StoreError::PayloadDecryptFailed`]), never a shred.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no event with that id exists; an I/O / decode /
    /// authentication error while reading the frame.
    #[cfg(feature = "payload-encryption")]
    #[cfg_attr(
        all(docsrs, not(batpak_stable_docs)),
        doc(cfg(feature = "payload-encryption"))
    )]
    pub fn read_delivery_payload(&self, event_id: EventId) -> Result<DeliveryPayload, StoreError> {
        let raw = event_id.as_u128();
        let entry = self
            .index
            .get_by_id(raw)
            .ok_or(StoreError::NotFound(event_id))?;
        if self.key_store.is_none() {
            // Encryption not configured: every payload is plaintext-present. The
            // returned bytes are exactly `read_raw`'s, so the delivered envelope is
            // byte-identical to a non-encryption build.
            return Ok(DeliveryPayload::Readable(Box::new(
                self.reader.read_entry_raw(&entry.disk_pos)?,
            )));
        }
        self.read_raw_delivery_maybe_encrypted(&entry.disk_pos)
    }

    /// Key-aware delivery read: decrypt an encrypted frame to plaintext bytes,
    /// pass a plaintext frame through unchanged, or report
    /// [`DeliveryPayload::Shredded`]. Shared internals of
    /// [`read_delivery_payload`](Self::read_delivery_payload); reads the RAW frame
    /// so ciphertext never routes through the Value-decode seam.
    #[cfg(feature = "payload-encryption")]
    fn read_raw_delivery_maybe_encrypted(
        &self,
        pos: &crate::store::index::DiskPos,
    ) -> Result<DeliveryPayload, StoreError> {
        let raw = self.reader.read_entry_raw(pos)?;
        // Cloned so `raw` can be moved into the returned payload below without the
        // `meta` borrow outliving it.
        let Some(meta) = raw.event.header.payload_encryption.clone() else {
            // Plaintext / system-carve-out event: the stored bytes ARE the
            // plaintext — byte-identical to `read_raw`.
            return Ok(DeliveryPayload::Readable(Box::new(raw)));
        };
        let event_id = raw.event.header.event_id;
        match self.open_encrypted_payload_bytes(
            &raw.coordinate,
            raw.event.header.event_kind,
            event_id,
            &meta,
            &raw.event.payload,
        )? {
            PayloadPlaintext::Shredded => Ok(DeliveryPayload::Shredded { event_id }),
            PayloadPlaintext::Plaintext(plaintext) => {
                let StoredEvent { coordinate, event } = raw;
                Ok(DeliveryPayload::Readable(Box::new(StoredEvent {
                    coordinate,
                    event: crate::event::Event {
                        header: event.header,
                        payload: plaintext,
                        hash_chain: event.hash_chain,
                    },
                })))
            }
        }
    }

    /// READ: fetch a single event by ID with the payload left as raw
    /// MessagePack bytes.
    /// Mirrors [`get`](Self::get) but skips the JSON-decode step, suitable
    /// for the `RawMsgpackInput` lane of a multi-event reactor.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading
    /// from disk fails.
    pub fn read_raw(&self, event_id: EventId) -> Result<StoredEvent<Vec<u8>>, StoreError> {
        let raw = event_id.as_u128();
        let entry = self
            .index
            .get_by_id(raw)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry_raw(&entry.disk_pos)
    }

    /// Verify ack-shaped append receipt fields against the store's signing-key
    /// registry and current index state.
    ///
    /// Wire transports omit [`AppendReceipt::disk_pos`]; this helper hydrates
    /// it from the committed index entry before delegating to
    /// [`Self::verify_append_receipt`].
    #[must_use]
    pub fn verify_append_receipt_wire_detailed(
        &self,
        event_id: EventId,
        global_sequence: u64,
        content_hash: [u8; 32],
        key_id: [u8; 32],
        signature: Option<[u8; 64]>,
        extensions: BTreeMap<ExtensionKey, EncodedBytes>,
    ) -> ReceiptVerification {
        let Some(entry) = self.index.get_by_id(event_id.as_u128()) else {
            return ReceiptVerification::Invalid(ReceiptVerificationError::MissingCommittedEvent);
        };
        let receipt = AppendReceipt {
            event_id,
            global_sequence,
            disk_pos: entry.disk_pos,
            content_hash,
            key_id,
            signature,
            extensions,
        };
        self.verify_append_receipt(&receipt)
    }

    /// Verify a full persisted append receipt and return the exact acceptance
    /// or rejection reason.
    ///
    /// This API expects the native [`AppendReceipt`], including its committed
    /// disk position. Wire transports that only carry ack-shaped fields should
    /// use [`Self::verify_append_receipt_wire_detailed`] so the store can
    /// hydrate the disk position from the committed index entry.
    #[must_use]
    pub fn verify_append_receipt(&self, receipt: &AppendReceipt) -> ReceiptVerification {
        let Some(entry) = self.index.get_by_id(receipt.event_id.as_u128()) else {
            return ReceiptVerification::Invalid(ReceiptVerificationError::MissingCommittedEvent);
        };
        if let Some(error) = append_receipt_index_mismatch(receipt, &entry) {
            return ReceiptVerification::Invalid(error);
        }
        self.runtime.signing_registry.verify_append_receipt(
            receipt,
            &entry.coord,
            entry.kind,
            entry.hash_chain.prev_hash,
        )
    }

    /// Verify a persisted denial receipt and return the exact acceptance or
    /// rejection reason.
    #[must_use]
    pub fn verify_denial_receipt(&self, receipt: &DenialReceipt) -> ReceiptVerification {
        let Some(entry) = self.index.get_by_id(receipt.event_id.as_u128()) else {
            return ReceiptVerification::Invalid(ReceiptVerificationError::MissingCommittedEvent);
        };
        if let Some(error) = denial_receipt_index_mismatch(receipt, &entry) {
            return ReceiptVerification::Invalid(error);
        }
        self.runtime.signing_registry.verify_denial_receipt(
            receipt,
            &entry.coord,
            entry.kind,
            entry.hash_chain.prev_hash,
        )
    }

    /// VERIFY: recompute and check the blake3 hash chain over every committed,
    /// visible event.
    ///
    /// A plain read trusts the self-reported `event_hash` (guarded only by the
    /// per-frame CRC). This pass instead recomputes blake3 over each event's
    /// actual content bytes and confirms it matches the stored `event_hash`,
    /// then confirms every non-genesis `prev_hash` references a verified event —
    /// the on-demand tamper-evidence check. By default a store does NOT run this
    /// at open (it is `O(events)`); opt into [`ChainVerification::Recompute`] to
    /// run it automatically.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`]/[`StoreError::Serialization`] if a committed
    /// event cannot be re-read from disk.
    pub fn verify_chain(&self) -> Result<ChainVerificationReport, StoreError> {
        let mut entries = self.query(&Region::all());
        entries.sort_by_key(IndexEntry::global_sequence);
        let mut report = ChainVerificationReport::default();
        let mut verified_hashes: std::collections::BTreeSet<[u8; 32]> =
            std::collections::BTreeSet::new();
        for entry in &entries {
            report.events_checked += 1;
            let stored = self.read_raw(entry.event_id())?;
            let recomputed = crate::event::hash::compute_hash(&stored.event.payload);
            if recomputed == entry.hash_chain().event_hash {
                verified_hashes.insert(entry.hash_chain().event_hash);
            } else {
                report.content_hash_mismatches.push(entry.event_id());
            }
        }
        for entry in &entries {
            let prev = entry.hash_chain().prev_hash;
            if prev != [0u8; 32] && !verified_hashes.contains(&prev) {
                report.dangling_links.push(entry.event_id());
            }
        }
        Ok(report)
    }

    /// READ: return every currently visible index entry matching a Region.
    ///
    /// This is a convenience snapshot read for small, already-bounded regions.
    /// For replay, audit, host parity, or user-facing pagination, prefer
    /// [`Self::query_entries_after`], which pages strictly by
    /// `global_sequence`.
    #[must_use]
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: return every currently visible index entry matching a Region on
    /// one exact DAG lane.
    ///
    /// The explicit `lane` argument is authoritative. Passing a `Region` that
    /// already carries a lane is only valid when it matches this argument.
    #[must_use]
    pub fn query_lane(&self, region: &Region, lane: u32) -> Vec<IndexEntry> {
        debug_assert!(
            region.lane.is_none() || region.lane == Some(lane),
            "query_lane lane argument must match any pre-set Region lane"
        );
        self.index.query(&region.clone().with_lane(lane))
    }

    /// READ: query a bounded page of visible events by Region in ascending
    /// `global_sequence` order.
    ///
    /// Pass `None` for the first page. Pass the last returned entry's
    /// [`IndexEntry::global_sequence`] as `Some(after_global_sequence)` to
    /// resume strictly after that entry. `limit == 0` returns an empty page.
    ///
    /// This is commit-order pagination, not a live cursor or server-held
    /// session. Durable delivery cursors live under the delivery APIs.
    #[must_use]
    pub fn query_entries_after(
        &self,
        region: &Region,
        after_global_sequence: Option<u64>,
        limit: usize,
    ) -> Vec<IndexEntry> {
        let after_seq = after_global_sequence.unwrap_or(0);
        let started = after_global_sequence.is_some();
        self.index
            .query_hits_after(region, after_seq, started, limit)
            .into_iter()
            .filter_map(|hit| self.index.upgrade_hit(hit))
            .collect()
    }

    /// READ: walk bounded hash-chain ancestors from an event id.
    ///
    /// This is substrate ancestry, not domain graph traversal.
    ///
    /// Returns only the ancestor events; the returned `Vec` cannot tell a
    /// complete chain (reached genesis) from one truncated at a dangling link
    /// (e.g. a retention-dropped mid-chain event). Use
    /// [`Store::walk_ancestors_outcome`] when that boundary matters.
    pub fn walk_ancestors(
        &self,
        event_id: EventId,
        limit: usize,
    ) -> Vec<StoredEvent<serde_json::Value>> {
        self.walk_ancestors_outcome(event_id, limit).ancestors
    }

    /// READ: walk bounded hash-chain ancestors from an event id, reporting
    /// where the walk stopped.
    ///
    /// Like [`Store::walk_ancestors`], but the returned [`AncestorWalk`] also
    /// carries the [`AncestryBoundary`] at which traversal ended, so callers
    /// can distinguish a chain that genuinely reached genesis
    /// ([`AncestryBoundary::ReachedGenesis`]) from one truncated at a missing
    /// parent link ([`AncestryBoundary::MissingParent`]) — for example, a
    /// surviving descendant of a Retention-dropped mid-chain event — as well
    /// as the `limit`, read-failure, cycle, and no-anchor boundaries.
    ///
    /// This is substrate ancestry, not domain graph traversal.
    pub fn walk_ancestors_outcome(&self, event_id: EventId, limit: usize) -> AncestorWalk {
        ancestry::walk_ancestors_outcome(self, event_id.as_u128(), limit)
    }

    /// PROJECT: reconstruct typed state from events, with cache support.
    ///
    /// # Errors
    /// Returns any replay, deserialization, cache, or disk-read error surfaced
    /// while reconstructing the projection state.
    pub fn project<T>(&self, entity: &str, freshness: &Freshness) -> Result<Option<T>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project(self, entity, freshness)
    }

    /// PROJECT: reconstruct two typed states from one consistent direct replay.
    ///
    /// Both projections must use the same replay input lane, and each is folded
    /// over only its declared [`EventSourced::relevant_event_kinds`]. This
    /// fused path intentionally bypasses projection caches so cache watermarks
    /// remain projection-specific.
    ///
    /// # Errors
    /// Returns any disk-read or replay decode error surfaced while loading the
    /// shared event stream.
    pub fn project_fused2<Left, Right>(
        &self,
        entity: &str,
    ) -> Result<(Option<Left>, Option<Right>), StoreError>
    where
        Left: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        Right: EventSourced<Input = Left::Input>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + 'static,
        Left::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_fused2(self, entity)
    }

    /// PROJECT: reconstruct three typed states from one consistent direct replay.
    ///
    /// The projections must use the same replay input lane. A projection whose
    /// [`EventSourced::relevant_event_kinds`] slice is empty receives the full
    /// shared stream; other projections receive only their declared kinds.
    ///
    /// # Errors
    /// Returns any disk-read or replay decode error surfaced while loading the
    /// shared event stream.
    pub fn project_fused3<First, Second, Third>(
        &self,
        entity: &str,
    ) -> Result<super::ProjectionFusion3<First, Second, Third>, StoreError>
    where
        First: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        Second: EventSourced<Input = First::Input>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + 'static,
        Third: EventSourced<Input = First::Input>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + 'static,
        First::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_fused3(self, entity)
    }

    /// Return the current per-entity generation if the entity exists.
    ///
    /// Generations advance monotonically on every insert for that entity.
    /// When entity-group overlays are disabled, this falls back to the entity
    /// stream length so callers still get a stable monotonic skip token.
    pub fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.index.entity_generation(entity)
    }

    /// Project only when the entity changed since `last_seen_generation`.
    ///
    /// Returns `Ok(None)` when no change is observed. Otherwise returns the
    /// generation at which the returned state was materialized together with
    /// the freshly projected state. The returned generation is honest: a
    /// cache-hit path returns the generation at which the cache was
    /// stamped, a replay path returns the generation sampled before replay
    /// started. Callers who persist this generation as a watermark (e.g.
    /// [`ProjectionWatcher`]) will not silently consume a relevant append
    /// against stale state (F5). To preserve that property, this API treats
    /// [`Freshness::MaybeStale`] the same as [`Freshness::Consistent`].
    ///
    /// # Errors
    /// Returns any error surfaced by [`Store::project`] when the entity has
    /// changed and the projection must be rebuilt.
    pub fn project_if_changed<T>(
        &self,
        entity: &str,
        last_seen_generation: u64,
        freshness: &Freshness,
    ) -> Result<Option<(u64, Option<T>)>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_if_changed(self, entity, last_seen_generation, freshness)
    }

    /// READ: query all events for an exact entity id.
    #[must_use]
    pub fn by_entity(&self, entity: &str) -> Vec<IndexEntry> {
        self.index.stream(entity)
    }

    /// READ: query all events for an exact entity id on one DAG lane.
    #[must_use]
    pub fn by_entity_lane(&self, entity: &str, lane: u32) -> Vec<IndexEntry> {
        self.index.stream_lane(entity, lane)
    }

    /// READ: query all events for an exact entity id on one DAG lane.
    #[must_use]
    pub fn stream_lane(&self, entity: &str, lane: u32) -> Vec<IndexEntry> {
        self.by_entity_lane(entity, lane)
    }

    /// READ: return the latest visible event for an entity on one DAG lane.
    #[must_use]
    pub fn latest_lane(&self, entity: &str, lane: u32) -> Option<IndexEntry> {
        self.index.get_latest(entity, lane)
    }

    /// READ: query all events in the given scope.
    #[must_use]
    pub fn by_scope(&self, scope: &str) -> Vec<IndexEntry> {
        self.query(&Region::scope(scope))
    }

    /// READ: query all events of the given event kind across all entities and scopes.
    #[must_use]
    pub fn by_fact(&self, kind: EventKind) -> Vec<IndexEntry> {
        self.query(&Region::all().with_fact(KindFilter::Exact(kind)))
    }

    /// READ (typed): query all events whose kind matches `T::KIND`.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`.
    #[must_use]
    pub fn by_fact_typed<T: EventPayload>(&self) -> Vec<IndexEntry> {
        self.by_fact(T::KIND)
    }

    /// CURSOR: pull-based, ordered delivery from the in-memory index.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`. This cursor is
    /// process-local durable-delivery vocabulary, not query pagination. It
    /// does not persist its position, so restart-time at-least-once semantics
    /// require the checkpoint-bound cursor worker surface rather than this
    /// constructor.
    pub fn cursor_guaranteed(&self, region: &Region) -> Cursor {
        Cursor::new(region.clone(), Arc::clone(&self.index))
    }
}

fn append_receipt_index_mismatch(
    receipt: &AppendReceipt,
    entry: &IndexEntry,
) -> Option<ReceiptVerificationError> {
    if receipt.event_id.as_u128() != entry.event_id {
        return Some(ReceiptVerificationError::EventIdMismatch);
    }
    if receipt.global_sequence != entry.global_sequence {
        return Some(ReceiptVerificationError::SequenceMismatch);
    }
    if receipt.disk_pos != entry.disk_pos {
        return Some(ReceiptVerificationError::DiskPositionMismatch);
    }
    if receipt.content_hash != entry.hash_chain.event_hash {
        return Some(ReceiptVerificationError::ContentHashMismatch);
    }
    if receipt.extensions != entry.receipt_extensions {
        return Some(ReceiptVerificationError::ExtensionsMismatch);
    }
    None
}

fn denial_receipt_index_mismatch(
    receipt: &DenialReceipt,
    entry: &IndexEntry,
) -> Option<ReceiptVerificationError> {
    if entry.kind != EventKind::SYSTEM_DENIAL {
        return Some(ReceiptVerificationError::DenialKindMismatch);
    }
    if receipt.event_id.as_u128() != entry.event_id {
        return Some(ReceiptVerificationError::EventIdMismatch);
    }
    if receipt.global_sequence != entry.global_sequence {
        return Some(ReceiptVerificationError::SequenceMismatch);
    }
    if receipt.disk_pos != entry.disk_pos {
        return Some(ReceiptVerificationError::DiskPositionMismatch);
    }
    if receipt.content_hash != entry.hash_chain.event_hash {
        return Some(ReceiptVerificationError::ContentHashMismatch);
    }
    if receipt.extensions != entry.receipt_extensions {
        return Some(ReceiptVerificationError::ExtensionsMismatch);
    }
    None
}

#[cfg(test)]
#[path = "read_api_tests.rs"]
mod tests;
