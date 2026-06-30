//! PROVES: `Store::open` defaults to `EventPayloadValidation::FailFast`, so a
//! binary that links an `EventPayload` declaring `#[batpak(version = N)]` with
//! `N > 1` but NO registered upcast chain REFUSES to open — naming the kind and
//! the missing hop(s) — instead of letting events stored at the older versions
//! silently become undecodable (`UpcastError::MissingStep`) at read time (W2).
//! CATCHES: a regression that drops the open-time upcast-chain completeness scan
//! (or re-defaults it to a looser policy), reopening the stranded-events footgun.
//! SEEDED: deterministic / no randomness.
//!
//! This file uses the REAL author-facing path: a `#[derive(EventPayload)]` with
//! `version = 2` and a deliberately-absent `register_upcast!`. That also proves
//! the derive stamps `payload_version` into the link-time registration (without
//! it, the open-time scan could not tell this kind is version 2).

use batpak_testkit::prelude::*;
use serde::{Deserialize, Serialize};

const STRANDED_CATEGORY: u8 = 0xE;
const STRANDED_TYPE_ID: u16 = 0x7A2;

/// A `version = 2` payload with NO `register_upcast!`: its registered chain is
/// empty, so the `1 -> 2` hop is missing. This is the W2 footgun verbatim. The
/// derived `Serialize`/`Deserialize` impls read `value`, so it is not dead.
#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x7A2, version = 2)]
struct StrandedV2 {
    value: u64,
}

/// The seeded incomplete chain must be visible to the binary-wide registry scan
/// (otherwise the open assertions below could pass vacuously). Returns the
/// missing hops the scanner reports for the seeded kind.
fn scanner_missing_hops_for_stranded() -> Vec<u16> {
    let err: UpcastChainRegistryError = validate_upcast_chain_registry()
        .expect_err("PROPERTY: a version=2 kind with no registered upcast must be reported");
    let chains: &[IncompleteUpcastChain] = err.incomplete_chains();
    chains
        .iter()
        .find(|chain| chain.kind == StrandedV2::KIND)
        .map(|chain| chain.missing_from_versions.clone())
        .unwrap_or_default()
}

#[test]
fn default_policy_fail_fast_refuses_open_on_incomplete_upcast_chain() {
    // Precondition: the seeded version=2-without-chain kind is actually linked
    // and seen by the binary-wide scanner, missing exactly hop 1 (the `1 -> 2`
    // step). Without this the open assertion could pass even if the scan were a
    // no-op.
    let missing = scanner_missing_hops_for_stranded();
    assert_eq!(
        missing,
        vec![1u16],
        "PROPERTY: the version=2 StrandedV2 kind must be reported missing exactly the 1->2 hop, got {missing:?}"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    // Default config: no `.with_event_payload_validation(...)`. The default is
    // `FailFast`, so the incomplete chain must make the open FAIL with an error
    // that names the seeded kind and its missing hop. `Store` is not `Debug`, so
    // inspect only the error half of the result.
    let open_error = Store::open(StoreConfig::new(dir.path())).err();
    assert!(
        matches!(
            open_error.as_ref(),
            Some(StoreError::UpcastChainIncomplete(registry_error))
                if registry_error.incomplete_chains().iter().any(|chain| {
                    chain.kind == StrandedV2::KIND
                        && chain.kind.category() == STRANDED_CATEGORY
                        && chain.kind.type_id() == STRANDED_TYPE_ID
                        && chain.current_version == 2
                        && chain.missing_from_versions == vec![1u16]
                })
        ),
        "PROPERTY: default-policy open must fail with StoreError::UpcastChainIncomplete naming the \
         ({STRANDED_CATEGORY:#X}, {STRANDED_TYPE_ID:#X}) kind declaring version 2 and missing the \
         1->2 hop, got {open_error:?}"
    );
}

#[test]
fn explicit_warn_opt_in_still_opens_on_incomplete_upcast_chain() {
    // The same incomplete chain stays linked; the loose log-and-proceed policy
    // is reachable only as an explicit opt-out and must still open.
    assert_eq!(
        scanner_missing_hops_for_stranded(),
        vec![1u16],
        "PROPERTY: the seeded incomplete chain must remain visible to the scanner"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path()).with_event_payload_validation(EventPayloadValidation::Warn),
    )
    .expect("explicit Warn opt-in opens despite the incomplete upcast chain");
    store.close().expect("close store");
}
