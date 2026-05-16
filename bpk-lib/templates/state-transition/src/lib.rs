use batpak::transition::{
    build_state_transition_report, state_transition_report_body_hash, StateTransitionEvent,
    TransitionCauseRef, TransitionId, TransitionMachineId, TransitionSubjectId,
    STATE_TRANSITION_EVENT_SCHEMA_VERSION,
};

pub fn run() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let event = StateTransitionEvent {
        schema_version: STATE_TRANSITION_EVENT_SCHEMA_VERSION,
        machine_id: TransitionMachineId([1; 32]),
        subject_id: TransitionSubjectId([2; 32]),
        previous_state: 0,
        next_state: 1,
        transition_id: TransitionId([3; 32]),
        causes: vec![TransitionCauseRef {
            lane: 1,
            opaque_key: vec![1],
        }],
        ordering_sequence: Some(1),
        frontier_digest: None,
    };
    let report = build_state_transition_report(&event, &[(0, 1)])?;
    Ok(state_transition_report_body_hash(&report)?)
}
