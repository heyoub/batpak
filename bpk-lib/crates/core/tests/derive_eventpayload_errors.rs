//! Compile-fail coverage for the `#[derive(EventPayload)]` parser contract.
//! Harness pattern: Fault-Injection Harness (compile-fail lane).
//!
//! Every fixture in `tests/ui/ep_*.rs` is a stand-alone crate that should
//! fail to compile because of a specific violation of the pinned attribute
//! contract (ADR-0010). The `.stderr` files next to them pin the exact
//! error messages so regressions in span quality or wording show up as a
//! trybuild diff instead of a silent behaviour change.

#[test]
#[serial_test::file_serial(trybuild)]
fn compile_fail_derive_errors() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/ep_missing_attr.rs");
    t.compile_fail("tests/ui/ep_on_enum.rs");
    t.compile_fail("tests/ui/ep_on_tuple_struct.rs");
    t.compile_fail("tests/ui/ep_on_unit_struct.rs");
    t.compile_fail("tests/ui/ep_generic_enum.rs");
    t.compile_fail("tests/ui/ep_generic_struct.rs");
    t.compile_fail("tests/ui/ep_unknown_key.rs");
    t.compile_fail("tests/ui/ep_duplicate_key.rs");
    t.compile_fail("tests/ui/ep_missing_key.rs");
    t.compile_fail("tests/ui/ep_invalid_category.rs");
    t.compile_fail("tests/ui/ep_category_overflows_u8.rs");
    t.compile_fail("tests/ui/ep_invalid_type_id.rs");
    t.compile_fail("tests/ui/ep_multiple_attrs.rs");
}
