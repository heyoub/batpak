#![allow(clippy::panic)]

use clawbat as cb;

fn explicit_err<T>(result: Result<T, batpak::guard::Denial>) -> batpak::guard::Denial {
    match result {
        Ok(_) => panic!("PROPERTY: expected clawbat requirement gate denial but got Ok"),
        Err(error) => error,
    }
}

#[cb::operation(
    descriptor = ECHO,
    register = register_echo,
    register_item = echo_item,
    name = "claw.echo",
    effect = Compute,
    input_schema = "schema.claw.echo.input.v1",
    output_schema = "schema.claw.echo.output.v1",
    receipt_kind = "receipt.claw.echo.v1",
    title = "Claw Echo"
)]
fn echo(input: &[u8], cx: &mut syncbat::Cx<'_>) -> syncbat::HandlerResult {
    assert_eq!(cx.descriptor().name(), "claw.echo");
    let mut output = b"cb:".to_vec();
    output.extend_from_slice(input);
    Ok(output)
}

#[test]
fn cb_operation_macro_generates_syncbat_descriptor() {
    assert_eq!(ECHO.name(), "claw.echo");
    assert_eq!(ECHO.title(), Some("Claw Echo"));
    assert_eq!(ECHO.effect, cb::EffectClass::Compute);
    assert_eq!(ECHO.input_schema_ref(), "schema.claw.echo.input.v1");
    assert_eq!(ECHO.output_schema_ref(), "schema.claw.echo.output.v1");
    assert_eq!(ECHO.receipt_kind(), "receipt.claw.echo.v1");
}

#[test]
fn generated_registration_invokes_through_syncbat_core() {
    let mut builder = syncbat::Core::builder();
    register_echo(&mut builder).expect("register generated cb operation");
    let mut core = builder.build().expect("syncbat core builds");

    let result = core
        .invoke("claw.echo", b"hello".to_vec())
        .expect("syncbat invokes cb operation");

    assert_eq!(result.descriptor().name(), "claw.echo");
    assert_eq!(result.output().as_slice(), b"cb:hello");
    assert!(result.recorded_receipt().is_none());
}

#[test]
fn cb_vocabulary_maps_to_syncbat_without_runtime_ownership() {
    let pass = cb::PassRef::new("pass.local.validate").expect("valid pass ref");
    let capability =
        cb::CapabilityRef::new("capability.store:append").expect("valid capability ref");

    let descriptor: cb::OperationDescriptor = syncbat::OperationDescriptor::new(
        "claw.vocab",
        cb::EffectClass::Inspect,
        "schema.claw.vocab.input.v1",
        "schema.claw.vocab.output.v1",
        "receipt.claw.vocab.v1",
    );
    let pass_descriptor = cb::PassDescriptor::new(pass).with_title("Local validation");
    let capability_descriptor =
        cb::CapabilityDescriptor::new(capability).with_title("Store append");
    let passes = [pass];
    let capabilities = [capability];
    let item = cb::OperationKitItem::new(descriptor.clone(), &passes, &capabilities);
    let envelope = cb::ReceiptEnvelope::new(&descriptor, cb::ReceiptOutcome::Completed);

    assert_eq!(descriptor.name(), "claw.vocab");
    assert_eq!(descriptor.effect, syncbat::EffectClass::Inspect);
    assert_eq!(pass_descriptor.id(), pass);
    assert_eq!(pass_descriptor.title(), Some("Local validation"));
    assert_eq!(capability_descriptor.id(), capability);
    assert_eq!(capability_descriptor.title(), Some("Store append"));
    assert_eq!(item.descriptor(), &descriptor);
    assert_eq!(item.passes(), &[pass]);
    assert_eq!(item.capabilities(), &[capability]);
    assert_eq!(envelope.descriptor_name, "claw.vocab");
    assert_eq!(envelope.outcome, syncbat::ReceiptOutcome::Completed);
}

#[test]
fn cb_kit_item_builds_syncbat_register_item_without_running_runtime() {
    let item = cb::OperationKitItem::new(ECHO.clone(), &[], &[]);
    let register_item = item.register_item(echo);

    assert_eq!(register_item.descriptor(), &ECHO);
    assert_eq!(echo_item().descriptor(), &ECHO);
}

#[test]
fn cb_operation_requirements_compile_to_batpak_gates_and_pass_when_satisfied() {
    let pass = cb::PassRef::new("pass.local.validate").expect("valid pass ref");
    let capability =
        cb::CapabilityRef::new("capability.store:append").expect("valid capability ref");
    let passes = [pass];
    let capabilities = [capability];
    let item = cb::OperationKitItem::new(ECHO.clone(), &passes, &capabilities);
    let gates = item.compile_gate_set::<cb::RequirementEvidence>();
    let context = cb::RequirementEvidence::new()
        .with_pass(pass)
        .with_capability(capability);

    assert_eq!(
        gates.names(),
        vec![
            cb::REQUIRED_PASS_GATE_NAME,
            cb::REQUIRED_CAPABILITY_GATE_NAME
        ]
    );
    let receipt = gates
        .evaluate(&context, batpak::pipeline::Proposal::new("payload"))
        .expect("all clawbat requirements satisfied");

    assert_eq!(
        receipt.gates_passed(),
        &[
            cb::REQUIRED_PASS_GATE_NAME,
            cb::REQUIRED_CAPABILITY_GATE_NAME
        ]
    );
    assert_eq!(receipt.payload(), &"payload");
}

#[test]
fn cb_operation_requirement_gates_deny_missing_pass() {
    let pass = cb::PassRef::new("pass.local.validate").expect("valid pass ref");
    let capability =
        cb::CapabilityRef::new("capability.store:append").expect("valid capability ref");
    let passes = [pass];
    let capabilities = [capability];
    let item = cb::OperationKitItem::new(ECHO.clone(), &passes, &capabilities);
    let gates = item.compile_gate_set::<cb::RequirementEvidence>();
    let context = cb::RequirementEvidence::new().with_capability(capability);

    let denial = explicit_err(gates.evaluate(&context, batpak::pipeline::Proposal::new(())));

    assert_eq!(denial.gate, cb::REQUIRED_PASS_GATE_NAME);
    assert_eq!(denial.code, cb::MISSING_PASS_CODE);
    assert!(denial
        .context
        .iter()
        .any(|(key, value)| key == "operation" && value == "claw.echo"));
    assert!(denial
        .context
        .iter()
        .any(|(key, value)| key == "pass" && value == "pass.local.validate"));
}

#[test]
fn cb_operation_requirement_gates_deny_missing_capability() {
    let pass = cb::PassRef::new("pass.local.validate").expect("valid pass ref");
    let capability =
        cb::CapabilityRef::new("capability.store:append").expect("valid capability ref");
    let passes = [pass];
    let capabilities = [capability];
    let item = cb::OperationKitItem::new(ECHO.clone(), &passes, &capabilities);
    let gates = item.compile_gate_set::<cb::RequirementEvidence>();
    let context = cb::RequirementEvidence::new().with_pass(pass);

    let denial = explicit_err(gates.evaluate(&context, batpak::pipeline::Proposal::new(())));

    assert_eq!(denial.gate, cb::REQUIRED_CAPABILITY_GATE_NAME);
    assert_eq!(denial.code, cb::MISSING_CAPABILITY_CODE);
    assert!(denial
        .context
        .iter()
        .any(|(key, value)| key == "operation" && value == "claw.echo"));
    assert!(denial
        .context
        .iter()
        .any(|(key, value)| key == "capability" && value == "capability.store:append"));
}

#[test]
fn cb_operation_requirement_compiler_aggregates_passes_and_capabilities_by_gate_class() {
    let pass_a = cb::PassRef::new("pass.local.validate").expect("valid pass ref");
    let pass_b = cb::PassRef::new("pass.local.review").expect("valid pass ref");
    let capability_a =
        cb::CapabilityRef::new("capability.store:append").expect("valid capability ref");
    let capability_b =
        cb::CapabilityRef::new("capability.mail:send").expect("valid capability ref");
    let passes = [pass_a, pass_b];
    let capabilities = [capability_a, capability_b];
    let item = cb::OperationKitItem::new(ECHO.clone(), &passes, &capabilities);
    let gates = item.compile_gate_set::<cb::RequirementEvidence>();

    assert_eq!(
        gates.names(),
        vec![
            cb::REQUIRED_PASS_GATE_NAME,
            cb::REQUIRED_CAPABILITY_GATE_NAME
        ],
        "compiler should emit one gate per requirement class, not one duplicate-named gate per ref"
    );

    let missing_second_pass =
        cb::RequirementEvidence::from_refs([pass_a], [capability_a, capability_b]);
    let denial =
        explicit_err(gates.evaluate(&missing_second_pass, batpak::pipeline::Proposal::new(())));
    assert_eq!(denial.gate, cb::REQUIRED_PASS_GATE_NAME);
    assert_eq!(denial.code, cb::MISSING_PASS_CODE);
    assert!(denial
        .context
        .iter()
        .any(|(key, value)| key == "pass" && value == "pass.local.review"));

    let all_present =
        cb::RequirementEvidence::from_refs([pass_a, pass_b], [capability_a, capability_b]);
    gates
        .evaluate(&all_present, batpak::pipeline::Proposal::new(()))
        .expect("all aggregate requirements satisfied");
}

#[test]
fn cb_descriptors_compile_individual_requirement_gates() {
    let pass = cb::PassRef::new("pass.local.validate").expect("valid pass ref");
    let capability =
        cb::CapabilityRef::new("capability.store:append").expect("valid capability ref");
    let pass_gate = cb::PassDescriptor::new(pass).required_gate("claw.echo");
    let capability_gate = cb::CapabilityDescriptor::new(capability).required_gate("claw.echo");
    let mut gates = batpak::guard::GateSet::<cb::RequirementEvidence>::new();
    gates.push(pass_gate);
    gates.push(capability_gate);
    let context = cb::RequirementEvidence::from_refs([pass], [capability]);

    let receipt = gates
        .evaluate(&context, batpak::pipeline::Proposal::new(7_u8))
        .expect("descriptor-compiled gates pass");

    assert_eq!(receipt.payload(), &7);
    assert_eq!(
        receipt.gates_passed(),
        &[
            cb::REQUIRED_PASS_GATE_NAME,
            cb::REQUIRED_CAPABILITY_GATE_NAME
        ]
    );
}

#[test]
fn cb_operation_requirements_compile_to_batpak_pipeline() {
    let pass = cb::PassRef::new("pass.local.validate").expect("valid pass ref");
    let passes = [pass];
    let item = cb::OperationKitItem::new(ECHO.clone(), &passes, &[]);
    let pipeline = item.compile_pipeline::<cb::RequirementEvidence>();
    let context = cb::RequirementEvidence::new().with_pass(pass);

    let receipt = pipeline
        .evaluate(&context, batpak::pipeline::Proposal::new("pipeline"))
        .expect("pipeline evaluates clawbat gate set");

    assert_eq!(receipt.payload(), &"pipeline");
    assert_eq!(receipt.gates_passed(), &[cb::REQUIRED_PASS_GATE_NAME]);
}

#[test]
fn cb_refs_reject_invalid_values() {
    assert!(matches!(cb::PassRef::new(""), Err(cb::RefError::Empty)));
    assert!(matches!(
        cb::CapabilityRef::new("capability with space"),
        Err(cb::RefError::InvalidByte {
            index: 10,
            byte: b' '
        })
    ));
    assert!(matches!(
        cb::CapabilityRef::new(".capability"),
        Err(cb::RefError::InvalidBoundary {
            index: 0,
            byte: b'.'
        })
    ));
    assert!(matches!(
        cb::CapabilityRef::new("capability."),
        Err(cb::RefError::InvalidBoundary {
            index: 10,
            byte: b'.'
        })
    ));
    assert!(matches!(
        cb::CapabilityRef::new("capability..store"),
        Err(cb::RefError::RepeatedSeparator {
            index: 11,
            byte: b'.'
        })
    ));
}

#[test]
fn cb_ref_validation_drills_ascii_byte_space() {
    for byte in 0_u8..=127 {
        let value = [b'a', byte, b'z'];
        let value = std::str::from_utf8(&value).expect("ascii fixture");
        let accepted = cb::CapabilityRef::new(Box::leak(value.to_owned().into_boxed_str()));
        let should_accept = matches!(
            byte,
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'
        );

        assert_eq!(
            accepted.is_ok(),
            should_accept,
            "unexpected validation result for byte 0x{byte:02x}"
        );
    }

    assert!(cb::CapabilityRef::new("a.b_c:d-e").is_ok());
    assert!(matches!(
        cb::CapabilityRef::new("a.-z"),
        Err(cb::RefError::RepeatedSeparator {
            index: 2,
            byte: b'-'
        })
    ));
}
