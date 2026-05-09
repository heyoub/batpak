# Typed Event Store

Agent surface task: `append_typed_event`.

Problem: store a durable typed event without retyping its `EventKind` at every
callsite.

Correct API: `#[derive(EventPayload)]`, `Coordinate`, `Store::append_typed`.

Minimal code is mirrored by `templates/minimal-store` and
`crates/core/examples/quickstart.rs`.

Wrong tempting move: `serde_json::Value` plus a hand-picked `EventKind` for a
payload whose type is known at compile time.

Test command: `cargo test -p batpak --test event_payload_surface --all-features`.

Invariant protected: payload type and durable kind stay bound together.
