//! Compile-fail tests for typestate safety.
//! Verifies that Receipt forgery and invalid state construction fail to compile.
//! [SPEC:tests/typestate_safety.rs]

#[test]
fn compile_fail_tests() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/forge_receipt.rs");
    t.compile_fail("tests/ui/invalid_transition.rs");
}
