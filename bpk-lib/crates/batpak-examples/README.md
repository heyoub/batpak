# batpak-examples

Runnable entry points for the main `batpak` surfaces. Each program lives under
`src/bin/` as a Cargo binary target.

Run any demo with `cargo run -p batpak-examples --bin <name>` where `<name>` is
the file stem without `.rs`, for example `cargo run -p batpak-examples --bin quickstart`.

Build all demos: `cargo build -p batpak-examples --bins`.

## Start Here

- `eight_jobs.rs` - canonical full store path: open (with lifecycle observation), append, page, get, walk, verify, project, close. Run: `cargo run -p batpak-examples --bin eight_jobs`.
- `quickstart.rs` - minimal open, append, read flow (thin release smoke; see `eight_jobs` for the full path). Run: `cargo run -p batpak-examples --bin quickstart`.
- `batch_append.rs` - atomic multi-event append with intra-batch causation. Run: `cargo run -p batpak-examples --bin batch_append`.
- `caller_defined_gates.rs` - guard decisions before commit. Run: `cargo run -p batpak-examples --bin caller_defined_gates`.

## Cursor And Reactor Flows

- `cursor_worker.rs` - ordered pull delivery with optional checkpointing. Run: `cargo run -p batpak-examples --bin cursor_worker`.
- `typed_reactor.rs` - typed reaction loop for one event family. Run: `cargo run -p batpak-examples --bin typed_reactor`.
- `typed_reactor_multi.rs` - multi-event typed reactor dispatch. Run: `cargo run -p batpak-examples --bin typed_reactor_multi`.
- `outbox.rs` - cursor-driven side-effect handoff pattern. Run: `cargo run -p batpak-examples --bin outbox`.

## Durability And Visibility

- `append_with_gate.rs` - append-time gates, explicit durable waits, and visibility-fence publish. Run: `cargo run -p batpak-examples --bin append_with_gate`.
- `signed_receipts.rs` - signed append receipts and persisted denial receipts. Run: `cargo run -p batpak-examples --bin signed_receipts`.
- `read_only.rs` - side-effect-free read-only open. Run: `cargo run -p batpak-examples --bin read_only`.

## Projection And Performance Surfaces

- `event_sourced_counter.rs` - typed projection with derived replay logic. Run: `cargo run -p batpak-examples --bin event_sourced_counter`.
- `raw_projection_counter.rs` - hand-written raw projection. Run: `cargo run -p batpak-examples --bin raw_projection_counter`.
- `raw_projection_counter_derived.rs` - derived shape for the raw projection. Run: `cargo run -p batpak-examples --bin raw_projection_counter_derived`.

## Advanced Typestate

- `dungeon_typestate.rs` - typestate transition flow with compile-time shape. Run: `cargo run -p batpak-examples --bin dungeon_typestate`.
- `chat_room.rs` - larger end-to-end example that combines multiple surfaces. Run: `cargo run -p batpak-examples --bin chat_room`.
- `submit_pipeline.rs` - explicit submit pipeline and ticket handling. Run: `cargo run -p batpak-examples --bin submit_pipeline`.

## 0.9.0 Headline Features

- `fork_clone.rs` - fork a store into an isolated directory and reopen read-only. Run: `cargo run -p batpak-examples --bin fork_clone`.
- `import_events.rs` - re-apply events from a source store with import provenance. Run: `cargo run -p batpak-examples --bin import_events`.
- `lane_branch.rs` - append on independent DAG lanes for the same entity. Run: `cargo run -p batpak-examples --bin lane_branch`.
- `idempotent_pass.rs` - re-runnable durable idempotent append pass. Run: `cargo run -p batpak-examples --bin idempotent_pass`.
