//! Append-time schema validation on hostbat-mediated effect backends.

use std::sync::{Arc, Mutex};

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{Open, Store, StoreConfig};
use syncbat::{
    append_target, Ctx, EffectBackend, EffectClass, EffectError, Handler, HandlerError,
    HandlerResult, OperationDescriptor, OperationEffectRow, ReceiptEnvelope, ReceiptSink,
    ReceiptSinkError, RecordedReceipt, RuntimeError,
};

use crate::module::HostModule;
use crate::schema::{GoldenVector, SchemaDescriptor, SchemaId, SchemaRole, SchemaVersion};
use crate::HostBuilder;

const KIND_BOUND: EventKind = EventKind::custom(0xF, 1);
const KIND_UNBOUND: EventKind = EventKind::custom(0xF, 2);

fn canonical_bytes(value: &str) -> Vec<u8> {
    batpak::canonical::to_bytes(&value).expect("canonical fixture encodes")
}

fn persist_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    OperationDescriptor::new(
        "audit.append",
        EffectClass::Persist,
        "schema.in.v1",
        "schema.out.v1",
        "receipt.v1",
    )
    .with_effect_row(row)
}

type AppendLog = Arc<Mutex<Vec<(u16, Vec<u8>)>>>;

#[derive(Clone, Default)]
struct RecordingBackend {
    appended: AppendLog,
}

impl EffectBackend for RecordingBackend {
    fn append_event(&mut self, kind: EventKind, payload: &[u8]) -> Result<(), EffectError> {
        self.appended
            .lock()
            .expect("append lock")
            .push((kind.as_raw_u16(), payload.to_vec()));
        Ok(())
    }
}

struct AppendHandler {
    kind: EventKind,
}

impl Handler for AppendHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.event_append_handle()
            .append_event(self.kind, input)
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

#[derive(Clone)]
struct CapturingSink {
    seen: Arc<Mutex<Vec<ReceiptEnvelope>>>,
}

impl ReceiptSink for CapturingSink {
    fn record_receipt(
        &self,
        envelope: &ReceiptEnvelope,
    ) -> Result<RecordedReceipt, ReceiptSinkError> {
        self.seen
            .lock()
            .expect("capture lock")
            .push(envelope.clone());
        Ok(RecordedReceipt::new(envelope.clone()))
    }
}

fn schema_with_role(id: &str, role: SchemaRole, bytes: &[u8]) -> SchemaDescriptor {
    SchemaDescriptor::new(
        SchemaId::new(id).expect("id"),
        SchemaVersion(1),
        role,
        vec![GoldenVector::new("c", bytes.to_vec())],
    )
    .expect("descriptor")
}

fn append_host_module() -> HostModule {
    HostModule::builder("mod.a", 1)
        .operation(
            persist_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_BOUND))),
            AppendHandler { kind: KIND_BOUND },
        )
        .expect("operation")
        .schema(schema_with_role(
            "schema.in.v1",
            SchemaRole::OperationInput,
            &canonical_bytes("default-in"),
        ))
        .expect("input schema")
        .schema(schema_with_role(
            "schema.out.v1",
            SchemaRole::OperationOutput,
            &canonical_bytes("default-out"),
        ))
        .expect("output schema")
        .schema(schema_with_role(
            "receipt.v1",
            SchemaRole::ReceiptPayload,
            &canonical_bytes("default-receipt"),
        ))
        .expect("receipt schema")
        .schema(schema_with_role(
            "event.payload.v1",
            SchemaRole::EventPayload,
            &canonical_bytes("valid-event"),
        ))
        .expect("event schema")
        .bind_event_payload(KIND_BOUND, "event.payload.v1")
        .expect("binding")
        .build()
        .expect("module")
}

#[test]
fn bound_payload_append_reaches_backend() {
    let appended = Arc::new(Mutex::new(Vec::new()));
    let mut host = HostBuilder::new()
        .mount(append_host_module())
        .expect("mount")
        .effect_backend(RecordingBackend {
            appended: Arc::clone(&appended),
        })
        .receipt_sink(CapturingSink {
            seen: Arc::new(Mutex::new(Vec::new())),
        })
        .build()
        .expect("build");
    let payload = canonical_bytes("valid-event");
    let result = host
        .invoke("audit.append", payload.clone())
        .expect("invoke");
    assert_eq!(result.output(), payload.as_slice());
    let appended = appended.lock().expect("append lock").clone();
    assert_eq!(appended, vec![(KIND_BOUND.as_raw_u16(), payload)]);
}

#[test]
fn unbound_event_kind_fails_before_backend() -> Result<(), Box<dyn std::error::Error>> {
    let appended = Arc::new(Mutex::new(Vec::new()));
    let module = HostModule::builder("mod.a", 1)
        .operation(
            persist_descriptor(
                OperationEffectRow::new().appends_event(append_target(KIND_UNBOUND)),
            ),
            AppendHandler { kind: KIND_UNBOUND },
        )
        .expect("operation")
        .schema(schema_with_role(
            "schema.in.v1",
            SchemaRole::OperationInput,
            &canonical_bytes("default-in"),
        ))
        .expect("input schema")
        .schema(schema_with_role(
            "schema.out.v1",
            SchemaRole::OperationOutput,
            &canonical_bytes("default-out"),
        ))
        .expect("output schema")
        .schema(schema_with_role(
            "receipt.v1",
            SchemaRole::ReceiptPayload,
            &canonical_bytes("default-receipt"),
        ))
        .expect("receipt schema")
        .build()
        .expect("module");
    let mut host = HostBuilder::new()
        .mount(module)
        .expect("mount")
        .effect_backend(RecordingBackend {
            appended: Arc::clone(&appended),
        })
        .receipt_sink(CapturingSink {
            seen: Arc::new(Mutex::new(Vec::new())),
        })
        .build()
        .expect("build");
    let err = match host.invoke("audit.append", canonical_bytes("any")) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: unbound event kind must fail closed on host-mediated append",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.append"
    ));
    assert!(
        appended.lock().expect("append lock").is_empty(),
        "validation must fail before the inner backend performs the append",
    );
    Ok(())
}

#[test]
fn non_canonical_payload_fails_before_backend() -> Result<(), Box<dyn std::error::Error>> {
    let appended = Arc::new(Mutex::new(Vec::new()));
    let mut host = HostBuilder::new()
        .mount(append_host_module())
        .expect("mount")
        .effect_backend(RecordingBackend {
            appended: Arc::clone(&appended),
        })
        .receipt_sink(CapturingSink {
            seen: Arc::new(Mutex::new(Vec::new())),
        })
        .build()
        .expect("build");
    // 0xc1 is the byte MessagePack reserves as never-valid, so it cannot decode
    // under the canonical encoding — the fail-closed canonical-decode check must
    // reject it before the backend ever appends.
    let err = match host.invoke("audit.append", vec![0xc1]) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: non-canonical msgpack payload must fail before store append",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.append"
    ));
    assert!(
        appended.lock().expect("append lock").is_empty(),
        "schema validation must fail before the inner backend performs the append",
    );
    Ok(())
}

#[test]
fn raw_store_append_remains_unvalidated_by_hostbat() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::<Open>::open(StoreConfig::new(dir.path())).expect("open store");
    let coordinate = Coordinate::new("entity", "scope").expect("coordinate");
    let _receipt = store
        .append(
            &coordinate,
            KIND_UNBOUND,
            &canonical_bytes("raw-unvalidated"),
        )
        .expect("raw append");
    let _host = HostBuilder::new()
        .mount(append_host_module())
        .expect("mount")
        .build()
        .expect("host build without effect backend succeeds");
    Ok(())
}
