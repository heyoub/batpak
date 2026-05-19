//! Runtime handlers for [`crate::bank`] operations.
//!
//! These handlers capture an `Arc<batpak::store::Store>` so they can be
//! registered with [`syncbat::CoreBuilder::register`] from the `hbat`
//! binary. They live in their own module (not in `bank.rs`) because the
//! library half of `hbat` deliberately does NOT depend on a runtime
//! store handle — the descriptors and payload types are pure data and
//! must be linkable from `xtask` without dragging the runtime in.

use std::sync::Arc;

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, Store};
// Hex codec is the canonical netbat implementation; hbat does not
// re-roll its own. See netbat::transport::hex.
use netbat::{decode_hex_str, encode_hex_str};
use syncbat::{Ctx, Handler, HandlerError, HandlerResult};

use crate::bank::{BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest};

// ─── bank.commit handler ────────────────────────────────────────────────────

/// Handler binding for [`crate::bank::BANK_COMMIT_DESCRIPTOR`]. Captures
/// the runtime store handle.
pub struct BankCommitHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for BankCommitHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_bank_commit(&self.store, input)
    }
}

fn handle_bank_commit(store: &Store, input: &[u8]) -> HandlerResult {
    let request: BankCommitRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let coord = Coordinate::new(&request.entity, &request.scope)
        .map_err(|error| HandlerError::invalid_input(format!("coordinate: {error}")))?;

    let kind = EventKind::try_custom(request.kind_category, request.kind_type_id)
        .map_err(|error| HandlerError::invalid_input(format!("event kind: {error:?}")))?;

    let payload_bytes = decode_hex_str(&request.payload_hex)
        .map_err(|error| HandlerError::invalid_input(format!("payload_hex: {error}")))?;

    let item = BatchAppendItem::from_msgpack_bytes(
        coord,
        kind,
        payload_bytes,
        AppendOptions::new(),
        CausationRef::None,
    );

    let receipts = store
        .append_batch(vec![item])
        .map_err(|error| HandlerError::failed(format!("append: {error}")))?;

    let receipt = receipts
        .into_iter()
        .next()
        .ok_or_else(|| HandlerError::failed("append returned no receipt"))?;

    let ack = append_receipt_to_ack(&receipt);
    batpak::encoding::to_bytes(&ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))
}

fn append_receipt_to_ack(receipt: &AppendReceipt) -> BankCommitAck {
    let extensions = receipt
        .extensions
        .iter()
        .map(|(key, value)| (key.as_str().to_owned(), encode_hex_str(value)))
        .collect();
    BankCommitAck {
        event_id_hex: format!("{:032x}", u128::from(receipt.event_id)),
        sequence: receipt.sequence,
        content_hash_hex: encode_hex_str(&receipt.content_hash),
        key_id_hex: encode_hex_str(&receipt.key_id),
        signature_hex: receipt.signature.map(|s| encode_hex_str(&s)),
        extensions,
    }
}

// ─── event.get handler ──────────────────────────────────────────────────────

/// Handler binding for [`crate::bank::EVENT_GET_DESCRIPTOR`].
pub struct EventGetHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for EventGetHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_event_get(&self.store, input)
    }
}

fn handle_event_get(store: &Store, input: &[u8]) -> HandlerResult {
    let request: EventGetRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let event_id_bytes = decode_hex_str(&request.event_id_hex)
        .map_err(|error| HandlerError::invalid_input(format!("event_id_hex: {error}")))?;
    if event_id_bytes.len() != 16 {
        return Err(HandlerError::invalid_input(format!(
            "event_id_hex must decode to 16 bytes, got {}",
            event_id_bytes.len()
        )));
    }
    let mut be = [0_u8; 16];
    be.copy_from_slice(&event_id_bytes);
    let event_id = u128::from_be_bytes(be);

    let stored = store
        .read_raw(batpak::id::EventId::from(event_id))
        .map_err(|error| HandlerError::failed(format!("read event: {error}")))?;

    let ack = EventGetAck {
        event_id_hex: format!("{:032x}", u128::from(stored.event.header.event_id)),
        sequence: 0,
        timestamp_us: stored.event.header.timestamp_us,
        correlation_id_hex: format!(
            "{:032x}",
            u128::from(stored.event.header.correlation_id)
        ),
        causation_id_hex: stored
            .event
            .header
            .causation_id
            .map(|c| format!("{:032x}", u128::from(c))),
        kind_category: stored.event.header.event_kind.category(),
        kind_type_id: stored.event.header.event_kind.type_id(),
        entity: stored.coordinate.entity().to_owned(),
        scope: stored.coordinate.scope().to_owned(),
        payload_hex: encode_hex_str(&stored.event.payload),
        content_hash_hex: encode_hex_str(&stored.event.header.content_hash),
    };

    batpak::encoding::to_bytes(&ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use batpak::store::{Store, StoreConfig};

    use super::*;
    use crate::bank::{
        BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, BANK_COMMIT_DESCRIPTOR,
        EVENT_GET_DESCRIPTOR,
    };
    use crate::heartbeat::SystemHeartbeatRequest;
    use crate::EventPayloadFixture;
    use batpak::EventPayload;

    fn fresh_store() -> (Arc<Store>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false),
        )
        .expect("open store");
        (Arc::new(store), dir)
    }

    fn fresh_core(store: &Arc<Store>) -> syncbat::Core {
        let mut builder = syncbat::Core::builder();
        builder
            .register(
                BANK_COMMIT_DESCRIPTOR.clone(),
                BankCommitHandler {
                    store: Arc::clone(store),
                },
            )
            .expect("register bank.commit");
        builder
            .register(
                EVENT_GET_DESCRIPTOR.clone(),
                EventGetHandler {
                    store: Arc::clone(store),
                },
            )
            .expect("register event.get");
        builder.build().expect("build")
    }

    #[test]
    fn bank_commit_appends_a_heartbeat_request_event() {
        let (store, _dir) = fresh_store();
        let mut core = fresh_core(&store);

        let heartbeat = SystemHeartbeatRequest::fixture_value();
        let heartbeat_bytes = batpak::encoding::to_bytes(&heartbeat).expect("encode heartbeat");

        let request = BankCommitRequest {
            entity: "test:bank".to_owned(),
            scope: "test-scope".to_owned(),
            kind_category: SystemHeartbeatRequest::KIND.category(),
            kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
            payload_hex: encode_hex_str(&heartbeat_bytes),
        };
        let request_bytes = batpak::encoding::to_bytes(&request).expect("encode request");

        let result = core
            .invoke("bank.commit", request_bytes)
            .expect("invoke bank.commit");

        let ack: BankCommitAck = batpak::encoding::from_bytes(result.output()).expect("decode ack");
        assert_eq!(ack.event_id_hex.len(), 32);
        assert_eq!(ack.content_hash_hex.len(), 64);
        assert_eq!(ack.key_id_hex.len(), 64);
        assert!(ack.sequence >= 1);
    }

    #[test]
    fn event_get_returns_what_bank_commit_wrote() {
        let (store, _dir) = fresh_store();
        let mut core = fresh_core(&store);

        // Append via bank.commit.
        let heartbeat = SystemHeartbeatRequest::fixture_value();
        let heartbeat_bytes = batpak::encoding::to_bytes(&heartbeat).expect("encode heartbeat");
        let request = BankCommitRequest {
            entity: "test:bank".to_owned(),
            scope: "test-scope".to_owned(),
            kind_category: SystemHeartbeatRequest::KIND.category(),
            kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
            payload_hex: encode_hex_str(&heartbeat_bytes),
        };
        let request_bytes = batpak::encoding::to_bytes(&request).expect("encode request");
        let commit_result = core
            .invoke("bank.commit", request_bytes)
            .expect("invoke bank.commit");
        let ack: BankCommitAck =
            batpak::encoding::from_bytes(commit_result.output()).expect("decode ack");

        // Fetch via event.get.
        let get_request = EventGetRequest {
            event_id_hex: ack.event_id_hex.clone(),
        };
        let get_bytes = batpak::encoding::to_bytes(&get_request).expect("encode get");
        let get_result = core
            .invoke("event.get", get_bytes)
            .expect("invoke event.get");
        let event: EventGetAck =
            batpak::encoding::from_bytes(get_result.output()).expect("decode event");

        assert_eq!(event.event_id_hex, ack.event_id_hex);
        assert_eq!(event.entity, "test:bank");
        assert_eq!(event.scope, "test-scope");
        assert_eq!(event.kind_category, SystemHeartbeatRequest::KIND.category());
        assert_eq!(event.kind_type_id, SystemHeartbeatRequest::KIND.type_id());

        // Decoding the returned payload_hex back into the original payload
        // proves the bytes round-trip end-to-end through commit + get.
        let payload_bytes = decode_hex_str(&event.payload_hex).expect("decode payload hex");
        let decoded: SystemHeartbeatRequest =
            batpak::encoding::from_bytes(&payload_bytes).expect("decode payload");
        assert_eq!(decoded, heartbeat);
    }

    fn invoke_expect_err(core: &mut syncbat::Core, op: &str, input: Vec<u8>) -> String {
        match core.invoke(op, input) {
            Ok(_) => panic!("{op} must fail but returned Ok"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn bank_commit_rejects_reserved_kind_category() {
        let (store, _dir) = fresh_store();
        let mut core = fresh_core(&store);
        let request = BankCommitRequest {
            entity: "test:bank".to_owned(),
            scope: "test-scope".to_owned(),
            kind_category: 0x0,
            kind_type_id: 0xA01,
            payload_hex: "81a0".to_owned(),
        };
        let request_bytes = batpak::encoding::to_bytes(&request).expect("encode");
        let msg = invoke_expect_err(&mut core, "bank.commit", request_bytes);
        assert!(
            msg.contains("kind") || msg.contains("invalid_input"),
            "expected error mentioning invalid kind, got {msg:?}"
        );
    }

    #[test]
    fn bank_commit_rejects_invalid_coordinate() {
        let (store, _dir) = fresh_store();
        let mut core = fresh_core(&store);
        let request = BankCommitRequest {
            entity: "".to_owned(),
            scope: "test-scope".to_owned(),
            kind_category: 0xF,
            kind_type_id: 0xA01,
            payload_hex: "81a0".to_owned(),
        };
        let request_bytes = batpak::encoding::to_bytes(&request).expect("encode");
        let msg = invoke_expect_err(&mut core, "bank.commit", request_bytes);
        assert!(
            msg.contains("coordinate") || msg.contains("invalid_input"),
            "expected error mentioning coordinate, got {msg:?}"
        );
    }

    #[test]
    fn event_get_returns_failed_for_unknown_id() {
        let (store, _dir) = fresh_store();
        let mut core = fresh_core(&store);
        let request = EventGetRequest {
            event_id_hex: "deadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
        };
        let request_bytes = batpak::encoding::to_bytes(&request).expect("encode");
        let _ = invoke_expect_err(&mut core, "event.get", request_bytes);
    }

    #[test]
    fn event_get_rejects_malformed_event_id_hex() {
        let (store, _dir) = fresh_store();
        let mut core = fresh_core(&store);
        let request = EventGetRequest {
            event_id_hex: "not-hex".to_owned(),
        };
        let request_bytes = batpak::encoding::to_bytes(&request).expect("encode");
        let _ = invoke_expect_err(&mut core, "event.get", request_bytes);
    }
}
