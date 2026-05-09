#[test]
fn template_runs() -> Result<(), Box<dyn std::error::Error>> {
    let event_id = batpak_template_minimal_store::run()?;
    assert_ne!(event_id, 0);
    Ok(())
}
