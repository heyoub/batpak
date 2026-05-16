#![allow(clippy::panic)]

use syncbat::{Core, HandlerError, RuntimeError};

#[syncbat::operation(
    descriptor = ECHO,
    register = register_echo,
    register_item = echo_item,
    name = "echo",
    effect = Compute,
    input_schema = "schema.echo.input.v1",
    output_schema = "schema.echo.output.v1",
    receipt_kind = "receipt.echo.v1",
    title = "Echo"
)]
fn echo(input: &[u8], cx: &mut syncbat::Cx<'_>) -> syncbat::HandlerResult {
    assert_eq!(cx.descriptor().name(), "echo");
    let mut out = Vec::from(input);
    out.extend_from_slice(b":ok");
    Ok(out)
}

#[syncbat::operation(
    descriptor = FAILING,
    register = register_failing,
    name = "failing",
    effect = Compute,
    input_schema = "schema.failing.input.v1",
    output_schema = "schema.failing.output.v1",
    receipt_kind = "receipt.failing.v1"
)]
fn failing(_input: &[u8], _cx: &mut syncbat::Cx<'_>) -> syncbat::HandlerResult {
    Err(HandlerError::failed("boom"))
}

#[test]
fn operation_macro_generates_descriptor_fields() {
    assert_eq!(ECHO.name(), "echo");
    assert_eq!(ECHO.title(), Some("Echo"));
    assert_eq!(ECHO.effect, syncbat::EffectClass::Compute);
    assert_eq!(ECHO.input_schema_ref(), "schema.echo.input.v1");
    assert_eq!(ECHO.output_schema_ref(), "schema.echo.output.v1");
    assert_eq!(ECHO.receipt_kind(), "receipt.echo.v1");

    assert_eq!(FAILING.name(), "failing");
    assert_eq!(FAILING.title(), None);
}

#[test]
fn operation_macro_generates_register_item() {
    let item = echo_item();

    assert_eq!(item.descriptor(), &ECHO);
}

#[test]
fn generated_register_function_invokes_successfully() {
    let mut builder = Core::builder();
    register_echo(&mut builder).expect("register");
    let mut core = builder.build().expect("core builds");

    let result = core.invoke("echo", b"hello".to_vec()).expect("invoke");

    assert_eq!(result.descriptor().name(), "echo");
    assert_eq!(result.output().as_slice(), b"hello:ok");
}

#[test]
fn generated_register_function_maps_handler_failure() {
    let mut builder = Core::builder();
    register_failing(&mut builder).expect("register");
    let mut core = builder.build().expect("core builds");

    let err = match core.invoke("failing", Vec::new()) {
        Ok(_) => panic!("expected handler failure"),
        Err(error) => error,
    };

    assert!(
        matches!(
            err,
            RuntimeError::Handler { ref name, ref code, ref message }
                if name == "failing" && code == "failed" && message == "boom"
        ),
        "unexpected error: {err:?}"
    );
}
