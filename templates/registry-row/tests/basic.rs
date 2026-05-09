#[test]
fn template_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_ne!(batpak_template_registry_row::run()?, [0_u8; 32]);
    Ok(())
}
