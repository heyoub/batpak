//! Receipt signing-policy red fixtures (W1 verifiability).
//!
//! `SigningPolicy::Required` is the regulated/rigor opt-in: a store that cannot
//! sign must refuse to open rather than silently produce unsigned receipts that
//! "verify" as valid. `Optional` (the default) permits a keyless "regular
//! store" whose receipts verify structurally but are never `is_signed`.

use batpak::store::{ReceiptVerification, SigningKey, SigningPolicy, Store, StoreConfig};
use tempfile::TempDir;

#[test]
fn signing_required_refuses_to_open_a_keyless_store() {
    let dir = TempDir::new().expect("temp dir");
    // RED: Required + no signing key must fail closed at open — never produce a
    // keyless store whose unsigned receipts are accepted as valid.
    let opened =
        Store::open(StoreConfig::new(dir.path()).with_signing_policy(SigningPolicy::Required));
    assert!(
        opened.is_err(),
        "SigningPolicy::Required with no signing key must refuse to open"
    );
}

#[test]
fn signing_required_opens_once_a_key_is_configured() {
    let dir = TempDir::new().expect("temp dir");
    let opened = Store::open(
        StoreConfig::new(dir.path())
            .with_signing_policy(SigningPolicy::Required)
            .with_signing_key(SigningKey::from_bytes([7u8; 32])),
    );
    assert!(
        opened.is_ok(),
        "SigningPolicy::Required with a signing key must open"
    );
}

#[test]
fn is_signed_separates_cryptographic_proof_from_mere_validity() {
    // Proof of authenticity is `is_signed`, NOT `is_valid`: an unsigned receipt
    // accepted only because the store has no keys is valid-for-the-store but
    // carries no signature.
    assert!(ReceiptVerification::Signed.is_signed());
    assert!(!ReceiptVerification::UnsignedAccepted.is_signed());
    assert!(ReceiptVerification::UnsignedAccepted.is_valid());
}

#[test]
fn signing_downgrade_is_opt_in() {
    let dir = TempDir::new().expect("temp dir");
    // Best-effort downgrade on cover-build failure is an explicit opt-in; a
    // keyed store that enables it still opens. The default is fail-closed
    // (proven directly by the signing-registry unit test).
    let opened = Store::open(
        StoreConfig::new(dir.path())
            .with_signing_key(SigningKey::from_bytes([3u8; 32]))
            .with_signing_downgrade_allowed(true),
    );
    assert!(
        opened.is_ok(),
        "a keyed store with downgrade allowed must open"
    );
}
