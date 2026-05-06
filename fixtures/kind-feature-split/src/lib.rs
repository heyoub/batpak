use batpak::event::EventPayload as _;
use batpak::EventPayload;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x701)]
pub struct AlwaysPresentPayload {
    pub value: u64,
}

#[cfg(feature = "extra-payload")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x778)]
pub struct FeaturePayload {
    pub value: u64,
}

pub fn always_kind() -> batpak::event::EventKind {
    AlwaysPresentPayload::KIND
}

#[cfg(feature = "extra-payload")]
pub fn feature_kind() -> batpak::event::EventKind {
    FeaturePayload::KIND
}
