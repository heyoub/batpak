#[test]
fn template_runs() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(batpak_template_typed_reactor::run()?, 1);
    Ok(())
}
