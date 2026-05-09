# State Transition

Agent surface task: `state_transition`.

Problem: evaluate generic state transitions with deterministic causes and
allowed-edge evidence.

Correct API: `StateTransitionEvent`, `build_state_transition_report`,
`state_transition_report_body_hash`.

Minimal code is mirrored by `templates/state-transition`.

Wrong tempting move: put app workflow names or domain state labels into batpak
core.

Test command: `cargo test -p batpak --test lane_b3_transition_substrate --all-features`.

Invariant protected: transition causes sort deterministically and disallowed
edges become findings.
