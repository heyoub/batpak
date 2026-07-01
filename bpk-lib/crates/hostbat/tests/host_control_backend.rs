//! PROVES: a hostbat `Control` operation reaches host authority ONLY through a
//! bound [`hostbat::HostController`], and fails closed without one.
//! CATCHES: a `HostBuilder` that drops the host-control layer (so `Control` ops
//!          can never perform), a controller that is not actually invoked with
//!          the operation's declared control-id, and a rejecting controller that
//!          is allowed to silently succeed.
//!
//! RED-BEFORE: before `HostBuilder::host_control` composed the
//! `HostControlEffectBackend`, a `Control` operation had no backend that could
//! perform `use_host_control`, so `host_control_op_performs_through_bound_controller`
//! failed (the handle fell through to the fail-closed default and the invoke
//! surfaced `RuntimeError::Handler`). The two fail-closed tests pin that the
//! unbacked / rejecting shapes are still exactly that typed failure.

use std::sync::{Arc, Mutex};

use hostbat::{
    GoldenVector, HostBuilder, HostControlError, HostModule, SchemaDescriptor, SchemaId,
    SchemaRole, SchemaVersion,
};
use syncbat::{
    Ctx, EffectClass, Handler, HandlerError, HandlerResult, OperationDescriptor,
    OperationEffectRow, RuntimeError,
};

const HOST_CONTROL: &str = "ctrl.alpha";

fn canonical_bytes(value: &str) -> Vec<u8> {
    batpak::canonical::to_bytes(&value).expect("canonical fixture encodes")
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

fn control_descriptor() -> OperationDescriptor {
    OperationDescriptor::new(
        "host.reboot",
        EffectClass::Control,
        "schema.in.v1",
        "schema.out.v1",
        "receipt.v1",
    )
    .with_effect_row(OperationEffectRow::new().uses_host_control(HOST_CONTROL))
}

struct RebootHandler;

impl Handler for RebootHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.host_control_handle()
            .use_host_control(HOST_CONTROL)
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

fn control_module() -> HostModule {
    HostModule::builder("mod.control", 1)
        .operation(control_descriptor(), RebootHandler)
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
        .expect("module")
}

#[test]
fn host_control_op_performs_through_bound_controller() {
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let mut host = HostBuilder::new()
        .mount(control_module())
        .expect("mount")
        // The blanket `HostController` impl lets an `FnMut(&str)` controller
        // perform the identified control; it records what it was asked to do.
        .host_control(move |control: &str| -> Result<(), HostControlError> {
            sink.lock().expect("observed lock").push(control.to_owned());
            Ok(())
        })
        .build()
        .expect("build");

    let payload = canonical_bytes("go");
    let result = host.invoke("host.reboot", payload.clone()).expect("invoke");
    assert_eq!(result.output(), payload.as_slice());
    assert_eq!(
        observed.lock().expect("observed lock").clone(),
        vec![HOST_CONTROL.to_owned()],
        "the controller must be performed with the operation's declared control-id",
    );
}

#[test]
fn host_control_op_without_controller_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    // No `.host_control(...)`: the `Control` op has no backend that can perform
    // host controls, so the handle falls through to the fail-closed default.
    let mut host = HostBuilder::new()
        .mount(control_module())
        .expect("mount")
        .build()
        .expect("build");

    let err = match host.invoke("host.reboot", canonical_bytes("go")) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: a Control op without a bound controller must fail closed",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "host.reboot"
    ));
    Ok(())
}

#[test]
fn host_control_op_with_rejecting_controller_fails_closed() -> Result<(), Box<dyn std::error::Error>>
{
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let mut host = HostBuilder::new()
        .mount(control_module())
        .expect("mount")
        .host_control(move |control: &str| -> Result<(), HostControlError> {
            sink.lock().expect("observed lock").push(control.to_owned());
            Err(HostControlError::new("reboot refused"))
        })
        .build()
        .expect("build");

    let err = match host.invoke("host.reboot", canonical_bytes("go")) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: a rejecting controller must fail the handler closed",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        RuntimeError::Handler { ref name, .. } if name == "host.reboot"
    ));
    // The controller was reached (it saw the control-id) but its rejection means
    // the observed effect row records nothing: perform failed, so record is skipped.
    assert_eq!(
        observed.lock().expect("observed lock").clone(),
        vec![HOST_CONTROL.to_owned()],
    );
    Ok(())
}
