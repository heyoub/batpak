//! PROVES: INV-SYNCBAT-EFFECT-ROW-ENFORCEMENT
//! CATCHES: descriptor/class drift at registration; undeclared observed effects on
//!          every axis; missing backend on every axis; backend rejection before
//!          row observation; and append-only audit bypass. Effects flow only
//!          through `Ctx` capability handles, which perform the effect through
//!          the runtime-owned `EffectBackend` and record it in the same step.

use std::sync::{Arc, Mutex};

use batpak::event::EventKind;
use syncbat::{
    append_target, Core, Ctx, EffectBackend, EffectClass, EffectError, Handler, HandlerError,
    HandlerResult, OperationDescriptor, OperationEffectRow, ReceiptEnvelope, ReceiptSink,
    ReceiptSinkError, RecordedReceipt, RuntimeError,
};

const KIND_ALLOWED: EventKind = EventKind::custom(0xF, 1);
const KIND_OTHER: EventKind = EventKind::custom(0xF, 2);
const EVENT_CAT_ALLOWED: &str = "cat.inventory.v1";
const EVENT_CAT_OTHER: &str = "cat.other.v1";
const PROJECTION_ALLOWED: &str = "proj.orders.v1";
const PROJECTION_OTHER: &str = "proj.other.v1";
const RECEIPT_KIND: &str = "receipt.audit.v1";
const RECEIPT_OTHER: &str = "receipt.other.v1";
const EMIT_PAYLOAD: &[u8] = b"emit-evidence";
const HOST_CONTROL_ALLOWED: &str = "ctrl.alpha";
const HOST_CONTROL_OTHER: &str = "ctrl.beta";

const SCHEMA_IN: &str = "schema.audit.input.v1";
const SCHEMA_OUT: &str = "schema.audit.output.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RejectAxis {
    ReadEvent,
    QueryProjection,
    EmitReceipt,
    HostControl,
    Append,
}

type AppendLog = Arc<Mutex<Vec<(u16, Vec<u8>)>>>;
type StringLog = Arc<Mutex<Vec<String>>>;

#[derive(Clone, Default)]
struct RecordingState {
    appends: AppendLog,
    read_events: StringLog,
    projections: StringLog,
    receipts: StringLog,
    host_controls: StringLog,
}

impl RecordingState {
    fn new() -> Self {
        Self {
            appends: Arc::new(Mutex::new(Vec::new())),
            read_events: Arc::new(Mutex::new(Vec::new())),
            projections: Arc::new(Mutex::new(Vec::new())),
            receipts: Arc::new(Mutex::new(Vec::new())),
            host_controls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[derive(Clone)]
struct RecordingBackend {
    state: RecordingState,
    reject: Option<RejectAxis>,
}

impl RecordingBackend {
    fn new() -> Self {
        Self {
            state: RecordingState::new(),
            reject: None,
        }
    }

    fn rejecting(reject: RejectAxis) -> Self {
        Self {
            state: RecordingState::new(),
            reject: Some(reject),
        }
    }
}

impl EffectBackend for RecordingBackend {
    fn append_event(&mut self, kind: EventKind, payload: &[u8]) -> Result<(), EffectError> {
        if self.reject == Some(RejectAxis::Append) {
            return Err(EffectError::new("backend rejected append"));
        }
        self.state
            .appends
            .lock()
            .expect("append lock")
            .push((kind.as_raw_u16(), payload.to_vec()));
        Ok(())
    }

    fn read_event(&mut self, event_category: &str) -> Result<(), EffectError> {
        if self.reject == Some(RejectAxis::ReadEvent) {
            return Err(EffectError::new("backend rejected read_event"));
        }
        self.state
            .read_events
            .lock()
            .expect("read lock")
            .push(event_category.to_owned());
        Ok(())
    }

    fn query_projection(&mut self, projection_id: &str) -> Result<(), EffectError> {
        if self.reject == Some(RejectAxis::QueryProjection) {
            return Err(EffectError::new("backend rejected query_projection"));
        }
        self.state
            .projections
            .lock()
            .expect("projection lock")
            .push(projection_id.to_owned());
        Ok(())
    }

    fn emit_receipt(&mut self, receipt_kind: &str) -> Result<(), EffectError> {
        if self.reject == Some(RejectAxis::EmitReceipt) {
            return Err(EffectError::new("backend rejected emit_receipt"));
        }
        self.state
            .receipts
            .lock()
            .expect("receipt lock")
            .push(receipt_kind.to_owned());
        Ok(())
    }

    fn use_host_control(&mut self, control: &str) -> Result<(), EffectError> {
        if self.reject == Some(RejectAxis::HostControl) {
            return Err(EffectError::new("backend rejected use_host_control"));
        }
        self.state
            .host_controls
            .lock()
            .expect("host lock")
            .push(control.to_owned());
        Ok(())
    }
}

fn descriptor(
    name: &'static str,
    effect: EffectClass,
    row: OperationEffectRow,
) -> OperationDescriptor {
    OperationDescriptor::new(name, effect, SCHEMA_IN, SCHEMA_OUT, RECEIPT_KIND).with_effect_row(row)
}

fn persist_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    descriptor("audit.append", EffectClass::Persist, row)
}

fn inspect_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    descriptor("audit.inspect", EffectClass::Inspect, row)
}

fn read_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    descriptor("audit.read", EffectClass::Inspect, row)
}

fn projection_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    descriptor("audit.projection", EffectClass::Inspect, row)
}

fn emit_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    descriptor("audit.emit", EffectClass::Emit, row)
}

fn control_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    descriptor("audit.control", EffectClass::Control, row)
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

struct ReadEventHandler {
    category: String,
}

impl Handler for ReadEventHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.event_read_handle()
            .read_event(self.category.clone())
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

struct ProjectionHandler {
    projection_id: String,
}

impl Handler for ProjectionHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.projection_read_handle()
            .query_projection(self.projection_id.clone())
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

struct EmitReceiptHandler {
    receipt_kind: String,
    payload: Vec<u8>,
}

impl Handler for EmitReceiptHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.receipt_emit_handle()
            .emit_receipt(self.receipt_kind.clone(), self.payload.clone())
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

/// Mirror of the runtime-owned LOCAL drawer key stamped by
/// `ReceiptEmitHandle::emit_receipt` (`effect.rs`): the emitted payload lands
/// under this key in the banked invocation receipt.
fn emit_local_key(receipt_kind: &str) -> String {
    format!("syncbat.emit_receipt.{receipt_kind}")
}

struct HostControlHandler {
    control: String,
}

impl Handler for HostControlHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.host_control_handle()
            .use_host_control(self.control.clone())
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

fn build_core(
    descriptor: OperationDescriptor,
    handler: impl Handler + 'static,
    backend: Option<RecordingBackend>,
) -> Core {
    let mut builder = Core::builder();
    builder.register(descriptor, handler).expect("register");
    if let Some(backend) = backend {
        builder.effect_backend(backend);
    }
    builder.receipt_sink(CapturingSink {
        seen: Arc::new(Mutex::new(Vec::new())),
    });
    builder.build().expect("build")
}

#[test]
fn registration_rejects_effect_row_inconsistent_with_class() {
    let descriptor =
        inspect_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_ALLOWED)));
    let mut builder = Core::builder();

    let err = builder
        .register_operation(descriptor)
        .map(|_| ())
        .expect_err("inspect descriptor cannot declare event appends");

    assert!(matches!(err, syncbat::BuildError::InvalidOperation { .. }));
}

#[test]
fn declared_append_through_handle_completes_and_is_performed() {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        persist_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_ALLOWED))),
        AppendHandler { kind: KIND_ALLOWED },
        Some(backend),
    );

    let payload = b"payload".to_vec();
    let result = core
        .invoke("audit.append", payload.clone())
        .expect("invoke");
    assert_eq!(result.output(), payload.as_slice());
    assert_eq!(
        state.appends.lock().expect("append lock").clone(),
        vec![(KIND_ALLOWED.as_raw_u16(), payload)]
    );
}

#[test]
fn dispatch_denies_append_outside_declared_row() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = build_core(
        persist_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_ALLOWED))),
        AppendHandler { kind: KIND_OTHER },
        Some(RecordingBackend::new()),
    );
    let err = match core.invoke("audit.append", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: appending an undeclared event kind must be denied",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Denied { ref name, ref code, .. }
            if name == "audit.append" && code == "effect.violation"
    ));
    Ok(())
}

#[test]
fn append_without_a_bound_backend_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = build_core(
        persist_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_ALLOWED))),
        AppendHandler { kind: KIND_ALLOWED },
        None,
    );
    let err = match core.invoke("audit.append", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: appending without a bound backend must fail closed",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.append"
    ));
    Ok(())
}

#[test]
fn declared_read_event_through_handle_completes_and_is_performed() {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        read_descriptor(OperationEffectRow::new().reads_event(EVENT_CAT_ALLOWED)),
        ReadEventHandler {
            category: EVENT_CAT_ALLOWED.to_owned(),
        },
        Some(backend),
    );
    core.invoke("audit.read", b"payload".to_vec())
        .expect("invoke");
    assert_eq!(
        state.read_events.lock().expect("read lock").clone(),
        vec![EVENT_CAT_ALLOWED.to_owned()]
    );
}

#[test]
fn read_event_without_a_bound_backend_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = build_core(
        read_descriptor(OperationEffectRow::new().reads_event(EVENT_CAT_ALLOWED)),
        ReadEventHandler {
            category: EVENT_CAT_ALLOWED.to_owned(),
        },
        None,
    );
    let err = match core.invoke("audit.read", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: read_event without a bound backend must fail closed",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.read"
    ));
    Ok(())
}

#[test]
fn read_event_backend_reject_does_not_record_observation() -> Result<(), Box<dyn std::error::Error>>
{
    let backend = RecordingBackend::rejecting(RejectAxis::ReadEvent);
    let state = backend.state.clone();
    let mut core = build_core(
        read_descriptor(OperationEffectRow::new().reads_event(EVENT_CAT_ALLOWED)),
        ReadEventHandler {
            category: EVENT_CAT_ALLOWED.to_owned(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.read", b"payload".to_vec()) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: backend rejection must fail the handler").into(),
            )
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.read"
    ));
    assert!(
        state.read_events.lock().expect("read lock").is_empty(),
        "backend rejection must not record an observed read"
    );
    Ok(())
}

#[test]
fn dispatch_denies_read_event_outside_declared_row() -> Result<(), Box<dyn std::error::Error>> {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        read_descriptor(OperationEffectRow::new().reads_event(EVENT_CAT_ALLOWED)),
        ReadEventHandler {
            category: EVENT_CAT_OTHER.to_owned(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.read", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: undeclared read_event must be denied at checkout",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Denied { ref name, ref code, .. }
            if name == "audit.read" && code == "effect.violation"
    ));
    assert_eq!(
        state.read_events.lock().expect("read lock").clone(),
        vec![EVENT_CAT_OTHER.to_owned()]
    );
    Ok(())
}

#[test]
fn declared_projection_query_through_handle_completes_and_is_performed() {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        projection_descriptor(OperationEffectRow::new().queries_projection(PROJECTION_ALLOWED)),
        ProjectionHandler {
            projection_id: PROJECTION_ALLOWED.to_owned(),
        },
        Some(backend),
    );
    core.invoke("audit.projection", b"payload".to_vec())
        .expect("invoke");
    assert_eq!(
        state.projections.lock().expect("projection lock").clone(),
        vec![PROJECTION_ALLOWED.to_owned()]
    );
}

#[test]
fn query_projection_without_a_bound_backend_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = build_core(
        projection_descriptor(OperationEffectRow::new().queries_projection(PROJECTION_ALLOWED)),
        ProjectionHandler {
            projection_id: PROJECTION_ALLOWED.to_owned(),
        },
        None,
    );
    let err = match core.invoke("audit.projection", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: query_projection without a bound backend must fail closed",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.projection"
    ));
    Ok(())
}

#[test]
fn query_projection_backend_reject_does_not_record_observation(
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = RecordingBackend::rejecting(RejectAxis::QueryProjection);
    let state = backend.state.clone();
    let mut core = build_core(
        projection_descriptor(OperationEffectRow::new().queries_projection(PROJECTION_ALLOWED)),
        ProjectionHandler {
            projection_id: PROJECTION_ALLOWED.to_owned(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.projection", b"payload".to_vec()) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: backend rejection must fail the handler").into(),
            )
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.projection"
    ));
    assert!(
        state
            .projections
            .lock()
            .expect("projection lock")
            .is_empty(),
        "backend rejection must not record an observed projection query"
    );
    Ok(())
}

#[test]
fn dispatch_denies_projection_query_outside_declared_row() -> Result<(), Box<dyn std::error::Error>>
{
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        projection_descriptor(OperationEffectRow::new().queries_projection(PROJECTION_ALLOWED)),
        ProjectionHandler {
            projection_id: PROJECTION_OTHER.to_owned(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.projection", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: undeclared projection query must be denied at checkout",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Denied { ref name, ref code, .. }
            if name == "audit.projection" && code == "effect.violation"
    ));
    assert_eq!(
        state.projections.lock().expect("projection lock").clone(),
        vec![PROJECTION_OTHER.to_owned()]
    );
    Ok(())
}

#[test]
fn declared_receipt_emit_through_handle_completes_and_is_performed() {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        emit_descriptor(OperationEffectRow::new().emits_receipt(RECEIPT_KIND)),
        EmitReceiptHandler {
            receipt_kind: RECEIPT_KIND.to_owned(),
            payload: EMIT_PAYLOAD.to_vec(),
        },
        Some(backend),
    );
    let result = core
        .invoke("audit.emit", b"payload".to_vec())
        .expect("invoke");
    assert_eq!(
        state.receipts.lock().expect("receipt lock").clone(),
        vec![RECEIPT_KIND.to_owned()]
    );
    // Option B: the emitted payload rides the handle -> `ReceiptMetadata` path
    // into the runtime's single banked invocation receipt, stamped under the
    // runtime-owned LOCAL drawer key `syncbat.emit_receipt.{kind}`. Drop the
    // stamp-after-perform step in `emit_receipt` and this drawer entry is absent.
    let recorded = result
        .recorded_receipt()
        .expect("banked invocation receipt");
    assert_eq!(
        recorded
            .envelope
            .local_extensions
            .get(&emit_local_key(RECEIPT_KIND))
            .map(Vec::as_slice),
        Some(EMIT_PAYLOAD),
        "banked receipt must carry the emitted payload in its local drawer"
    );
}

#[test]
fn emit_receipt_without_a_bound_backend_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = build_core(
        emit_descriptor(OperationEffectRow::new().emits_receipt(RECEIPT_KIND)),
        EmitReceiptHandler {
            receipt_kind: RECEIPT_KIND.to_owned(),
            payload: EMIT_PAYLOAD.to_vec(),
        },
        None,
    );
    let err = match core.invoke("audit.emit", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: emit_receipt without a bound backend must fail closed",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.emit"
    ));
    Ok(())
}

#[test]
fn emit_receipt_backend_reject_does_not_record_observation(
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = RecordingBackend::rejecting(RejectAxis::EmitReceipt);
    let state = backend.state.clone();
    let mut core = build_core(
        emit_descriptor(OperationEffectRow::new().emits_receipt(RECEIPT_KIND)),
        EmitReceiptHandler {
            receipt_kind: RECEIPT_KIND.to_owned(),
            payload: EMIT_PAYLOAD.to_vec(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.emit", b"payload".to_vec()) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: backend rejection must fail the handler").into(),
            )
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.emit"
    ));
    assert!(
        state.receipts.lock().expect("receipt lock").is_empty(),
        "backend rejection must not record an observed receipt emit"
    );
    Ok(())
}

#[test]
fn dispatch_denies_receipt_emit_outside_declared_row() -> Result<(), Box<dyn std::error::Error>> {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        emit_descriptor(OperationEffectRow::new().emits_receipt(RECEIPT_KIND)),
        EmitReceiptHandler {
            receipt_kind: RECEIPT_OTHER.to_owned(),
            payload: EMIT_PAYLOAD.to_vec(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.emit", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: undeclared receipt emit must be denied at checkout",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Denied { ref name, ref code, .. }
            if name == "audit.emit" && code == "effect.violation"
    ));
    assert_eq!(
        state.receipts.lock().expect("receipt lock").clone(),
        vec![RECEIPT_OTHER.to_owned()]
    );
    Ok(())
}

#[test]
fn declared_host_control_through_handle_completes_and_is_performed() {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        control_descriptor(OperationEffectRow::new().uses_host_control(HOST_CONTROL_ALLOWED)),
        HostControlHandler {
            control: HOST_CONTROL_ALLOWED.to_owned(),
        },
        Some(backend),
    );
    core.invoke("audit.control", b"payload".to_vec())
        .expect("invoke");
    assert_eq!(
        state.host_controls.lock().expect("host lock").clone(),
        vec![HOST_CONTROL_ALLOWED.to_owned()]
    );
}

#[test]
fn use_host_control_without_a_bound_backend_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = build_core(
        control_descriptor(OperationEffectRow::new().uses_host_control(HOST_CONTROL_ALLOWED)),
        HostControlHandler {
            control: HOST_CONTROL_ALLOWED.to_owned(),
        },
        None,
    );
    let err = match core.invoke("audit.control", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: use_host_control without a bound backend must fail closed",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.control"
    ));
    Ok(())
}

#[test]
fn use_host_control_backend_reject_does_not_record_observation(
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = RecordingBackend::rejecting(RejectAxis::HostControl);
    let state = backend.state.clone();
    let mut core = build_core(
        control_descriptor(OperationEffectRow::new().uses_host_control(HOST_CONTROL_ALLOWED)),
        HostControlHandler {
            control: HOST_CONTROL_ALLOWED.to_owned(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.control", b"payload".to_vec()) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: backend rejection must fail the handler").into(),
            )
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "audit.control"
    ));
    assert!(
        state.host_controls.lock().expect("host lock").is_empty(),
        "backend rejection must not record host-control use"
    );
    Ok(())
}

#[test]
fn dispatch_denies_host_control_without_declared_authority(
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        inspect_descriptor(OperationEffectRow::new()),
        HostControlHandler {
            control: HOST_CONTROL_ALLOWED.to_owned(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.inspect", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: undeclared host control must be denied at checkout",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Denied { ref name, ref code, .. }
            if name == "audit.inspect" && code == "effect.violation"
    ));
    assert_eq!(
        state.host_controls.lock().expect("host lock").clone(),
        vec![HOST_CONTROL_ALLOWED.to_owned()]
    );
    Ok(())
}

#[test]
fn dispatch_denies_host_control_outside_declared_row() -> Result<(), Box<dyn std::error::Error>> {
    // Subset check on the host-control axis: the op declares only `ctrl.alpha`
    // but the handler performs `ctrl.beta` through the bound backend. The effect
    // is performed and observed, then checkout fails closed because the observed
    // control-id is not a subset of the declared row (mirrors the read/append
    // axes). This is the fixture that would go GREEN if the axis were left a bare
    // bool that records nothing and can't be subset-checked.
    let backend = RecordingBackend::new();
    let state = backend.state.clone();
    let mut core = build_core(
        control_descriptor(OperationEffectRow::new().uses_host_control(HOST_CONTROL_ALLOWED)),
        HostControlHandler {
            control: HOST_CONTROL_OTHER.to_owned(),
        },
        Some(backend),
    );
    let err = match core.invoke("audit.control", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: an undeclared host control id must be denied at checkout",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Denied { ref name, ref code, .. }
            if name == "audit.control" && code == "effect.violation"
    ));
    assert_eq!(
        state.host_controls.lock().expect("host lock").clone(),
        vec![HOST_CONTROL_OTHER.to_owned()]
    );
    Ok(())
}
