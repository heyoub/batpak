//! PROVES: INV-SYNCBAT-CAPABILITY-GRANT-ENFORCEMENT
//! CATCHES: a declared capability token checked against nothing. This is the
//!          declared-authority companion to the observed-effect subset check:
//!          checkout fails closed when a dispatched operation declares a required
//!          capability token (via `OperationEffectRow::requires_capability` or
//!          the `#[operation]` macro) that the Core was not granted. The SAME
//!          operation succeeds once the Core is granted the token through
//!          `CoreBuilder::grant_capability` / `CoreBuilder::grant_capabilities`.
//!          Effect-axis tokens auto-declared by the effect builders are ambient
//!          (mediated by the observed-effect subset check) and never need an
//!          explicit grant, so an operation with no extra capability tokens still
//!          runs on a Core granted nothing.

use std::sync::{Arc, Mutex};

use syncbat::{
    Core, Ctx, EffectClass, Handler, HandlerResult, OperationDescriptor, OperationEffectRow,
    ReceiptEnvelope, ReceiptOutcome, ReceiptSink, ReceiptSinkError, RecordedReceipt, RuntimeError,
};

const REQUIRED_CAP: &str = "cap.elevated.v1";
const SCHEMA_IN: &str = "schema.cap.input.v1";
const SCHEMA_OUT: &str = "schema.cap.output.v1";
const RECEIPT_KIND: &str = "receipt.cap.v1";

struct EchoHandler;

impl Handler for EchoHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        Ok(input.to_vec())
    }
}

/// An inspect operation that declares a free-form required capability token but
/// touches no effect handle, so the only thing standing between it and a
/// successful checkout is the runtime capability grant gate.
fn privileged_descriptor() -> OperationDescriptor {
    OperationDescriptor::new(
        "cap.op",
        EffectClass::Inspect,
        SCHEMA_IN,
        SCHEMA_OUT,
        RECEIPT_KIND,
    )
    .with_effect_row(OperationEffectRow::new().requires_capability(REQUIRED_CAP))
}

#[derive(Clone)]
struct CapturingSink {
    seen: Arc<Mutex<Vec<ReceiptEnvelope>>>,
}

impl CapturingSink {
    fn new() -> Self {
        Self {
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
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
fn dispatch_denies_operation_requiring_an_ungranted_capability(
) -> Result<(), Box<dyn std::error::Error>> {
    let sink = CapturingSink::new();
    let mut builder = Core::builder();
    builder
        .register(privileged_descriptor(), EchoHandler)
        .expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("build");

    let err = match core.invoke("cap.op", b"payload".to_vec()) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: an operation requiring an ungranted capability must be denied",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(
        matches!(
            err,
            RuntimeError::Denied { ref name, ref code, .. }
                if name == "cap.op" && code == "capability.denied"
        ),
        "expected a capability.denied denial, got {err:?}"
    );

    // The denial emits the same fail-closed receipt shape as an observed-effect
    // violation: a single `Denied` runtime receipt naming the missing token.
    let receipts = sink.seen.lock().expect("capture lock");
    assert_eq!(receipts.len(), 1, "exactly one denial receipt is recorded");
    let outcome = &receipts[0].outcome;
    assert!(
        matches!(
            outcome,
            ReceiptOutcome::Denied { code, message }
                if code == "capability.denied" && message.contains(REQUIRED_CAP)
        ),
        "denial must record a capability.denied outcome naming the token, got {outcome:?}"
    );
    Ok(())
}

#[test]
fn grant_capability_admits_the_same_operation() {
    // The SAME descriptor that was denied above now runs, because the Core was
    // granted the token. This is the non-vacuous other half of the pair.
    let mut builder = Core::builder();
    builder
        .register(privileged_descriptor(), EchoHandler)
        .expect("register");
    builder.grant_capability(REQUIRED_CAP);
    builder.without_receipts();
    let mut core = builder.build().expect("build");

    let payload = b"payload".to_vec();
    let result = core
        .invoke("cap.op", payload.clone())
        .expect("granted invoke succeeds");
    assert_eq!(result.output(), payload.as_slice());
}

#[test]
fn grant_capabilities_admits_the_same_operation() {
    // Grant the required token (plus an unrelated one) through the iterator
    // setter; the gate is satisfied by membership, not by the extra grant.
    let mut builder = Core::builder();
    builder
        .register(privileged_descriptor(), EchoHandler)
        .expect("register");
    builder.grant_capabilities([REQUIRED_CAP.to_owned(), "cap.other.v1".to_owned()]);
    builder.without_receipts();
    let mut core = builder.build().expect("build");

    let payload = b"granted".to_vec();
    let result = core
        .invoke("cap.op", payload.clone())
        .expect("granted invoke succeeds");
    assert_eq!(result.output(), payload.as_slice());
}

#[test]
fn operation_without_extra_capabilities_runs_on_an_ungranted_core() {
    // Safe default: an operation that declares no extra capability tokens runs
    // on a Core granted nothing, so the new gate does not regress ordinary ops.
    let descriptor = OperationDescriptor::new(
        "cap.plain",
        EffectClass::Inspect,
        SCHEMA_IN,
        SCHEMA_OUT,
        RECEIPT_KIND,
    );
    let mut builder = Core::builder();
    builder.register(descriptor, EchoHandler).expect("register");
    builder.without_receipts();
    let mut core = builder.build().expect("build");

    let payload = b"plain".to_vec();
    let result = core
        .invoke("cap.plain", payload.clone())
        .expect("plain invoke succeeds");
    assert_eq!(result.output(), payload.as_slice());
}
