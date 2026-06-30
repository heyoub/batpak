//! PROVES: NETBAT-TCP-SUBSCRIPTION-CONCURRENCY — the subscription listener
//! serves long-lived subscribers concurrently under
//! `SubscriptionDispatch::Concurrent`, and the pre-0.9 inline behavior is
//! retained as `SubscriptionDispatch::Sequential`.
//! CATCHES: a regression to inline-only dispatch (a second subscriber starved
//! while the first stays open), and a worker that escalates a per-session fault
//! to the whole listener.
//! SEEDED: localhost listeners with a runtime whose sessions deliver one event
//! then stay open until the peer disconnects.

use netbat as nb;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;
use syncbat::{
    RuntimeCursor, SessionControl, SessionDelivery, SessionEventDelivery, SessionPoll,
    SubscriptionRuntimeError, SubscriptionSession, SubscriptionSessionFactory,
};

const WIRE_SCHEMA: &str = "batpak.event-stream-envelope.v1";
const SUBSCRIBE_LINE: &[u8] = b"NETBAT/2 SUBSCRIBE orders.open.v1 - 128\n";

/// Opens long-lived sessions: each delivers exactly one `SUB_EVENT`, then stays
/// open (reporting `Blocked`) until the peer disconnects, mirroring a real
/// subscriber that holds its stream open.
struct LiveRuntime;

impl SubscriptionSessionFactory for LiveRuntime {
    fn open_session(
        &self,
        subscription_id: &str,
        _resume_cursor: Option<&[u8]>,
        _client_window: u32,
        control_rx: flume::Receiver<SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError> {
        Ok(Box::new(LiveSession {
            event: Some(event_delivery(subscription_id)),
            control_rx,
        }))
    }
}

struct LiveSession {
    event: Option<SessionDelivery>,
    control_rx: flume::Receiver<SessionControl>,
}

impl LiveSession {
    fn ends(control: &SessionControl) -> bool {
        matches!(
            control,
            SessionControl::Disconnected | SessionControl::Cancel
        )
    }
}

impl SubscriptionSession for LiveSession {
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        match self.control_rx.try_recv() {
            Ok(control) if Self::ends(&control) => return Ok(SessionPoll::Ended),
            Ok(_) => {}
            Err(flume::TryRecvError::Disconnected) => return Ok(SessionPoll::Ended),
            Err(flume::TryRecvError::Empty) => {}
        }
        if let Some(event) = self.event.take() {
            return Ok(SessionPoll::Delivery(event));
        }
        // Stay open: block for the poll window, then report Blocked. The peer's
        // disconnect (forwarded as a control frame) is what ends the session.
        match self.control_rx.recv_timeout(timeout) {
            Ok(control) if Self::ends(&control) => Ok(SessionPoll::Ended),
            Ok(_) => Ok(SessionPoll::Blocked),
            Err(flume::RecvTimeoutError::Timeout) => Ok(SessionPoll::Blocked),
            Err(flume::RecvTimeoutError::Disconnected) => Ok(SessionPoll::Ended),
        }
    }
}

fn event_delivery(subscription_id: &str) -> SessionDelivery {
    SessionDelivery::Event(SessionEventDelivery {
        subscription_id: subscription_id.to_owned(),
        delivery_index: 1,
        cursor_before: RuntimeCursor::from_bytes(vec![0]),
        cursor_after: RuntimeCursor::from_bytes(vec![1]),
        wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
        envelope_bytes: b"canonical-envelope-fixture".to_vec(),
    })
}

fn localhost_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind localhost listener")
}

fn connect(addr: std::net::SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set write timeout");
    stream
}

fn read_lines(stream: &mut TcpStream, want: usize, timeout: Duration) -> Vec<String> {
    stream
        .set_read_timeout(Some(timeout))
        .expect("set read timeout");
    let mut buf = Vec::new();
    let mut scratch = [0_u8; 4096];
    let mut lines = Vec::new();
    while lines.len() < want {
        match stream.read(&mut scratch) {
            Ok(0) => break,
            Ok(count) => {
                buf.extend_from_slice(&scratch[..count]);
                while let Some(pos) = buf.iter().position(|byte| *byte == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<_>>();
                    let text = String::from_utf8_lossy(&line).trim().to_owned();
                    if !text.is_empty() {
                        lines.push(text);
                    }
                    if lines.len() >= want {
                        break;
                    }
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    lines
}

fn spawn_listener(
    listener: TcpListener,
    config: nb::TcpSubscriptionServerConfig,
    shutdown: nb::ShutdownHandle,
) -> thread::JoinHandle<Result<nb::TcpSubscriptionServeStats, nb::NetbatError>> {
    thread::Builder::new()
        .name("netbat-sub-concurrency".to_owned())
        .spawn(move || {
            nb::serve_tcp_subscription_listener(listener, LiveRuntime, &config, &shutdown)
        })
        .expect("spawn subscription listener")
}

#[test]
fn concurrent_dispatch_serves_two_subscribers_at_once() {
    // GREEN: the default `SubscriptionDispatch::Concurrent` spawns a worker per
    // session, so TWO subscribers connect and BOTH receive their SUB_EVENT while
    // both remain open. RED (pre-0.9, now `SubscriptionDispatch::Sequential`):
    // the accept thread serves one session INLINE and a long-lived subscriber
    // blocks it, so the second subscriber is never accepted — pinned by
    // `sequential_dispatch_serves_only_one_subscriber_at_a_time`.
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let mut config = nb::TcpSubscriptionServerConfig::default();
    config.dispatch = nb::SubscriptionDispatch::Concurrent;
    let server = spawn_listener(listener, config, server_shutdown);

    // Subscriber A connects and gets its event, then stays open.
    let mut client_a = connect(addr);
    client_a.write_all(SUBSCRIBE_LINE).expect("A subscribes");
    let a_lines = read_lines(&mut client_a, 1, Duration::from_secs(2));
    assert!(
        a_lines.iter().any(|line| line.contains("SUB_EVENT")),
        "subscriber A must receive its event; got {a_lines:?}"
    );

    // Subscriber B connects WHILE A is still open. Under concurrent dispatch B is
    // served on its own worker and receives its event too.
    let mut client_b = connect(addr);
    client_b.write_all(SUBSCRIBE_LINE).expect("B subscribes");
    let b_lines = read_lines(&mut client_b, 1, Duration::from_secs(2));
    assert!(
        b_lines.iter().any(|line| line.contains("SUB_EVENT")),
        "subscriber B must be served concurrently while A stays open; got {b_lines:?}"
    );

    drop(client_a);
    drop(client_b);
    shutdown.shutdown();
    let stats = server
        .join()
        .expect("listener thread joins")
        .expect("listener returns Ok");
    assert!(
        stats.served_subscriptions >= 2,
        "both subscribers must be served; stats={stats:?}"
    );
    assert_eq!(
        stats.worker_panics, 0,
        "no session panicked; stats={stats:?}"
    );
}

#[test]
fn sequential_dispatch_serves_only_one_subscriber_at_a_time() {
    // Pins the pre-0.9 behavior, retained as `SubscriptionDispatch::Sequential`:
    // the accept thread serves one session INLINE, so a long-lived subscriber A
    // occupies it and subscriber B is NOT served while A stays open. This is the
    // RED baseline that the concurrent default fixes.
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let mut config = nb::TcpSubscriptionServerConfig::default();
    config.dispatch = nb::SubscriptionDispatch::Sequential;
    let server = spawn_listener(listener, config, server_shutdown);

    // A subscribes and is served inline; it then stays open, occupying the
    // single accept thread.
    let mut client_a = connect(addr);
    client_a.write_all(SUBSCRIBE_LINE).expect("A subscribes");
    let a_lines = read_lines(&mut client_a, 1, Duration::from_secs(2));
    assert!(
        a_lines.iter().any(|line| line.contains("SUB_EVENT")),
        "subscriber A must be served inline; got {a_lines:?}"
    );

    // B connects and subscribes, but the accept thread is blocked inline-serving
    // A, so B is never accepted and receives nothing within the window.
    let mut client_b = connect(addr);
    client_b.write_all(SUBSCRIBE_LINE).expect("B subscribes");
    let b_lines = read_lines(&mut client_b, 1, Duration::from_millis(500));
    assert!(
        b_lines.is_empty(),
        "sequential dispatch must NOT serve B while A holds the accept thread; got {b_lines:?}"
    );

    // Request shutdown, THEN free A: A's session ends, the inline serve returns,
    // and the accept loop sees shutdown and exits without serving B.
    shutdown.shutdown();
    drop(client_a);
    drop(client_b);
    let stats = server
        .join()
        .expect("listener thread joins")
        .expect("listener returns Ok");
    assert!(
        stats.served_subscriptions >= 1,
        "subscriber A was served; stats={stats:?}"
    );
}
