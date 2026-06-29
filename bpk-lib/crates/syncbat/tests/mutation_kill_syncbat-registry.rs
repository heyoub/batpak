//! PROVES: SubscriptionRoute equality, accessor, Debug, and registry route /
//! subscription-id validation behave exactly as written.
//! CATCHES: diff-scoped mutation survivors in
//! `crates/syncbat/src/subscription_runtime/registry.rs` (event_category,
//! Debug fmt, PartialEq + per-variant `*_eq` helpers, `freshness_same`, and the
//! `validate_*` route/id grammar functions reachable through the public API).

use std::sync::Arc;

use batpak::store::Freshness;
use batpak_testkit::red_counters::AllCounter;
use syncbat::subscription_runtime::ProjectionProjector;
use syncbat::OperationName;
use syncbat::{
    SubscriptionId, SubscriptionRegistry, SubscriptionRoute, SubscriptionRuntimeError,
    TypedProjectionProjector,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn projector() -> Arc<dyn ProjectionProjector> {
    Arc::new(TypedProjectionProjector::<AllCounter>::new())
}

fn ec(category: u8, wire: &str, inner: Option<&str>, cap: Option<usize>) -> SubscriptionRoute {
    SubscriptionRoute::EventCategory {
        category,
        wire_payload_schema_ref: wire.to_owned(),
        inner_event_payload_schema_ref: inner.map(str::to_owned),
        backpressure_capacity: cap,
    }
}

fn es(
    entity: &str,
    scope: &str,
    wire: &str,
    inner: Option<&str>,
    cap: Option<usize>,
) -> SubscriptionRoute {
    SubscriptionRoute::EntityStream {
        entity: entity.to_owned(),
        scope: scope.to_owned(),
        wire_payload_schema_ref: wire.to_owned(),
        inner_event_payload_schema_ref: inner.map(str::to_owned),
        backpressure_capacity: cap,
    }
}

fn proj(
    projection_id: &str,
    entity: &str,
    wire: &str,
    inner: Option<&str>,
    freshness: Freshness,
    cap: Option<usize>,
) -> SubscriptionRoute {
    SubscriptionRoute::Projection {
        projection_id: projection_id.to_owned(),
        entity: entity.to_owned(),
        wire_payload_schema_ref: wire.to_owned(),
        inner_projection_schema_ref: inner.map(str::to_owned),
        freshness,
        backpressure_capacity: cap,
        projector: projector(),
    }
}

fn ops(
    operation: OperationName,
    entity: &str,
    wire: &str,
    inner: Option<&str>,
    freshness: Freshness,
    cap: Option<usize>,
) -> SubscriptionRoute {
    SubscriptionRoute::OperationStatus {
        operation,
        entity: entity.to_owned(),
        wire_payload_schema_ref: wire.to_owned(),
        inner_status_schema_ref: inner.map(str::to_owned),
        freshness,
        backpressure_capacity: cap,
    }
}

fn rcpt(
    receipt_kind: &str,
    wire: &str,
    inner: Option<&str>,
    cap: Option<usize>,
) -> SubscriptionRoute {
    SubscriptionRoute::ReceiptStream {
        receipt_kind: receipt_kind.to_owned(),
        wire_payload_schema_ref: wire.to_owned(),
        inner_receipt_schema_ref: inner.map(str::to_owned),
        backpressure_capacity: cap,
    }
}

fn op(name: &str) -> Result<OperationName, Box<dyn std::error::Error>> {
    Ok(OperationName::new(name)?)
}

// ---- event_category accessor (line 166) -------------------------------------

#[test]
fn event_category_accessor_returns_real_category_and_none() -> TestResult {
    // Some(7) kills `-> None`, `-> Some(0)`, `-> Some(1)`.
    assert_eq!(ec(7, "w.ec", None, None).event_category(), Some(7));
    // A non-EventCategory route yields None; kills `-> Some(0)` / `-> Some(1)`
    // (which would force Some on every variant).
    assert_eq!(
        proj("p", "e", "w.pr", None, Freshness::Consistent, None).event_category(),
        None
    );
    assert_eq!(rcpt("k", "w.rc", None, None).event_category(), None);
    // The remaining non-EventCategory variants must also yield None, so a
    // variant-specific regression in OperationStatus or EntityStream is caught.
    assert_eq!(
        ops(
            op("mod.a.echo")?,
            "ent",
            "w.op",
            None,
            Freshness::Consistent,
            None
        )
        .event_category(),
        None
    );
    assert_eq!(es("e", "s", "w.es", None, None).event_category(), None);
    Ok(())
}

// ---- Debug fmt (line 286) ---------------------------------------------------

#[test]
fn debug_fmt_renders_variant_and_fields() {
    // `Ok(Default::default())` mutant produces an empty render.
    let rendered = format!("{:?}", ec(5, "wire.ref", Some("inner.ref"), Some(9)));
    assert!(rendered.contains("EventCategory"), "render: {rendered}");
    assert!(rendered.contains("category"), "render: {rendered}");
    assert!(rendered.contains('5'), "render: {rendered}");
    assert!(rendered.contains("wire.ref"), "render: {rendered}");

    let proj_render = format!(
        "{:?}",
        proj("pid", "ent", "w.pr", None, Freshness::Consistent, None)
    );
    assert!(proj_render.contains("Projection"), "render: {proj_render}");
}

// ---- distinct variants are never equal (kills every `*_eq -> true` and
//      `eq -> true`) -------------------------------------------------------

#[test]
fn distinct_variants_are_never_equal() -> TestResult {
    let routes = [
        ec(1, "w.ec", None, None),
        es("e", "s", "w.es", None, None),
        proj("p", "e", "w.pr", None, Freshness::Consistent, None),
        ops(
            op("mod.a.echo")?,
            "ent",
            "w.op",
            None,
            Freshness::Consistent,
            None,
        ),
        rcpt("k", "w.rc", None, None),
    ];
    for (i, left) in routes.iter().enumerate() {
        for (j, right) in routes.iter().enumerate() {
            if i != j {
                assert_ne!(left, right, "variants {i} and {j} compared equal");
            }
        }
    }
    Ok(())
}

// ---- event_category_eq (lines 383-401) --------------------------------------

#[test]
fn event_category_eq_is_field_sensitive() {
    let base = ec(3, "wire.ref", Some("inner.ref"), Some(8));
    // equal pair: kills `-> false`, `-> true`-via-eq, delete-arm, and the
    // PartialEq `||` (line 372) swap.
    assert_eq!(base, ec(3, "wire.ref", Some("inner.ref"), Some(8)));
    assert_ne!(base, ec(4, "wire.ref", Some("inner.ref"), Some(8))); // category
    assert_ne!(base, ec(3, "other.ref", Some("inner.ref"), Some(8))); // wire
    assert_ne!(base, ec(3, "wire.ref", Some("other"), Some(8))); // inner
    assert_ne!(base, ec(3, "wire.ref", Some("inner.ref"), Some(9))); // cap
}

// ---- entity_stream_eq (lines 408-429) ---------------------------------------

#[test]
fn entity_stream_eq_is_field_sensitive() {
    let base = es("ent", "scp", "wire.ref", Some("inner.ref"), Some(8));
    assert_eq!(
        base,
        es("ent", "scp", "wire.ref", Some("inner.ref"), Some(8))
    );
    assert_ne!(
        base,
        es("other", "scp", "wire.ref", Some("inner.ref"), Some(8))
    ); // entity
    assert_ne!(
        base,
        es("ent", "other", "wire.ref", Some("inner.ref"), Some(8))
    ); // scope
    assert_ne!(
        base,
        es("ent", "scp", "other.ref", Some("inner.ref"), Some(8))
    ); // wire
    assert_ne!(base, es("ent", "scp", "wire.ref", Some("other"), Some(8))); // inner
    assert_ne!(
        base,
        es("ent", "scp", "wire.ref", Some("inner.ref"), Some(9))
    ); // cap
}

// ---- projection_eq (lines 436-462) + freshness_same (525-535) ---------------

#[test]
fn projection_eq_is_field_sensitive() {
    let base = proj(
        "pid",
        "ent",
        "w.r",
        Some("i.r"),
        Freshness::Consistent,
        Some(4),
    );
    assert_eq!(
        base,
        proj(
            "pid",
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    );
    assert_ne!(
        base,
        proj(
            "other",
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    ); // projection_id
    assert_ne!(
        base,
        proj(
            "pid",
            "other",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    ); // entity
    assert_ne!(
        base,
        proj(
            "pid",
            "ent",
            "other",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    ); // wire
    assert_ne!(
        base,
        proj(
            "pid",
            "ent",
            "w.r",
            Some("other"),
            Freshness::Consistent,
            Some(4)
        )
    ); // inner
    assert_ne!(
        base,
        proj(
            "pid",
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::MaybeStale { max_stale_ms: 5 },
            Some(4)
        )
    ); // freshness mode differs
    assert_ne!(
        base,
        proj(
            "pid",
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(9)
        )
    ); // cap
}

#[test]
fn projection_eq_freshness_same_internals() {
    // MaybeStale equal pair: kills freshness_same `-> false`, the
    // (MaybeStale, MaybeStale) delete-arm, and the `left_ms == right_ms` swap.
    let ms = proj(
        "pid",
        "ent",
        "w.r",
        Some("i.r"),
        Freshness::MaybeStale { max_stale_ms: 5 },
        Some(4),
    );
    assert_eq!(
        ms,
        proj(
            "pid",
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::MaybeStale { max_stale_ms: 5 },
            Some(4)
        )
    );
    // Same mode, different max_stale_ms: kills the `==` -> `!=` swap (line 535).
    assert_ne!(
        ms,
        proj(
            "pid",
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::MaybeStale { max_stale_ms: 7 },
            Some(4)
        )
    );
}

// ---- operation_status_eq (lines 469-493) ------------------------------------

#[test]
fn operation_status_eq_is_field_sensitive() -> TestResult {
    let op_a = op("mod.a.echo")?;
    let base = ops(
        op_a.clone(),
        "ent",
        "w.r",
        Some("i.r"),
        Freshness::Consistent,
        Some(4),
    );
    assert_eq!(
        base,
        ops(
            op_a.clone(),
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    );
    assert_ne!(
        base,
        ops(
            op("mod.b.echo")?,
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    ); // operation
    assert_ne!(
        base,
        ops(
            op_a.clone(),
            "other",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    ); // entity
    assert_ne!(
        base,
        ops(
            op_a.clone(),
            "ent",
            "other",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    ); // wire
    assert_ne!(
        base,
        ops(
            op_a.clone(),
            "ent",
            "w.r",
            Some("other"),
            Freshness::Consistent,
            Some(4)
        )
    ); // inner
    assert_ne!(
        base,
        ops(
            op_a.clone(),
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::MaybeStale { max_stale_ms: 5 },
            Some(4)
        )
    ); // freshness mode (kills the `&&` before/after freshness_same)
    assert_ne!(
        base,
        ops(
            op_a.clone(),
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(9)
        )
    ); // cap

    // (Consistent, Consistent) delete-arm guard via operation-status path too.
    assert_eq!(
        ops(
            op_a.clone(),
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        ),
        ops(
            op_a,
            "ent",
            "w.r",
            Some("i.r"),
            Freshness::Consistent,
            Some(4)
        )
    );
    Ok(())
}

// ---- receipt_stream_eq (lines 500-518) --------------------------------------

#[test]
fn receipt_stream_eq_is_field_sensitive() {
    let base = rcpt("kind.a", "wire.ref", Some("inner.ref"), Some(8));
    assert_eq!(base, rcpt("kind.a", "wire.ref", Some("inner.ref"), Some(8)));
    assert_ne!(base, rcpt("kind.b", "wire.ref", Some("inner.ref"), Some(8))); // kind
    assert_ne!(
        base,
        rcpt("kind.a", "other.ref", Some("inner.ref"), Some(8))
    ); // wire
    assert_ne!(base, rcpt("kind.a", "wire.ref", Some("other"), Some(8))); // inner
    assert_ne!(base, rcpt("kind.a", "wire.ref", Some("inner.ref"), Some(9))); // cap
}

// ---- validate_* via the registry public surface -----------------------------

fn sid(id: &str) -> Result<SubscriptionId, Box<dyn std::error::Error>> {
    Ok(SubscriptionId::new(id)?)
}

fn invalid_route_reason(result: &Result<(), SubscriptionRuntimeError>) -> Option<&'static str> {
    match result {
        Err(SubscriptionRuntimeError::InvalidRoute { reason }) => Some(reason),
        _ => None,
    }
}

#[test]
fn validate_projection_route_rejects_empty_projection_id() -> TestResult {
    // Kills validate_projection_route `-> Ok(())`.
    let mut registry = SubscriptionRegistry::new();
    let outcome = registry.insert(
        sid("a.v1")?,
        proj("", "ent", "wire.ref", None, Freshness::Consistent, None),
    );
    assert_eq!(
        invalid_route_reason(&outcome),
        Some("projection id is empty")
    );
    Ok(())
}

#[test]
fn validate_entity_stream_route_rejects_invalid_coordinate() -> TestResult {
    // Kills validate_entity_stream_route `-> Ok(())` (Coordinate::new must run).
    let mut registry = SubscriptionRegistry::new();
    let outcome = registry.insert(sid("a.v1")?, es("", "scope:x", "wire.ref", None, None));
    assert_eq!(
        invalid_route_reason(&outcome),
        Some("entity coordinate is invalid")
    );
    Ok(())
}

#[test]
fn validate_receipt_stream_route_rejects_empty_kind() -> TestResult {
    // Kills validate_receipt_stream_route `-> Ok(())` and
    // validate_receipt_kind `-> Ok(())`.
    let mut registry = SubscriptionRegistry::new();
    let outcome = registry.insert(sid("a.v1")?, rcpt("", "wire.ref", None, None));
    assert_eq!(
        invalid_route_reason(&outcome),
        Some("receipt kind is empty")
    );
    Ok(())
}

#[test]
fn validate_wire_payload_schema_ref_length_boundary() -> TestResult {
    // 256-byte ref is accepted (orig `>` is false). Kills `>` -> `>=`.
    let mut registry = SubscriptionRegistry::new();
    let wire_256 = "a".repeat(256);
    registry.insert(sid("a.v1")?, ec(3, &wire_256, None, None))?;
    assert!(registry.get("a.v1").is_some());

    // 257-byte ref is rejected (sanity that the length gate exists at all).
    let mut registry = SubscriptionRegistry::new();
    let wire_257 = "a".repeat(257);
    let outcome = registry.insert(sid("a.v1")?, ec(3, &wire_257, None, None));
    assert_eq!(
        invalid_route_reason(&outcome),
        Some("wire payload schema ref longer than 256 bytes")
    );
    Ok(())
}

#[test]
fn validate_receipt_kind_length_boundary() -> TestResult {
    // 256-byte kind accepted (orig `>` false). Kills `>` -> `>=`.
    let mut registry = SubscriptionRegistry::new();
    let kind_256 = "a".repeat(256);
    registry.insert(sid("a.v1")?, rcpt(&kind_256, "wire.ref", None, None))?;
    assert!(registry.get("a.v1").is_some());

    // 257-byte kind rejected. Kills `>` -> `==` (257 == 256 is false).
    let mut registry = SubscriptionRegistry::new();
    let kind_257 = "a".repeat(257);
    let outcome = registry.insert(sid("a.v1")?, rcpt(&kind_257, "wire.ref", None, None));
    assert_eq!(
        invalid_route_reason(&outcome),
        Some("receipt kind is too long")
    );
    Ok(())
}

#[test]
fn validate_receipt_kind_dot_placement_disjunction() -> TestResult {
    // Leading dot only: kills the first `||` (line 766:31) -> `&&`.
    let mut registry = SubscriptionRegistry::new();
    let outcome = registry.insert(sid("a.v1")?, rcpt(".abc", "wire.ref", None, None));
    assert_eq!(
        invalid_route_reason(&outcome),
        Some("receipt kind has invalid dot placement")
    );

    // Trailing dot only: kills the second `||` (line 766:55) -> `&&`.
    let mut registry = SubscriptionRegistry::new();
    let outcome = registry.insert(sid("a.v1")?, rcpt("abc.", "wire.ref", None, None));
    assert_eq!(
        invalid_route_reason(&outcome),
        Some("receipt kind has invalid dot placement")
    );
    Ok(())
}

// ---- validate_subscription_id (lines 776-820) -------------------------------

fn subscription_id_reason(id: &str) -> Option<&'static str> {
    match SubscriptionId::new(id) {
        Err(SubscriptionRuntimeError::InvalidSubscriptionId { reason }) => Some(reason),
        _ => None,
    }
}

#[test]
fn validate_subscription_id_rejects_empty() {
    // Kills validate_subscription_id `-> Ok(())`.
    assert_eq!(subscription_id_reason(""), Some("empty subscription id"));
}

#[test]
fn validate_subscription_id_length_boundary() {
    // 128-byte id accepted (orig `>` false). Kills `>` -> `>=`.
    let id_128 = format!("{}.v1", "a".repeat(125));
    assert_eq!(id_128.len(), 128);
    assert!(SubscriptionId::new(&id_128).is_ok());

    // 129-byte id rejected. Kills `>` -> `==` (129 == 128 is false).
    let id_129 = format!("{}.v1", "a".repeat(126));
    assert_eq!(id_129.len(), 129);
    assert_eq!(
        subscription_id_reason(&id_129),
        Some("subscription id longer than 128 bytes")
    );
}

#[test]
fn validate_subscription_id_trailing_dot_disjunction() {
    // Trailing dot must be flagged by the leading/trailing check (line 783),
    // not deferred to a later rule. Kills `||` -> `&&`: with `&&`, the
    // trailing-dot id slips past 783 and errors later with a different reason.
    assert_eq!(
        subscription_id_reason("a.v1."),
        Some("subscription id has a leading or trailing '.'")
    );
}

#[test]
fn validate_subscription_id_version_zero_disjunction() {
    // Version "01" starts with '0'. Kills line 814 `||` -> `&&`: with `&&`
    // the zero-leading version slips through and the id is wrongly accepted.
    assert_eq!(
        subscription_id_reason("a.v01"),
        Some("subscription id version must start with 1-9")
    );
}
