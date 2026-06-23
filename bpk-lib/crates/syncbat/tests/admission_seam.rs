//! Seam fixtures for the pre-handler admission guard + the `Ctx` receipt-metadata
//! collector (the generic syncbat enhancement consumed by host layers).
//!
//! These are red/green fixtures: each asserts a behavior that a stubbed
//! implementation would break (a guard that is never consulted; metadata that is
//! never drained into the receipt).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use syncbat::{
    AdmissionDecision, Core, Ctx, EffectClass, Handler, HandlerResult, OperationDescriptor,
    ReceiptEnvelope, ReceiptOutcome, ReceiptSink, ReceiptSinkError, RecordedReceipt, RuntimeError,
};

const ECHO: OperationDescriptor = OperationDescriptor::new(
    "echo",
    EffectClass::Inspect,
    "echo.request",
    "echo.ack",
    "receipt.echo.v1",
);

/// Receipt sink that records every envelope it is handed, shared with the test
/// via an `Arc` so the captured receipts can be inspected after dispatch.
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

/// Handler that flips a flag when (and only when) it actually runs, and stamps
/// receipt metadata via the invocation context.
struct StampingHandler {
    ran: Arc<AtomicBool>,
}

impl Handler for StampingHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        self.ran.store(true, Ordering::SeqCst);
        cx.attach_signed_extension("attempt", b"A1".to_vec());
        cx.attach_local_extension("note", b"diag".to_vec());
        Ok(input.to_vec())
    }
}

fn build_core(
    seen: &Arc<Mutex<Vec<ReceiptEnvelope>>>,
    ran: &Arc<AtomicBool>,
    guard: Option<AdmissionDecision>,
) -> Core {
    let mut builder = Core::builder();
    builder
        .register(
            ECHO.clone(),
            StampingHandler {
                ran: Arc::clone(ran),
            },
        )
        .expect("register echo");
    builder.receipt_sink(CapturingSink {
        seen: Arc::clone(seen),
    });
    if let Some(decision) = guard {
        builder.admission_guard(
            move |_d: &OperationDescriptor, _i: &[u8], cx: &mut Ctx<'_>| {
                cx.attach_signed_extension("guard", b"seen".to_vec());
                decision.clone()
            },
        );
    }
    builder.build().expect("build core")
}

#[test]
fn admission_guard_denies_before_handler_and_records_denied_receipt() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ran = Arc::new(AtomicBool::new(false));
    let mut core = build_core(
        &seen,
        &ran,
        Some(AdmissionDecision::deny("policy", "blocked")),
    );

    let result = core.invoke("echo", b"hi".to_vec());

    // The call is denied with a typed denial error...
    match result {
        Err(RuntimeError::Denied {
            name,
            code,
            message,
        }) => {
            assert_eq!(name, "echo");
            assert_eq!(code, "policy");
            assert_eq!(message, "blocked");
        }
        Err(other) => panic_unexpected("denied error", &format!("{other:?}")),
        Ok(_) => panic_unexpected("denied error", "Ok(checkout result)"),
    }

    // ...the handler never ran...
    assert!(
        !ran.load(Ordering::SeqCst),
        "handler must not run when admission denies"
    );

    // ...and a single `Denied` receipt was recorded, carrying the guard's
    // stamped metadata.
    let captured = seen.lock().expect("capture lock");
    assert_eq!(captured.len(), 1, "exactly one receipt on denial");
    let envelope = &captured[0];
    assert_eq!(envelope.descriptor_name, "echo");
    assert!(matches!(envelope.outcome, ReceiptOutcome::Denied { .. }));
    assert_eq!(
        envelope.signed_extensions.get("guard").map(Vec::as_slice),
        Some(b"seen".as_slice()),
        "guard metadata must reach the denied receipt"
    );
}

#[test]
fn handler_attached_metadata_reaches_the_completed_receipt() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ran = Arc::new(AtomicBool::new(false));
    let mut core = build_core(&seen, &ran, None);

    let result = core.invoke("echo", b"hi".to_vec()).expect("invoke echo");
    assert_eq!(result.output(), b"hi");
    assert!(
        ran.load(Ordering::SeqCst),
        "handler should run when admitted"
    );

    let captured = seen.lock().expect("capture lock");
    assert_eq!(captured.len(), 1);
    let envelope = &captured[0];
    assert!(matches!(envelope.outcome, ReceiptOutcome::Completed));
    assert_eq!(
        envelope.signed_extensions.get("attempt").map(Vec::as_slice),
        Some(b"A1".as_slice()),
        "handler signed metadata must reach the receipt's signed drawer"
    );
    assert_eq!(
        envelope.local_extensions.get("note").map(Vec::as_slice),
        Some(b"diag".as_slice()),
        "handler local metadata must reach the receipt's local drawer"
    );
}

#[test]
fn admitting_guard_metadata_merges_with_handler_metadata() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ran = Arc::new(AtomicBool::new(false));
    let mut core = build_core(&seen, &ran, Some(AdmissionDecision::Admit));

    core.invoke("echo", b"hi".to_vec()).expect("invoke echo");
    assert!(ran.load(Ordering::SeqCst));

    let captured = seen.lock().expect("capture lock");
    let envelope = &captured[0];
    assert!(matches!(envelope.outcome, ReceiptOutcome::Completed));
    // Both the admitting guard's pre-handler stamp and the handler's stamp are
    // present on the same receipt.
    assert_eq!(
        envelope.signed_extensions.get("guard").map(Vec::as_slice),
        Some(b"seen".as_slice()),
    );
    assert_eq!(
        envelope.signed_extensions.get("attempt").map(Vec::as_slice),
        Some(b"A1".as_slice()),
    );
}

#[track_caller]
fn panic_unexpected(want: &str, got: &str) {
    assert!(std::hint::black_box(false), "expected {want}, got {got}");
}
