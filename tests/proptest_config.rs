mod common;

#[test]
fn proptest_cases_env_is_a_floor_not_a_cap() {
    assert_eq!(common::proptest::effective_cases(2048, None), 2048);
    assert_eq!(common::proptest::effective_cases(2048, Some("256")), 2048);
    assert_eq!(common::proptest::effective_cases(2048, Some("4096")), 4096);
    assert_eq!(
        common::proptest::effective_cases(2048, Some("not-a-number")),
        2048
    );
}
