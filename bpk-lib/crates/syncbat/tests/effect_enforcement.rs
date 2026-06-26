//! PROVES: INV-SYNCBAT-EFFECT-ROW-ENFORCEMENT
//! CATCHES: descriptor/class drift at registration; an operation appending an
//!          event kind outside its declared row; and an operation appending
//!          without the runtime-owned backend bound. Appends flow only through
//!          the `Ctx` capability handle, which performs the append through the
//!          runtime's `EffectBackend` and records it in the same step, so the
//!          observed row is authoritative — an operation cannot append an event
//!          the runtime did not mediate, and `checkout` fails closed when the
//!          observed appends are not a subset of the declared appends.
//! SEEDED: fixed descriptors, kinds, and a recording backend.

use std::sync::{Arc, Mutex};

use batpak::event::EventKind;
use syncbat::{
    append_target, Core, Ctx, EffectBackend, EffectClass, EffectError, Handler, HandlerError,
    HandlerResult, OperationDescriptor, OperationEffectRow, ReceiptEnvelope, ReceiptOutcome,
    ReceiptSink, ReceiptSinkError, RecordedReceipt, RuntimeError,
};

const KIND_ALLOWED: EventKind = EventKind::custom(0xF, 1);
const KIND_OTHER: EventKind = EventKind::custom(0xF, 2);

fn persist_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    OperationDescriptor::new(
        "audit.append",
        EffectClass::Persist,
        "schema.audit.input.v1",
        "schema.audit.output.v1",
        "receipt.audit.v1",
    )
    .with_effect_row(row)
}

fn inspect_descriptor(row: OperationEffectRow) -> OperationDescriptor {
    OperationDescriptor::new(
        "audit.inspect",
        EffectClass::Inspect,
        "schema.audit.input.v1",
        "schema.audit.output.v1",
        "receipt.audit.v1",
    )
    .with_effect_row(row)
}

/// Shared log of `(kind-bits, payload)` appends performed by the backend.
type AppendLog = Arc<Mutex<Vec<(u16, Vec<u8>)>>>;

/// Records every append it performs, standing in for a store-backed backend.
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

/// Appends one event of `kind` through the Ctx handle — the only path it has.
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
    let appended = Arc::new(Mutex::new(Vec::new()));
    let descriptor =
        persist_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_ALLOWED)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut builder = Core::builder();
    builder
        .register(descriptor, AppendHandler { kind: KIND_ALLOWED })
        .expect("register");
    builder.effect_backend(RecordingBackend {
        appended: Arc::clone(&appended),
    });
    builder.receipt_sink(CapturingSink {
        seen: Arc::clone(&seen),
    });
    let mut core = builder.build().expect("build");

    let result = core
        .invoke("audit.append", b"payload".to_vec())
        .expect("invoke");
    assert_eq!(result.output(), b"payload");
    // The append actually flowed through the backend, not a notepad.
    let appended = appended.lock().expect("append lock").clone();
    assert_eq!(
        appended,
        vec![(KIND_ALLOWED.as_raw_u16(), b"payload".to_vec())]
    );
    let envelopes = seen.lock().expect("capture lock").clone();
    assert!(matches!(envelopes[0].outcome, ReceiptOutcome::Completed));
}

#[test]
fn dispatch_denies_append_outside_declared_row() -> Result<(), Box<dyn std::error::Error>> {
    // Declares it may append KIND_ALLOWED, but the handler appends KIND_OTHER.
    let descriptor =
        persist_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_ALLOWED)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut builder = Core::builder();
    builder
        .register(descriptor, AppendHandler { kind: KIND_OTHER })
        .expect("register");
    builder.effect_backend(RecordingBackend::default());
    builder.receipt_sink(CapturingSink {
        seen: Arc::clone(&seen),
    });
    let mut core = builder.build().expect("build");

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
    let envelopes = seen.lock().expect("capture lock").clone();
    assert!(matches!(
        envelopes[0].outcome,
        ReceiptOutcome::Denied { .. }
    ));
    Ok(())
}

#[test]
fn append_without_a_bound_backend_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    // No effect backend configured: the runtime owns no store path for this
    // invocation, so the handler's append must fail closed rather than reach a
    // store the runtime did not mediate.
    let descriptor =
        persist_descriptor(OperationEffectRow::new().appends_event(append_target(KIND_ALLOWED)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut builder = Core::builder();
    builder
        .register(descriptor, AppendHandler { kind: KIND_ALLOWED })
        .expect("register");
    builder.receipt_sink(CapturingSink {
        seen: Arc::clone(&seen),
    });
    let mut core = builder.build().expect("build");

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
