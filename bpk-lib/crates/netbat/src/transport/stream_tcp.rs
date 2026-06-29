//! NETBAT/2 subscription streaming TCP adaptation (Packet C).

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use syncbat::{
    unknown_subscription_error, RuntimeCursor, SessionControl, SessionDelivery, SessionEnd,
    SessionError, SessionPoll, SubscriptionRuntimeError, SubscriptionSession,
    SubscriptionSessionFactory,
};

use super::error::NetbatError;
use super::limits::IoTimeouts;
use super::limits::Limits;
use super::stream_frame::{
    decode_stream_line, encode_stream_frame, CursorBytes, DeliveryIndex, MaybeCursor,
    PayloadSchemaRef, StreamFrame, StreamReasonCode, SubEndFrame, SubErrFrame, SubEventFrame,
    SubWatermarkFrame, SubscribeFrame, SubscriptionToken,
};
use super::tcp::{apply_timeouts, read_line, ShutdownHandle};

const CURSOR_INVALID_CODE: &str = "cursor_invalid";
const CURSOR_MISMATCH_CODE: &str = "cursor_mismatch";

/// Summary returned after a NETBAT/2 subscription listener exits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct TcpSubscriptionServeStats {
    /// Accepted TCP connections.
    pub accepted_connections: usize,
    /// Successfully opened subscription streams.
    pub served_subscriptions: usize,
    /// Failed subscription streams.
    pub failed_subscriptions: usize,
    /// Pre-subscribe malformed frames.
    pub malformed_pre_subscribe: usize,
    /// Post-subscribe runtime failures.
    pub runtime_failures: usize,
    /// Peer-driven connection IO failures.
    pub connection_io_failures: usize,
    /// True when shutdown was requested.
    pub shutdown_requested: bool,
}

/// Blocking NETBAT/2 subscription listener configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct TcpSubscriptionServerConfig {
    /// Line and stream limits.
    pub limits: Limits,
    /// Optional per-connection read/write timeouts.
    pub timeouts: IoTimeouts,
    /// Maximum accepted connections before returning.
    pub max_connections: usize,
    /// Idle sleep for nonblocking accept loops.
    pub idle_sleep: Duration,
}

impl Default for TcpSubscriptionServerConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            timeouts: IoTimeouts::default()
                .with_read(Some(Duration::from_millis(250)))
                .with_write(Some(Duration::from_secs(5))),
            max_connections: super::tcp::DEFAULT_MAX_CONNECTIONS,
            idle_sleep: Duration::from_millis(10),
        }
    }
}

/// Serve one NETBAT/2 subscription stream over split reader/writer handles.
///
/// The first frame must be `SUBSCRIBE`. Post-subscribe control frames are read
/// on a dedicated thread and forwarded through a bounded flume control lane.
///
/// # Errors
/// IO failures, malformed frames, runtime poll errors, or control-thread spawn failure.
pub fn serve_subscription_stream(
    reader: impl Read + Send + 'static,
    mut writer: impl Write + Send + 'static,
    runtime: &(impl SubscriptionSessionFactory + ?Sized),
    limits: &Limits,
) -> Result<TcpSubscriptionServeStats, NetbatError> {
    let mut stats = TcpSubscriptionServeStats::default();
    let mut reader = reader;
    let first_line = match read_line(&mut reader, limits.max_line_bytes) {
        Ok(line) => line,
        Err(NetbatError::EmptyStream) => return Ok(stats),
        Err(error) => return Err(error),
    };
    let subscribe = match decode_subscribe_request(&first_line, limits) {
        Ok(frame) => frame,
        Err(_) => {
            stats.failed_subscriptions += 1;
            stats.malformed_pre_subscribe += 1;
            return Ok(stats);
        }
    };
    let resume_bytes = maybe_cursor_bytes(subscribe.resume_cursor);
    let (control_tx, control_rx) = flume::bounded(16);
    let stop_control_reader = Arc::new(AtomicBool::new(false));
    let mut session = match runtime.open_session(
        subscribe.subscription_id.as_str(),
        resume_bytes.as_deref(),
        subscribe.client_window.get(),
        control_rx,
    ) {
        Ok(session) => session,
        Err(error @ SubscriptionRuntimeError::InvalidConfig { .. }) => {
            return Err(map_runtime_error(&error));
        }
        Err(error) => {
            stats.failed_subscriptions += 1;
            let delivery = map_open_error(subscribe.subscription_id.as_str(), &error);
            write_delivery(&mut writer, &delivery, limits)?;
            return Ok(stats);
        }
    };
    spawn_control_reader(
        reader,
        control_tx,
        *limits,
        subscribe.subscription_id.clone(),
        Arc::clone(&stop_control_reader),
    )?;
    stats.served_subscriptions += 1;
    let result = run_subscription_loop(&mut writer, session.as_mut(), limits);
    stop_control_reader.store(true, Ordering::Release);
    result?;
    Ok(stats)
}

/// Serve a blocking NETBAT/2 subscription TCP listener sequentially.
///
/// # Errors
/// Listener configuration, accept, timeout, response write, or runtime poll failures.
pub fn serve_tcp_subscription_listener(
    listener: TcpListener,
    runtime: &(impl SubscriptionSessionFactory + ?Sized),
    config: &TcpSubscriptionServerConfig,
    shutdown: &ShutdownHandle,
) -> Result<TcpSubscriptionServeStats, NetbatError> {
    listener.set_nonblocking(true)?;
    let mut stats = TcpSubscriptionServeStats::default();
    while !shutdown.is_shutdown() && stats.accepted_connections < config.max_connections {
        match listener.accept() {
            Ok((stream, _addr)) => {
                stats.accepted_connections += 1;
                stream.set_nonblocking(false)?;
                apply_timeouts(&stream, config.timeouts)?;
                match serve_tcp_subscription_connection(stream, runtime, config) {
                    Ok(connection_stats) => merge_stats(&mut stats, connection_stats),
                    Err(NetbatError::Io { .. }) => stats.connection_io_failures += 1,
                    Err(error) => return Err(error),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(config.idle_sleep);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    stats.shutdown_requested = shutdown.is_shutdown();
    drop(listener);
    Ok(stats)
}

fn serve_tcp_subscription_connection(
    stream: TcpStream,
    runtime: &(impl SubscriptionSessionFactory + ?Sized),
    config: &TcpSubscriptionServerConfig,
) -> Result<TcpSubscriptionServeStats, NetbatError> {
    let reader = stream.try_clone().map_err(NetbatError::from)?;
    serve_subscription_stream(reader, stream, runtime, &config.limits)
}

fn run_subscription_loop(
    writer: &mut impl Write,
    session: &mut dyn SubscriptionSession,
    limits: &Limits,
) -> Result<(), NetbatError> {
    loop {
        match session.poll(Duration::from_millis(50)) {
            Ok(SessionPoll::Delivery(delivery)) => {
                write_delivery(writer, &delivery, limits)?;
                if terminal_delivery(&delivery) {
                    return Ok(());
                }
            }
            Ok(SessionPoll::Blocked) => {}
            Ok(SessionPoll::Ended) => return Ok(()),
            Err(error) => {
                return Err(map_runtime_error(&error));
            }
        }
    }
}

fn write_delivery(
    writer: &mut impl Write,
    delivery: &SessionDelivery,
    limits: &Limits,
) -> Result<(), NetbatError> {
    let frame = delivery_to_frame(delivery, limits)?;
    writer.write_all(&encode_stream_frame(&frame))?;
    Ok(())
}

fn delivery_to_frame(
    delivery: &SessionDelivery,
    limits: &Limits,
) -> Result<StreamFrame, NetbatError> {
    match delivery {
        SessionDelivery::Event(event) => {
            let subscription_id = subscription_token(&event.subscription_id, limits)?;
            let schema =
                PayloadSchemaRef::new(event.wire_payload_schema_ref.clone()).map_err(|_| {
                    NetbatError::MalformedStreamFrame {
                        reason: "payload schema ref invalid",
                    }
                })?;
            Ok(StreamFrame::SubEvent(SubEventFrame {
                subscription_id,
                delivery_index: delivery_index(event.delivery_index)?,
                cursor_before: encode_maybe_cursor(&event.cursor_before),
                cursor_after: encode_maybe_cursor(&event.cursor_after),
                payload_schema_ref: schema,
                payload: event.envelope_bytes.clone(),
            }))
        }
        SessionDelivery::Watermark(watermark) => Ok(StreamFrame::SubWatermark(SubWatermarkFrame {
            subscription_id: subscription_token(&watermark.subscription_id, limits)?,
            delivery_index: delivery_index(watermark.delivery_index)?,
            cursor_after: encode_required_cursor(&watermark.cursor_after),
        })),
        SessionDelivery::Error(error) => Ok(StreamFrame::SubErr(map_error_frame(error, limits)?)),
        SessionDelivery::End(end) => Ok(StreamFrame::SubEnd(map_end_frame(end, limits)?)),
    }
}

fn decode_subscribe_request(line: &[u8], limits: &Limits) -> Result<SubscribeFrame, NetbatError> {
    match decode_stream_line(line, limits)? {
        StreamFrame::Subscribe(frame) => Ok(frame),
        StreamFrame::SubEvent(_)
        | StreamFrame::SubWatermark(_)
        | StreamFrame::SubAck(_)
        | StreamFrame::SubCancel(_)
        | StreamFrame::SubErr(_)
        | StreamFrame::SubEnd(_) => Err(NetbatError::MalformedStreamFrame {
            reason: "first stream frame must be SUBSCRIBE",
        }),
    }
}

fn maybe_cursor_bytes(cursor: MaybeCursor) -> Option<Vec<u8>> {
    match cursor {
        MaybeCursor::Absent => None,
        MaybeCursor::Present(bytes) => Some(bytes.into_bytes()),
    }
}

fn encode_maybe_cursor(cursor: &RuntimeCursor) -> MaybeCursor {
    MaybeCursor::Present(CursorBytes::new(cursor.as_bytes().to_vec()))
}

fn encode_required_cursor(cursor: &RuntimeCursor) -> CursorBytes {
    CursorBytes::new(cursor.as_bytes().to_vec())
}

fn spawn_control_reader(
    mut reader: impl Read + Send + 'static,
    control_tx: flume::Sender<SessionControl>,
    limits: Limits,
    subscription_id: SubscriptionToken,
    stop_reader: Arc<AtomicBool>,
) -> Result<(), NetbatError> {
    thread::Builder::new()
        .name("netbat.sub-control".to_owned())
        .spawn(move || {
            let _ = read_control_loop(
                &mut reader,
                &control_tx,
                &limits,
                &subscription_id,
                &stop_reader,
            );
        })
        .map_err(|error| NetbatError::Io { kind: error.kind() })?;
    Ok(())
}

fn read_control_loop(
    reader: &mut impl Read,
    control_tx: &flume::Sender<SessionControl>,
    limits: &Limits,
    subscription_id: &SubscriptionToken,
    stop_reader: &AtomicBool,
) -> Result<(), NetbatError> {
    loop {
        if stop_reader.load(Ordering::Acquire) {
            break;
        }
        let line = match read_line(reader, limits.max_line_bytes) {
            Ok(line) => line,
            Err(NetbatError::Io { kind }) if timeout_kind(kind) => {
                if stop_reader.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }
            Err(NetbatError::EmptyStream) | Err(NetbatError::Io { .. }) => {
                let _ = control_tx.send(SessionControl::Disconnected);
                break;
            }
            Err(_) => {
                let _ = control_tx.send(SessionControl::Malformed);
                break;
            }
        };
        let frame = match decode_stream_line(&line, limits) {
            Ok(frame) => frame,
            Err(_) => {
                let _ = control_tx.send(SessionControl::Malformed);
                break;
            }
        };
        match frame {
            StreamFrame::SubAck(frame) => {
                if frame.subscription_id.as_str() != subscription_id.as_str() {
                    let _ = control_tx.send(SessionControl::Malformed);
                    break;
                }
                let cursor = RuntimeCursor::from_bytes(frame.cursor_after.into_bytes());
                let _ = control_tx.send(SessionControl::Ack {
                    delivery_index: frame.delivery_index.get(),
                    cursor,
                });
            }
            StreamFrame::SubCancel(frame) => {
                if frame.subscription_id.as_str() != subscription_id.as_str() {
                    let _ = control_tx.send(SessionControl::Malformed);
                    break;
                }
                let _ = control_tx.send(SessionControl::Cancel);
                break;
            }
            StreamFrame::Subscribe(_)
            | StreamFrame::SubEvent(_)
            | StreamFrame::SubWatermark(_)
            | StreamFrame::SubErr(_)
            | StreamFrame::SubEnd(_) => {
                let _ = control_tx.send(SessionControl::Malformed);
                break;
            }
        }
    }
    Ok(())
}

fn timeout_kind(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

fn map_open_error(subscription_id: &str, error: &SubscriptionRuntimeError) -> SessionDelivery {
    match error {
        SubscriptionRuntimeError::UnknownSubscription { .. } => {
            unknown_subscription_error(subscription_id)
        }
        SubscriptionRuntimeError::CursorInvalid { reason } => {
            open_error_for_subscription(subscription_id, CURSOR_INVALID_CODE, reason)
        }
        SubscriptionRuntimeError::CursorMismatch { reason } => {
            open_error_for_subscription(subscription_id, CURSOR_MISMATCH_CODE, reason)
        }
        SubscriptionRuntimeError::InvalidSubscriptionId { reason } => {
            open_error_for_subscription(subscription_id, CURSOR_INVALID_CODE, reason)
        }
        SubscriptionRuntimeError::DuplicateSubscription { .. } => open_error_for_subscription(
            subscription_id,
            CURSOR_INVALID_CODE,
            "duplicate subscription route",
        ),
        SubscriptionRuntimeError::InvalidRoute { reason }
        | SubscriptionRuntimeError::InvalidConfig { reason } => {
            open_error_for_subscription(subscription_id, CURSOR_INVALID_CODE, reason)
        }
        SubscriptionRuntimeError::Store(_) => open_error_for_subscription(
            subscription_id,
            CURSOR_INVALID_CODE,
            "store error during subscribe",
        ),
        SubscriptionRuntimeError::EnvelopeEncoding(_) => open_error_for_subscription(
            subscription_id,
            CURSOR_INVALID_CODE,
            "envelope encoding failed",
        ),
        SubscriptionRuntimeError::Worker(_) => open_error_for_subscription(
            subscription_id,
            CURSOR_INVALID_CODE,
            "subscription worker failed",
        ),
        SubscriptionRuntimeError::AckInvalid { reason } => {
            open_error_for_subscription(subscription_id, CURSOR_INVALID_CODE, reason)
        }
    }
}

fn open_error_for_subscription(
    subscription_id: &str,
    code: &'static str,
    reason: &'static str,
) -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: Some(subscription_id.to_owned()),
        code,
        last_delivered_cursor: None,
        last_acked_cursor: None,
        message: reason.as_bytes().to_vec(),
    })
}

fn map_error_frame(error: &SessionError, limits: &Limits) -> Result<SubErrFrame, NetbatError> {
    let subscription_id = match error.subscription_id.as_deref() {
        Some(id) => subscription_token(id, limits)?,
        None => subscription_token("unknown.invalid.v1", limits).map_err(|_| {
            NetbatError::MalformedStreamFrame {
                reason: "missing subscription id on stream error",
            }
        })?,
    };
    Ok(SubErrFrame {
        subscription_id,
        code: StreamReasonCode::new(error.code).map_err(|_| NetbatError::MalformedStreamFrame {
            reason: "stream error code invalid",
        })?,
        last_delivered_cursor: optional_cursor(error.last_delivered_cursor.as_ref()),
        last_acked_cursor: optional_cursor(error.last_acked_cursor.as_ref()),
        message: error.message.clone(),
    })
}

fn map_end_frame(end: &SessionEnd, limits: &Limits) -> Result<SubEndFrame, NetbatError> {
    Ok(SubEndFrame {
        subscription_id: subscription_token(&end.subscription_id, limits)?,
        cursor_after: end
            .cursor_after
            .as_ref()
            .map(encode_maybe_cursor)
            .unwrap_or(MaybeCursor::Absent),
        reason_code: StreamReasonCode::new(end.reason_code).map_err(|_| {
            NetbatError::MalformedStreamFrame {
                reason: "stream end code invalid",
            }
        })?,
    })
}

fn subscription_token(id: &str, limits: &Limits) -> Result<SubscriptionToken, NetbatError> {
    SubscriptionToken::new(id, limits)
}

fn delivery_index(value: u64) -> Result<DeliveryIndex, NetbatError> {
    DeliveryIndex::new(value)
}

fn terminal_delivery(delivery: &SessionDelivery) -> bool {
    matches!(
        delivery,
        SessionDelivery::Error(_) | SessionDelivery::End(_)
    )
}

fn optional_cursor(cursor: Option<&RuntimeCursor>) -> MaybeCursor {
    match cursor {
        Some(cursor) => encode_maybe_cursor(cursor),
        None => MaybeCursor::Absent,
    }
}

fn merge_stats(total: &mut TcpSubscriptionServeStats, connection: TcpSubscriptionServeStats) {
    total.served_subscriptions += connection.served_subscriptions;
    total.failed_subscriptions += connection.failed_subscriptions;
    total.malformed_pre_subscribe += connection.malformed_pre_subscribe;
    total.runtime_failures += connection.runtime_failures;
    total.connection_io_failures += connection.connection_io_failures;
}

fn map_runtime_error(error: &SubscriptionRuntimeError) -> NetbatError {
    NetbatError::MalformedStreamFrame {
        reason: match error {
            SubscriptionRuntimeError::Store(_) => "store error during stream poll",
            SubscriptionRuntimeError::InvalidSubscriptionId { reason } => reason,
            SubscriptionRuntimeError::DuplicateSubscription { .. } => {
                "duplicate subscription route"
            }
            SubscriptionRuntimeError::InvalidRoute { reason }
            | SubscriptionRuntimeError::InvalidConfig { reason } => reason,
            SubscriptionRuntimeError::UnknownSubscription { .. } => "unknown subscription",
            SubscriptionRuntimeError::CursorInvalid { reason } => reason,
            SubscriptionRuntimeError::CursorMismatch { reason } => reason,
            SubscriptionRuntimeError::EnvelopeEncoding(_) => "envelope encoding failed",
            SubscriptionRuntimeError::Worker(_) => "subscription worker failed",
            SubscriptionRuntimeError::AckInvalid { reason } => reason,
        },
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for private NETBAT/2 stream-TCP helpers. The public
    //! `serve_*` entry points are exercised end-to-end in
    //! `tests/mutation_kill_netbat-transport.rs`; these cover the small
    //! pure helpers and the control-reader loop without TCP timing.

    use super::*;
    use std::io::Cursor;
    use syncbat::SessionEventDelivery;

    fn cursor(byte: u8) -> RuntimeCursor {
        RuntimeCursor::from_bytes(vec![byte])
    }

    fn token() -> SubscriptionToken {
        SubscriptionToken::new("orders.open.v1", &Limits::default()).expect("token")
    }

    #[test]
    fn maybe_cursor_bytes_maps_present_and_absent() {
        // KILLS stream_tcp.rs:264 (-> None / Some(vec![0]) / Some(vec![1]) /
        // Some(vec![])). Present must yield the exact wrapped bytes; Absent
        // must yield None.
        assert_eq!(maybe_cursor_bytes(MaybeCursor::Absent), None);
        assert_eq!(
            maybe_cursor_bytes(MaybeCursor::Present(CursorBytes::new(vec![7, 9, 3]))),
            Some(vec![7, 9, 3])
        );
    }

    #[test]
    fn timeout_kind_classifies_block_and_timeout_only() {
        // KILLS stream_tcp.rs:369 (-> false / true). WouldBlock and TimedOut
        // are timeout kinds; BrokenPipe is not.
        assert!(timeout_kind(io::ErrorKind::WouldBlock));
        assert!(timeout_kind(io::ErrorKind::TimedOut));
        assert!(!timeout_kind(io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn terminal_delivery_is_true_only_for_error_and_end() {
        // KILLS stream_tcp.rs:475 (-> false). End is terminal; an Event is not.
        let end = SessionDelivery::End(SessionEnd {
            subscription_id: "orders.open.v1".to_owned(),
            reason_code: "stream.complete",
            cursor_after: None,
        });
        let event = SessionDelivery::Event(SessionEventDelivery {
            subscription_id: "orders.open.v1".to_owned(),
            delivery_index: 1,
            cursor_before: cursor(1),
            cursor_after: cursor(2),
            wire_payload_schema_ref: "hostbat.event.orders.v1".to_owned(),
            envelope_bytes: vec![0],
        });
        assert!(terminal_delivery(&end));
        assert!(!terminal_delivery(&event));
    }

    #[test]
    fn merge_stats_sums_each_field() {
        // KILLS stream_tcp.rs:489-493 (each `+=` -> `*=`/`-=`) and the
        // merge_stats -> () body-drop. Distinct nonzero source values mean a
        // dropped or multiplied/subtracted merge cannot reproduce the sums.
        let mut total = TcpSubscriptionServeStats::default();
        let connection = TcpSubscriptionServeStats {
            served_subscriptions: 2,
            failed_subscriptions: 3,
            malformed_pre_subscribe: 4,
            runtime_failures: 5,
            connection_io_failures: 6,
            ..Default::default()
        };
        merge_stats(&mut total, connection);
        assert_eq!(total.served_subscriptions, 2);
        assert_eq!(total.failed_subscriptions, 3);
        assert_eq!(total.malformed_pre_subscribe, 4);
        assert_eq!(total.runtime_failures, 5);
        assert_eq!(total.connection_io_failures, 6);
    }

    const CANCEL_LINE: &[u8] = b"NETBAT/2 SUB_CANCEL orders.open.v1 client.cancel\n";

    fn run_loop(reader: &mut impl Read, stop: &AtomicBool) -> Vec<SessionControl> {
        let (tx, rx) = flume::bounded(16);
        let limits = Limits::default();
        let id = token();
        let _ = read_control_loop(reader, &tx, &limits, &id, stop);
        drop(tx);
        rx.try_iter().collect()
    }

    #[test]
    fn read_control_loop_matching_cancel_emits_cancel() {
        // KILLS stream_tcp.rs:348 (`!=` -> `==`). A SUB_CANCEL whose id MATCHES
        // the session must forward Cancel; under the inverted comparison a
        // matching id would be reported Malformed instead.
        let mut reader = Cursor::new(CANCEL_LINE.to_vec());
        let stop = AtomicBool::new(false);
        let got = run_loop(&mut reader, &stop);
        assert!(
            matches!(got.first(), Some(SessionControl::Cancel)),
            "expected Cancel, got {got:?}"
        );
    }

    /// Returns WouldBlock once, then replays `rest`.
    struct WouldBlockThen {
        fired: bool,
        rest: Cursor<Vec<u8>>,
    }

    impl Read for WouldBlockThen {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.fired {
                self.fired = true;
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            self.rest.read(buf)
        }
    }

    #[test]
    fn read_control_loop_retries_after_timeout_then_reads_frame() {
        // KILLS stream_tcp.rs:313 (timeout_kind guard -> false). A WouldBlock
        // must be retried so the following SUB_CANCEL is read and forwarded as
        // Cancel; with the guard false the WouldBlock is treated as a
        // disconnect and Cancel never arrives.
        let mut reader = WouldBlockThen {
            fired: false,
            rest: Cursor::new(CANCEL_LINE.to_vec()),
        };
        let stop = AtomicBool::new(false);
        let got = run_loop(&mut reader, &stop);
        assert!(
            matches!(got.first(), Some(SessionControl::Cancel)),
            "expected Cancel after timeout retry, got {got:?}"
        );
    }

    /// Returns BrokenPipe and flips `stop` so the loop cannot spin forever
    /// when the timeout guard is forced true.
    struct BrokenPipeSetsStop {
        stop: Arc<AtomicBool>,
    }

    impl Read for BrokenPipeSetsStop {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            self.stop.store(true, Ordering::Release);
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn read_control_loop_reports_disconnect_on_broken_pipe() {
        // KILLS stream_tcp.rs:313 (timeout_kind guard -> true). A BrokenPipe is
        // NOT a timeout, so the loop must emit Disconnected. Under the guard
        // forced true the error is mistaken for a timeout and (with stop now
        // set) the loop breaks WITHOUT emitting Disconnected.
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = BrokenPipeSetsStop {
            stop: Arc::clone(&stop),
        };
        let got = run_loop(&mut reader, &stop);
        assert!(
            matches!(got.first(), Some(SessionControl::Disconnected)),
            "expected Disconnected on broken pipe, got {got:?}"
        );
    }
}
