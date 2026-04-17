//! Downstream path-hygiene fixture for `#[derive(EventPayload)]`.
//!
//! This crate depends on `batpak` the way an external user would: as a
//! normal path dependency, without any direct reference to
//! `batpak-macros` or `batpak-macros-support`. The derive's generated
//! `::batpak::...` paths must all resolve cleanly from here.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 10)]
pub struct OrderPlaced {
    pub order_id: u64,
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 11)]
pub struct OrderCancelled {
    pub order_id: u64,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use batpak::prelude::*;

    #[test]
    fn kind_constant_is_correct() {
        assert_eq!(OrderPlaced::KIND, EventKind::custom(2, 10));
        assert_eq!(OrderCancelled::KIND, EventKind::custom(2, 11));
    }

    #[test]
    fn derive_works_via_prelude_star_import() {
        // Prelude brings both the trait (type namespace) and the derive
        // (macro namespace) into scope under the same name.
        let _k: EventKind = OrderPlaced::KIND;
    }
}
