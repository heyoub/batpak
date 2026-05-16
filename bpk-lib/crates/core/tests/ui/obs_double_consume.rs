use batpak::store::{AtLeastOnce, CheckpointId, IdempotencyKey, ObservedOnce};

fn main() {
    let _observed = ObservedOnce::new(
        AtLeastOnce::new(CheckpointId::new("obs-double-consume")),
        IdempotencyKey::from_bytes([7; 32]),
    );
}
