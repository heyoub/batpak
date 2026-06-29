//! Shared chaos-iteration depth helpers for the chaos_testing test family.
//!
//! Included via `#[path = "support/chaos_testing.rs"]` by every chaos_testing*
//! binary that needs the repo-standard iteration depth. The depth is read once
//! from `CHAOS_ITERATIONS` (default 500) so every chaos lane scales together.
//! Each consumer uses these functions directly, keeping the `dead_code` surface
//! honest the same way `support/small_store.rs` does (see ADR-0012).

/// Repo-truth default chaos iteration count when `CHAOS_ITERATIONS` is unset.
pub const DEFAULT_CHAOS_ITERATIONS: usize = 500;

/// Resolve the effective iteration depth from an optional env value.
///
/// Unparseable or absent values fall back to [`DEFAULT_CHAOS_ITERATIONS`]; a
/// parsed `0` is clamped up to `1` so no chaos lane runs vacuously.
pub fn effective_chaos_iterations(env_value: Option<&str>) -> usize {
    env_value
        .and_then(|value| value.parse::<usize>().ok())
        .map(|iterations| iterations.max(1))
        .unwrap_or(DEFAULT_CHAOS_ITERATIONS)
}

/// Effective chaos iteration depth, reading the live `CHAOS_ITERATIONS` env var.
pub fn chaos_iterations() -> usize {
    effective_chaos_iterations(std::env::var("CHAOS_ITERATIONS").ok().as_deref())
}
