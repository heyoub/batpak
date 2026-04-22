#![deny(unused_must_use)]

use batpak::store::{AtLeastOnce, CheckpointId, IdempotencyKey, ObservedOnce};

fn main() {
    let _at_least_once = AtLeastOnce::new(CheckpointId::new("obs-must-use"));
    let _observed = ObservedOnce::new(
        AtLeastOnce::new(CheckpointId::new("obs-must-use")),
        IdempotencyKey::from_bytes([3; 32]),
    );
}
