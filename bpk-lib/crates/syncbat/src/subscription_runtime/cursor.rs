use super::error::SubscriptionRuntimeError;

pub use super::entity_cursor::EntityStreamCursorV1;

/// Fixed cursor magic bytes (`BPSC`).
pub const CURSOR_MAGIC: [u8; 4] = *b"BPSC";
/// Supported cursor schema version.
pub const CURSOR_VERSION: u8 = 0x01;
/// Event-category stream source kind.
pub const SOURCE_KIND_EVENT_CATEGORY: u8 = 0x01;
/// Projection stream source kind.
pub const SOURCE_KIND_PROJECTION: u8 = 0x02;
/// Operation-status stream source kind.
pub const SOURCE_KIND_OPERATION_STATUS: u8 = 0x03;
/// Receipt stream source kind.
pub const SOURCE_KIND_RECEIPT_STREAM: u8 = 0x04;
/// Entity stream source kind.
pub const SOURCE_KIND_ENTITY_STREAM: u8 = 0x05;
/// Fixed on-wire event cursor byte length.
pub const CURSOR_V1_LEN: usize = 40;
/// Fixed on-wire projection cursor byte length.
pub const PROJECTION_CURSOR_V1_LEN: usize = 56;
/// Fixed on-wire operation-status cursor byte length.
pub const OPERATION_STATUS_CURSOR_V1_LEN: usize = 56;
/// Fixed on-wire receipt-stream cursor byte length.
pub const RECEIPT_STREAM_CURSOR_V1_LEN: usize = 56;
/// Fixed on-wire entity-stream cursor byte length.
pub const ENTITY_STREAM_CURSOR_V1_LEN: usize = 56;

const HASH_DOMAIN: &[u8] = b"syncbat.event-stream.cursor.subscription-id.v1\0";

/// Resume position encoded inside [`EventStreamCursorV1`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionKind {
    /// Start before the first visible event in the category stream.
    Beginning,
    /// Resume strictly after the given commit-order sequence.
    AfterGlobalSequence(u64),
}

/// Versioned opaque event-stream cursor owned by syncbat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventStreamCursorV1 {
    /// Exported event category filter.
    pub event_category: u8,
    /// Route binding hash for the subscription id.
    pub subscription_id_hash: [u8; 16],
    /// Visible HLC wall milliseconds at the cursor point (metadata).
    pub hlc_wall_ms: u64,
    /// Commit-order resume position encoded by the cursor.
    pub position: PositionKind,
}

impl EventStreamCursorV1 {
    /// Encode the subscription-stream origin cursor for a route.
    #[must_use]
    pub fn beginning(subscription_id: &str, event_category: u8) -> Self {
        Self {
            event_category,
            subscription_id_hash: subscription_id_hash(subscription_id),
            hlc_wall_ms: 0,
            position: PositionKind::Beginning,
        }
    }

    /// Encode a resume cursor strictly after `global_sequence`.
    #[must_use]
    pub fn after_global_sequence(
        subscription_id: &str,
        event_category: u8,
        global_sequence: u64,
        hlc_wall_ms: u64,
    ) -> Self {
        Self {
            event_category,
            subscription_id_hash: subscription_id_hash(subscription_id),
            hlc_wall_ms,
            position: PositionKind::AfterGlobalSequence(global_sequence),
        }
    }

    /// Return the exclusive lower bound for `query_entries_after`, if any.
    #[must_use]
    pub fn resume_after_global_sequence(&self) -> Option<u64> {
        match self.position {
            PositionKind::Beginning => None,
            PositionKind::AfterGlobalSequence(seq) => Some(seq),
        }
    }

    /// Encode to fixed 40-byte big-endian layout.
    #[must_use]
    pub fn encode(&self) -> [u8; CURSOR_V1_LEN] {
        let mut out = [0_u8; CURSOR_V1_LEN];
        out[0..4].copy_from_slice(&CURSOR_MAGIC);
        out[4] = CURSOR_VERSION;
        out[5] = SOURCE_KIND_EVENT_CATEGORY;
        out[6] = match self.position {
            PositionKind::Beginning => 0x00,
            PositionKind::AfterGlobalSequence(_) => 0x01,
        };
        out[7] = self.event_category;
        out[8..24].copy_from_slice(&self.subscription_id_hash);
        out[24..32].copy_from_slice(&self.hlc_wall_ms.to_be_bytes());
        let global_sequence = match self.position {
            PositionKind::Beginning => 0,
            PositionKind::AfterGlobalSequence(seq) => seq,
        };
        out[32..40].copy_from_slice(&global_sequence.to_be_bytes());
        out
    }

    /// Decode fixed-layout cursor bytes.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorInvalid`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SubscriptionRuntimeError> {
        if bytes.len() != CURSOR_V1_LEN {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor length is not 40 bytes",
            });
        }
        if bytes[0..4] != CURSOR_MAGIC {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor magic mismatch",
            });
        }
        if bytes[4] != CURSOR_VERSION {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor version mismatch",
            });
        }
        if bytes[5] != SOURCE_KIND_EVENT_CATEGORY {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor source kind mismatch",
            });
        }
        let position = match bytes[6] {
            0x00 => {
                let hlc_wall_ms = read_u64_be(bytes, 24);
                let global_sequence = read_u64_be(bytes, 32);
                if hlc_wall_ms != 0 || global_sequence != 0 {
                    return Err(SubscriptionRuntimeError::CursorInvalid {
                        reason: "beginning cursor has nonzero numeric fields",
                    });
                }
                PositionKind::Beginning
            }
            0x01 => {
                let global_sequence = read_u64_be(bytes, 32);
                PositionKind::AfterGlobalSequence(global_sequence)
            }
            _ => {
                return Err(SubscriptionRuntimeError::CursorInvalid {
                    reason: "cursor position kind is invalid",
                });
            }
        };
        let mut subscription_id_hash = [0_u8; 16];
        subscription_id_hash.copy_from_slice(&bytes[8..24]);
        Ok(Self {
            event_category: bytes[7],
            subscription_id_hash,
            hlc_wall_ms: read_u64_be(bytes, 24),
            position,
        })
    }

    /// Validate cursor binding against a subscription route.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorMismatch`].
    pub fn validate_route(
        &self,
        subscription_id: &str,
        event_category: u8,
    ) -> Result<(), SubscriptionRuntimeError> {
        if self.event_category != event_category {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor event category mismatch",
            });
        }
        if self.subscription_id_hash != subscription_id_hash(subscription_id) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor subscription id hash mismatch",
            });
        }
        Ok(())
    }
}

/// Compute the route-binding hash for a subscription id.
#[must_use]
pub fn subscription_id_hash(subscription_id: &str) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HASH_DOMAIN);
    hasher.update(subscription_id.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0_u8; 16];
    out.copy_from_slice(&digest.as_bytes()[0..16]);
    out
}

pub(super) fn read_u64_be(bytes: &[u8], offset: usize) -> u64 {
    let mut buf = [0_u8; 8];
    buf.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_be_bytes(buf)
}

const PROJECTION_SUBSCRIPTION_HASH_DOMAIN: &[u8] =
    b"syncbat.projection-stream.cursor.subscription-id.v1\0";
const PROJECTION_ID_HASH_DOMAIN: &[u8] = b"syncbat.projection-stream.cursor.projection-id.v1\0";
const PROJECTION_ENTITY_HASH_DOMAIN: &[u8] = b"syncbat.projection-stream.cursor.entity.v1\0";

/// Resume position encoded inside [`ProjectionStreamCursorV1`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionPositionKind {
    /// Start before the first entity generation in the projection stream.
    Beginning,
    /// Resume strictly after the given entity generation.
    AfterEntityGeneration(u64),
}

/// Versioned opaque projection-stream cursor owned by syncbat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionStreamCursorV1 {
    /// Route binding hash for the subscription id.
    pub subscription_id_hash: [u8; 16],
    /// Route binding hash for the projection id.
    pub projection_id_hash: [u8; 16],
    /// Route binding hash for the entity coordinate.
    pub entity_hash: [u8; 8],
    /// Entity generation encoded at the cursor point.
    pub entity_generation: u64,
    /// Resume position encoded by the cursor.
    pub position: ProjectionPositionKind,
}

impl ProjectionStreamCursorV1 {
    /// Encode the projection-stream origin cursor for a route.
    #[must_use]
    pub fn beginning(subscription_id: &str, projection_id: &str, entity: &str) -> Self {
        Self {
            subscription_id_hash: projection_subscription_id_hash(subscription_id),
            projection_id_hash: projection_id_hash(projection_id),
            entity_hash: projection_entity_hash(entity),
            entity_generation: 0,
            position: ProjectionPositionKind::Beginning,
        }
    }

    /// Encode a resume cursor strictly after `entity_generation`.
    #[must_use]
    pub fn after_entity_generation(
        subscription_id: &str,
        projection_id: &str,
        entity: &str,
        entity_generation: u64,
    ) -> Self {
        Self {
            subscription_id_hash: projection_subscription_id_hash(subscription_id),
            projection_id_hash: projection_id_hash(projection_id),
            entity_hash: projection_entity_hash(entity),
            entity_generation,
            position: ProjectionPositionKind::AfterEntityGeneration(entity_generation),
        }
    }

    /// Return the exclusive lower bound for entity-generation resume, if any.
    #[must_use]
    pub fn resume_after_entity_generation(&self) -> Option<u64> {
        match self.position {
            ProjectionPositionKind::Beginning => None,
            ProjectionPositionKind::AfterEntityGeneration(gen) => Some(gen),
        }
    }

    /// Encode to fixed 56-byte big-endian layout.
    #[must_use]
    pub fn encode(&self) -> [u8; PROJECTION_CURSOR_V1_LEN] {
        let mut out = [0_u8; PROJECTION_CURSOR_V1_LEN];
        out[0..4].copy_from_slice(&CURSOR_MAGIC);
        out[4] = CURSOR_VERSION;
        out[5] = SOURCE_KIND_PROJECTION;
        out[6] = match self.position {
            ProjectionPositionKind::Beginning => 0x00,
            ProjectionPositionKind::AfterEntityGeneration(_) => 0x02,
        };
        out[7] = 0x00;
        out[8..24].copy_from_slice(&self.subscription_id_hash);
        out[24..40].copy_from_slice(&self.projection_id_hash);
        out[40..48].copy_from_slice(&self.entity_hash);
        out[48..56].copy_from_slice(&self.entity_generation.to_be_bytes());
        out
    }

    /// Decode fixed-layout projection cursor bytes.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorInvalid`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SubscriptionRuntimeError> {
        if bytes.len() != PROJECTION_CURSOR_V1_LEN {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor length is not 56 bytes",
            });
        }
        if bytes[0..4] != CURSOR_MAGIC {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor magic mismatch",
            });
        }
        if bytes[4] != CURSOR_VERSION {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor version mismatch",
            });
        }
        if bytes[5] != SOURCE_KIND_PROJECTION {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor source kind mismatch",
            });
        }
        if bytes[7] != 0x00 {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor reserved byte is nonzero",
            });
        }
        let position = match bytes[6] {
            0x00 => {
                let entity_generation = read_u64_be(bytes, 48);
                if entity_generation != 0 {
                    return Err(SubscriptionRuntimeError::CursorInvalid {
                        reason: "beginning cursor has nonzero entity generation",
                    });
                }
                ProjectionPositionKind::Beginning
            }
            0x02 => {
                let entity_generation = read_u64_be(bytes, 48);
                ProjectionPositionKind::AfterEntityGeneration(entity_generation)
            }
            _ => {
                return Err(SubscriptionRuntimeError::CursorInvalid {
                    reason: "cursor position kind is invalid",
                });
            }
        };
        let mut subscription_id_hash = [0_u8; 16];
        subscription_id_hash.copy_from_slice(&bytes[8..24]);
        let mut projection_id_hash = [0_u8; 16];
        projection_id_hash.copy_from_slice(&bytes[24..40]);
        let mut entity_hash = [0_u8; 8];
        entity_hash.copy_from_slice(&bytes[40..48]);
        Ok(Self {
            subscription_id_hash,
            projection_id_hash,
            entity_hash,
            entity_generation: read_u64_be(bytes, 48),
            position,
        })
    }

    /// Validate cursor binding against a projection subscription route.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorMismatch`].
    pub fn validate_route(
        &self,
        subscription_id: &str,
        projection_id: &str,
        entity: &str,
    ) -> Result<(), SubscriptionRuntimeError> {
        if self.subscription_id_hash != projection_subscription_id_hash(subscription_id) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor subscription id hash mismatch",
            });
        }
        if self.projection_id_hash != projection_id_hash(projection_id) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor projection id hash mismatch",
            });
        }
        if self.entity_hash != projection_entity_hash(entity) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor entity hash mismatch",
            });
        }
        Ok(())
    }
}

/// Compute the route-binding hash for a projection subscription id.
#[must_use]
pub fn projection_subscription_id_hash(subscription_id: &str) -> [u8; 16] {
    hash_prefix_16(
        PROJECTION_SUBSCRIPTION_HASH_DOMAIN,
        subscription_id.as_bytes(),
    )
}

/// Compute the route-binding hash for a projection id.
#[must_use]
pub fn projection_id_hash(projection_id: &str) -> [u8; 16] {
    hash_prefix_16(PROJECTION_ID_HASH_DOMAIN, projection_id.as_bytes())
}

/// Compute the route-binding hash for an entity coordinate.
#[must_use]
pub fn projection_entity_hash(entity: &str) -> [u8; 8] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PROJECTION_ENTITY_HASH_DOMAIN);
    hasher.update(entity.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0_u8; 8];
    out.copy_from_slice(&digest.as_bytes()[0..8]);
    out
}

pub(super) fn hash_prefix_16(domain: &[u8], value: &[u8]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(value);
    let digest = hasher.finalize();
    let mut out = [0_u8; 16];
    out.copy_from_slice(&digest.as_bytes()[0..16]);
    out
}

const OPERATION_STATUS_SUBSCRIPTION_HASH_DOMAIN: &[u8] =
    b"syncbat.operation-status-stream.cursor.subscription-id.v1\0";
const OPERATION_STATUS_OPERATION_HASH_DOMAIN: &[u8] =
    b"syncbat.operation-status-stream.cursor.operation.v1\0";
const OPERATION_STATUS_ENTITY_HASH_DOMAIN: &[u8] =
    b"syncbat.operation-status-stream.cursor.entity.v1\0";

/// Resume position encoded inside [`OperationStatusStreamCursorV1`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationStatusPositionKind {
    /// Start before the first entity generation in the operation-status stream.
    Beginning,
    /// Resume strictly after the given entity generation.
    AfterEntityGeneration(u64),
}

/// Versioned opaque operation-status-stream cursor owned by syncbat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationStatusStreamCursorV1 {
    /// Route binding hash for the subscription id.
    pub subscription_id_hash: [u8; 16],
    /// Route binding hash for the operation name.
    pub operation_hash: [u8; 16],
    /// Route binding hash for the entity coordinate.
    pub entity_hash: [u8; 8],
    /// Entity generation encoded at the cursor point.
    pub entity_generation: u64,
    /// Resume position encoded by the cursor.
    pub position: OperationStatusPositionKind,
}

impl OperationStatusStreamCursorV1 {
    /// Encode the operation-status-stream origin cursor for a route.
    #[must_use]
    pub fn beginning(subscription_id: &str, operation: &str, entity: &str) -> Self {
        Self {
            subscription_id_hash: operation_status_subscription_id_hash(subscription_id),
            operation_hash: operation_status_operation_hash(operation),
            entity_hash: operation_status_entity_hash(entity),
            entity_generation: 0,
            position: OperationStatusPositionKind::Beginning,
        }
    }

    /// Encode a resume cursor strictly after `entity_generation`.
    #[must_use]
    pub fn after_entity_generation(
        subscription_id: &str,
        operation: &str,
        entity: &str,
        entity_generation: u64,
    ) -> Self {
        Self {
            subscription_id_hash: operation_status_subscription_id_hash(subscription_id),
            operation_hash: operation_status_operation_hash(operation),
            entity_hash: operation_status_entity_hash(entity),
            entity_generation,
            position: OperationStatusPositionKind::AfterEntityGeneration(entity_generation),
        }
    }

    /// Return the exclusive lower bound for entity-generation resume, if any.
    #[must_use]
    pub fn resume_after_entity_generation(&self) -> Option<u64> {
        match self.position {
            OperationStatusPositionKind::Beginning => None,
            OperationStatusPositionKind::AfterEntityGeneration(gen) => Some(gen),
        }
    }

    /// Encode to fixed 56-byte big-endian layout.
    #[must_use]
    pub fn encode(&self) -> [u8; OPERATION_STATUS_CURSOR_V1_LEN] {
        let mut out = [0_u8; OPERATION_STATUS_CURSOR_V1_LEN];
        out[0..4].copy_from_slice(&CURSOR_MAGIC);
        out[4] = CURSOR_VERSION;
        out[5] = SOURCE_KIND_OPERATION_STATUS;
        out[6] = match self.position {
            OperationStatusPositionKind::Beginning => 0x00,
            OperationStatusPositionKind::AfterEntityGeneration(_) => 0x02,
        };
        out[7] = 0x00;
        out[8..24].copy_from_slice(&self.subscription_id_hash);
        out[24..40].copy_from_slice(&self.operation_hash);
        out[40..48].copy_from_slice(&self.entity_hash);
        out[48..56].copy_from_slice(&self.entity_generation.to_be_bytes());
        out
    }

    /// Decode fixed-layout operation-status cursor bytes.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorInvalid`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SubscriptionRuntimeError> {
        if bytes.len() != OPERATION_STATUS_CURSOR_V1_LEN {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor length is not 56 bytes",
            });
        }
        if bytes[0..4] != CURSOR_MAGIC {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor magic mismatch",
            });
        }
        if bytes[4] != CURSOR_VERSION {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor version mismatch",
            });
        }
        if bytes[5] != SOURCE_KIND_OPERATION_STATUS {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor source kind mismatch",
            });
        }
        if bytes[7] != 0x00 {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor reserved byte is nonzero",
            });
        }
        let position = match bytes[6] {
            0x00 => {
                let entity_generation = read_u64_be(bytes, 48);
                if entity_generation != 0 {
                    return Err(SubscriptionRuntimeError::CursorInvalid {
                        reason: "beginning cursor has nonzero entity generation",
                    });
                }
                OperationStatusPositionKind::Beginning
            }
            0x02 => {
                let entity_generation = read_u64_be(bytes, 48);
                OperationStatusPositionKind::AfterEntityGeneration(entity_generation)
            }
            _ => {
                return Err(SubscriptionRuntimeError::CursorInvalid {
                    reason: "cursor position kind is invalid",
                });
            }
        };
        let mut subscription_id_hash = [0_u8; 16];
        subscription_id_hash.copy_from_slice(&bytes[8..24]);
        let mut operation_hash = [0_u8; 16];
        operation_hash.copy_from_slice(&bytes[24..40]);
        let mut entity_hash = [0_u8; 8];
        entity_hash.copy_from_slice(&bytes[40..48]);
        Ok(Self {
            subscription_id_hash,
            operation_hash,
            entity_hash,
            entity_generation: read_u64_be(bytes, 48),
            position,
        })
    }

    /// Validate cursor binding against an operation-status subscription route.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorMismatch`].
    pub fn validate_route(
        &self,
        subscription_id: &str,
        operation: &str,
        entity: &str,
    ) -> Result<(), SubscriptionRuntimeError> {
        if self.subscription_id_hash != operation_status_subscription_id_hash(subscription_id) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor subscription id hash mismatch",
            });
        }
        if self.operation_hash != operation_status_operation_hash(operation) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor operation hash mismatch",
            });
        }
        if self.entity_hash != operation_status_entity_hash(entity) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor entity hash mismatch",
            });
        }
        Ok(())
    }
}

/// Compute the route-binding hash for an operation-status subscription id.
#[must_use]
pub fn operation_status_subscription_id_hash(subscription_id: &str) -> [u8; 16] {
    hash_prefix_16(
        OPERATION_STATUS_SUBSCRIPTION_HASH_DOMAIN,
        subscription_id.as_bytes(),
    )
}

/// Compute the route-binding hash for an operation name.
#[must_use]
pub fn operation_status_operation_hash(operation: &str) -> [u8; 16] {
    hash_prefix_16(OPERATION_STATUS_OPERATION_HASH_DOMAIN, operation.as_bytes())
}

/// Compute the route-binding hash for an entity coordinate.
#[must_use]
pub fn operation_status_entity_hash(entity: &str) -> [u8; 8] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(OPERATION_STATUS_ENTITY_HASH_DOMAIN);
    hasher.update(entity.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0_u8; 8];
    out.copy_from_slice(&digest.as_bytes()[0..8]);
    out
}

const RECEIPT_STREAM_SUBSCRIPTION_HASH_DOMAIN: &[u8] =
    b"syncbat.receipt-stream.cursor.subscription-id.v1\0";
const RECEIPT_STREAM_RECEIPT_KIND_HASH_DOMAIN: &[u8] =
    b"syncbat.receipt-stream.cursor.receipt-kind.v1\0";

/// Versioned opaque receipt-stream cursor owned by syncbat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptStreamCursorV1 {
    /// Route binding hash for the subscription id.
    pub subscription_id_hash: [u8; 16],
    /// Route binding hash for the receipt kind filter.
    pub receipt_kind_hash: [u8; 16],
    /// Visible HLC wall milliseconds at the cursor point (metadata).
    pub hlc_wall_ms: u64,
    /// Commit-order resume position encoded by the cursor.
    pub position: PositionKind,
}

impl ReceiptStreamCursorV1 {
    /// Encode the receipt-stream origin cursor for a route.
    #[must_use]
    pub fn beginning(subscription_id: &str, receipt_kind: &str) -> Self {
        Self {
            subscription_id_hash: receipt_stream_subscription_id_hash(subscription_id),
            receipt_kind_hash: receipt_stream_receipt_kind_hash(receipt_kind),
            hlc_wall_ms: 0,
            position: PositionKind::Beginning,
        }
    }

    /// Encode a resume cursor strictly after `global_sequence`.
    #[must_use]
    pub fn after_global_sequence(
        subscription_id: &str,
        receipt_kind: &str,
        global_sequence: u64,
        hlc_wall_ms: u64,
    ) -> Self {
        Self {
            subscription_id_hash: receipt_stream_subscription_id_hash(subscription_id),
            receipt_kind_hash: receipt_stream_receipt_kind_hash(receipt_kind),
            hlc_wall_ms,
            position: PositionKind::AfterGlobalSequence(global_sequence),
        }
    }

    /// Return the exclusive lower bound for `query_entries_after`, if any.
    #[must_use]
    pub fn resume_after_global_sequence(&self) -> Option<u64> {
        match self.position {
            PositionKind::Beginning => None,
            PositionKind::AfterGlobalSequence(seq) => Some(seq),
        }
    }

    /// Encode to fixed 56-byte big-endian layout.
    #[must_use]
    pub fn encode(&self) -> [u8; RECEIPT_STREAM_CURSOR_V1_LEN] {
        let mut out = [0_u8; RECEIPT_STREAM_CURSOR_V1_LEN];
        out[0..4].copy_from_slice(&CURSOR_MAGIC);
        out[4] = CURSOR_VERSION;
        out[5] = SOURCE_KIND_RECEIPT_STREAM;
        out[6] = match self.position {
            PositionKind::Beginning => 0x00,
            PositionKind::AfterGlobalSequence(_) => 0x01,
        };
        out[7] = 0x00;
        out[8..24].copy_from_slice(&self.subscription_id_hash);
        out[24..40].copy_from_slice(&self.receipt_kind_hash);
        out[40..48].copy_from_slice(&self.hlc_wall_ms.to_be_bytes());
        let global_sequence = match self.position {
            PositionKind::Beginning => 0,
            PositionKind::AfterGlobalSequence(seq) => seq,
        };
        out[48..56].copy_from_slice(&global_sequence.to_be_bytes());
        out
    }

    /// Decode fixed-layout receipt-stream cursor bytes.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorInvalid`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SubscriptionRuntimeError> {
        if bytes.len() != RECEIPT_STREAM_CURSOR_V1_LEN {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor length is not 56 bytes",
            });
        }
        if bytes[0..4] != CURSOR_MAGIC {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor magic mismatch",
            });
        }
        if bytes[4] != CURSOR_VERSION {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor version mismatch",
            });
        }
        if bytes[5] != SOURCE_KIND_RECEIPT_STREAM {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor source kind mismatch",
            });
        }
        if bytes[7] != 0x00 {
            return Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor reserved byte is nonzero",
            });
        }
        let position = match bytes[6] {
            0x00 => {
                let hlc_wall_ms = read_u64_be(bytes, 40);
                let global_sequence = read_u64_be(bytes, 48);
                if hlc_wall_ms != 0 || global_sequence != 0 {
                    return Err(SubscriptionRuntimeError::CursorInvalid {
                        reason: "beginning cursor has nonzero numeric fields",
                    });
                }
                PositionKind::Beginning
            }
            0x01 => {
                let global_sequence = read_u64_be(bytes, 48);
                PositionKind::AfterGlobalSequence(global_sequence)
            }
            _ => {
                return Err(SubscriptionRuntimeError::CursorInvalid {
                    reason: "cursor position kind is invalid",
                });
            }
        };
        let mut subscription_id_hash = [0_u8; 16];
        subscription_id_hash.copy_from_slice(&bytes[8..24]);
        let mut receipt_kind_hash = [0_u8; 16];
        receipt_kind_hash.copy_from_slice(&bytes[24..40]);
        Ok(Self {
            subscription_id_hash,
            receipt_kind_hash,
            hlc_wall_ms: read_u64_be(bytes, 40),
            position,
        })
    }

    /// Validate cursor binding against a receipt-stream subscription route.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorMismatch`].
    pub fn validate_route(
        &self,
        subscription_id: &str,
        receipt_kind: &str,
    ) -> Result<(), SubscriptionRuntimeError> {
        if self.subscription_id_hash != receipt_stream_subscription_id_hash(subscription_id) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor subscription id hash mismatch",
            });
        }
        if self.receipt_kind_hash != receipt_stream_receipt_kind_hash(receipt_kind) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor receipt kind hash mismatch",
            });
        }
        Ok(())
    }
}

/// Compute the route-binding hash for a receipt-stream subscription id.
#[must_use]
pub fn receipt_stream_subscription_id_hash(subscription_id: &str) -> [u8; 16] {
    hash_prefix_16(
        RECEIPT_STREAM_SUBSCRIPTION_HASH_DOMAIN,
        subscription_id.as_bytes(),
    )
}

/// Compute the route-binding hash for a receipt kind filter.
#[must_use]
pub fn receipt_stream_receipt_kind_hash(receipt_kind: &str) -> [u8; 16] {
    hash_prefix_16(
        RECEIPT_STREAM_RECEIPT_KIND_HASH_DOMAIN,
        receipt_kind.as_bytes(),
    )
}

#[cfg(test)]
mod cursor_helper_tests {
    use super::{
        operation_status_subscription_id_hash, projection_subscription_id_hash,
        receipt_stream_subscription_id_hash, subscription_id_hash, EventStreamCursorV1,
        ReceiptStreamCursorV1, SubscriptionRuntimeError,
    };

    #[test]
    fn event_cursor_decode_rejects_beginning_with_nonzero_hlc_only() {
        // Beginning cursor (kind byte 0x00) with hlc_wall_ms != 0 but global_sequence == 0.
        // Real `||` rejects when either numeric field is nonzero; a `&&` mutant would accept.
        let mut bytes = EventStreamCursorV1::beginning("sub-a", 3).encode();
        bytes[24..32].copy_from_slice(&7_u64.to_be_bytes());
        assert!(matches!(
            EventStreamCursorV1::decode(&bytes),
            Err(SubscriptionRuntimeError::CursorInvalid { .. })
        ));
    }

    #[test]
    fn receipt_cursor_decode_rejects_beginning_with_nonzero_hlc_only() {
        let mut bytes = ReceiptStreamCursorV1::beginning("sub-a", "kind-a").encode();
        bytes[40..48].copy_from_slice(&7_u64.to_be_bytes());
        assert!(matches!(
            ReceiptStreamCursorV1::decode(&bytes),
            Err(SubscriptionRuntimeError::CursorInvalid { .. })
        ));
    }

    #[test]
    fn subscription_id_hashes_are_input_dependent_and_nonconstant() {
        let pairs = [
            subscription_id_hash("alpha"),
            subscription_id_hash("beta"),
            projection_subscription_id_hash("alpha"),
            projection_subscription_id_hash("beta"),
            operation_status_subscription_id_hash("alpha"),
            operation_status_subscription_id_hash("beta"),
            receipt_stream_subscription_id_hash("alpha"),
            receipt_stream_subscription_id_hash("beta"),
        ];
        for chunk in pairs.chunks_exact(2) {
            assert_ne!(chunk[0], chunk[1]);
            assert_ne!(chunk[0], [0_u8; 16]);
            assert_ne!(chunk[0], [1_u8; 16]);
        }
    }
}
