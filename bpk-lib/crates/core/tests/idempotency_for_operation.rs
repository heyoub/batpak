// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-IDEMPOTENCY-DURABLE-WINDOW; integration tests rely on expect/panic; clippy allowances are standard for harness tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! `IdempotencyKey::for_operation` determinism + collision-resistance.
//!
//! PROVES: INV-IDEMPOTENCY-DURABLE-WINDOW (operation-identity derivation).
//! `for_operation` is deterministic (same domain + components -> same key) and
//! its LENGTH-DELIMITED encoding keeps component boundaries distinct, so
//! `["ab","c"]` != `["a","bc"]` and a domain/component swap does not collide.
//! CATCHES: a naive concatenation (e.g. join without length prefixes) that
//! would alias distinct operations to the same key.
//! SEEDED: pure function, fixed string inputs.

use batpak::id::{EntityIdType, IdempotencyKey};

#[test]
fn for_operation_is_deterministic() {
    let a = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2", "req:42"]);
    let b = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2", "req:42"]);
    assert_eq!(a, b, "same operation identity must produce the same key");
}

#[test]
fn for_operation_length_delimiting_prevents_boundary_collisions() {
    // The classic concatenation-collision: ["ab","c"] vs ["a","bc"].
    let ab_c = IdempotencyKey::for_operation("d", &["ab", "c"]);
    let a_bc = IdempotencyKey::for_operation("d", &["a", "bc"]);
    assert_ne!(
        ab_c, a_bc,
        "length-delimited encoding must keep component boundaries distinct"
    );

    // Empty-component sensitivity: ["", "x"] vs ["x", ""] vs ["x"].
    let empty_then_x = IdempotencyKey::for_operation("d", &["", "x"]);
    let x_then_empty = IdempotencyKey::for_operation("d", &["x", ""]);
    let just_x = IdempotencyKey::for_operation("d", &["x"]);
    assert_ne!(empty_then_x, x_then_empty);
    assert_ne!(empty_then_x, just_x);
    assert_ne!(x_then_empty, just_x);
}

#[test]
fn for_operation_domain_is_distinguished_from_components() {
    // Moving a token between domain and first component must NOT collide.
    let domain_x = IdempotencyKey::for_operation("x", &["y"]);
    let component_x = IdempotencyKey::for_operation("", &["x", "y"]);
    assert_ne!(
        domain_x, component_x,
        "domain is length-delimited separately from components"
    );
}

#[test]
fn for_operation_distinct_operations_differ() {
    let transfer = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2"]);
    let refund = IdempotencyKey::for_operation("refund", &["acct:1", "acct:2"]);
    assert_ne!(transfer, refund, "different domains -> different keys");

    let order = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2"]);
    let reordered = IdempotencyKey::for_operation("transfer", &["acct:2", "acct:1"]);
    assert_ne!(order, reordered, "component order is significant");
}

#[test]
fn for_operation_is_nonzero_and_typed() {
    let key = IdempotencyKey::for_operation("op", &["a"]);
    assert_ne!(
        key.as_u128(),
        0,
        "derived key must not be the nil sentinel for realistic inputs"
    );
}
