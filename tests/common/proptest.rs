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
///   `default_cases` and never lowers a file's coded default.
/// - `failure_persistence`: writes failing seeds to a `proptest-regressions/` directory
///   next to the test source so a flake can be reproduced deterministically. Without this,
///   every failure is one-shot and the seed is lost the moment the process exits.
pub(crate) fn effective_cases(default_cases: u32, env_value: Option<&str>) -> u32 {
    env_value
        .and_then(|value| value.parse::<u32>().ok())
        .map_or(default_cases, |cases| cases.max(default_cases))
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
