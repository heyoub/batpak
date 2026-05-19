//! PROVES: INV-NETBAT-LINE-PROTOCOL-STABLE, INV-NETBAT-BOUNDARY-THIN
//! CATCHES: request/response grammar drift, limit bypasses, and runtime-boundary ownership leaks.
//! SEEDED: in-memory streams and fixed syncbat handlers.
#![allow(clippy::panic)]

use netbat as nb;
use std::io::{self, Cursor, Read, Write};
use syncbat::{
    Core, EffectClass, Handler, HandlerError, HandlerResult, Module, OperationDescriptor,
};

const REQUEST_CALL_V1_HEX: &str = include_str!("golden/request_call_v1.hex");
const REQUEST_CALL_LEGACY_HEX: &str = include_str!("golden/request_call_legacy.hex");
const REQUEST_DECODE_INPUT_HEX: &str = include_str!("golden/request_decode_input.hex");
const REQUEST_EMPTY_LINE_HEX: &str = include_str!("golden/request_empty_line.hex");
const REQUEST_INPUT_TOO_LARGE_HEX: &str = include_str!("golden/request_input_too_large.hex");
const REQUEST_MISSING_VERB_HEX: &str = include_str!("golden/request_missing_verb.hex");
const REQUEST_UNSUPPORTED_PROTOCOL_HEX: &str =
    include_str!("golden/request_unsupported_protocol.hex");
const RESPONSE_OK_HEX: &str = include_str!("golden/response_ok.hex");
const RESPONSE_OK_HI_HEX: &str = include_str!("golden/response_ok_hi.hex");
const RESPONSE_ERR_MALFORMED_HEX: &str = include_str!("golden/response_err_malformed.hex");

const PING: OperationDescriptor = OperationDescriptor::new(
    "ping",
    EffectClass::Inspect,
    "schema.ping.input.v1",
    "schema.ping.output.v1",
    "receipt.ping.v1",
);

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn fixture_bytes(name: &str, hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    assert!(
        hex.len().is_multiple_of(2),
        "golden fixture {name} must contain even-length hex"
    );
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("fixture hex is utf8");
            u8::from_str_radix(pair, 16).expect("fixture hex decodes")
        })
        .collect()
}

struct PingHandler;

impl Handler for PingHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        Ok(input.to_vec())
    }
}

struct FailingHandler;

impl Handler for FailingHandler {
    fn handle(&mut self, _input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        Err(HandlerError::invalid_input("bad payload"))
    }
}

struct CountingHandler {
    count: std::rc::Rc<std::cell::Cell<u32>>,
}

impl Handler for CountingHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        self.count.set(self.count.get() + 1);
        Ok(input.to_vec())
    }
}

struct InterruptedThenData {
    data: Cursor<Vec<u8>>,
    interrupted: bool,
    written: Vec<u8>,
}

impl InterruptedThenData {
    fn new(data: Vec<u8>) -> Self {
        Self {
            data: Cursor::new(data),
            interrupted: false,
            written: Vec::new(),
        }
    }
}

impl Read for InterruptedThenData {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        self.data.read(buf)
    }
}

impl Write for InterruptedThenData {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn core_with_ping() -> Core {
    let mut builder = Core::builder();
    builder.register(PING, PingHandler).expect("register");
    builder.build().expect("core builds")
}

#[test]
fn exposes_syncbat_module_as_boundary_routes_without_dispatch() {
    let module = Module::from_operations("health", [PING]).expect("module builds");
    let server_module = nb::ServerModule::expose(module, "/nb").expect("module exposes");

    assert_eq!(server_module.name(), "health");
    assert_eq!(server_module.operation_count(), 1);
    assert_eq!(server_module.routes().len(), 1);
    assert_eq!(server_module.routes()[0].method(), "CALL");
    assert_eq!(server_module.routes()[0].operation_name(), "ping");
    assert_eq!(server_module.routes()[0].path(), "/nb/ping");
}

#[test]
fn server_introspection_reports_modules_routes_and_layer_rule() {
    let module = Module::from_operations("health", [PING]).expect("module builds");
    let mut server = nb::Server::new();
    server
        .mount(nb::ServerModule::expose(module, "api").expect("module exposes"))
        .expect("module mounts");

    let report = server.introspect();

    assert_eq!(report.module_count, 1);
    assert_eq!(report.operation_count, 1);
    assert_eq!(report.route_count, 1);
    assert_eq!(report.layer_rule, "nb exposes, sb dispatches, bp records");
    assert_eq!(server.routes().count(), 1);
}

#[test]
fn inspects_borrowed_syncbat_core_without_invoking_handlers() {
    let core = core_with_ping();

    let health = nb::inspect_core_operations(&core, ["ping", "missing"]);

    assert!(!health.is_healthy());
    assert_eq!(health.mounted_operations, vec!["ping"]);
    assert_eq!(health.missing_operations, vec!["missing"]);
    assert_eq!(health.layer_rule, nb::LAYER_RULE);
}

#[test]
fn decodes_line_protocol_frame() {
    let frame = nb::decode_line(
        &fixture_bytes("request_call_legacy", REQUEST_CALL_LEGACY_HEX),
        &nb::Limits::default(),
    )
    .expect("frame decodes");

    assert_eq!(frame.operation(), "ping");
    assert_eq!(frame.input(), b"hello");
}

#[test]
fn decodes_versioned_line_protocol_frame() {
    let frame = nb::decode_line(
        &nb::encode_request("ping", b"hello"),
        &nb::Limits::default(),
    )
    .expect("versioned frame decodes");

    assert_eq!(frame.operation(), "ping");
    assert_eq!(frame.input(), b"hello");
}

#[test]
fn encodes_request_with_stable_versioned_line_protocol() {
    let encoded = nb::encode_request("ping", b"hi");
    assert_eq!(hex(&encoded), REQUEST_CALL_V1_HEX.trim());

    let decoded = nb::decode_line(&encoded, &nb::Limits::default()).expect("request decodes");
    assert_eq!(decoded, nb::RequestFrame::new("ping", b"hi".to_vec()));
}

#[test]
fn rejects_unsupported_line_protocol_version() {
    let err = match nb::decode_line(
        &fixture_bytes(
            "request_unsupported_protocol",
            REQUEST_UNSUPPORTED_PROTOCOL_HEX,
        ),
        &nb::Limits::default(),
    ) {
        Ok(_) => panic!("expected unsupported protocol version"),
        Err(error) => error,
    };

    assert_eq!(
        err,
        nb::NetbatError::UnsupportedProtocolVersion {
            version: "NETBAT/2".to_owned()
        }
    );
}

#[test]
fn rejects_versioned_frame_with_missing_fields() {
    let missing_verb = match nb::decode_line(
        &fixture_bytes("request_missing_verb", REQUEST_MISSING_VERB_HEX),
        &nb::Limits::default(),
    ) {
        Ok(_) => panic!("expected missing verb"),
        Err(error) => error,
    };
    let missing_operation = match nb::decode_line(b"NETBAT/1 CALL\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected missing operation"),
        Err(error) => error,
    };
    let missing_input = match nb::decode_line(b"NETBAT/1 CALL ping\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected missing input"),
        Err(error) => error,
    };

    assert_eq!(
        missing_verb,
        nb::NetbatError::MalformedRequest {
            reason: "missing verb"
        }
    );
    assert_eq!(
        missing_operation,
        nb::NetbatError::MalformedRequest {
            reason: "missing operation"
        }
    );
    assert_eq!(
        missing_input,
        nb::NetbatError::MalformedRequest {
            reason: "missing input"
        }
    );
}

#[test]
fn decodes_crlf_and_bare_cr_line_endings() {
    let crlf =
        nb::decode_line(b"CALL ping 6f6b\r\n", &nb::Limits::default()).expect("crlf decodes");
    let cr = nb::decode_line(b"CALL ping 6f6b\r", &nb::Limits::default()).expect("cr decodes");

    assert_eq!(crlf.input(), b"ok");
    assert_eq!(cr.input(), b"ok");
}

#[test]
fn dispatches_decoded_frame_through_syncbat_core() {
    let mut core = core_with_ping();
    let frame = nb::RequestFrame::new("ping", b"roundtrip".to_vec());

    let response =
        nb::dispatch_frame(&mut core, frame, &nb::Limits::default()).expect("dispatch succeeds");

    assert_eq!(response.output(), b"roundtrip");
}

#[test]
fn dispatch_revalidates_public_request_frames() {
    let mut core = core_with_ping();
    let limits = nb::Limits::default()
        .with_max_operation_name_bytes(3)
        .with_max_input_bytes(1);

    let name_err = match nb::dispatch_frame(
        &mut core,
        nb::RequestFrame::new("ping", Vec::<u8>::new()),
        &limits,
    ) {
        Ok(_) => panic!("expected operation limit failure"),
        Err(error) => error,
    };
    let input_err =
        match nb::dispatch_frame(&mut core, nb::RequestFrame::new("ok", vec![0, 1]), &limits) {
            Ok(_) => panic!("expected input limit failure"),
            Err(error) => error,
        };
    let grammar_err = match nb::dispatch_frame(
        &mut core,
        nb::RequestFrame::new("bad/name", Vec::<u8>::new()),
        &nb::Limits::default(),
    ) {
        Ok(_) => panic!("expected operation grammar failure"),
        Err(error) => error,
    };

    assert_eq!(name_err, nb::NetbatError::OperationNameTooLong { max: 3 });
    assert_eq!(input_err, nb::NetbatError::InputTooLarge { max: 1 });
    assert_eq!(
        grammar_err,
        nb::NetbatError::MalformedRequest {
            reason: "operation has invalid bytes"
        }
    );
}

#[test]
fn serve_stream_writes_stable_success_response() {
    let mut core = core_with_ping();
    let mut stream = Cursor::new(fixture_bytes("request_call_v1", REQUEST_CALL_V1_HEX));

    let response =
        nb::serve_stream(&mut stream, &mut core, &nb::Limits::default()).expect("served");

    assert_eq!(response.output(), b"hi");
    assert!(stream
        .into_inner()
        .ends_with(&fixture_bytes("response_ok_hi", RESPONSE_OK_HI_HEX)));
}

#[test]
fn unknown_operation_maps_to_stable_error_response() {
    let mut core = core_with_ping();
    let mut stream = Cursor::new(b"CALL missing 00\n".to_vec());

    let err = match nb::serve_stream(&mut stream, &mut core, &nb::Limits::default()) {
        Ok(_) => panic!("expected unknown operation"),
        Err(error) => error,
    };

    assert!(matches!(
        err,
        nb::NetbatError::Runtime(syncbat::RuntimeError::UnknownOperation { .. })
    ));
    let bytes = stream.into_inner();
    let text = std::str::from_utf8(&bytes).expect("utf8 response");
    assert!(text.ends_with("ERR unknown_operation 72756e74696d65206572726f723a20756e6b6e6f776e206f7065726174696f6e20606d697373696e6760\n"));
}

#[test]
fn handler_failure_maps_without_losing_class_or_message() {
    let mut builder = Core::builder();
    builder.register(PING, FailingHandler).expect("register");
    let mut core = builder.build().expect("core builds");
    let mut stream = Cursor::new(fixture_bytes(
        "request_decode_input",
        REQUEST_DECODE_INPUT_HEX,
    ));

    let err = match nb::serve_stream(&mut stream, &mut core, &nb::Limits::default()) {
        Ok(_) => panic!("expected handler failure"),
        Err(error) => error,
    };

    assert!(matches!(
        err,
        nb::NetbatError::Runtime(syncbat::RuntimeError::Handler { .. })
    ));
    let bytes = stream.into_inner();
    let text = std::str::from_utf8(&bytes).expect("utf8 response");
    assert!(text.contains("ERR handler "));
    assert!(text.contains("696e76616c69645f696e707574"));
    assert!(text.contains("626164207061796c6f6164"));
}

#[test]
fn rejects_line_too_long() {
    let limits = nb::Limits::default().with_max_line_bytes(4);

    let err = match nb::decode_line(
        &fixture_bytes("request_decode_input", REQUEST_DECODE_INPUT_HEX),
        &limits,
    ) {
        Ok(_) => panic!("expected line limit failure"),
        Err(error) => error,
    };

    assert_eq!(err, nb::NetbatError::LineTooLong { max: 4 });
}

#[test]
fn rejects_operation_name_too_long() {
    let limits = nb::Limits::default().with_max_operation_name_bytes(3);

    let err = match nb::decode_line(
        &fixture_bytes("request_decode_input", REQUEST_DECODE_INPUT_HEX),
        &limits,
    ) {
        Ok(_) => panic!("expected operation limit failure"),
        Err(error) => error,
    };

    assert_eq!(err, nb::NetbatError::OperationNameTooLong { max: 3 });
}

#[test]
fn rejects_input_body_too_large() {
    let limits = nb::Limits::default().with_max_input_bytes(1);

    let err = match nb::decode_line(
        &fixture_bytes("request_input_too_large", REQUEST_INPUT_TOO_LARGE_HEX),
        &limits,
    ) {
        Ok(_) => panic!("expected input limit failure"),
        Err(error) => error,
    };

    assert_eq!(err, nb::NetbatError::InputTooLarge { max: 1 });
}

#[test]
fn rejects_malformed_hex_and_token_count() {
    let hex_err = match nb::decode_line(b"CALL ping nope\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected malformed hex"),
        Err(error) => error,
    };
    let token_err = match nb::decode_line(b"CALL ping 00 extra\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected malformed token count"),
        Err(error) => error,
    };

    assert_eq!(
        hex_err,
        nb::NetbatError::MalformedRequest {
            reason: "input is not hex"
        }
    );
    assert_eq!(
        token_err,
        nb::NetbatError::MalformedRequest {
            reason: "too many fields"
        }
    );
}

#[test]
fn rejects_odd_hex_unsupported_verb_missing_fields_and_whitespace_operation() {
    let odd = match nb::decode_line(b"CALL ping 0\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected odd hex rejection"),
        Err(error) => error,
    };
    let verb = match nb::decode_line(b"POST ping 00\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected unsupported verb rejection"),
        Err(error) => error,
    };
    let missing = match nb::decode_line(b"CALL ping\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected missing input rejection"),
        Err(error) => error,
    };
    let whitespace = match nb::decode_line(b"CALL ping\tname 00\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected whitespace operation rejection"),
        Err(error) => error,
    };
    let dot_segment = match nb::decode_line(b"CALL ping..name 00\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected dot-segment operation rejection"),
        Err(error) => error,
    };

    assert_eq!(
        odd,
        nb::NetbatError::MalformedRequest {
            reason: "hex input has odd length"
        }
    );
    assert_eq!(
        verb,
        nb::NetbatError::MalformedRequest {
            reason: "unsupported verb"
        }
    );
    assert_eq!(
        missing,
        nb::NetbatError::MalformedRequest {
            reason: "missing input"
        }
    );
    assert_eq!(
        whitespace,
        nb::NetbatError::MalformedRequest {
            reason: "operation has invalid bytes"
        }
    );
    assert_eq!(
        dot_segment,
        nb::NetbatError::MalformedRequest {
            reason: "operation dot segments must be non-empty"
        }
    );
}

#[test]
fn rejects_empty_line_and_non_utf8_operation() {
    let empty = match nb::decode_line(
        &fixture_bytes("request_empty_line", REQUEST_EMPTY_LINE_HEX),
        &nb::Limits::default(),
    ) {
        Ok(_) => panic!("expected empty-line rejection"),
        Err(error) => error,
    };
    let non_utf8 = match nb::decode_line(b"CALL \xff 00\n", &nb::Limits::default()) {
        Ok(_) => panic!("expected non-utf8 operation rejection"),
        Err(error) => error,
    };

    assert_eq!(
        empty,
        nb::NetbatError::MalformedRequest {
            reason: "empty line"
        }
    );
    assert_eq!(
        non_utf8,
        nb::NetbatError::MalformedRequest {
            reason: "operation has invalid bytes"
        }
    );
}

#[test]
fn partial_read_followed_by_eof_is_a_complete_frame() {
    let mut core = core_with_ping();
    let mut stream = Cursor::new(b"CALL ping 6f6b".to_vec());

    let response =
        nb::serve_stream(&mut stream, &mut core, &nb::Limits::default()).expect("served");

    assert_eq!(response.output(), b"ok");
    assert!(stream
        .into_inner()
        .ends_with(&fixture_bytes("response_ok", RESPONSE_OK_HEX)));
}

#[test]
fn serve_stream_writes_stable_error_for_line_read_failures() {
    let mut core = core_with_ping();
    let limits = nb::Limits::default().with_max_line_bytes(4);
    let mut too_long = Cursor::new(fixture_bytes(
        "request_decode_input",
        REQUEST_DECODE_INPUT_HEX,
    ));
    let mut empty = Cursor::new(Vec::new());

    let long_err = match nb::serve_stream(&mut too_long, &mut core, &limits) {
        Ok(_) => panic!("expected line-too-long failure"),
        Err(error) => error,
    };
    let empty_err = match nb::serve_stream(&mut empty, &mut core, &nb::Limits::default()) {
        Ok(_) => panic!("expected empty stream failure"),
        Err(error) => error,
    };

    assert_eq!(long_err, nb::NetbatError::LineTooLong { max: 4 });
    assert_eq!(empty_err, nb::NetbatError::EmptyStream);
    let too_long_bytes = too_long.into_inner();
    let empty_bytes = empty.into_inner();
    assert!(too_long_bytes
        .windows(b"ERR line_too_long ".len())
        .any(|window| window == b"ERR line_too_long "));
    assert!(empty_bytes.starts_with(b"ERR empty_stream "));
}

#[test]
fn serve_stream_retries_interrupted_reads() {
    let mut core = core_with_ping();
    let mut stream = InterruptedThenData::new(b"CALL ping 6f6b\n".to_vec());

    let response =
        nb::serve_stream(&mut stream, &mut core, &nb::Limits::default()).expect("served");

    assert_eq!(response.output(), b"ok");
    assert_eq!(
        hex(&stream.written),
        RESPONSE_OK_HEX.trim(),
        "stable response fixture changed"
    );
}

#[test]
fn stable_response_encoder_shapes_success_and_error() {
    let success = nb::encode_response(Ok(b"ok"));
    let error = nb::encode_response(Err(&nb::NetbatError::MalformedRequest { reason: "bad" }));

    assert_eq!(hex(&success), RESPONSE_OK_HEX.trim());
    assert_eq!(hex(&error), RESPONSE_ERR_MALFORMED_HEX.trim());
}

#[test]
fn output_limit_fails_closed_after_dispatch() {
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut builder = Core::builder();
    builder
        .register(
            PING,
            CountingHandler {
                count: std::rc::Rc::clone(&count),
            },
        )
        .expect("register");
    let mut core = builder.build().expect("core builds");
    let limits = nb::Limits::default().with_max_output_bytes(1);

    let err = match nb::dispatch_frame(
        &mut core,
        nb::RequestFrame::new("ping", b"hi".to_vec()),
        &limits,
    ) {
        Ok(_) => panic!("expected output limit failure"),
        Err(error) => error,
    };

    assert_eq!(err, nb::NetbatError::OutputTooLarge { max: 1 });
    assert_eq!(count.get(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire-compat emit gate
//
// PROVES: emitter byte-stability against the same golden .hex fixtures
// already used by the parser. The previous boundary suite proves
// `decode_line` accepts these bytes; this section proves
// `encode_request` / `encode_response` produces them. Without this
// gate, the encoder could drift while the parser still accepts the
// older shape — a silent wire-format break.
// CATCHES: any change to encoder framing (separator, trailing newline,
// hex case, protocol version token, operation-name placement, OK/ERR
// keyword spelling) that would not be caught by parser-only tests.
// SEEDED: the same `tests/golden/*.hex` artifacts the parser tests
// consume. New goldens added MUST also appear here.

#[test]
fn emit_request_v1_matches_golden_bytes() {
    let frame = nb::encode_request("ping", b"hi");
    let golden_hex = REQUEST_CALL_V1_HEX.trim();
    let golden_bytes = nb::decode_hex_str(golden_hex).expect("golden hex decodes");
    assert_eq!(
        frame, golden_bytes,
        "encode_request drifted from request_call_v1.hex"
    );
}

#[test]
fn emit_response_ok_matches_golden_bytes() {
    let frame = nb::encode_response(Ok(b"ok"));
    let golden_bytes = nb::decode_hex_str(RESPONSE_OK_HEX.trim()).expect("golden hex decodes");
    assert_eq!(
        frame, golden_bytes,
        "encode_response drifted from response_ok.hex"
    );
}

#[test]
fn emit_response_ok_hi_matches_golden_bytes() {
    let frame = nb::encode_response(Ok(b"hi"));
    let golden_bytes = nb::decode_hex_str(RESPONSE_OK_HI_HEX.trim()).expect("golden hex decodes");
    assert_eq!(
        frame, golden_bytes,
        "encode_response drifted from response_ok_hi.hex"
    );
}

#[test]
fn emit_response_err_carries_typed_code_and_hex_message() {
    let error = nb::NetbatError::MalformedRequest { reason: "bad" };
    let frame = nb::encode_response(Err(&error));
    let golden_bytes =
        nb::decode_hex_str(RESPONSE_ERR_MALFORMED_HEX.trim()).expect("golden hex decodes");
    assert_eq!(
        frame, golden_bytes,
        "encode_response(Err) drifted from response_err_malformed.hex"
    );
}

#[test]
fn emit_request_then_parse_returns_input_unchanged() {
    // Closed-loop round-trip across every (op, payload) shape we ship.
    for (op, payload) in [
        ("ping", &[][..]),
        ("ping", &b"hi"[..]),
        ("system.heartbeat", &[0_u8, 1, 255][..]),
        ("bank.commit", &b"\x81\xa0"[..]),
    ] {
        let frame = nb::encode_request(op, payload);
        let parsed = nb::decode_line(&frame, &nb::Limits::default()).expect("parse");
        assert_eq!(parsed.operation(), op);
        assert_eq!(parsed.input(), payload);
    }
}

#[test]
fn emit_response_ok_then_parse_returns_output_unchanged() {
    for payload in [&[][..], &b"hi"[..], &[0_u8, 1, 255][..]] {
        let frame = nb::encode_response(Ok(payload));
        let line = frame
            .strip_prefix(b"OK ")
            .and_then(|s| s.strip_suffix(b"\n"))
            .expect("response shape OK <hex>\\n");
        let decoded =
            nb::decode_hex_str(std::str::from_utf8(line).expect("hex ascii")).expect("hex decodes");
        assert_eq!(decoded, payload);
    }
}
