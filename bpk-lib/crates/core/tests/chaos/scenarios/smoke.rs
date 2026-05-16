use crate::chaos::dm_flakey::FlakeyDevice;

fn chaos_enabled() -> bool {
    std::env::var_os("BATPAK_RUN_CHAOS").is_some()
}

#[test]
fn dm_flakey_wrapper_create_flip_teardown_round_trip() {
    if !chaos_enabled() {
        eprintln!("skipping privileged dm-flakey smoke; set BATPAK_RUN_CHAOS=1 to run it");
        return;
    }

    let device = FlakeyDevice::create(64 * 1024 * 1024).expect("create flakey device");
    device
        .format_and_mount_ext4_with_sync()
        .expect("format and mount");

    let test_file = device.mount_point.join("test.bin");
    std::fs::write(&test_file, b"before flip").expect("write before flip");

    device.flip_to_error().expect("flip");

    let after = std::fs::write(&test_file, b"after flip");
    assert!(after.is_err(), "PROPERTY: writes after flip must fail");
}
