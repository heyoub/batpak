# Examples

These examples are runnable entry points for the main `batpak` surfaces.

Run any example with `cargo run --example <name>` where `<name>` is the file
stem without `.rs`, for example `cargo run --example quickstart`.

## Start Here

- `quickstart.rs` - minimal open, append, read flow. Run: `cargo run --example quickstart`.
- `batch_append.rs` - atomic multi-event append with intra-batch causation. Run: `cargo run --example batch_append`.
- `caller_defined_gates.rs` - guard decisions before commit. Run: `cargo run --example caller_defined_gates`.
- `cross_crate_payloads.rs` - startup validation for composed typed payload crates. Run: `cargo run --example cross_crate_payloads`.

## Cursor And Reactor Flows

- `cursor_worker.rs` - ordered pull delivery with optional checkpointing. Run: `cargo run --example cursor_worker`.
- `typed_reactor.rs` - typed reaction loop for one event family. Run: `cargo run --example typed_reactor`.
- `typed_reactor_multi.rs` - multi-event typed reactor dispatch. Run: `cargo run --example typed_reactor_multi`.
- `outbox.rs` - cursor-driven side-effect handoff pattern. Run: `cargo run --example outbox`.

## Durability And Visibility

- `wait_for_durable.rs` - explicit wait for durable frontier advancement. Run: `cargo run --example wait_for_durable`.
- `append_with_gate.rs` - append-time durability, visibility, or applied gates. Run: `cargo run --example append_with_gate`.
- `signed_receipts.rs` - signed append receipts and persisted denial receipts. Run: `cargo run --example signed_receipts`.
- `lifecycle_observer.rs` - durable `SYSTEM_OPEN_COMPLETED` lifecycle event observation. Run: `cargo run --example lifecycle_observer`.
- `visibility_fence.rs` - hidden work made visible after explicit commit. Run: `cargo run --example visibility_fence`.
- `read_only.rs` - side-effect-free read-only open. Run: `cargo run --example read_only`.

## Projection And Performance Surfaces

- `event_sourced_counter.rs` - typed projection with derived replay logic. Run: `cargo run --example event_sourced_counter`.
- `raw_projection_counter.rs` - hand-written raw projection. Run: `cargo run --example raw_projection_counter`.
- `raw_projection_counter_derived.rs` - derived shape for the raw projection. Run: `cargo run --example raw_projection_counter_derived`.

## Advanced Typestate

- `dungeon_typestate.rs` - typestate transition flow with compile-time shape. Run: `cargo run --example dungeon_typestate`.
- `chat_room.rs` - larger end-to-end example that combines multiple surfaces. Run: `cargo run --example chat_room`.
- `submit_pipeline.rs` - explicit submit pipeline and ticket handling. Run: `cargo run --example submit_pipeline`.
