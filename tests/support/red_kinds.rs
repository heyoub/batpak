//! Kind-A + JSON payload helpers for the unified red suite.
//!
//! Included via `#[path = "support/red_kinds.rs"]` by every unified red test
//! that writes at least one event. `kind_a()` and `payload()` are the
//! minimum shared surface, so every consumer uses both — no dead_code
//! pressure (see ADR-0012).

use batpak::prelude::EventKind;

/// Red-test kind A — a custom user-category event kind.
pub fn kind_a() -> EventKind {
    EventKind::custom(0xF, 1)
}

/// Red-test JSON payload: `{"i": <i>}`.
pub fn payload(i: u32) -> serde_json::Value {
    serde_json::json!({ "i": i })
}
