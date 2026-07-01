use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use syncbat::{CheckoutFrame, Core, EffectClass, Handler, HandlerResult, OperationDescriptor};

const ECHO: OperationDescriptor = OperationDescriptor::new(
    "echo",
    EffectClass::Compute,
    "schema.echo.input.v1",
    "schema.echo.output.v1",
    "receipt.echo.v1",
);

struct EchoHandler;

impl Handler for EchoHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        let mut out = Vec::from(input);
        out.extend_from_slice(b":ok");
        Ok(out)
    }
}

fn core() -> Core {
    let mut builder = Core::builder();
    builder.register(ECHO, EchoHandler).expect("register echo");
    builder.without_receipts();
    builder.build().expect("build syncbat core")
}

fn bench_dispatch(c: &mut Criterion) {
    let mut core = core();
    c.bench_function("syncbat_checkout_frame_echo", |b| {
        b.iter(|| {
            let frame = CheckoutFrame::new("echo", b"payload".to_vec());
            let result = core.checkout_frame(frame).expect("dispatch echo");
            black_box(result.into_output());
        });
    });
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
