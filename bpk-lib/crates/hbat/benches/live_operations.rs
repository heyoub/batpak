use std::sync::Arc;

use batpak::store::{Store, StoreConfig};
use batpak::EventPayload;
use criterion::{criterion_group, criterion_main, Criterion};
use hbat::{
    BankCommitHandler, EventQueryHandler, EventQueryRequest, HeartbeatHandler,
    SystemHeartbeatRequest, BANK_COMMIT_DESCRIPTOR, EVENT_GET_DESCRIPTOR, EVENT_QUERY_DESCRIPTOR,
    HEARTBEAT_DESCRIPTOR,
};
use netbat::encode_hex_str;
use std::hint::black_box;
use syncbat::{CheckoutFrame, Core};
use tempfile::TempDir;

fn heartbeat_frame() -> CheckoutFrame {
    let request = SystemHeartbeatRequest {
        nonce: "bench-heartbeat".to_owned(),
    };
    CheckoutFrame::new(
        HEARTBEAT_DESCRIPTOR.name(),
        batpak::encoding::to_bytes(&request).expect("encode heartbeat"),
    )
}

fn bank_commit_frame() -> CheckoutFrame {
    let payload = batpak::encoding::to_bytes(&SystemHeartbeatRequest {
        nonce: "bench-bank".to_owned(),
    })
    .expect("encode nested heartbeat");
    let request = hbat::BankCommitRequest {
        entity: "bench:hbat".to_owned(),
        scope: "bench-scope".to_owned(),
        kind_category: SystemHeartbeatRequest::KIND.category(),
        kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
        payload_hex: encode_hex_str(&payload),
    };
    CheckoutFrame::new(
        BANK_COMMIT_DESCRIPTOR.name(),
        batpak::encoding::to_bytes(&request).expect("encode bank.commit request"),
    )
}

fn event_query_frame() -> CheckoutFrame {
    let request = EventQueryRequest {
        entity: Some("bench:hbat".to_owned()),
        scope: Some("bench-scope".to_owned()),
        kind_category: Some(SystemHeartbeatRequest::KIND.category()),
        kind_type_id: Some(SystemHeartbeatRequest::KIND.type_id()),
        after_global_sequence: None,
        limit: 16,
    };
    CheckoutFrame::new(
        EVENT_QUERY_DESCRIPTOR.name(),
        batpak::encoding::to_bytes(&request).expect("encode event.query request"),
    )
}

fn core() -> (Core, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open store"));
    let mut builder = Core::builder();
    builder
        .register(HEARTBEAT_DESCRIPTOR.clone(), HeartbeatHandler)
        .expect("register heartbeat");
    builder
        .register(
            BANK_COMMIT_DESCRIPTOR.clone(),
            BankCommitHandler {
                store: Arc::clone(&store),
            },
        )
        .expect("register bank.commit");
    builder
        .register(
            EVENT_GET_DESCRIPTOR.clone(),
            hbat::EventGetHandler {
                store: Arc::clone(&store),
            },
        )
        .expect("register event.get");
    builder
        .register(
            EVENT_QUERY_DESCRIPTOR.clone(),
            EventQueryHandler {
                store: Arc::clone(&store),
            },
        )
        .expect("register event.query");
    (builder.build().expect("build hbat core"), dir)
}

fn bench_live_operations(c: &mut Criterion) {
    let (mut core, _dir) = core();

    c.bench_function("hbat_system_heartbeat", |b| {
        b.iter(|| {
            let result = core
                .checkout_frame(heartbeat_frame())
                .expect("heartbeat dispatch");
            black_box(result.into_output());
        });
    });

    c.bench_function("hbat_bank_commit", |b| {
        b.iter(|| {
            let result = core
                .checkout_frame(bank_commit_frame())
                .expect("bank.commit dispatch");
            black_box(result.into_output());
        });
    });

    c.bench_function("hbat_event_query", |b| {
        b.iter(|| {
            let result = core
                .checkout_frame(event_query_frame())
                .expect("event.query dispatch");
            black_box(result.into_output());
        });
    });
}

criterion_group!(benches, bench_live_operations);
criterion_main!(benches);
