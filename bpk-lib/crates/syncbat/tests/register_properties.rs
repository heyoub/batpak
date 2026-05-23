//! PROVES: INV-SYNCBAT-REGISTER-CATALOG-DETERMINISTIC
//! CATCHES: duplicate operation names, lookup drift, and cache-register projection mismatch.
//! SEEDED: fixed register descriptor sets.
#![allow(clippy::panic)]

use syncbat::{EffectClass, OperationDescriptor, Register};

const ALPHA: OperationDescriptor = OperationDescriptor::new(
    "alpha",
    EffectClass::Inspect,
    "schema.alpha.input.v1",
    "schema.alpha.output.v1",
    "receipt.alpha.v1",
);

const BRAVO: OperationDescriptor = OperationDescriptor::new(
    "bravo",
    EffectClass::Compute,
    "schema.bravo.input.v1",
    "schema.bravo.output.v1",
    "receipt.bravo.v1",
);

const CHARLIE: OperationDescriptor = OperationDescriptor::new(
    "charlie",
    EffectClass::Emit,
    "schema.charlie.input.v1",
    "schema.charlie.output.v1",
    "receipt.charlie.v1",
);

#[test]
fn register_order_is_deterministic_independent_of_insertion_order() {
    let left = Register::from_operations([CHARLIE, ALPHA, BRAVO]).expect("left register");
    let right = Register::from_operations([BRAVO, CHARLIE, ALPHA]).expect("right register");

    assert_eq!(
        left.names().collect::<Vec<_>>(),
        right.names().collect::<Vec<_>>()
    );
    assert_eq!(
        left.names().collect::<Vec<_>>(),
        vec!["alpha", "bravo", "charlie"]
    );
}

#[test]
fn cache_register_is_projection_equivalent_to_register() {
    let register = Register::from_operations([CHARLIE, ALPHA, BRAVO]).expect("register");
    let cache = syncbat::CacheRegister::from_register(&register);

    assert_eq!(cache.len(), register.len());
    assert!(!cache.is_empty());
    assert_eq!(
        cache.names().collect::<Vec<_>>(),
        register.names().collect::<Vec<_>>()
    );
    assert_eq!(cache.descriptors().count(), register.len());
    assert_eq!(
        cache
            .descriptors()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        register.names().collect::<Vec<_>>()
    );

    for (name, descriptor) in register.descriptors() {
        assert_eq!(cache.descriptor(name), Some(descriptor));
    }
}

#[test]
fn register_as_map_exposes_inserted_operations() {
    let register = Register::from_operations([ALPHA, BRAVO]).expect("register");
    let map = register.as_map();

    assert_eq!(map.len(), 2);
    assert!(map.contains_key("alpha"));
    assert!(map.contains_key("bravo"));
}

#[test]
fn duplicate_operation_names_are_rejected() {
    let err = match Register::from_operations([ALPHA, ALPHA]) {
        Ok(_) => panic!("expected duplicate rejection"),
        Err(error) => error,
    };

    assert!(matches!(
        err,
        syncbat::RegisterValidationError::DuplicateOperationName { ref name }
            if name == "alpha"
    ));
}

#[test]
fn unknown_checkout_returns_none() {
    let register = Register::from_operations([ALPHA]).expect("register");

    assert!(register.checkout("missing", Vec::new()).is_none());
}

#[test]
fn valid_generated_names_survive_insert_and_lookup() {
    for index in 0..128 {
        let name = format!("generated.operation-{index}_v1");
        let descriptor = OperationDescriptor::new(
            Box::leak(name.clone().into_boxed_str()),
            EffectClass::Compute,
            "schema.generated.input.v1",
            "schema.generated.output.v1",
            "receipt.generated.v1",
        );
        let register = Register::from_operations([descriptor]).expect("register");

        assert!(register.contains_operation(&name));
        assert_eq!(register.operation(&name).expect("descriptor").name(), name);
    }
}
