//! PROVES: INV-SYNCBAT-REGISTER-CATALOG-DETERMINISTIC
//! CATCHES: macro-generated descriptor/register item drift before runtime registration.
//! SEEDED: fixed operation macro declarations.

use syncbat::{
    Core, HandlerError, OperationDescriptor, OperationEffectRow, RegisterOperationRowV1,
    RuntimeError,
};

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
fn echo(input: &[u8], cx: &mut syncbat::Ctx<'_>) -> syncbat::HandlerResult {
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
fn failing(_input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> syncbat::HandlerResult {
    Err(HandlerError::failed("boom"))
}

#[syncbat::operation(
    descriptor = APPEND_AUDIT,
    register = register_append_audit,
    name = "audit.append",
    effect = Persist,
    input_schema = "schema.audit.input.v1",
    output_schema = "schema.audit.output.v1",
    receipt_kind = "receipt.audit.v1",
    appends_events = ["evt.f001"]
)]
fn append_audit(input: &[u8], cx: &mut syncbat::Ctx<'_>) -> syncbat::HandlerResult {
    cx.event_append_handle()
        .append_event(batpak::event::EventKind::custom(0xF, 1), input)
        .map_err(|error| HandlerError::failed(error.to_string()))?;
    Ok(input.to_vec())
}

/// No-op effect backend so the effectful operation can append through `Ctx`.
struct NoopBackend;

impl syncbat::EffectBackend for NoopBackend {
    fn append_event(
        &mut self,
        _kind: batpak::event::EventKind,
        _payload: &[u8],
    ) -> Result<(), syncbat::EffectError> {
        Ok(())
    }
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
fn operation_macro_effect_row_converges_with_hand_built_descriptor_bytes() {
    let hand_built = OperationDescriptor::new(
        "audit.append",
        syncbat::EffectClass::Persist,
        "schema.audit.input.v1",
        "schema.audit.output.v1",
        "receipt.audit.v1",
    )
    .with_effect_row(OperationEffectRow::new().appends_event("evt.f001"));

    let macro_row = RegisterOperationRowV1::from_descriptor(&APPEND_AUDIT);
    let hand_row = RegisterOperationRowV1::from_descriptor(&hand_built);
    let macro_bytes = batpak::canonical::to_bytes(&macro_row).expect("macro row encodes");
    let hand_bytes = batpak::canonical::to_bytes(&hand_row).expect("hand row encodes");

    assert_eq!(&*APPEND_AUDIT, &hand_built);
    assert_eq!(macro_bytes, hand_bytes);
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
fn generated_effectful_register_function_invokes_successfully() {
    let mut builder = Core::builder();
    register_append_audit(&mut builder).expect("register");
    builder.effect_backend(NoopBackend);
    let mut core = builder.build().expect("core builds");

    let result = core
        .invoke("audit.append", b"event".to_vec())
        .expect("invoke");

    assert_eq!(result.descriptor().name(), "audit.append");
    assert_eq!(result.output().as_slice(), b"event");
}

#[test]
fn generated_register_function_maps_handler_failure() {
    let mut builder = Core::builder();
    register_failing(&mut builder).expect("register");
    let mut core = builder.build().expect("core builds");

    let err = core
        .invoke("failing", Vec::new())
        .map(|_| ())
        .expect_err("expected handler failure");

    assert!(
        matches!(
            err,
            RuntimeError::Handler { ref name, ref code, ref message }
                if name == "failing" && code == "failed" && message == "boom"
        ),
        "unexpected error: {err:?}"
    );
}
