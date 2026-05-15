#![allow(clippy::panic)]

use syncbat::{
    Core, EffectClass, Handler, HandlerError, HandlerResult, Module, OperationDescriptor, Register,
    RuntimeError,
};

const ECHO: OperationDescriptor = OperationDescriptor::new(
    "echo",
    EffectClass::Compute,
    "schema.echo.input.v1",
    "schema.echo.output.v1",
    "receipt.echo.v1",
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

#[test]
fn register_and_cache_lookup_descriptor_by_name() {
    let mut register = Register::new();
    register
        .insert_operation(ECHO)
        .expect("operation inserts once");

    let cache = syncbat::CacheRegister::from_register(&register);
    assert!(cache.contains_operation("echo"));
    assert_eq!(
        cache.operation("echo").expect("descriptor").receipt_kind,
        "receipt.echo.v1"
    );
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
