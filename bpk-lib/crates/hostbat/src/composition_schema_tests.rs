//! Tests for the content-addressed composition schema manifest + collision gate.

use super::*;

use crate::schema::{GoldenVector, SchemaId, SchemaVersion};

fn descriptor(id_str: &str, version: u32, bytes: &[u8]) -> SchemaDescriptor {
    SchemaDescriptor::new(
        SchemaId::new(id_str).expect("id"),
        SchemaVersion(version),
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", bytes.to_vec())],
    )
    .expect("descriptor")
}

#[test]
fn aggregates_distinct_schemas_into_a_content_addressed_manifest() {
    let mut builder = CompositionSchemaBuilder::default();
    builder
        .add("mod.a", &descriptor("hostbat.op.a.in", 1, b"a"))
        .expect("add a");
    builder
        .add("mod.b", &descriptor("hostbat.op.b.in", 1, b"b"))
        .expect("add b");
    let manifest = builder.seal().expect("seal");
    assert_eq!(manifest.len(), 2);
    assert!(manifest.verify_digest().expect("verify"));
    assert!(manifest
        .find("hostbat.op.a.in", 1, SchemaRole::OperationInput)
        .is_some());
}

#[test]
fn digest_is_aggregation_order_independent() {
    let a = descriptor("hostbat.op.a.in", 1, b"a");
    let b = descriptor("hostbat.op.b.in", 1, b"b");

    let mut forward = CompositionSchemaBuilder::default();
    forward.add("mod.a", &a).expect("a");
    forward.add("mod.b", &b).expect("b");
    let forward = forward.seal().expect("seal");

    let mut reverse = CompositionSchemaBuilder::default();
    reverse.add("mod.b", &b).expect("b");
    reverse.add("mod.a", &a).expect("a");
    let reverse = reverse.seal().expect("seal");

    assert_eq!(
        forward.digest(),
        reverse.digest(),
        "the composition digest seals the schema set, not the add order",
    );
}

/// Two modules declaring the SAME schema identity with the SAME bytes is fine —
/// it deduplicates to the first owner. (A schema legitimately shared by two
/// modules, e.g. a common event payload.)
#[test]
fn identical_redeclaration_across_modules_deduplicates() {
    let shared = descriptor("hostbat.event.shared", 1, b"payload");
    let mut builder = CompositionSchemaBuilder::default();
    builder.add("mod.a", &shared).expect("a");
    builder.add("mod.b", &shared).expect("b same bytes");
    let manifest = builder.seal().expect("seal");
    assert_eq!(manifest.len(), 1, "identical declarations collapse to one");
    let entry = manifest
        .find("hostbat.event.shared", 1, SchemaRole::OperationInput)
        .expect("present");
    assert_eq!(entry.owner_module(), "mod.a", "first declarer owns it");
}

/// The HARD ERROR: same `(id, version, role)`, DIFFERENT bytes ⇒ fail-closed.
#[test]
fn colliding_schema_identity_with_differing_bytes_is_rejected() {
    let mut builder = CompositionSchemaBuilder::default();
    builder
        .add("mod.a", &descriptor("hostbat.op.shared.in", 1, b"shape-a"))
        .expect("first");
    let outcome = builder.add("mod.b", &descriptor("hostbat.op.shared.in", 1, b"shape-b"));
    assert!(
        matches!(outcome, Err(HostError::SchemaCollision { .. })),
        "a differing encoding at the same identity is a hard error",
    );
}

/// The same gate, exercised end-to-end through the host builder's `mount`.
#[test]
fn host_mount_rejects_cross_module_schema_collision() {
    use crate::module::HostModule;

    let module = |id: &'static str, bytes: &'static [u8]| {
        HostModule::builder(id, 1)
            .schema(descriptor("hostbat.op.shared.in", 1, bytes))
            .expect("schema")
            .build()
            .expect("module")
    };

    let outcome = crate::HostBuilder::new()
        .mount(module("mod.a", b"shape-a"))
        .expect("first mount")
        .mount(module("mod.b", b"shape-b"));
    assert!(
        matches!(outcome, Err(HostError::SchemaCollision { .. })),
        "the host fails closed on a cross-module wire-identity conflict",
    );
}

/// A schema-only module is a legitimate unit (it contributes wire identity even
/// with no operations/hooks/jobs), and its schemas reach the composition.
#[test]
fn schema_only_module_contributes_to_the_composition() {
    use crate::module::HostModule;

    let module = HostModule::builder("mod.schemas", 1)
        .schema(descriptor("hostbat.op.only.in", 1, b"x"))
        .expect("schema")
        .build()
        .expect("schema-only module builds");
    let host = crate::HostBuilder::new()
        .mount(module)
        .expect("mount")
        .build()
        .expect("build");
    assert_eq!(host.composition_schemas().len(), 1);
    assert!(host.composition_schemas().verify_digest().expect("verify"));
}
