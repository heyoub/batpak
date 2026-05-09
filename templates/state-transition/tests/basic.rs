#[test]
fn template_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_ne!(batpak_template_state_transition::run()?, [0_u8; 32]);
    Ok(())
}
