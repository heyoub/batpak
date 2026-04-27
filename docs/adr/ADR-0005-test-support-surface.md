# ADR-0005: Test Support Feature Boundary

## Status
Accepted

## Context
Some tests need privileged hooks such as intentional writer panic injection.
Those hooks must remain feature-gated and explicit in the API surface, even
though this repository enables them in its default feature set so ordinary
`cargo test` exercises the crash-recovery and fault-injection harnesses.

## Decision
Privileged test hooks live behind the `dangerous-test-hooks` feature and are
exercised by targeted integration tests plus CI feature matrices. The feature is
enabled by default for this repository's developer/test profile; downstream
consumers that want the lean production surface can opt out with
`default-features = false`.

## Consequences
- Default repository tests see the hooks, so panic/fault recovery does not rely
  on a special local incantation.
- Downstream lean builds can omit test-only APIs by disabling default features.
- Feature-isolation CI validates both the default surface and opt-out surfaces.
- The repo can keep adversarial tests without widening the production surface.
