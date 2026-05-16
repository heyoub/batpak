use batpak::event::EventPayload as _;
use batpak::EventPayload;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x777)]
pub struct CollisionB {
    pub label: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x778)]
pub struct FeatureCollisionB {
    pub label: String,
}

pub fn kind() -> batpak::event::EventKind {
    CollisionB::KIND
}

pub fn feature_collision_kind() -> batpak::event::EventKind {
    FeatureCollisionB::KIND
}
