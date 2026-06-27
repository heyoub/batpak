//! Composition tests + negative-path (red) fixtures for the host.
//!
//! Green fixtures prove a valid composition builds, dispatches, guards, runs
//! hooks in deterministic order, and supervises jobs. Red fixtures prove each
//! collision / tamper / empty gate fails closed by returning its specific typed
//! error — a gate that silently admitted any of these would be the over-claim the
//! gauntlet hunts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use syncbat::{
    AdmissionDecision, Ctx, EffectClass, HandlerResult, OperationDescriptor, OperationEffectRow,
};

use crate::descriptor::{GuardDescriptor, HookPhase};
use crate::error::{HostError, HostRuntimeError};
use crate::host::Host;
use crate::module::{HostModule, HostModuleBuilder};
use crate::schema::{
    DiagnosticRustType, GoldenVector, SchemaDescriptor, SchemaId, SchemaRole, SchemaVersion,
};
use crate::{HostBuilder, SchemaShape};

fn op(name: &'static str) -> OperationDescriptor {
    OperationDescriptor::new(
        name,
        EffectClass::Inspect,
        "schema.in.v1",
        "schema.out.v1",
        "receipt.v1",
    )
}

fn op_with_row(name: &'static str, row: OperationEffectRow) -> OperationDescriptor {
    op(name).with_effect_row(row)
}

/// An echo handler: returns its input unchanged.
fn echo(input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
    Ok(input.to_vec())
}

fn canonical_bytes(value: &str) -> Vec<u8> {
    batpak::canonical::to_bytes(&value).expect("canonical fixture encodes")
}

fn invalid_canonical_bytes() -> Vec<u8> {
    vec![0xc1]
}

/// A module with one operation and a stable id.
fn single_op_module(id: &'static str, op_name: &'static str) -> HostModule {
    with_default_operation_schemas(
        HostModule::builder(id, 1)
            .operation(op(op_name), echo)
            .expect("operation registers"),
    )
    .build()
    .expect("module builds")
}

fn with_default_operation_schemas(builder: HostModuleBuilder) -> HostModuleBuilder {
    builder
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
}

fn module_builder_with_op(id: &'static str, op_name: &'static str) -> HostModuleBuilder {
    with_default_operation_schemas(
        HostModule::builder(id, 1)
            .operation(op(op_name), echo)
            .expect("op"),
    )
}

fn single_op_module_with_descriptor(
    id: &'static str,
    descriptor: OperationDescriptor,
) -> HostModule {
    with_default_operation_schemas(
        HostModule::builder(id, 1)
            .operation(descriptor, echo)
            .expect("operation registers"),
    )
    .build()
    .expect("module builds")
}

// ---- green: identity ----------------------------------------------------

#[test]
fn module_digest_is_deterministic() {
    let a = single_op_module("mod.a", "mod.a.echo");
    let b = single_op_module("mod.a", "mod.a.echo");
    assert_eq!(
        a.manifest().digest(),
        b.manifest().digest(),
        "identical declared parts yield identical H_module",
    );
    assert!(a.manifest().verify_hash().expect("verify"));
}

#[test]
fn module_digest_changes_with_declared_parts() {
    let a = single_op_module("mod.a", "mod.a.echo");
    let b = single_op_module("mod.a", "mod.a.other");
    assert_ne!(
        a.manifest().digest(),
        b.manifest().digest(),
        "a different operation name changes H_module",
    );
}

#[test]
fn host_fingerprint_is_mount_order_independent() {
    let forward = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount a")
        .mount(single_op_module("mod.b", "mod.b.echo"))
        .expect("mount b")
        .build()
        .expect("build")
        .fingerprint();
    let reverse = HostBuilder::new()
        .mount(single_op_module("mod.b", "mod.b.echo"))
        .expect("mount b")
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount a")
        .build()
        .expect("build")
        .fingerprint();
    assert_eq!(
        forward, reverse,
        "H_host depends on the module set, not order"
    );
}

#[test]
fn host_fingerprint_changes_with_module_set() {
    let two = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount a")
        .mount(single_op_module("mod.b", "mod.b.echo"))
        .expect("mount b")
        .build()
        .expect("build")
        .fingerprint();
    let one = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount a")
        .build()
        .expect("build")
        .fingerprint();
    assert_ne!(two, one, "dropping a module changes H_host");
}

#[test]
fn red_missing_operation_schema_ref_is_rejected_at_build() {
    let module = HostModule::builder("mod.a", 1)
        .operation(op("mod.a.echo"), echo)
        .expect("op")
        .build()
        .expect("module");
    let outcome = HostBuilder::new().mount(module).expect("mount").build();
    assert!(matches!(
        outcome,
        Err(HostError::SchemaReferenceMissing { .. })
    ));
}

// ---- green: schema descriptors fold into module identity ----------------

fn schema_with_role(id: &str, role: SchemaRole, bytes: &[u8]) -> SchemaDescriptor {
    SchemaDescriptor::new(
        SchemaId::new(id).expect("id"),
        SchemaVersion(1),
        role,
        vec![GoldenVector::new("c", bytes.to_vec())],
    )
    .expect("descriptor")
    .with_shape(SchemaShape::string())
    .expect("shape")
}

fn schema(id: &str, bytes: &[u8]) -> SchemaDescriptor {
    schema_with_role(id, SchemaRole::OperationInput, bytes)
}

#[test]
fn declaring_a_schema_changes_module_identity() {
    let plain = single_op_module("mod.a", "mod.a.echo");
    let with_schema = module_builder_with_op("mod.a", "mod.a.echo")
        .schema(schema("hostbat.op.a.in", &canonical_bytes("shape")))
        .expect("schema")
        .build()
        .expect("module");
    assert_ne!(
        plain.manifest().digest(),
        with_schema.manifest().digest(),
        "a declared schema is sealed into H_module",
    );
    assert!(with_schema.manifest().verify_hash().expect("verify"));
    assert_eq!(with_schema.manifest().schemas().count(), 4);
}

#[test]
fn schema_bytes_change_module_identity() {
    let make = |value: &str| {
        module_builder_with_op("mod.a", "mod.a.echo")
            .schema(schema("hostbat.op.a.in", &canonical_bytes(value)))
            .expect("schema")
            .build()
            .expect("module")
    };
    assert_ne!(
        make("shape-a").manifest().digest(),
        make("shape-b").manifest().digest(),
        "a wire-shape byte change changes the module digest",
    );
}

/// The diagnostic Rust type does NOT touch module identity: renaming or removing
/// it leaves H_module unchanged. This is the structural guarantee that deleting a
/// Rust type (the `refbat::*` failure) cannot break wire identity.
#[test]
fn diagnostic_rust_type_does_not_change_module_identity() {
    let bare = module_builder_with_op("mod.a", "mod.a.echo")
        .schema(schema("hostbat.op.a.in", &canonical_bytes("shape")))
        .expect("schema")
        .build()
        .expect("module");
    let with_type = module_builder_with_op("mod.a", "mod.a.echo")
        .schema(
            schema("hostbat.op.a.in", &canonical_bytes("shape"))
                .with_diagnostic_rust_type(DiagnosticRustType::new("any_crate::AnyType")),
        )
        .expect("schema")
        .build()
        .expect("module");
    assert_eq!(
        bare.manifest().digest(),
        with_type.manifest().digest(),
        "the informational Rust type is excluded from H_module",
    );
}

fn schema_without_shape(id: &str, bytes: &[u8]) -> SchemaDescriptor {
    SchemaDescriptor::new(
        SchemaId::new(id).expect("id"),
        SchemaVersion(1),
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", bytes.to_vec())],
    )
    .expect("descriptor")
}

#[test]
fn red_duplicate_schema_identity_within_module_is_rejected() {
    let outcome = HostModule::builder("mod.a", 1)
        .schema(schema_without_shape("hostbat.op.a.in", b"x"))
        .expect("first schema")
        .schema(schema_without_shape("hostbat.op.a.in", b"y"));
    assert!(matches!(outcome, Err(HostError::SchemaInvalid { .. })));
}

// ---- green: dispatch + guard --------------------------------------------

#[test]
fn host_dispatches_to_the_composed_core() {
    let mut host = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build");
    let input = canonical_bytes("ping");
    let result = host.invoke("mod.a.echo", input.clone()).expect("invoke");
    assert_eq!(
        result.output(),
        input.as_slice(),
        "the host delegates dispatch to syncbat"
    );
}

#[test]
fn host_rejects_noncanonical_operation_input_before_handler_runs() {
    struct FlaggedHandler {
        ran: Arc<AtomicBool>,
    }

    impl syncbat::Handler for FlaggedHandler {
        fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
            self.ran.store(true, Ordering::SeqCst);
            Ok(input.to_vec())
        }
    }

    let ran = Arc::new(AtomicBool::new(false));
    let module = with_default_operation_schemas(
        HostModule::builder("mod.schema.input", 1)
            .operation(
                op("mod.schema.input.echo"),
                FlaggedHandler {
                    ran: Arc::clone(&ran),
                },
            )
            .expect("operation"),
    )
    .build()
    .expect("module");
    let mut host = HostBuilder::new()
        .mount(module)
        .expect("mount")
        .build()
        .expect("build");

    let err = match host.invoke("mod.schema.input.echo", invalid_canonical_bytes()) {
        Ok(_) => {
            assert!(
                std::hint::black_box(false),
                "PROPERTY: non-canonical input must be rejected before handler execution"
            );
            return;
        }
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            syncbat::RuntimeError::Handler {
                ref code,
                ref message,
                ..
            } if code == "invalid_input" && message.contains("input schema validation failed")
        ),
        "PROPERTY: invalid input must surface as schema-validation-backed invalid_input, got {err:?}"
    );
    assert!(
        !ran.load(Ordering::SeqCst),
        "PROPERTY: invalid input must not reach the user handler"
    );
}

#[test]
fn host_rejects_noncanonical_operation_output() {
    fn bad_output(_input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        Ok(invalid_canonical_bytes())
    }

    let module = with_default_operation_schemas(
        HostModule::builder("mod.schema.output", 1)
            .operation(op("mod.schema.output.bad"), bad_output)
            .expect("operation"),
    )
    .build()
    .expect("module");
    let mut host = HostBuilder::new()
        .mount(module)
        .expect("mount")
        .build()
        .expect("build");
    let input = canonical_bytes("valid");

    let err = match host.invoke("mod.schema.output.bad", input) {
        Ok(_) => {
            assert!(
                std::hint::black_box(false),
                "PROPERTY: non-canonical output must be rejected before checkout completion"
            );
            return;
        }
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            syncbat::RuntimeError::Handler {
                ref code,
                ref message,
                ..
            } if code == "failed" && message.contains("output schema validation failed")
        ),
        "PROPERTY: invalid output must surface as schema-validation-backed handler failure, got {err:?}"
    );
}

#[test]
fn guard_governs_only_its_own_modules_operations() {
    fn deny(_d: &OperationDescriptor, _i: &[u8], _c: &mut Ctx<'_>) -> AdmissionDecision {
        AdmissionDecision::deny("test.policy", "blocked")
    }
    let guarded = module_builder_with_op("mod.guarded", "mod.guarded.echo")
        .guard(GuardDescriptor::new("test.guard.v1"), deny)
        .expect("guard")
        .build()
        .expect("module");
    let open = single_op_module("mod.open", "mod.open.echo");

    let mut host = HostBuilder::new()
        .mount(guarded)
        .expect("mount guarded")
        .mount(open)
        .expect("mount open")
        .build()
        .expect("build");

    let input = canonical_bytes("x");
    assert!(
        host.invoke("mod.guarded.echo", input.clone()).is_err(),
        "the guarded module's op is denied by its guard",
    );
    assert!(
        host.invoke("mod.open.echo", input).is_ok(),
        "an op from a module with no guard is admitted",
    );
}

// ---- green: lifecycle hooks ---------------------------------------------

#[test]
fn startup_hooks_run_in_global_deterministic_order() {
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let record = |log: &Arc<Mutex<Vec<String>>>, tag: &'static str| {
        let log = Arc::clone(log);
        move || {
            log.lock().expect("lock").push(tag.to_owned());
            Ok(())
        }
    };

    // mod.a declares its hooks out of order (2 then 0); mod.b sits between (1).
    let a = module_builder_with_op("mod.a", "mod.a.echo")
        .hook(HookPhase::Startup, "late", 2, record(&log, "a-late"))
        .hook(HookPhase::Startup, "early", 0, record(&log, "a-early"))
        .build()
        .expect("module a");
    let b = module_builder_with_op("mod.b", "mod.b.echo")
        .hook(HookPhase::Startup, "mid", 1, record(&log, "b-mid"))
        .build()
        .expect("module b");

    let mut host = HostBuilder::new()
        .mount(a)
        .expect("mount a")
        .mount(b)
        .expect("mount b")
        .build()
        .expect("build");
    host.start().expect("start runs hooks");
    assert!(host.is_started());
    assert_eq!(
        *log.lock().expect("lock"),
        vec!["a-early", "b-mid", "a-late"],
        "hooks run by (order, module, name) across all modules",
    );
}

#[test]
fn a_failing_startup_hook_aborts_start_fail_closed() {
    let module = module_builder_with_op("mod.a", "mod.a.echo")
        .hook(HookPhase::Startup, "boom", 0, || {
            Err("precondition failed".to_owned())
        })
        .build()
        .expect("module");
    let mut host = HostBuilder::new()
        .mount(module)
        .expect("mount")
        .build()
        .expect("build");
    let outcome = host.start();
    assert!(
        matches!(outcome, Err(HostRuntimeError::StartupHook(_))),
        "a failing startup hook fails the host closed",
    );
    assert!(
        !host.is_started(),
        "the host is not marked started after a hook failure"
    );
}

// ---- green: supervised jobs ---------------------------------------------

#[test]
fn a_supervised_job_runs_and_joins_on_shutdown() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_for_job = Arc::clone(&ran);
    let factory = move || -> Box<dyn FnOnce() + Send + 'static> {
        let ran = Arc::clone(&ran_for_job);
        Box::new(move || ran.store(true, Ordering::Release))
    };
    let module = module_builder_with_op("mod.worker", "mod.worker.echo")
        .job("background", factory)
        .expect("job")
        .build()
        .expect("module");
    let mut host = HostBuilder::new()
        .mount(module)
        .expect("mount")
        .build()
        .expect("build");

    host.spawn_job("background").expect("spawn declared job");
    assert_eq!(host.supervisor().job_count(), 1);
    host.shutdown().expect("shutdown joins the job");
    assert!(
        ran.load(Ordering::Acquire),
        "the supervised job ran to completion before shutdown returned",
    );
}

#[test]
fn spawning_an_undeclared_job_kind_is_rejected() {
    let mut host = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build");
    assert!(
        matches!(
            host.spawn_job("ghost"),
            Err(HostRuntimeError::UnknownJobKind { .. })
        ),
        "no module declares the kind, so the supervisor refuses it",
    );
}

// ---- red: collisions + tamper + empty -----------------------------------

#[test]
fn red_duplicate_module_id_is_rejected() {
    let outcome = HostBuilder::new()
        .mount(single_op_module("dup.id", "dup.id.a"))
        .expect("first mount")
        .mount(single_op_module("dup.id", "dup.id.b"));
    assert!(matches!(outcome, Err(HostError::DuplicateModuleId { .. })));
}

#[test]
fn red_duplicate_operation_across_modules_is_rejected() {
    let outcome = HostBuilder::new()
        .mount(single_op_module("mod.a", "shared.op"))
        .expect("first mount")
        .mount(single_op_module("mod.b", "shared.op"));
    assert!(matches!(outcome, Err(HostError::DuplicateOperation { .. })));
}

#[test]
fn red_operation_effect_conflict_across_modules_is_rejected() {
    let outcome = HostBuilder::new()
        .mount(single_op_module_with_descriptor("mod.a", op("shared.op")))
        .expect("first mount")
        .mount(single_op_module_with_descriptor(
            "mod.b",
            op_with_row(
                "shared.op",
                OperationEffectRow::new().reads_event("event.shared.v1"),
            ),
        ));
    assert!(matches!(outcome, Err(HostError::EffectConflict { .. })));
}

#[test]
fn red_duplicate_receipt_namespace_is_rejected() {
    let make = |id: &'static str, op_name: &'static str| {
        module_builder_with_op(id, op_name)
            .receipt_namespace("shared.ns")
            .expect("ns")
            .build()
            .expect("module")
    };
    let outcome = HostBuilder::new()
        .mount(make("mod.a", "mod.a.echo"))
        .expect("first mount")
        .mount(make("mod.b", "mod.b.echo"));
    assert!(matches!(
        outcome,
        Err(HostError::DuplicateReceiptNamespace { .. })
    ));
}

#[test]
fn red_duplicate_job_kind_across_modules_is_rejected() {
    let factory = || -> Box<dyn FnOnce() + Send + 'static> { Box::new(|| {}) };
    let make = |id: &'static str, op_name: &'static str| {
        module_builder_with_op(id, op_name)
            .job("shared.kind", factory)
            .expect("job")
            .build()
            .expect("module")
    };
    let outcome = HostBuilder::new()
        .mount(make("mod.a", "mod.a.echo"))
        .expect("first mount")
        .mount(make("mod.b", "mod.b.echo"));
    assert!(matches!(outcome, Err(HostError::DuplicateJobKind { .. })));
}

#[test]
fn red_within_module_hook_order_collision_is_rejected() {
    let outcome = HostModule::builder("mod.a", 1)
        .operation(op("mod.a.echo"), echo)
        .expect("op")
        .hook(HookPhase::Startup, "first", 0, || Ok(()))
        .hook(HookPhase::Startup, "second", 0, || Ok(()))
        .build();
    assert!(matches!(outcome, Err(HostError::ModuleCoherence { .. })));
}

#[test]
fn red_within_module_duplicate_operation_is_rejected() {
    let outcome = HostModule::builder("mod.a", 1)
        .operation(op("mod.a.echo"), echo)
        .expect("first op")
        .operation(op("mod.a.echo"), echo);
    assert!(matches!(outcome, Err(HostError::ModuleCoherence { .. })));
}

#[test]
fn red_empty_module_is_rejected() {
    let outcome = HostModule::builder("mod.empty", 1).build();
    assert!(matches!(outcome, Err(HostError::ModuleCoherence { .. })));
}

#[test]
fn red_empty_host_is_rejected() {
    let outcome = HostBuilder::new().build();
    assert!(matches!(outcome, Err(HostError::EmptyHost)));
}

#[test]
fn red_malformed_module_id_is_rejected() {
    let outcome = HostModule::builder("Bad..Id", 1)
        .operation(op("mod.a.echo"), echo)
        .expect("op")
        .build();
    assert!(matches!(outcome, Err(HostError::ModuleCoherence { .. })));
}

#[test]
fn red_tampered_manifest_hash_is_rejected_at_mount() {
    let mut module = single_op_module("mod.a", "mod.a.echo");
    // The manifest is sealed from the parts; corrupt the stored digest so it no
    // longer matches. Mount must catch the mismatch and refuse to wire it.
    module.tamper_manifest_for_fixture();
    assert!(
        !module.manifest().verify_hash().expect("verify"),
        "the tampered manifest no longer matches its parts",
    );
    let outcome = HostBuilder::new().mount(module);
    assert!(matches!(outcome, Err(HostError::ModuleHashMismatch { .. })));
}

/// A built host is the runnable artifact; keep a smoke reference so the type is
/// exercised end to end here.
#[test]
fn host_type_is_constructible_and_startable() {
    let mut host: Host = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build");
    host.start().expect("start");
    host.shutdown().expect("shutdown");
}
