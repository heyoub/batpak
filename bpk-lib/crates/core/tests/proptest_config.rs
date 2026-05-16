#[path = "common/proptest.rs"]
mod proptest_support;

#[test]
fn proptest_cases_env_is_a_floor_not_a_cap() {
    assert_eq!(proptest_support::effective_cases(2048, None), 2048);
    assert_eq!(proptest_support::effective_cases(2048, Some("256")), 2048);
    assert_eq!(proptest_support::effective_cases(2048, Some("4096")), 4096);
}

#[test]
fn cfg_builds_project_defaults_and_failure_persistence() {
    let cfg = proptest_support::cfg(2048);
    assert_eq!(cfg.cases, 2048);
    assert!(
        cfg.failure_persistence.is_some(),
        "project proptest config must persist failing seeds next to the source file"
    );
}

#[test]
fn malformed_proptest_cases_panics_loudly() {
    let panic = std::panic::catch_unwind(|| {
        let _ = proptest_support::effective_cases(2048, Some("not-a-number"));
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
