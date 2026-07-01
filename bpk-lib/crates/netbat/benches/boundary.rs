use criterion::{criterion_group, criterion_main, Criterion};
use netbat::{decode_line, dispatch_frame, encode_request, Limits, RequestFrame};
use std::hint::black_box;
use syncbat::{Core, EffectClass, Handler, HandlerResult, OperationDescriptor};

const PING: OperationDescriptor = OperationDescriptor::new(
    "ping",
    EffectClass::Inspect,
    "schema.ping.input.v1",
    "schema.ping.output.v1",
    "receipt.ping.v1",
);

struct PingHandler;

impl Handler for PingHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        Ok(input.to_vec())
    }
}

fn core() -> Core {
    let mut builder = Core::builder();
    builder.register(PING, PingHandler).expect("register ping");
    builder.without_receipts();
    builder.build().expect("build core")
}

fn bench_boundary(c: &mut Criterion) {
    let limits = Limits::default();
    let request_line = encode_request("ping", b"netbat-payload");

    c.bench_function("netbat_decode_line", |b| {
        b.iter(|| {
            let frame = decode_line(&request_line, &limits).expect("decode frame");
            black_box(frame);
        });
    });

    let mut core = core();
    c.bench_function("netbat_dispatch_frame", |b| {
        b.iter(|| {
            let response = dispatch_frame(
                &mut core,
                RequestFrame::new("ping", b"netbat-payload".to_vec()),
                &limits,
            )
            .expect("dispatch frame");
            black_box(response.into_output());
        });
    });
}

criterion_group!(benches, bench_boundary);
criterion_main!(benches);
