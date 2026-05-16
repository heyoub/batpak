#[test]
fn operation_macro_rejects_invalid_inputs() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/operation_macro_missing_name.rs");
    t.compile_fail("tests/ui/operation_macro_missing_descriptor.rs");
    t.compile_fail("tests/ui/operation_macro_unknown_key.rs");
    t.compile_fail("tests/ui/operation_macro_duplicate_key.rs");
    t.compile_fail("tests/ui/operation_macro_bad_effect.rs");
    t.compile_fail("tests/ui/operation_macro_async_fn.rs");
    t.compile_fail("tests/ui/operation_macro_generic_fn.rs");
    t.compile_fail("tests/ui/operation_macro_unsafe_fn.rs");
    t.compile_fail("tests/ui/operation_macro_non_rust_abi.rs");
    t.compile_fail("tests/ui/operation_macro_wrong_signature.rs");
}
