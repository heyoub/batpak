use batpak::event::EventPayload as _;
use batpak::EventPayload;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x777)]
pub struct CollisionA {
    pub value: u64,
}

pub fn kind() -> batpak::event::EventKind {
    CollisionA::KIND
}
