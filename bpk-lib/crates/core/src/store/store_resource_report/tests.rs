use super::store_data_dir_identity_hash;

#[test]
fn data_dir_identity_hash_canonicalizes_existing_path_spellings() {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let raw_spelling = dir.path().join(".");
    let canonical =
        crate::store::platform::fs::canonicalize(dir.path()).expect("canonicalize temp dir");

    assert_eq!(
        store_data_dir_identity_hash(&raw_spelling),
        store_data_dir_identity_hash(&canonical)
    );
}
