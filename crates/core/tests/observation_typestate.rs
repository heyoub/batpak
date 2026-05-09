// justifies: INV-TEST-PANIC-AS-ASSERTION; trybuild harness failures surface through panic and are the intended assertion shape for UI compile-fail tests.
#![allow(clippy::panic)]

#[test]
#[serial_test::file_serial(trybuild)]
fn compile_fail_observation_typestate_guards() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/obs_double_consume.rs");
    t.compile_fail("tests/ui/obs_forge.rs");
    t.compile_fail("tests/ui/obs_must_use.rs");
}
