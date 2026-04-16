mod common;

#[test]
fn proptest_cases_env_is_a_floor_not_a_cap() {
    assert_eq!(common::proptest::effective_cases(2048, None), 2048);
    assert_eq!(common::proptest::effective_cases(2048, Some("256")), 2048);
    assert_eq!(common::proptest::effective_cases(2048, Some("4096")), 4096);
}

#[test]
fn malformed_proptest_cases_panics_loudly() {
    let panic = std::panic::catch_unwind(|| {
        let _ = common::proptest::effective_cases(2048, Some("not-a-number"));
    })
    .expect_err("malformed PROPTEST_CASES must panic");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&'static str>().copied())
        .unwrap_or("<non-string panic>");
    assert!(
        message.contains("PROPTEST_CASES must be an unsigned integer"),
        "panic should explain the bad PROPTEST_CASES value, got: {message}"
    );
}
