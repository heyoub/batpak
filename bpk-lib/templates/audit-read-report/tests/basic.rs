#[test]
fn template_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_ne!(batpak_template_audit_read_report::run()?, [0_u8; 32]);
    Ok(())
}
