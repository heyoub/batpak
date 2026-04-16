//! Project-wide proptest configuration.
//!
//! Failing seeds persist next to the test source (via `FileFailurePersistence::SourceParallel`)
//! so flakes are reproducible across CI cycles. Without this, every flake is one-shot:
//! the failing seed is lost the moment the process exits and can never be replayed.
//!
//! Call `common::proptest::cfg(N)` from any test file, where `N` is the default
//! number of cases for that file. `PROPTEST_CASES`, when set, acts as a floor
//! instead of silently capping deeper local defaults.

use proptest::prelude::ProptestConfig;
use proptest::test_runner::FileFailurePersistence;

/// Build a `ProptestConfig` with the project-wide settings.
///
/// - `cases`: reads `PROPTEST_CASES` from the environment as a floor; falls back to
///   `default_cases`, never lowers a file's coded default, and panics loudly on
///   malformed values so typoed CI/local overrides do not silently weaken proof depth.
/// - `failure_persistence`: writes failing seeds to a `proptest-regressions/` directory
///   next to the test source so a flake can be reproduced deterministically. Without this,
///   every failure is one-shot and the seed is lost the moment the process exits.
pub(crate) fn effective_cases(default_cases: u32, env_value: Option<&str>) -> u32 {
    match env_value {
        None => default_cases,
        Some(value) => {
            let cases = value
                .parse::<u32>()
                .unwrap_or_else(|_| invalid_proptest_cases(value));
            cases.max(default_cases)
        }
    }
}

#[allow(clippy::panic)] // invalid shared proof-depth knobs must fail loudly in test harness code
fn invalid_proptest_cases(value: &str) -> u32 {
    panic!(
        "PROPTEST_CASES must be an unsigned integer, got `{value}`. \
         Use a numeric floor such as `PROPTEST_CASES=256`."
    );
}

pub fn cfg(default_cases: u32) -> ProptestConfig {
    let cases = effective_cases(
        default_cases,
        std::env::var("PROPTEST_CASES").ok().as_deref(),
    );
    ProptestConfig {
        cases,
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}
