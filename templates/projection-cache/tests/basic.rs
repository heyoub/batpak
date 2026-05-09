#[test]
fn template_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert!(batpak_template_projection_cache::run()?);
    Ok(())
}
