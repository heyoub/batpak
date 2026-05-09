use batpak::reservation::{
    reservation_ledger_report_body_hash, simulate_reservation_ledger, ReservationId,
    ReservationQuantity, ReservationSubjectRef, ReservationTransition, RESERVATION_OP_RESERVE,
    RESERVATION_TRANSITION_SCHEMA_VERSION,
};

pub fn run() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let tx = ReservationTransition {
        schema_version: RESERVATION_TRANSITION_SCHEMA_VERSION,
        sequence: 1,
        reservation_id: ReservationId([1; 32]),
        op: RESERVATION_OP_RESERVE,
        quantity_units: 1,
        subject: Some(ReservationSubjectRef {
            namespace: 1,
            key_bytes: b"template".to_vec(),
        }),
        cause_refs: Vec::new(),
    };
    let report = simulate_reservation_ledger(&[tx])?;
    let _quantity: ReservationQuantity = report.entries_sorted[0].quantity;
    Ok(reservation_ledger_report_body_hash(&report)?)
}
