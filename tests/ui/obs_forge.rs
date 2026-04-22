use batpak::store::delivery::observation::{AtLeastOnce, CheckpointId, IdempotencyKey, ObservedOnce};

fn main() {
    let _forged = ObservedOnce {
        _seal: (),
        at_least_once: AtLeastOnce::new(CheckpointId::new("obs-forge")),
        idempotency_key: IdempotencyKey::from_bytes([9; 32]),
    };
}
