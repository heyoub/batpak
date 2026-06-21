# Lane Branch

Agent surface task: `lane_branch`.

Problem: write and read independent per-entity branches while keeping one store
timeline and one global sequence space.

Correct API: `AppendPositionHint::branch_root`, `Region::with_lane`,
`Store::stream_lane`, `Store::latest_lane`, `Store::query_lane`, and
`Store::project_fused2` / `Store::project_fused3` when multiple projections can
share one replay.

Minimal shape:

```rust
store.append(&coord, EventKind::DATA, &serde_json::json!({"lane": 0}))?;
store.append_with_options(
    &coord,
    EventKind::DATA,
    &serde_json::json!({"lane": 1}),
    AppendOptions::new().with_position_hint(AppendPositionHint::branch_root(1, 0)),
)?;

let lane_one = store.stream_lane(coord.entity(), 1);
let scoped = store.query(&Region::entity(coord.entity()).with_lane(1));
```

Lanes are substrate data. `u32` lane ids are not interpreted by batpak; callers
own any policy meaning above this layer. `Region { lane: None }` is the
compatibility read path.

Wrong tempting move: use one per-entity head and merely label events with lanes.
That flattens branches into one hash/clock chain and makes lanes cosmetic.

Test command: `cargo test -p batpak --test lane_branches --test per_lane_frontier --test projection_fusion`.

Invariant protected: INV-LANE-BRANCH-ISOLATION, INV-PER-LANE-FRONTIER,
INV-PROJECTION-FUSION-EQUIVALENT.
