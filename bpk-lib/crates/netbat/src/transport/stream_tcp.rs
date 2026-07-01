//! NETBAT/2 subscription streaming TCP adaptation (Packet C).

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use syncbat::{
    unknown_subscription_error, RuntimeCursor, SessionControl, SessionDelivery, SessionEnd,
    SessionError, SessionPoll, SubscriptionRuntimeError, SubscriptionSession,
    SubscriptionSessionFactory,
};

use super::error::NetbatError;
use super::limiter::{stats_lane, Admission, ConnectionLimit, ConnectionPermit, Limiter};
use super::limits::IoTimeouts;
use super::limits::Limits;
use super::stream_frame::{
    decode_stream_line, encode_stream_frame, CursorBytes, DeliveryIndex, MaybeCursor,
    PayloadSchemaRef, StreamFrame, StreamReasonCode, SubEndFrame, SubErrFrame, SubEventFrame,
    SubWatermarkFrame, SubscribeFrame, SubscriptionToken,
};
use super::tcp::{apply_timeouts, read_line, ShutdownHandle, TransportSecurity};

const CURSOR_INVALID_CODE: &str = "cursor_invalid";
const CURSOR_MISMATCH_CODE: &str = "cursor_mismatch";

/// Poll window the subscription delivery loop waits on the runtime's
/// event/watermark wakeup before re-checking control input. Shared by the
/// plaintext writer thread and the single-threaded TLS session loop so both
/// paths drive deliveries off the same wakeup cadence (never a sleep-spin).
pub(super) const SUBSCRIPTION_POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    /// Subscription workers whose session path unwound on a panic. The panic is
    /// caught at the worker boundary so one bad session cannot poison the
    /// listener's join nor stop the accept loop; counting it keeps the
    /// server-side fault observable instead of silently swallowed. Always zero
    /// in [`SubscriptionDispatch::Sequential`] mode, where a panic propagates
    /// inline as it did pre-0.9.
    pub worker_panics: usize,
    /// Subscription sessions whose TLS handshake failed on the worker (a
    /// cleartext peer against a TLS subscription listener, a truncated/garbage
    /// ClientHello, a handshake read timeout, etc.). Mirrors the request
    /// listener's `TcpServeStats::tls_handshake_failures`: the failure is
    /// counted and the session dropped, never listener-fatal. Present only
    /// under the `tls` feature.
    #[cfg(feature = "tls")]
    pub tls_handshake_failures: usize,
    /// True when shutdown was requested.
    pub shutdown_requested: bool,
}

/// How the subscription listener dispatches each accepted session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SubscriptionDispatch {
    /// Spawn a worker thread per subscription session (mirrors the request
    /// listener), so N subscribers stream concurrently while the accept loop
    /// stays free. Gated by the config's [`ConnectionLimit`] permit pool, with
    /// the same `catch_unwind` containment as the request path. The default.
    #[default]
    Concurrent,
    /// Serve each session inline on the accept thread, one at a time — the
    /// pre-0.9 behavior. A long-lived subscriber blocks the accept loop until
    /// its session ends, so only one subscriber streams at a time. Retained as
    /// an explicit opt-in.
    Sequential,
}

/// Blocking NETBAT/2 subscription listener configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct TcpSubscriptionServerConfig {
    /// Line and stream limits.
    pub limits: Limits,
    /// Optional per-connection read/write timeouts.
    pub timeouts: IoTimeouts,
    /// How accepted subscription connections are capped. Defaults to
    /// [`ConnectionLimit::Concurrent`].
    pub connection_limit: ConnectionLimit,
    /// Whether sessions are served concurrently (default) or inline on the
    /// accept thread.
    pub dispatch: SubscriptionDispatch,
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
            connection_limit: ConnectionLimit::default(),
            dispatch: SubscriptionDispatch::default(),
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
    let (control_tx, control_rx) = flume::bounded(16);
    let stop_control_reader = Arc::new(AtomicBool::new(false));
    let mut session = match open_session_for_subscribe(
        runtime,
        &subscribe,
        &mut writer,
        limits,
        control_rx,
        &mut stats,
    )? {
        Some(session) => session,
        None => return Ok(stats),
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

/// Serve a blocking NETBAT/2 subscription TCP listener.
///
/// Admission is governed by [`TcpSubscriptionServerConfig::connection_limit`]
/// (the same [`ConnectionLimit`] permit pool as the request listener). By
/// default ([`SubscriptionDispatch::Concurrent`]) each accepted session is
/// served on its own worker thread — mirroring the request listener's
/// `catch_unwind`-contained worker — so the accept loop stays free and N
/// subscribers stream concurrently; the per-session control lane and watermark
/// delivery are unchanged, only moved off the accept thread.
/// [`SubscriptionDispatch::Sequential`] keeps the pre-0.9 inline behavior (one
/// subscriber at a time). The runtime is shared across workers (wrapped in an
/// `Arc` internally), hence the `Send + Sync + 'static` bound — mirroring the
/// request listener taking its `CoreFactory` by value.
///
/// # Errors
/// Listener configuration, accept, timeout, worker spawn, or — in `Sequential`
/// mode only — a per-session response-write/runtime-poll failure (concurrent
/// workers contain and count their own session failures).
pub fn serve_tcp_subscription_listener<R>(
    listener: TcpListener,
    runtime: R,
    config: &TcpSubscriptionServerConfig,
    shutdown: &ShutdownHandle,
) -> Result<TcpSubscriptionServeStats, NetbatError>
where
    R: SubscriptionSessionFactory + Send + Sync + 'static,
{
    serve_tcp_subscription_listener_secured(
        listener,
        runtime,
        config,
        &TransportSecurity::Plaintext,
        shutdown,
    )
}

/// Serve a NETBAT/2 subscription listener with a chosen [`TransportSecurity`].
///
/// Identical to [`serve_tcp_subscription_listener`] but takes a
/// [`TransportSecurity`]: pass [`TransportSecurity::Plaintext`] for the
/// unencrypted two-thread session (what `serve_tcp_subscription_listener`
/// does), or — under the `tls` feature — `TransportSecurity::Tls(..)` to wrap
/// each accepted session in server-only rustls TLS.
///
/// A rustls stream is stateful record-layer machinery that is NOT safe to touch
/// from two threads, so the TLS path cannot `try_clone` the socket and split a
/// control-reader thread from the delivery writer the way the plaintext path
/// does. Instead each TLS session is served on ONE worker thread that
/// multiplexes client control-frame reads with delivery writes over the single
/// stream: deliveries are driven by the runtime's event/watermark wakeup (the
/// same `session.poll` wait the plaintext writer uses — never a sleep-spin), and
/// pending control frames are drained opportunistically between polls with a
/// non-blocking read so a control read never starves deliveries. The plaintext
/// path is byte-for-byte unchanged.
///
/// The accept loop accepts the RAW `TcpStream`, acquires the concurrency permit,
/// and dispatches the session; the TLS handshake (when configured) runs INSIDE
/// the worker, post-permit. A handshake failure is counted in
/// [`TcpSubscriptionServeStats::tls_handshake_failures`] and the session is
/// dropped — never listener-fatal, so a slow or hostile handshake occupies at
/// most one worker+permit slot.
///
/// # Errors
/// Listener configuration, accept, timeout, worker spawn, or — in `Sequential`
/// mode only — a per-session response-write/runtime-poll failure (concurrent
/// workers contain and count their own session failures).
pub fn serve_tcp_subscription_listener_secured<R>(
    listener: TcpListener,
    runtime: R,
    config: &TcpSubscriptionServerConfig,
    security: &TransportSecurity,
    shutdown: &ShutdownHandle,
) -> Result<TcpSubscriptionServeStats, NetbatError>
where
    R: SubscriptionSessionFactory + Send + Sync + 'static,
{
    listener.set_nonblocking(true)?;
    let runtime = Arc::new(runtime);
    let mut stats = TcpSubscriptionServeStats::default();
    let limiter = Limiter::from_limit(config.connection_limit);
    let (stats_tx, stats_rx) = stats_lane(config.connection_limit);
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while !shutdown.is_shutdown() && limiter.accepting(stats.accepted_connections) {
        drain_subscription_stats(&mut stats, &stats_rx);
        workers.retain(|worker| !worker.is_finished());
        match listener.accept() {
            Ok((stream, _addr)) => {
                let permit = match limiter.admit(shutdown, config.idle_sleep) {
                    Admission::Permit(permit) => permit,
                    Admission::Shutdown => break,
                };
                stats.accepted_connections += 1;
                stream.set_nonblocking(false)?;
                apply_timeouts(&stream, config.timeouts)?;
                if let Some(worker) = dispatch_subscription(
                    stream, &runtime, config, security, &stats_tx, &mut stats, permit,
                )? {
                    workers.push(worker);
                }
            }
            Err(error) => match classify_accept_error(error.kind()) {
                AcceptError::WouldBlock => thread::sleep(config.idle_sleep),
                AcceptError::Interrupted => {}
                AcceptError::Fatal => return Err(error.into()),
            },
        }
    }
    for worker in workers {
        worker.join().map_err(|_| NetbatError::Io {
            kind: io::ErrorKind::Other,
        })?;
    }
    drain_subscription_stats(&mut stats, &stats_rx);
    stats.shutdown_requested = shutdown.is_shutdown();
    drop(listener);
    Ok(stats)
}

/// Route one accepted subscription connection per
/// [`TcpSubscriptionServerConfig::dispatch`]. Sequential serves inline (updating
/// `stats`, propagating a fatal per-session error as before); Concurrent spawns
/// a contained worker and returns its handle for the listener to join.
fn dispatch_subscription<R>(
    stream: TcpStream,
    runtime: &Arc<R>,
    config: &TcpSubscriptionServerConfig,
    security: &TransportSecurity,
    stats_tx: &flume::Sender<TcpSubscriptionServeStats>,
    stats: &mut TcpSubscriptionServeStats,
    permit: ConnectionPermit,
) -> Result<Option<JoinHandle<()>>, NetbatError>
where
    R: SubscriptionSessionFactory + Send + Sync + 'static,
{
    match config.dispatch {
        SubscriptionDispatch::Sequential => {
            serve_subscription_inline(stream, runtime.as_ref(), config, security, stats, permit)?;
            Ok(None)
        }
        SubscriptionDispatch::Concurrent => Ok(Some(spawn_subscription_worker(
            stream,
            Arc::clone(runtime),
            *config,
            security.clone(),
            stats_tx.clone(),
            permit,
        )?)),
    }
}

/// Pre-0.9 inline path: serve the session on the accept thread. `permit`
/// releases the slot when the session ends. A non-IO error is fatal to the
/// listener, exactly as before this mode became opt-in.
fn serve_subscription_inline<R>(
    stream: TcpStream,
    runtime: &R,
    config: &TcpSubscriptionServerConfig,
    security: &TransportSecurity,
    stats: &mut TcpSubscriptionServeStats,
    permit: ConnectionPermit,
) -> Result<(), NetbatError>
where
    R: SubscriptionSessionFactory,
{
    let _permit = permit;
    match serve_tcp_subscription_connection(stream, runtime, config, security) {
        Ok(connection_stats) => merge_stats(stats, connection_stats),
        Err(NetbatError::Io { .. }) => stats.connection_io_failures += 1,
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Spawn a worker thread that serves one subscription session, mirroring the
/// request listener's `spawn_connection_worker`: the session's control lane and
/// watermark delivery run unchanged on this thread, a panic is caught at the
/// worker boundary (counted, never listener-fatal), and `permit` releases the
/// concurrency slot on every exit path including that caught panic.
fn spawn_subscription_worker<R>(
    stream: TcpStream,
    runtime: Arc<R>,
    config: TcpSubscriptionServerConfig,
    security: TransportSecurity,
    stats_tx: flume::Sender<TcpSubscriptionServeStats>,
    permit: ConnectionPermit,
) -> Result<JoinHandle<()>, NetbatError>
where
    R: SubscriptionSessionFactory + Send + Sync + 'static,
{
    thread::Builder::new()
        .name("netbat.sub-conn".to_owned())
        .spawn(move || {
            // Held OUTSIDE catch_unwind so a panic cannot skip the slot release.
            let _permit = permit;
            let mut conn_stats = TcpSubscriptionServeStats::default();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                serve_tcp_subscription_connection(stream, runtime.as_ref(), &config, &security)
            }));
            match outcome {
                Ok(Ok(connection_stats)) => conn_stats = connection_stats,
                // Peer-driven IO: count, do not escalate.
                Ok(Err(NetbatError::Io { .. })) => conn_stats.connection_io_failures += 1,
                // A non-IO session error (invalid config, runtime poll failure)
                // is contained to this worker and counted, not escalated to a
                // listener-wide failure: one bad subscriber must not tear down
                // the others now that sessions run concurrently.
                Ok(Err(_)) => conn_stats.failed_subscriptions += 1,
                Err(_panic) => conn_stats.worker_panics += 1,
            }
            let _ = stats_tx.send(conn_stats);
        })
        .map_err(|error| NetbatError::Io { kind: error.kind() })
}

fn drain_subscription_stats(
    total: &mut TcpSubscriptionServeStats,
    stats_rx: &flume::Receiver<TcpSubscriptionServeStats>,
) {
    while let Ok(partial) = stats_rx.try_recv() {
        merge_stats(total, partial);
    }
}

/// Serve one accepted subscription session under the listener's
/// [`TransportSecurity`]. The single dispatch seam between plaintext and TLS.
///
/// Plaintext keeps the proven two-thread model: a `try_clone` reader thread
/// forwards control frames over the flume lane while the delivery writer runs on
/// this thread. TLS cannot do that — a rustls `Connection` is stateful
/// record-layer machinery that is unsound to touch from two threads, and
/// `StreamOwned` is not cloneable — so the TLS path serves the whole session on
/// ONE thread (see [`stream_tcp_tls::serve_tls_subscription_connection`]).
fn serve_tcp_subscription_connection(
    stream: TcpStream,
    runtime: &(impl SubscriptionSessionFactory + ?Sized),
    config: &TcpSubscriptionServerConfig,
    security: &TransportSecurity,
) -> Result<TcpSubscriptionServeStats, NetbatError> {
    match security {
        TransportSecurity::Plaintext => {
            let reader = stream.try_clone().map_err(NetbatError::from)?;
            serve_subscription_stream(reader, stream, runtime, &config.limits)
        }
        #[cfg(feature = "tls")]
        TransportSecurity::Tls(tls) => {
            stream_tcp_tls::serve_tls_subscription_connection(stream, tls, runtime, config)
        }
    }
}

/// Open the runtime session for a decoded `SUBSCRIBE`, mapping open failures the
/// one way both transports use.
///
/// Returns `Ok(Some(session))` for a successful open; `Ok(None)` when the open
/// was rejected (the terminal error frame has already been written and
/// `failed_subscriptions` bumped, so the caller returns its stats); `Err` only
/// for an [`SubscriptionRuntimeError::InvalidConfig`], which is a server
/// misconfiguration the caller propagates. Shared by the plaintext two-thread
/// path and the single-threaded TLS path so the open-error mapping lives in one
/// place.
fn open_session_for_subscribe<W: Write>(
    runtime: &(impl SubscriptionSessionFactory + ?Sized),
    subscribe: &SubscribeFrame,
    writer: &mut W,
    limits: &Limits,
    control_rx: flume::Receiver<SessionControl>,
    stats: &mut TcpSubscriptionServeStats,
) -> Result<Option<Box<dyn SubscriptionSession>>, NetbatError> {
    let resume_bytes = maybe_cursor_bytes(subscribe.resume_cursor.clone());
    match runtime.open_session(
        subscribe.subscription_id.as_str(),
        resume_bytes.as_deref(),
        subscribe.client_window.get(),
        control_rx,
    ) {
        Ok(session) => Ok(Some(session)),
        Err(error @ SubscriptionRuntimeError::InvalidConfig { .. }) => {
            Err(map_runtime_error(&error))
        }
        Err(error) => {
            stats.failed_subscriptions += 1;
            let delivery = map_open_error(subscribe.subscription_id.as_str(), &error);
            write_delivery(writer, &delivery, limits)?;
            Ok(None)
        }
    }
}

fn run_subscription_loop(
    writer: &mut impl Write,
    session: &mut dyn SubscriptionSession,
    limits: &Limits,
) -> Result<(), NetbatError> {
    loop {
        match session.poll(SUBSCRIPTION_POLL_INTERVAL) {
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
        let classified = classify_control_line(&line, limits, subscription_id);
        let terminal = classified.terminal;
        let _ = control_tx.send(classified.control);
        if terminal {
            break;
        }
    }
    Ok(())
}

/// One client control frame mapped to its [`SessionControl`] plus whether it
/// ends the control stream.
///
/// `terminal` is true for everything that stops the reader: a `SUB_CANCEL`, any
/// id mismatch, an unexpected frame kind, or an undecodable line (all of which
/// map to `Malformed` except the matching cancel). Only a well-formed,
/// id-matching `SUB_ACK` is non-terminal. Pure over the line bytes so it is the
/// single decode seam shared by the plaintext control-reader thread and the
/// single-threaded TLS control drain.
pub(super) struct ClassifiedControl {
    pub(super) control: SessionControl,
    pub(super) terminal: bool,
}

pub(super) fn classify_control_line(
    line: &[u8],
    limits: &Limits,
    subscription_id: &SubscriptionToken,
) -> ClassifiedControl {
    let frame = match decode_stream_line(line, limits) {
        Ok(frame) => frame,
        Err(_) => return malformed_control(),
    };
    match frame {
        StreamFrame::SubAck(frame) => {
            if frame.subscription_id.as_str() != subscription_id.as_str() {
                return malformed_control();
            }
            let cursor = RuntimeCursor::from_bytes(frame.cursor_after.into_bytes());
            ClassifiedControl {
                control: SessionControl::Ack {
                    delivery_index: frame.delivery_index.get(),
                    cursor,
                },
                terminal: false,
            }
        }
        StreamFrame::SubCancel(frame) => {
            if frame.subscription_id.as_str() != subscription_id.as_str() {
                return malformed_control();
            }
            ClassifiedControl {
                control: SessionControl::Cancel,
                terminal: true,
            }
        }
        StreamFrame::Subscribe(_)
        | StreamFrame::SubEvent(_)
        | StreamFrame::SubWatermark(_)
        | StreamFrame::SubErr(_)
        | StreamFrame::SubEnd(_) => malformed_control(),
    }
}

fn malformed_control() -> ClassifiedControl {
    ClassifiedControl {
        control: SessionControl::Malformed,
        terminal: true,
    }
}

/// How the accept loop reacts to a failed `TcpListener::accept()`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcceptError {
    /// Nonblocking listener with no pending connection: sleep, then retry.
    WouldBlock,
    /// Syscall interrupted by a signal (EINTR): retry immediately.
    Interrupted,
    /// Unrecoverable accept failure: surface to the caller.
    Fatal,
}

/// Classify an `accept()` error kind into the loop's reaction.
///
/// Extracted as a pure seam because the `Interrupted` and `Fatal` arms cannot
/// be driven through a real `std::net::TcpListener` (you cannot make `accept()`
/// return EINTR or an arbitrary fatal kind on demand); the loop's `WouldBlock`
/// path is additionally covered end-to-end by the nonblocking listener tests.
fn classify_accept_error(kind: io::ErrorKind) -> AcceptError {
    if kind == io::ErrorKind::WouldBlock {
        AcceptError::WouldBlock
    } else if kind == io::ErrorKind::Interrupted {
        AcceptError::Interrupted
    } else {
        AcceptError::Fatal
    }
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
    total.worker_panics += connection.worker_panics;
    #[cfg(feature = "tls")]
    {
        total.tls_handshake_failures += connection.tls_handshake_failures;
    }
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

/// Single-threaded TLS subscription session: the rustls stream cannot be
/// thread-split, so control reads and delivery writes are multiplexed on one
/// worker. Gated on `feature = "tls"`; `#[path]`-split to keep this module
/// within the file-size cap.
#[cfg(feature = "tls")]
#[path = "stream_tcp_tls.rs"]
mod stream_tcp_tls;

#[cfg(test)]
#[path = "stream_tcp_tests.rs"]
mod tests;
