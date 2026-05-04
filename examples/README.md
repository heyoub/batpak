# Examples

These examples are runnable entry points for the main `batpak` surfaces.

Run any example with `cargo run --example <name>` where `<name>` is the file
stem without `.rs`, for example `cargo run --example quickstart`.

## Start Here

- `quickstart.rs` - minimal open, append, read flow.
- `batch_append.rs` - atomic multi-event append with intra-batch causation.
- `policy_gates.rs` - guard decisions before commit.

## Cursor And Reactor Flows

- `cursor_worker.rs` - ordered pull delivery with optional checkpointing.
- `typed_reactor.rs` - typed reaction loop for one event family.
- `typed_reactor_multi.rs` - multi-event typed reactor dispatch.
- `outbox.rs` - cursor-driven side-effect handoff pattern.

## Durability And Visibility

- `wait_for_durable.rs` - explicit wait for durable frontier advancement.
- `append_with_gate.rs` - append-time durability, visibility, or applied gates.
- `visibility_fence.rs` - hidden work made visible after explicit commit.
- `read_only.rs` - side-effect-free read-only open.

## Projection And Performance Surfaces

- `event_sourced_counter.rs` - typed projection with derived replay logic.
- `raw_projection_counter.rs` - hand-written raw projection.
- `raw_projection_counter_derived.rs` - derived shape for the raw projection.

## Advanced Typestate

- `dungeon_typestate.rs` - typestate transition flow with compile-time shape.
- `chat_room.rs` - larger end-to-end example that combines multiple surfaces.
- `submit_pipeline.rs` - explicit submit pipeline and ticket handling.
