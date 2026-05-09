#[test]
fn template_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_ne!(batpak_template_reservation_ledger::run()?, [0_u8; 32]);
    Ok(())
}
