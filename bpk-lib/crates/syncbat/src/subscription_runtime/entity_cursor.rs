use super::cursor::{
    hash_prefix_16, read_u64_be, CURSOR_MAGIC, CURSOR_VERSION, ENTITY_STREAM_CURSOR_V1_LEN,
    SOURCE_KIND_ENTITY_STREAM,
};
use super::error::SubscriptionRuntimeError;

use crate::subscription_runtime::PositionKind;

const ENTITY_STREAM_SUBSCRIPTION_HASH_DOMAIN: &[u8] =
    b"syncbat.entity-stream.cursor.subscription-id.v1\0";
const ENTITY_STREAM_COORDINATE_HASH_DOMAIN: &[u8] = b"syncbat.entity-stream.cursor.coordinate.v1\0";

/// Versioned opaque entity-stream cursor owned by syncbat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityStreamCursorV1 {
    /// Route binding hash for the subscription id.
    pub subscription_id_hash: [u8; 16],
    /// Route binding hash for the entity coordinate.
    pub coordinate_hash: [u8; 16],
    /// Visible HLC wall milliseconds at the cursor point (metadata).
    pub hlc_wall_ms: u64,
    /// Commit-order resume position encoded by the cursor.
    pub position: PositionKind,
}

impl EntityStreamCursorV1 {
    /// Encode the entity-stream origin cursor for a route.
    #[must_use]
    pub fn beginning(subscription_id: &str, entity: &str, scope: &str) -> Self {
        Self {
            subscription_id_hash: entity_stream_subscription_id_hash(subscription_id),
            coordinate_hash: entity_stream_coordinate_hash(entity, scope),
            hlc_wall_ms: 0,
            position: PositionKind::Beginning,
        }
    }

    /// Encode a resume cursor strictly after `global_sequence`.
    #[must_use]
    pub fn after_global_sequence(
        subscription_id: &str,
        entity: &str,
        scope: &str,
        global_sequence: u64,
        hlc_wall_ms: u64,
    ) -> Self {
        Self {
            subscription_id_hash: entity_stream_subscription_id_hash(subscription_id),
            coordinate_hash: entity_stream_coordinate_hash(entity, scope),
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
    pub fn encode(&self) -> [u8; ENTITY_STREAM_CURSOR_V1_LEN] {
        let mut out = [0_u8; ENTITY_STREAM_CURSOR_V1_LEN];
        out[0..4].copy_from_slice(&CURSOR_MAGIC);
        out[4] = CURSOR_VERSION;
        out[5] = SOURCE_KIND_ENTITY_STREAM;
        out[6] = match self.position {
            PositionKind::Beginning => 0x00,
            PositionKind::AfterGlobalSequence(_) => 0x01,
        };
        out[7] = 0x00;
        out[8..24].copy_from_slice(&self.subscription_id_hash);
        out[24..40].copy_from_slice(&self.coordinate_hash);
        out[40..48].copy_from_slice(&self.hlc_wall_ms.to_be_bytes());
        let global_sequence = match self.position {
            PositionKind::Beginning => 0,
            PositionKind::AfterGlobalSequence(seq) => seq,
        };
        out[48..56].copy_from_slice(&global_sequence.to_be_bytes());
        out
    }

    /// Decode fixed-layout entity-stream cursor bytes.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorInvalid`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SubscriptionRuntimeError> {
        if bytes.len() != ENTITY_STREAM_CURSOR_V1_LEN {
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
        if bytes[5] != SOURCE_KIND_ENTITY_STREAM {
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
        let mut coordinate_hash = [0_u8; 16];
        coordinate_hash.copy_from_slice(&bytes[24..40]);
        Ok(Self {
            subscription_id_hash,
            coordinate_hash,
            hlc_wall_ms: read_u64_be(bytes, 40),
            position,
        })
    }

    /// Validate cursor binding against an entity-stream subscription route.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::CursorMismatch`].
    pub fn validate_route(
        &self,
        subscription_id: &str,
        entity: &str,
        scope: &str,
    ) -> Result<(), SubscriptionRuntimeError> {
        if self.subscription_id_hash != entity_stream_subscription_id_hash(subscription_id) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor subscription id hash mismatch",
            });
        }
        if self.coordinate_hash != entity_stream_coordinate_hash(entity, scope) {
            return Err(SubscriptionRuntimeError::CursorMismatch {
                reason: "cursor coordinate hash mismatch",
            });
        }
        Ok(())
    }
}

/// Compute the route-binding hash for an entity-stream subscription id.
#[must_use]
pub fn entity_stream_subscription_id_hash(subscription_id: &str) -> [u8; 16] {
    hash_prefix_16(
        ENTITY_STREAM_SUBSCRIPTION_HASH_DOMAIN,
        subscription_id.as_bytes(),
    )
}

/// Compute the route-binding hash for an entity coordinate.
#[must_use]
pub fn entity_stream_coordinate_hash(entity: &str, scope: &str) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENTITY_STREAM_COORDINATE_HASH_DOMAIN);
    hasher.update(entity.as_bytes());
    hasher.update(&[0]);
    hasher.update(scope.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0_u8; 16];
    out.copy_from_slice(&digest.as_bytes()[0..16]);
    out
}
