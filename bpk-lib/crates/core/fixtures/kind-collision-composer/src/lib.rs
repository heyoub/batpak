#[cfg(test)]
mod tests {
    use batpak::prelude::*;

    #[test]
    fn dependency_payload_collision_is_visible_to_composer() {
        let _ = batpak_kind_collision_a::kind();
        let _ = batpak_kind_collision_b::kind();
        let _ = batpak_kind_collision_b::feature_collision_kind();
        let _ = batpak_kind_feature_split::always_kind();
        let _ = batpak_kind_feature_split::feature_kind();

        let err = match validate_event_payload_registry() {
            Ok(()) => panic!("expected composed dependency payload collision"),
            Err(err) => err,
        };
        assert!(
            err.collisions().iter().any(|collision| {
                collision.category == 0xE
                    && collision.type_id == 0x777
                    && collision.first_type_name.contains("Collision")
                    && collision.second_type_name.contains("Collision")
            }),
            "expected category/type collision from dependency crates, got {err:?}"
        );
        assert!(
            err.collisions().iter().any(|collision| {
                collision.category == 0xE
                    && collision.type_id == 0x778
                    && (collision.first_type_name.contains("Feature")
                        || collision.second_type_name.contains("Feature"))
            }),
            "expected feature-enabled payload collision from dependency crates, got {err:?}"
        );
    }

    #[test]
    fn default_warn_mode_does_not_fail_store_open() {
        let _ = batpak_kind_collision_a::kind();
        let _ = batpak_kind_collision_b::kind();
        let _ = batpak_kind_feature_split::feature_kind();

        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("warn mode open");
        store.close().expect("close store");
    }

    #[test]
    fn fail_fast_mode_rejects_store_open() {
        let _ = batpak_kind_collision_a::kind();
        let _ = batpak_kind_collision_b::kind();
        let _ = batpak_kind_feature_split::feature_kind();

        let dir = tempfile::tempdir().expect("temp dir");
        let config =
            StoreConfig::new(dir.path()).with_event_payload_validation(EventPayloadValidation::FailFast);
        let err = match Store::open(config) {
            Ok(_) => panic!("expected fail-fast payload registry error"),
            Err(err) => err,
        };
        assert!(
            matches!(err, StoreError::EventPayloadRegistry(_)),
            "expected payload registry error, got {err:?}"
        );
    }
}
