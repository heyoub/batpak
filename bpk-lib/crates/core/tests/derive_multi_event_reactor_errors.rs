//! Compile-fail coverage for the `#[derive(MultiEventReactor)]` attribute contract.
//! Harness pattern: Fault-Injection Harness (compile-fail lane).
//!
//! Each fixture in `tests/ui/mer_*.rs` violates a specific contract rule and
//! must fail to compile with a span-pointed error. The `.stderr` files pin
//! the exact error wording so regressions in message clarity or span quality
//! surface as trybuild diffs.

#[test]
#[serial_test::file_serial(trybuild)]
fn compile_fail_multi_event_reactor_derive_errors() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/mer_on_unit_struct.rs");
}
