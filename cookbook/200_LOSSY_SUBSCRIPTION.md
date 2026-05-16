# Lossy Subscription

Agent surface tasks: `lossy_subscription`, `subscriber_frontier_evidence`.

Problem: observe live events when push delivery is allowed to be lossy.

Correct API: `Store::subscribe`, subscription delivery state, and
`SubscriberFrontierEvidenceReport`.

Minimal code is mirrored by `bpk-lib/crates/core/examples/outbox.rs`.

Wrong tempting move: use subscription state as durable replay proof.

Test command: `cargo test -p batpak --test subscriber_frontier_observations --all-features`.

Invariant protected: unknown loss precision remains explicit and is not emitted
as observed loss.
