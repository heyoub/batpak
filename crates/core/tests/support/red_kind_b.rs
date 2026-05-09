//! Kind-B helper for the unified red tests that need a second event kind.
//!
//! Included via `#[path = "support/red_kind_b.rs"]` only by tests that
//! distinguish two kinds (kind-filter, mixed-fact queries). Splitting
//! `kind_b` out keeps `kind_a`-only consumers dead_code-clean
//! (see ADR-0012).

use batpak::prelude::EventKind;

/// Red-test kind B — a second custom user-category event kind for
/// kind-filter and mixed-fact tests.
pub fn kind_b() -> EventKind {
    EventKind::custom(0xF, 2)
}
