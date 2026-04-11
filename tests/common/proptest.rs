//! Project-wide proptest configuration.
//!
//! Failing seeds persist next to the test source (via `FileFailurePersistence::SourceParallel`)
//! so flakes are reproducible across CI cycles. Without this, every flake is one-shot:
//! the failing seed is lost the moment the process exits and can never be replayed.
//!
//! Call `common::proptest::cfg(N)` from any test file, where `N` is the default
//! number of cases for that file when `PROPTEST_CASES` is not set.

use proptest::prelude::ProptestConfig;
use proptest::test_runner::FileFailurePersistence;

/// Build a `ProptestConfig` with the project-wide settings.
///
/// - `cases`: reads `PROPTEST_CASES` from the environment; falls back to `default_cases`.
/// - `failure_persistence`: writes failing seeds to a `proptest-regressions/` directory
///   next to the test source so a flake can be reproduced deterministically. Without this,
///   every failure is one-shot and the seed is lost the moment the process exits.
pub fn cfg(default_cases: u32) -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default_cases);
    ProptestConfig {
        cases,
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}
