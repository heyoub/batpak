//! Wire payload and `to_core()` mapping tests for `hbat::evidence`.

use anyhow::Result;
use batpak::store::{ChainWalkStartRef, Freshness};
use hbat::evidence::{
    ChainWalkEvidenceAck, ChainWalkEvidenceRequest, EvidenceRequestError, ProjectionRunEvidenceAck,
    ProjectionRunEvidenceRequest, ReadWalkEvidenceAck, ReadWalkEvidenceRequest,
    StoreResourceEvidenceAck, StoreResourceEvidenceRequest, EVIDENCE_MAX_LIMIT,
    EVIDENCE_READ_WALK_PROOF_MAX_LIMIT,
};
use hbat::EventPayloadFixture;

#[test]
fn chain_walk_request_fixture_roundtrips() -> Result<()> {
    let value = ChainWalkEvidenceRequest::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: ChainWalkEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn chain_walk_ack_fixture_roundtrips() -> Result<()> {
    let value = ChainWalkEvidenceAck::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: ChainWalkEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn chain_walk_request_maps_event_id_start() -> Result<()> {
    let core = ChainWalkEvidenceRequest::fixture_value().to_core()?;
    assert!(matches!(core.start, ChainWalkStartRef::EventId(_)));
    assert_eq!(core.limit, 16);
    assert_eq!(core.mode, batpak::store::ChainWalkMode::Linear);
    Ok(())
}

#[test]
fn chain_walk_request_maps_receipt_start_when_hash_present() -> Result<()> {
    let request = ChainWalkEvidenceRequest {
        start_expected_hash_hex: Some(
            "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        ),
        ..ChainWalkEvidenceRequest::fixture_value()
    };
    let core = request.to_core()?;
    assert!(matches!(core.start, ChainWalkStartRef::Receipt { .. }));
    Ok(())
}

#[test]
fn chain_walk_request_bounds_limit() -> Result<()> {
    let request = ChainWalkEvidenceRequest {
        limit: EVIDENCE_MAX_LIMIT * 4,
        ..ChainWalkEvidenceRequest::fixture_value()
    };
    let core = request.to_core()?;
    assert_eq!(core.limit, usize::try_from(EVIDENCE_MAX_LIMIT)?);
    Ok(())
}

#[test]
fn chain_walk_request_rejects_zero_limit() {
    let request = ChainWalkEvidenceRequest {
        limit: 0,
        ..ChainWalkEvidenceRequest::fixture_value()
    };
    assert_eq!(
        request.to_core().expect_err("zero limit must be rejected"),
        EvidenceRequestError::ZeroLimit
    );
}

#[test]
fn store_resource_request_fixture_roundtrips() -> Result<()> {
    let value = StoreResourceEvidenceRequest::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: StoreResourceEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn store_resource_ack_fixture_roundtrips() -> Result<()> {
    let value = StoreResourceEvidenceAck::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: StoreResourceEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn read_walk_request_fixture_roundtrips() -> Result<()> {
    let value = ReadWalkEvidenceRequest::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: ReadWalkEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn read_walk_ack_fixture_roundtrips() -> Result<()> {
    let value = ReadWalkEvidenceAck::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: ReadWalkEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn read_walk_request_rejects_zero_limit() {
    let request = ReadWalkEvidenceRequest {
        limit: Some(0),
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    assert_eq!(
        request.to_core().expect_err("zero limit must be rejected"),
        EvidenceRequestError::ZeroLimit
    );
}

#[test]
fn read_walk_request_maps_kind_category() -> Result<()> {
    let request = ReadWalkEvidenceRequest {
        kind_category: Some(0x1),
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    let core = request.to_core()?;
    assert!(matches!(
        core.region.fact(),
        Some(batpak::coordinate::KindFilter::Category(0x1))
    ));
    Ok(())
}

#[test]
fn read_walk_request_rejects_kind_type_without_category() {
    let request = ReadWalkEvidenceRequest {
        kind_category: None,
        kind_type_id: Some(1),
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    assert!(matches!(
        request.to_core().expect_err("kind without category"),
        EvidenceRequestError::InvalidKind {
            field: "kind_type_id",
            ..
        }
    ));
}

#[test]
fn read_walk_request_maps_clock_range() -> Result<()> {
    let request = ReadWalkEvidenceRequest {
        start_clock: Some(1),
        end_clock: Some(5),
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    let core = request.to_core()?;
    assert_eq!(core.region.clock_range(), Some((1, 5)));
    Ok(())
}

#[test]
fn read_walk_request_rejects_partial_clock_range() {
    let request = ReadWalkEvidenceRequest {
        start_clock: Some(1),
        end_clock: None,
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    assert!(matches!(
        request.to_core().expect_err("partial clock range"),
        EvidenceRequestError::InvalidClockRange { .. }
    ));
}

#[test]
fn read_walk_request_maps_freshness() {
    let consistent = ReadWalkEvidenceRequest::fixture_value();
    assert!(matches!(
        consistent.to_core().expect("consistent").freshness_intent,
        Freshness::Consistent
    ));
    let stale = ReadWalkEvidenceRequest {
        max_stale_ms: Some(250),
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    assert!(matches!(
        stale.to_core().expect("stale").freshness_intent,
        Freshness::MaybeStale { max_stale_ms: 250 }
    ));
}

#[test]
fn read_walk_request_bounds_limit_without_proof_refs() -> Result<()> {
    let request = ReadWalkEvidenceRequest {
        entity: Some("fixture:bank".to_owned()),
        scope: Some("fixture-scope".to_owned()),
        limit: Some(EVIDENCE_MAX_LIMIT * 4),
        include_proof_refs: false,
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    let core = request.to_core()?;
    assert_eq!(core.limit, Some(usize::try_from(EVIDENCE_MAX_LIMIT)?));
    assert!(!core.include_proof_refs);
    Ok(())
}

#[test]
fn read_walk_request_uses_tighter_bound_with_proof_refs() -> Result<()> {
    let capped = ReadWalkEvidenceRequest {
        entity: Some("fixture:bank".to_owned()),
        scope: None,
        limit: Some(EVIDENCE_MAX_LIMIT),
        include_proof_refs: true,
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    assert_eq!(
        capped.to_core()?.limit,
        Some(usize::try_from(EVIDENCE_READ_WALK_PROOF_MAX_LIMIT)?)
    );

    let unbounded = ReadWalkEvidenceRequest {
        entity: None,
        scope: None,
        limit: None,
        include_proof_refs: true,
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    assert_eq!(
        unbounded.to_core()?.limit,
        Some(usize::try_from(EVIDENCE_READ_WALK_PROOF_MAX_LIMIT)?)
    );
    Ok(())
}

#[test]
fn read_walk_request_bounds_omitted_limit() -> Result<()> {
    let request = ReadWalkEvidenceRequest {
        entity: None,
        scope: None,
        limit: None,
        include_proof_refs: false,
        ..ReadWalkEvidenceRequest::fixture_value()
    };
    let core = request.to_core()?;
    assert_eq!(core.limit, Some(usize::try_from(EVIDENCE_MAX_LIMIT)?));
    Ok(())
}

#[test]
fn projection_run_request_fixture_roundtrips() -> Result<()> {
    let value = ProjectionRunEvidenceRequest::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: ProjectionRunEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn projection_run_ack_fixture_roundtrips() -> Result<()> {
    let value = ProjectionRunEvidenceAck::fixture_value();
    let bytes = batpak::encoding::to_bytes(&value)?;
    let decoded: ProjectionRunEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
    assert_eq!(decoded, value);
    Ok(())
}

#[test]
fn projection_run_request_maps_freshness() {
    let consistent = ProjectionRunEvidenceRequest::fixture_value();
    assert!(matches!(consistent.freshness(), Freshness::Consistent));
    let stale = ProjectionRunEvidenceRequest {
        max_stale_ms: Some(250),
        ..ProjectionRunEvidenceRequest::fixture_value()
    };
    assert!(matches!(
        stale.freshness(),
        Freshness::MaybeStale { max_stale_ms: 250 }
    ));
}
