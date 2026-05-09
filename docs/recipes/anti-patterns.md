# Anti-Patterns

Agent surface task: every task in `traceability/agent_surface.yaml`.

Bad shape: add `tokio`, `async-std`, or `smol` to production batpak.
Correct replacement: keep batpak sync-only; put async adapters above batpak.
Invariant: `INV-STORE-SYNC-ONLY`.
Detector: `cargo xtask structural`, `cargo xtask agent-doctor`.

Bad shape: use `serde_json::Value` for a known durable typed event.
Correct replacement: derive `EventPayload` and call `Store::append_typed`.
Invariant: typed payload kind allocation is collision-checked.
Detector: recipe and template rails.

Bad shape: write `Unknown` because the mechanism was not wired.
Correct replacement: wire `Known` when batpak owns the fact; use `Unavailable`
for deterministic acquisition failure and `NotApplicable` when the field does
not apply.
Invariant: no fake uncertainty.
Detector: evidence report tests and `cargo xtask evidence-audit`.

Bad shape: hand-roll `body_hash`.
Correct replacement: use the canonical body helper owned by the module.
Invariant: deterministic body identity.
Detector: evidence report tests and mutation policy.

Bad shape: expose domain/product vocabulary in substrate APIs.
Correct replacement: keep domain meaning above batpak and name core types by
generic substrate physics.
Invariant: domain-free substrate surface.
Detector: `cargo xtask agent-doctor`.

Bad shape: use debug/display strings as identity.
Correct replacement: use newtypes, canonical body structs, and sorted canonical
bytes.
Invariant: canonical identity is not presentation.
Detector: module tests and structural review.

Bad shape: add an unbounded public read API.
Correct replacement: require `Region`, `Coordinate`, cursor, or a named evidence
request.
Invariant: bounded public read discipline.
Detector: agent surface recipes and store surface tests.

Bad shape: create `common.rs`, `utils.rs`, `helpers.rs`, or `misc.rs`.
Correct replacement: name the owner surface, such as `repo_surface.rs`,
`public_surface.rs`, `frontier.rs`, or `findings.rs`.
Invariant: one owner per file family; facade files stay thin.
Detector: code review plus structural naming checks when promoted.
