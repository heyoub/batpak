# Build With Batpak

Agent surface task: all tasks in `traceability/agent_surface.yaml`.

1. Open `Store` with `StoreConfig`.
2. Define typed payloads with `#[derive(EventPayload)]`.
3. Append through `Store::append_typed`.
4. Read through `Region` or typed query helpers.
5. Use `cursor_guaranteed` / typed reactor surfaces for replay.
6. Build projections through projection APIs, not local ordering logic.
7. Produce evidence with read, chain, subscriber, projection, and schema report APIs.
8. Use artifact, registry, backup, transition, and reservation substrates for generic proof objects.

Correct API:

```rust
use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xA, type_id = 1)]
struct ItemRecorded {
    value: u64,
}
```

Wrong tempting move: start with raw JSON and invented string identities for a
typed event stream. Use raw events only when the kind is genuinely dynamic.

Test command: `cargo test -p batpak --test event_payload_surface --all-features`.

Invariant protected: typed event identity is declared at the payload boundary and
collision-checked by the registry.
