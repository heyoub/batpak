//! PROVES: S12-SUBSCRIPTION-RUNTIME-EVENTS netbat TCP/stream adaptation.
//! CATCHES: subscribe handshake, frame mapping, ACK control, and terminal errors on wire.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use netbat as nb;
use syncbat::{
    EventStreamCursorV1, RuntimeCursor, SessionControl, SessionDelivery, SessionEnd, SessionError,
    SessionEventDelivery, SessionPoll, SessionWatermarkDelivery, SubscriptionRuntimeError,
    SubscriptionSession, SubscriptionSessionFactory,
};

const SUBSCRIPTION_ID: &str = "orders.open.v1";
const CATEGORY: u8 = 0x0A;
const WIRE_SCHEMA: &str = "batpak.event-stream-envelope.v1";

enum FakeOpen {
    Deliver {
        initial: Vec<SessionDelivery>,
        after_ack: Option<SessionDelivery>,
    },
    Unknown,
    CursorInvalid,
}

struct FakeRuntime {
    open: FakeOpen,
}

impl FakeRuntime {
    fn deliver(initial: Vec<SessionDelivery>, after_ack: Option<SessionDelivery>) -> Self {
        Self {
            open: FakeOpen::Deliver { initial, after_ack },
        }
    }

    fn unknown() -> Self {
        Self {
            open: FakeOpen::Unknown,
        }
    }

    fn cursor_invalid() -> Self {
        Self {
            open: FakeOpen::CursorInvalid,
        }
    }
}

impl SubscriptionSessionFactory for FakeRuntime {
    fn open_session(
        &self,
        subscription_id: &str,
        _resume_cursor: Option<&[u8]>,
        _client_window: u32,
        control_rx: flume::Receiver<SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError> {
        match &self.open {
            FakeOpen::Deliver { initial, after_ack } => Ok(Box::new(FakeSession {
                subscription_id: subscription_id.to_owned(),
                deliveries: initial.iter().cloned().collect(),
                after_ack: after_ack.clone(),
                control_rx,
            })),
            FakeOpen::Unknown => Err(SubscriptionRuntimeError::UnknownSubscription {
                id: subscription_id.to_owned(),
            }),
            FakeOpen::CursorInvalid => Err(SubscriptionRuntimeError::CursorInvalid {
                reason: "cursor fixture invalid",
            }),
        }
    }
}

struct FakeSession {
    subscription_id: String,
    deliveries: VecDeque<SessionDelivery>,
    after_ack: Option<SessionDelivery>,
    control_rx: flume::Receiver<SessionControl>,
}

impl FakeSession {
    fn drain_control(&mut self) -> Option<SessionPoll> {
        loop {
            match self.control_rx.try_recv() {
                Ok(control) => {
                    if let Some(poll) = self.apply_control(&control) {
                        return Some(poll);
                    }
                }
                Err(flume::TryRecvError::Empty) => return None,
                Err(flume::TryRecvError::Disconnected) => return Some(SessionPoll::Ended),
            }
        }
    }

    fn apply_control(&mut self, control: &SessionControl) -> Option<SessionPoll> {
        match control {
            SessionControl::Ack { .. } => {
                if let Some(delivery) = self.after_ack.take() {
                    self.deliveries.push_back(delivery);
                }
                None
            }
            SessionControl::Cancel => {
                Some(SessionPoll::Delivery(SessionDelivery::End(SessionEnd {
                    subscription_id: self.subscription_id.clone(),
                    reason_code: "client_cancelled",
                    cursor_after: None,
                })))
            }
            SessionControl::Disconnected => Some(SessionPoll::Ended),
            SessionControl::Malformed => Some(SessionPoll::Delivery(SessionDelivery::Error(
                SessionError {
                    subscription_id: Some(self.subscription_id.clone()),
                    code: "malformed_stream_frame",
                    last_delivered_cursor: None,
                    last_acked_cursor: None,
                    message: b"malformed stream control frame".to_vec(),
                },
            ))),
        }
    }
}

impl SubscriptionSession for FakeSession {
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        if let Some(poll) = self.drain_control() {
            return Ok(poll);
        }
        if let Some(delivery) = self.deliveries.pop_front() {
            return Ok(SessionPoll::Delivery(delivery));
        }
        match self.control_rx.recv_timeout(timeout) {
            Ok(control) => Ok(self.apply_control(&control).unwrap_or(SessionPoll::Blocked)),
            Err(flume::RecvTimeoutError::Timeout) => Ok(SessionPoll::Blocked),
            Err(flume::RecvTimeoutError::Disconnected) => Ok(SessionPoll::Ended),
        }
    }
}

fn localhost_listener() -> Result<TcpListener, Box<dyn std::error::Error>> {
    Ok(TcpListener::bind("127.0.0.1:0")?)
}

#[test]
fn stream_runtime_event_tcp_maps_replay_watermark_and_ack_wake(
) -> Result<(), Box<dyn std::error::Error>> {
    let first = cursor_after(1);
    let second = cursor_after(2);
    let watermark = cursor_after(2);
    let live = cursor_after(3);
    let runtime = FakeRuntime::deliver(
        vec![
            event_delivery(1, cursor_beginning(), first.clone()),
            event_delivery(2, first, second),
            watermark_delivery(3, watermark.clone()),
        ],
        Some(event_delivery(4, watermark.clone(), live)),
    );
    let listener = localhost_listener()?;
    let addr = listener.local_addr()?;
    let server = thread::Builder::new()
        .name("netbat-test-sub-replay-live".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept()?;
            let reader = stream.try_clone()?;
            nb::serve_subscription_stream(reader, stream, &runtime, &nb::Limits::default())
        })?;

    let mut client = TcpStream::connect(addr)?;
    client.set_read_timeout(Some(Duration::from_secs(2)))?;
    client.write_all(format!("NETBAT/2 SUBSCRIBE {SUBSCRIPTION_ID} - 128\n").as_bytes())?;
    let lines = read_lines(&mut client, 3)?;
    assert!(
        lines.iter().any(|line| line.contains("SUB_EVENT")),
        "PROPERTY: server must map runtime deliveries to SUB_EVENT frames"
    );
    assert!(
        lines.iter().any(|line| line.contains("SUB_WATERMARK")),
        "PROPERTY: server must map runtime watermark deliveries to SUB_WATERMARK"
    );

    client.write_all(
        format!(
            "NETBAT/2 SUB_ACK {SUBSCRIPTION_ID} 3 {}\n",
            hex_lower(watermark.as_bytes())
        )
        .as_bytes(),
    )?;
    let more = read_lines(&mut client, 1)?;
    assert!(
        more.iter().any(|line| line.contains("SUB_EVENT")),
        "PROPERTY: ACK control must reach the syncbat session and release follow-up delivery"
    );
    client
        .write_all(format!("NETBAT/2 SUB_CANCEL {SUBSCRIPTION_ID} client.cancel\n").as_bytes())?;
    drop(client);
    server
        .join()
        .map_err(|_| std::io::Error::other("PROPERTY: subscription server thread panicked"))??;
    Ok(())
}

#[test]
fn stream_runtime_event_unknown_subscription_emits_sub_err(
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = FakeRuntime::unknown();
    let listener = localhost_listener()?;
    let addr = listener.local_addr()?;
    let server = thread::Builder::new()
        .name("netbat-test-sub-unknown".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept()?;
            let reader = stream.try_clone()?;
            nb::serve_subscription_stream(reader, stream, &runtime, &nb::Limits::default())
        })?;
    let mut client = TcpStream::connect(addr)?;
    client.write_all(format!("NETBAT/2 SUBSCRIBE {SUBSCRIPTION_ID} - 128\n").as_bytes())?;
    let lines = read_lines(&mut client, 1)?;
    assert!(
        lines[0].contains("SUB_ERR") && lines[0].contains("unknown_subscription"),
        "PROPERTY: unknown route must emit SUB_ERR unknown_subscription"
    );
    drop(client);
    server
        .join()
        .map_err(|_| std::io::Error::other("PROPERTY: subscription server thread panicked"))??;
    Ok(())
}

#[test]
fn stream_runtime_event_invalid_resume_cursor_sub_err_uses_requested_subscription_id(
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = FakeRuntime::cursor_invalid();
    let listener = localhost_listener()?;
    let addr = listener.local_addr()?;
    let server = thread::Builder::new()
        .name("netbat-test-sub-cursor-invalid".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept()?;
            let reader = stream.try_clone()?;
            nb::serve_subscription_stream(reader, stream, &runtime, &nb::Limits::default())
        })?;
    let mut client = TcpStream::connect(addr)?;
    client.write_all(format!("NETBAT/2 SUBSCRIBE {SUBSCRIPTION_ID} 00 128\n").as_bytes())?;
    let lines = read_lines(&mut client, 1)?;
    assert!(
        lines[0].contains("SUB_ERR")
            && lines[0].contains(SUBSCRIPTION_ID)
            && lines[0].contains("cursor_invalid"),
        "PROPERTY: invalid resume cursor must emit SUB_ERR for requested subscription id"
    );
    assert!(
        !lines[0].contains("unknown.invalid.v1"),
        "PROPERTY: cursor errors after valid SUBSCRIBE must not use synthetic unknown id"
    );
    drop(client);
    server
        .join()
        .map_err(|_| std::io::Error::other("PROPERTY: subscription server thread panicked"))??;
    Ok(())
}

#[test]
fn stream_runtime_event_slow_consumer_emits_sub_err() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = FakeRuntime::deliver(vec![slow_consumer_error()], None);
    let listener = localhost_listener()?;
    let addr = listener.local_addr()?;
    let server = thread::Builder::new()
        .name("netbat-test-sub-slow".to_owned())
        .spawn(move || {
            let (stream, _) = listener.accept()?;
            let reader = stream.try_clone()?;
            nb::serve_subscription_stream(reader, stream, &runtime, &nb::Limits::default())
        })?;
    let mut client = TcpStream::connect(addr)?;
    client.set_read_timeout(Some(Duration::from_secs(2)))?;
    client.write_all(format!("NETBAT/2 SUBSCRIBE {SUBSCRIPTION_ID} - 1\n").as_bytes())?;
    let lines = read_lines(&mut client, 1)?;
    assert!(
        lines
            .iter()
            .any(|line| line.contains("SUB_ERR") && line.contains("slow_consumer")),
        "PROPERTY: bounded queue overflow must emit SUB_ERR slow_consumer"
    );
    drop(client);
    server
        .join()
        .map_err(|_| std::io::Error::other("PROPERTY: subscription server thread panicked"))??;
    Ok(())
}

#[test]
fn stream_runtime_event_client_disconnect_without_cancel_exits_server(
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = FakeRuntime::deliver(Vec::new(), None);
    let listener = localhost_listener()?;
    let addr = listener.local_addr()?;
    let (done_tx, done_rx) = flume::bounded(1);
    let server = thread::Builder::new()
        .name("netbat-test-sub-disconnect".to_owned())
        .spawn(move || {
            let result = (|| -> Result<(), String> {
                let (stream, _) = listener.accept().map_err(|error| error.to_string())?;
                let reader = stream.try_clone().map_err(|error| error.to_string())?;
                nb::serve_subscription_stream(reader, stream, &runtime, &nb::Limits::default())
                    .map_err(|error| error.to_string())?;
                Ok(())
            })();
            let _ = done_tx.send(result);
        })?;

    let mut client = TcpStream::connect(addr)?;
    client.write_all(format!("NETBAT/2 SUBSCRIBE {SUBSCRIPTION_ID} - 128\n").as_bytes())?;
    drop(client);

    let outcome = done_rx.recv_timeout(Duration::from_secs(2)).map_err(|_| {
        std::io::Error::other(
            "PROPERTY: server must exit after peer disconnects without SUB_CANCEL",
        )
    })?;
    outcome.map_err(std::io::Error::other)?;
    server
        .join()
        .map_err(|_| std::io::Error::other("PROPERTY: subscription server thread panicked"))?;
    Ok(())
}

fn event_delivery(
    delivery_index: u64,
    cursor_before: RuntimeCursor,
    cursor_after: RuntimeCursor,
) -> SessionDelivery {
    SessionDelivery::Event(SessionEventDelivery {
        subscription_id: SUBSCRIPTION_ID.to_owned(),
        delivery_index,
        cursor_before,
        cursor_after,
        wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
        envelope_bytes: b"canonical-envelope-fixture".to_vec(),
    })
}

fn watermark_delivery(delivery_index: u64, cursor_after: RuntimeCursor) -> SessionDelivery {
    SessionDelivery::Watermark(SessionWatermarkDelivery {
        subscription_id: SUBSCRIPTION_ID.to_owned(),
        delivery_index,
        cursor_after,
    })
}

fn slow_consumer_error() -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: Some(SUBSCRIPTION_ID.to_owned()),
        code: "slow_consumer",
        last_delivered_cursor: None,
        last_acked_cursor: None,
        message: b"delivery window full".to_vec(),
    })
}

fn cursor_beginning() -> RuntimeCursor {
    RuntimeCursor::from_bytes(
        EventStreamCursorV1::beginning(SUBSCRIPTION_ID, CATEGORY)
            .encode()
            .to_vec(),
    )
}

fn cursor_after(global_sequence: u64) -> RuntimeCursor {
    RuntimeCursor::from_bytes(
        EventStreamCursorV1::after_global_sequence(
            SUBSCRIPTION_ID,
            CATEGORY,
            global_sequence,
            global_sequence.saturating_mul(10),
        )
        .encode()
        .to_vec(),
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0F)]));
    }
    out
}

fn read_lines(
    reader: &mut TcpStream,
    max_lines: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut buf = Vec::new();
    let mut scratch = [0_u8; 4096];
    let mut lines = Vec::new();
    while lines.len() < max_lines {
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(count) => {
                buf.extend_from_slice(&scratch[..count]);
                while let Some(pos) = buf.iter().position(|byte| *byte == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<_>>();
                    let text = String::from_utf8_lossy(&line).trim().to_owned();
                    if !text.is_empty() {
                        lines.push(text);
                    }
                    if lines.len() >= max_lines {
                        break;
                    }
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(lines)
}
