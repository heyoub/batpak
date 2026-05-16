#![allow(clippy::panic)]

use std::cell::RefCell;
use std::rc::Rc;

use syncbat::{
    CheckoutFrame, Core, EffectClass, Handler, HandlerError, HandlerResult, Module,
    OperationDescriptor, ReceiptEnvelope, ReceiptHash, ReceiptHashPolicy, ReceiptOutcome,
    ReceiptSink, ReceiptSinkError, RecordedReceipt, Register, RuntimeError,
};

const ECHO: OperationDescriptor = OperationDescriptor::new(
    "echo",
    EffectClass::Compute,
    "schema.echo.input.v1",
    "schema.echo.output.v1",
    "receipt.echo.v1",
);

const PING: OperationDescriptor = OperationDescriptor::new(
    "ping",
    EffectClass::Inspect,
    "schema.ping.input.v1",
    "schema.ping.output.v1",
    "receipt.ping.v1",
);

struct EchoHandler;

impl Handler for EchoHandler {
    fn handle(&mut self, input: &[u8], cx: &mut syncbat::Cx<'_>) -> HandlerResult {
        assert_eq!(cx.descriptor().name(), "echo");
        let mut out = Vec::from(input);
        out.extend_from_slice(b":ok");
        Ok(out)
    }
}

struct FailingHandler;

impl Handler for FailingHandler {
    fn handle(&mut self, _input: &[u8], _cx: &mut syncbat::Cx<'_>) -> HandlerResult {
        Err(HandlerError::failed("boom"))
    }
}

#[derive(Clone, Default)]
struct RecordingReceiptSink {
    envelopes: Rc<RefCell<Vec<ReceiptEnvelope>>>,
}

impl RecordingReceiptSink {
    fn envelopes(&self) -> Vec<ReceiptEnvelope> {
        self.envelopes.borrow().clone()
    }
}

impl ReceiptSink for RecordingReceiptSink {
    fn record_receipt(
        &self,
        envelope: &ReceiptEnvelope,
    ) -> Result<RecordedReceipt, ReceiptSinkError> {
        self.envelopes.borrow_mut().push(envelope.clone());
        Ok(RecordedReceipt::new(envelope.clone()))
    }
}

struct FailingReceiptSink;

impl ReceiptSink for FailingReceiptSink {
    fn record_receipt(
        &self,
        _envelope: &ReceiptEnvelope,
    ) -> Result<RecordedReceipt, ReceiptSinkError> {
        Err(ReceiptSinkError::new("sink down"))
    }
}

fn test_hash(bytes: &[u8]) -> ReceiptHash {
    let mut hash = [0_u8; 32];
    for (index, byte) in bytes.iter().enumerate() {
        hash[index % 32] = hash[index % 32]
            .wrapping_add(*byte)
            .wrapping_add(u8::try_from(index % 251).expect("bounded index"));
    }
    hash[31] = u8::try_from(bytes.len() % 256).expect("bounded length");
    hash
}

#[test]
fn register_and_cache_lookup_descriptor_by_name() {
    let mut register = Register::new();
    register
        .insert_operation(ECHO)
        .expect("operation inserts once");
    register
        .insert_operation(PING)
        .expect("operation inserts once");

    let names = register.names().collect::<Vec<_>>();
    assert_eq!(names, vec!["echo", "ping"]);
    let descriptor_names = register
        .descriptors()
        .map(|(name, descriptor)| (name, descriptor.name()))
        .collect::<Vec<_>>();
    assert_eq!(descriptor_names, vec![("echo", "echo"), ("ping", "ping")]);

    let cache = syncbat::CacheRegister::from_register(&register);
    assert!(cache.contains_operation("echo"));
    assert_eq!(
        cache.operation("echo").expect("descriptor").receipt_kind(),
        "receipt.echo.v1"
    );
    assert_eq!(
        cache.descriptor("ping").expect("descriptor").effect,
        EffectClass::Inspect
    );
    assert_eq!(cache.names().collect::<Vec<_>>(), vec!["echo", "ping"]);
}

#[test]
fn cache_register_is_rebuilt_projection_over_register() {
    let mut register = Register::new();
    register
        .insert_operation(ECHO)
        .expect("operation inserts once");

    {
        let cache = syncbat::CacheRegister::from_register(&register);
        assert_eq!(cache.names().collect::<Vec<_>>(), vec!["echo"]);
    }

    register
        .insert_operation(PING)
        .expect("operation inserts once");
    let cache = syncbat::CacheRegister::from_register(&register);
    assert_eq!(cache.names().collect::<Vec<_>>(), vec!["echo", "ping"]);
}

#[test]
fn builder_mounts_module_data_and_invokes_handler() {
    let mut module = Module::new("test");
    module
        .insert_operation(ECHO)
        .expect("operation inserts once");

    let mut builder = Core::builder();
    builder.mount(module).expect("module mounts");
    builder
        .register_handler("echo", EchoHandler)
        .expect("handler registers");
    let mut core = builder.build().expect("core builds");

    let result = core.invoke("echo", b"hello".to_vec()).expect("invoke");
    assert_eq!(result.descriptor().name(), "echo");
    assert_eq!(result.output().as_slice(), b"hello:ok");
    assert!(result.recorded_receipt().is_none());
}

#[test]
fn builder_rejects_missing_handler() {
    let mut builder = Core::builder();
    builder.register_operation(ECHO).expect("register");
    let err = match builder.build() {
        Ok(_) => panic!("expected build to reject missing handler"),
        Err(error) => error,
    };

    assert!(matches!(err, syncbat::BuildError::MissingHandler { name } if name == "echo"));
}

#[test]
fn invoke_maps_handler_failure_to_runtime_error() {
    let mut builder = Core::builder();
    builder.register(ECHO, FailingHandler).expect("register");
    let mut core = builder.build().expect("core builds");

    let err = match core.invoke("echo", Vec::new()) {
        Ok(_) => panic!("expected handler failure"),
        Err(error) => error,
    };

    assert!(
        matches!(
            err,
            RuntimeError::Handler { ref name, ref code, ref message }
                if name == "echo" && code == "failed" && message == "boom"
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn completed_receipt_is_recorded_once() {
    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("core builds");

    let result = core.invoke("echo", b"hello".to_vec()).expect("invoke");

    let recorded = result.recorded_receipt().expect("recorded receipt");
    assert_eq!(recorded.envelope.descriptor_name, "echo");
    assert_eq!(recorded.envelope.receipt_kind, "receipt.echo.v1");
    assert_eq!(recorded.envelope.outcome, ReceiptOutcome::Completed);
    assert_eq!(sink.envelopes(), vec![recorded.envelope.clone()]);
}

#[test]
fn failed_receipt_is_recorded_once() {
    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, FailingHandler).expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("core builds");

    let err = match core.invoke("echo", b"bad".to_vec()) {
        Ok(_) => panic!("expected handler failure"),
        Err(error) => error,
    };

    assert!(
        matches!(
            err,
            RuntimeError::Handler { ref name, ref code, ref message }
                if name == "echo" && code == "failed" && message == "boom"
        ),
        "unexpected error: {err:?}"
    );
    let envelopes = sink.envelopes();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].descriptor_name, "echo");
    assert_eq!(envelopes[0].receipt_kind, "receipt.echo.v1");
    assert_eq!(
        envelopes[0].outcome,
        ReceiptOutcome::failed("failed", "boom")
    );
}

#[test]
fn no_receipt_sink_preserves_current_success_behavior() {
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    let mut core = builder.build().expect("core builds");

    let result = core.invoke("echo", b"plain".to_vec()).expect("invoke");

    assert_eq!(result.output().as_slice(), b"plain:ok");
    assert!(result.recorded_receipt().is_none());
}

#[test]
fn unknown_operation_does_not_emit_receipt() {
    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("core builds");

    let err = match core.invoke("missing", b"plain".to_vec()) {
        Ok(_) => panic!("expected unknown operation"),
        Err(error) => error,
    };

    assert!(matches!(err, RuntimeError::UnknownOperation { name } if name == "missing"));
    assert!(sink.envelopes().is_empty());
}

#[test]
fn unknown_checkout_frame_does_not_emit_receipt() {
    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("core builds");

    let err = match core.checkout_frame(CheckoutFrame::new("missing", b"plain".to_vec())) {
        Ok(_) => panic!("expected unknown operation"),
        Err(error) => error,
    };

    assert!(matches!(err, RuntimeError::UnknownOperation { name } if name == "missing"));
    assert!(sink.envelopes().is_empty());
}

#[test]
fn register_resolved_checkout_records_completed_receipt_once() {
    let sink = RecordingReceiptSink::default();
    let register = Register::from_operations([ECHO]).expect("register builds");
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("core builds");

    let checkout = register
        .checkout("echo", b"hello".to_vec())
        .expect("checkout resolves");
    let result = core.checkout(checkout).expect("checkout runs");

    assert_eq!(result.output().as_slice(), b"hello:ok");
    let recorded = result.recorded_receipt().expect("recorded receipt");
    assert_eq!(sink.envelopes(), vec![recorded.envelope.clone()]);
}

#[test]
fn checkout_uses_runtime_descriptor_when_resolved_descriptor_is_stale() {
    const STALE_ECHO: OperationDescriptor = OperationDescriptor::new(
        "echo",
        EffectClass::Emit,
        "schema.stale.input.v1",
        "schema.stale.output.v1",
        "receipt.stale.v1",
    );

    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("core builds");

    let checkout = syncbat::Checkout::new(STALE_ECHO, b"hello".to_vec());
    let result = core.checkout(checkout).expect("checkout runs");

    assert_eq!(result.descriptor(), &ECHO);
    let recorded = result.recorded_receipt().expect("recorded receipt");
    assert_eq!(recorded.envelope.descriptor_name, "echo");
    assert_eq!(recorded.envelope.receipt_kind, "receipt.echo.v1");
    assert_eq!(sink.envelopes(), vec![recorded.envelope.clone()]);
}

#[test]
fn register_resolved_checkout_records_failed_receipt_once() {
    let sink = RecordingReceiptSink::default();
    let register = Register::from_operations([ECHO]).expect("register builds");
    let mut builder = Core::builder();
    builder.register(ECHO, FailingHandler).expect("register");
    builder.receipt_sink(sink.clone());
    let mut core = builder.build().expect("core builds");

    let checkout = register
        .checkout("echo", b"bad".to_vec())
        .expect("checkout resolves");
    let err = match core.checkout(checkout) {
        Ok(_) => panic!("expected handler failure"),
        Err(error) => error,
    };

    assert!(matches!(err, RuntimeError::Handler { .. }));
    let envelopes = sink.envelopes();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(
        envelopes[0].outcome,
        ReceiptOutcome::failed("failed", "boom")
    );
}

#[test]
fn deferred_hash_policy_leaves_hashes_empty() {
    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(sink);
    let mut core = builder.build().expect("core builds");

    let result = core.invoke("echo", b"hello".to_vec()).expect("invoke");
    let envelope = &result
        .recorded_receipt()
        .expect("recorded receipt")
        .envelope;

    assert_eq!(envelope.input_hash, None);
    assert_eq!(envelope.output_hash, None);
}

#[test]
fn raw_byte_hash_policy_sets_input_and_output_hashes_on_success() {
    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(sink);
    builder.receipt_hash_policy(ReceiptHashPolicy::raw_bytes(test_hash));
    let mut core = builder.build().expect("core builds");

    let result = core.invoke("echo", b"hash".to_vec()).expect("invoke");
    let envelope = &result
        .recorded_receipt()
        .expect("recorded receipt")
        .envelope;

    assert_eq!(envelope.input_hash, Some(test_hash(b"hash")));
    assert_eq!(envelope.output_hash, Some(test_hash(b"hash:ok")));
}

#[test]
fn raw_byte_hash_policy_sets_only_input_hash_on_failure() {
    let sink = RecordingReceiptSink::default();
    let mut builder = Core::builder();
    builder.register(ECHO, FailingHandler).expect("register");
    builder.receipt_sink(sink.clone());
    builder.receipt_hash_policy(ReceiptHashPolicy::raw_bytes(test_hash));
    let mut core = builder.build().expect("core builds");

    let err = match core.invoke("echo", b"hash".to_vec()) {
        Ok(_) => panic!("expected handler failure"),
        Err(error) => error,
    };

    assert!(matches!(err, RuntimeError::Handler { .. }));
    let envelopes = sink.envelopes();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].input_hash, Some(test_hash(b"hash")));
    assert_eq!(envelopes[0].output_hash, None);
}

#[test]
fn receipt_sink_failure_is_fail_closed() {
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register");
    builder.receipt_sink(FailingReceiptSink);
    let mut core = builder.build().expect("core builds");

    let err = match core.invoke("echo", b"hello".to_vec()) {
        Ok(_) => panic!("expected receipt sink failure"),
        Err(error) => error,
    };

    assert!(
        matches!(
            err,
            RuntimeError::ReceiptSink { ref name, ref message }
                if name == "echo" && message == "sink down"
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn failed_handler_plus_sink_failure_is_fail_closed() {
    let mut builder = Core::builder();
    builder.register(ECHO, FailingHandler).expect("register");
    builder.receipt_sink(FailingReceiptSink);
    let mut core = builder.build().expect("core builds");

    let err = match core.invoke("echo", b"hello".to_vec()) {
        Ok(_) => panic!("expected receipt sink failure"),
        Err(error) => error,
    };

    assert!(
        matches!(
            err,
            RuntimeError::ReceiptSink { ref name, ref message }
                if name == "echo" && message == "sink down"
        ),
        "unexpected error: {err:?}"
    );
}
