use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use syncbat::CoreFactory;

use super::error::NetbatError;
use super::frame::{decode_line, dispatch_frame, encode_response, ResponseFrame};
use super::limiter::{stats_lane, Admission, ConnectionLimit, ConnectionPermit, Limiter};
use super::limits::{IoTimeouts, Limits};

/// Default maximum requests served from one accepted TCP connection.
pub const DEFAULT_MAX_REQUESTS_PER_CONNECTION: usize = 1;

/// Blocking TCP server limits.
///
/// `#[non_exhaustive]` so adding TLS config, listen-backlog, or
/// connection-accept timeouts post-0.8.0 stays SemVer-safe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct TcpServerConfig {
    /// Line-protocol request and response limits.
    pub limits: Limits,
    /// Optional per-connection read/write timeouts.
    pub timeouts: IoTimeouts,
    /// How accepted connections are capped. Defaults to
    /// [`ConnectionLimit::Concurrent`] — an in-flight permit pool, not a
    /// lifetime accept budget.
    pub connection_limit: ConnectionLimit,
    /// Maximum requests served per accepted connection.
    pub max_requests_per_connection: usize,
    /// Sleep interval used by the nonblocking accept loop when no connection
    /// is ready.
    pub idle_sleep: Duration,
}

impl Default for TcpServerConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            timeouts: IoTimeouts::default(),
            connection_limit: ConnectionLimit::default(),
            max_requests_per_connection: DEFAULT_MAX_REQUESTS_PER_CONNECTION,
            idle_sleep: Duration::from_millis(10),
        }
    }
}

impl TcpServerConfig {
    /// Construct the default TCP server config. Equivalent to
    /// [`TcpServerConfig::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the line-protocol [`Limits`].
    #[must_use]
    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Override the read/write [`IoTimeouts`].
    #[must_use]
    pub const fn with_timeouts(mut self, timeouts: IoTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Override [`TcpServerConfig::connection_limit`].
    #[must_use]
    pub const fn with_connection_limit(mut self, value: ConnectionLimit) -> Self {
        self.connection_limit = value;
        self
    }

    /// Override [`TcpServerConfig::max_requests_per_connection`].
    #[must_use]
    pub const fn with_max_requests_per_connection(mut self, value: usize) -> Self {
        self.max_requests_per_connection = value;
        self
    }

    /// Override [`TcpServerConfig::idle_sleep`].
    #[must_use]
    pub const fn with_idle_sleep(mut self, value: Duration) -> Self {
        self.idle_sleep = value;
        self
    }
}

/// Shared shutdown flag for blocking TCP listener loops.
#[derive(Clone, Debug, Default)]
pub struct ShutdownHandle {
    inner: Arc<AtomicBool>,
}

impl ShutdownHandle {
    /// Create a new unset shutdown handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request listener shutdown.
    pub fn shutdown(&self) {
        self.inner.store(true, Ordering::Release);
    }

    /// Return true once shutdown has been requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.inner.load(Ordering::Acquire)
    }
}

/// Summary returned after a blocking TCP listener exits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct TcpServeStats {
    /// Number of accepted TCP connections.
    pub accepted_connections: usize,
    /// Number of request frames that produced a successful response.
    pub served_requests: usize,
    /// Number of request frames that produced an error response.
    pub failed_requests: usize,
    /// Failed requests rejected by malformed framing or unsupported protocol.
    pub malformed_requests: usize,
    /// Failed requests rejected by configured line/input/output limits.
    pub limit_failures: usize,
    /// Failed requests rejected by syncbat dispatch.
    pub runtime_failures: usize,
    /// Connections torn down by a peer-driven IO error (BrokenPipe /
    /// ConnectionReset on read or write, etc.) after the empty-stream
    /// short-circuit. These are dropped silently per-connection so a
    /// misbehaving peer can't tear down the whole listener; counting
    /// them keeps the failure mode observable for operators.
    pub connection_io_failures: usize,
    /// Connection workers whose per-connection path unwound on a panic
    /// (a buggy handler, an arithmetic overflow under `overflow-checks`,
    /// etc.). The panic is caught at the worker boundary so it can't
    /// poison the listener's join or stop the accept loop from serving
    /// other connections; counting it keeps a server-side fault
    /// observable instead of silently swallowed.
    pub worker_panics: usize,
    /// True when the listener exited because its shutdown handle was set.
    pub shutdown_requested: bool,
}

/// Serve one request from an already-accepted blocking stream.
///
/// The caller owns listener setup, accept loops, thread pools, TLS, shutdown,
/// admission, and any timeout application. This helper reads one bounded
/// request line, dispatches it through syncbat, writes one response line, and
/// returns the dispatch result.
///
/// # Errors
/// Returns [`NetbatError`] when reading, decoding, dispatching, or writing
/// fails.
///
/// `max_output_bytes` is a transport serialization limit. It is enforced after
/// syncbat dispatch returns output bytes; use runtime gates or handler-level
/// validation when output size must be an admission rule.
#[tracing::instrument(name = "netbat.serve_stream", skip_all)]
pub fn serve_stream<S>(
    stream: &mut S,
    core: &mut syncbat::Core,
    limits: &Limits,
) -> Result<ResponseFrame, NetbatError>
where
    S: Read + Write,
{
    let line = match read_line(stream, limits.max_line_bytes) {
        Ok(line) => line,
        Err(NetbatError::EmptyStream) => {
            // Connect-and-close: the client closed before sending any
            // bytes. Writing an ERR frame here would race a
            // BrokenPipe/ConnectionReset IO error, which
            // `serve_tcp_connection` treats as fatal — letting a single
            // connect-and-close client kill the whole listener. Return
            // the typed EmptyStream so the caller's graceful arm
            // handles it. PROVES: tcp_transport.rs ::
            // connect_and_close_does_not_kill_the_listener.
            tracing::debug!("client closed before sending request");
            return Err(NetbatError::EmptyStream);
        }
        Err(error) => {
            let encoded = encode_response(Err(&error));
            // Best-effort write: if the peer half-closed, the ERR write
            // surfaces as BrokenPipe which we DROP rather than escalate.
            // Same reasoning — a misbehaving client must not be able to
            // tear down the listener via the consequences of its own
            // half-shut state.
            let _ = stream.write_all(&encoded);
            return Err(error);
        }
    };
    let frame = decode_line(&line, limits);
    let response = match frame {
        Ok(frame) => match dispatch_frame(core, frame, limits) {
            Ok(response) => {
                let encoded = encode_response(Ok(response.output()));
                stream.write_all(&encoded)?;
                return Ok(response);
            }
            Err(error) => {
                let encoded = encode_response(Err(&error));
                stream.write_all(&encoded)?;
                Err(error)
            }
        },
        Err(error) => {
            let encoded = encode_response(Err(&error));
            stream.write_all(&encoded)?;
            Err(error)
        }
    };
    response
}

/// Serve a blocking TCP listener until shutdown or limits stop it.
///
/// The listener is switched to nonblocking mode so [`ShutdownHandle`] can stop
/// the accept loop without opening a synthetic connection. Each accepted stream
/// is switched back to blocking mode before reads: on Windows (and most
/// non-Linux platforms) accepted sockets inherit the listener's nonblocking
/// flag, which would otherwise surface `WouldBlock` from `read_line` instead of
/// waiting for request bytes.
///
/// # Concurrency
/// The accept loop stays on the caller's thread and spawns one worker thread
/// per accepted connection. Each worker opens a fresh [`syncbat::Core`] from
/// `core_factory` because `Core` dispatch is `&mut` and handlers are not
/// required to be `Send`. Connection stats are merged through a flume lane as
/// workers finish, so a slow client cannot head-of-line-block accept or service
/// of other clients.
///
/// Admission is governed by [`TcpServerConfig::connection_limit`]. The default
/// [`ConnectionLimit::Concurrent`] is a flume permit pool: a connection acquires
/// a permit before serving and releases it on every exit path, so the in-flight
/// count never exceeds the cap and a freed slot is reused; the accept loop
/// blocks for a free slot when the pool is empty. [`ConnectionLimit::Lifetime`]
/// retains the pre-0.9 behavior (stop accepting after N total). Finished worker
/// handles are pruned each accept iteration so a long-lived `Concurrent` or
/// `Unlimited` listener does not accumulate join handles without bound.
///
/// A panic inside a connection worker (a buggy handler, an overflow-checked
/// wrap, etc.) is caught at the worker boundary, counted in
/// [`TcpServeStats::worker_panics`], and otherwise contained: it never poisons
/// the listener's worker join nor stops the accept loop from serving the next
/// connection.
///
/// # Errors
/// Returns [`NetbatError`] when listener configuration, accept, worker spawn,
/// timeout configuration, or response writes fail. Per-request decode/runtime
/// errors are counted in [`TcpServeStats::failed_requests`] after their error
/// response is written.
#[tracing::instrument(name = "netbat.serve_tcp_listener", skip_all, fields(
    addr = %listener.local_addr().map(|a| a.to_string()).unwrap_or_default(),
    connection_limit = ?config.connection_limit,
))]
pub fn serve_tcp_listener<F>(
    listener: TcpListener,
    core_factory: F,
    config: &TcpServerConfig,
    shutdown: &ShutdownHandle,
) -> Result<TcpServeStats, NetbatError>
where
    F: CoreFactory + Send + 'static,
{
    listener.set_nonblocking(true)?;
    let mut stats = TcpServeStats::default();
    let limiter = Limiter::from_limit(config.connection_limit);
    let (stats_tx, stats_rx) = stats_lane(config.connection_limit);
    let factory = Arc::new(Mutex::new(core_factory));
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    tracing::info!("accept loop started");

    while !shutdown.is_shutdown() && limiter.accepting(stats.accepted_connections) {
        drain_connection_stats(&mut stats, &stats_rx);
        workers.retain(|worker| !worker.is_finished());
        match listener.accept() {
            Ok((stream, addr)) => {
                // Acquire a concurrency permit BEFORE serving (blocks for the
                // `Concurrent` pool, instant otherwise). Holding the just-
                // accepted stream while blocking is the intended back-pressure:
                // we never spawn beyond the in-flight cap.
                let permit = match limiter.admit(shutdown, config.idle_sleep) {
                    Admission::Permit(permit) => permit,
                    Admission::Shutdown => break,
                };
                stats.accepted_connections += 1;
                tracing::debug!(peer = %addr, "connection accepted");
                stream.set_nonblocking(false)?;
                apply_timeouts(&stream, config.timeouts)?;
                let config = *config;
                let stats_tx = stats_tx.clone();
                let factory = Arc::clone(&factory);
                workers.push(spawn_connection_worker(
                    stream, config, stats_tx, factory, permit,
                )?);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(config.idle_sleep);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }

    for worker in workers {
        worker.join().map_err(|_| NetbatError::Io {
            kind: io::ErrorKind::Other,
        })?;
    }
    drain_connection_stats(&mut stats, &stats_rx);

    stats.shutdown_requested = shutdown.is_shutdown();
    tracing::info!(
        accepted = stats.accepted_connections,
        served = stats.served_requests,
        failed = stats.failed_requests,
        shutdown = stats.shutdown_requested,
        "accept loop exiting",
    );
    drop(listener);
    Ok(stats)
}

fn spawn_connection_worker<F>(
    stream: TcpStream,
    config: TcpServerConfig,
    stats_tx: flume::Sender<TcpServeStats>,
    factory: Arc<Mutex<F>>,
    permit: ConnectionPermit,
) -> Result<JoinHandle<()>, NetbatError>
where
    F: CoreFactory + Send + 'static,
{
    thread::Builder::new()
        .name("netbat-tcp-conn".to_owned())
        .spawn(move || {
            // The permit lives for the whole worker body and releases its
            // concurrency slot on drop — on the early `return` below, the
            // normal return, the error return, AND the caught-panic path —
            // because `Drop` runs on every scope exit, unwinding included. Held
            // OUTSIDE the `catch_unwind` so a panic cannot skip the release.
            let _permit = permit;
            let mut core = match factory.lock() {
                Ok(mut factory) => factory.open_core(),
                Err(_) => return,
            };
            let mut conn_stats = TcpServeStats::default();
            // Contain a per-connection panic HERE rather than letting the
            // worker thread unwind out. An escaped panic would surface at the
            // listener's `worker.join()` as an `Err`, which the join loop
            // escalated to a listener-wide failure AND short-circuited (so
            // every later worker went un-joined). Catching at the worker
            // boundary keeps `join()` infallible, the listener `Ok`, and the
            // accept loop serving; the panic is counted so it stays observable.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                serve_tcp_connection(stream, &mut core, &config, &mut conn_stats)
            }));
            match outcome {
                Ok(Ok(())) => {
                    let _ = stats_tx.send(conn_stats);
                }
                // serve_tcp_connection already classifies its own fatal
                // returns; preserve the prior behaviour of not reporting
                // stats for that connection.
                Ok(Err(_)) => {}
                Err(_panic) => {
                    conn_stats.worker_panics += 1;
                    let _ = stats_tx.send(conn_stats);
                }
            }
        })
        .map_err(NetbatError::from)
}

fn drain_connection_stats(total: &mut TcpServeStats, stats_rx: &flume::Receiver<TcpServeStats>) {
    while let Ok(partial) = stats_rx.try_recv() {
        merge_tcp_serve_stats(total, partial);
    }
}

fn merge_tcp_serve_stats(total: &mut TcpServeStats, partial: TcpServeStats) {
    total.served_requests += partial.served_requests;
    total.failed_requests += partial.failed_requests;
    total.malformed_requests += partial.malformed_requests;
    total.limit_failures += partial.limit_failures;
    total.runtime_failures += partial.runtime_failures;
    total.connection_io_failures += partial.connection_io_failures;
    total.worker_panics += partial.worker_panics;
}

fn serve_tcp_connection(
    mut stream: TcpStream,
    core: &mut syncbat::Core,
    config: &TcpServerConfig,
    stats: &mut TcpServeStats,
) -> Result<(), NetbatError> {
    serve_connection_loop(&mut stream, core, config, stats)
}

/// Drive one accepted connection through up to
/// `max_requests_per_connection` request/response rounds.
///
/// Per-connection IO failures are peer-driven: a client that sends a
/// request and closes/resets before reading the response surfaces here
/// as BrokenPipe / ConnectionReset on the response write_all. Drop the
/// connection and continue the listener — escalating any single
/// client's IO state to a listener-wide fatal would be a trivial
/// remote DoS path. The accept loop's own IO errors are still fatal
/// at the listener scope (see [`serve_tcp_listener`]). PROVES:
/// tcp.rs::tests::peer_io_failure_does_not_propagate_from_connection,
/// tests/tcp_transport.rs::peer_close_mid_response_does_not_kill_the_listener.
fn serve_connection_loop<S: Read + Write>(
    stream: &mut S,
    core: &mut syncbat::Core,
    config: &TcpServerConfig,
    stats: &mut TcpServeStats,
) -> Result<(), NetbatError> {
    for _ in 0..config.max_requests_per_connection {
        match serve_stream(stream, core, &config.limits) {
            Ok(_) => stats.served_requests += 1,
            Err(NetbatError::EmptyStream) => return Ok(()),
            Err(NetbatError::Io { .. }) => {
                stats.connection_io_failures += 1;
                tracing::debug!("connection torn down by peer IO error");
                return Ok(());
            }
            // LineTooLong cuts off the request line mid-stream — the
            // unread bytes from `max_line_bytes + 1` onwards remain on
            // the wire and are NOT followed by a fresh frame boundary.
            // Continuing this connection would re-parse that garbage as
            // the next request and emit a cascade of ERR frames or
            // worse, mis-frame on subsequent newlines. Record the
            // failure (ERR was already written by serve_stream) and
            // drop the connection so framing stays synchronized on
            // persistent sessions. PROVES: tcp_transport.rs ::
            // line_too_long_closes_connection_to_keep_framing_synchronized.
            Err(error @ NetbatError::LineTooLong { .. }) => {
                stats.failed_requests += 1;
                record_request_failure(stats, &error);
                tracing::debug!("closing connection after LineTooLong to resync framing");
                return Ok(());
            }
            Err(error) => {
                stats.failed_requests += 1;
                record_request_failure(stats, &error);
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_timeouts(stream: &TcpStream, timeouts: IoTimeouts) -> Result<(), NetbatError> {
    stream.set_read_timeout(timeouts.read)?;
    stream.set_write_timeout(timeouts.write)?;
    Ok(())
}

pub(crate) fn read_line<R: Read>(
    reader: &mut R,
    max_line_bytes: usize,
) -> Result<Vec<u8>, NetbatError> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];

    loop {
        match reader.read(&mut byte) {
            Ok(0) if line.is_empty() => return Err(NetbatError::EmptyStream),
            Ok(0) => return Ok(line),
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > max_line_bytes {
                    return Err(NetbatError::LineTooLong {
                        max: max_line_bytes,
                    });
                }
                if byte[0] == b'\n' {
                    return Ok(line);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn record_request_failure(stats: &mut TcpServeStats, error: &NetbatError) {
    match error {
        NetbatError::LineTooLong { .. }
        | NetbatError::OperationNameTooLong { .. }
        | NetbatError::InputTooLarge { .. }
        | NetbatError::OutputTooLarge { .. } => stats.limit_failures += 1,
        NetbatError::MalformedRequest { .. } | NetbatError::UnsupportedProtocolVersion { .. } => {
            stats.malformed_requests += 1;
        }
        NetbatError::MalformedStreamFrame { .. } => stats.malformed_requests += 1,
        NetbatError::SubscriptionIdTooLong { .. }
        | NetbatError::CursorTooLarge { .. }
        | NetbatError::StreamPayloadTooLarge { .. }
        | NetbatError::StreamMessageTooLarge { .. } => stats.limit_failures += 1,
        NetbatError::Runtime(_) => stats.runtime_failures += 1,
        NetbatError::Io { .. } | NetbatError::EmptyStream => {}
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the per-connection loop's failure handling.
    //! These avoid TCP timing by driving `serve_connection_loop` directly
    //! with a mock Read+Write — the integration counterpart in
    //! `tests/tcp_transport.rs` exercises the same property end-to-end.

    use super::*;
    use std::io::Cursor;
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

    fn core_with_ping() -> Core {
        let mut builder = Core::builder();
        builder.register(PING, PingHandler).expect("register");
        builder.without_receipts();
        builder.build().expect("build")
    }

    /// Read+Write that returns the request bytes once on read, then BrokenPipe
    /// on every write — simulating a peer that sent a valid frame and then
    /// reset the connection before the server's response could land.
    struct WriteFailsAfterRead {
        request: Cursor<Vec<u8>>,
    }

    impl WriteFailsAfterRead {
        fn new(request: &[u8]) -> Self {
            Self {
                request: Cursor::new(request.to_vec()),
            }
        }
    }

    impl Read for WriteFailsAfterRead {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.request.read(buf)
        }
    }

    impl Write for WriteFailsAfterRead {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn merge_tcp_serve_stats_sums_connection_io_failures() {
        // KILLS mutants at tcp.rs:336 (`+=` -> `*=`/`-=` on
        // connection_io_failures). With `*=` the merged total stays 0; with
        // `-=` it underflows. Only real addition yields 5.
        let mut total = TcpServeStats::default();
        let partial = TcpServeStats {
            connection_io_failures: 5,
            ..Default::default()
        };
        merge_tcp_serve_stats(&mut total, partial);
        assert_eq!(total.connection_io_failures, 5);
    }

    #[test]
    fn merge_tcp_serve_stats_sums_worker_panics() {
        // KILLS mutants on the `worker_panics += partial.worker_panics`
        // merge line (`+=` -> `*=`/`-=`, or a dropped merge). With `*=` the
        // merged total stays 0; with `-=` it underflows. Only real addition
        // yields 3.
        let mut total = TcpServeStats::default();
        let partial = TcpServeStats {
            worker_panics: 3,
            ..Default::default()
        };
        merge_tcp_serve_stats(&mut total, partial);
        assert_eq!(total.worker_panics, 3);
    }

    #[test]
    fn record_request_failure_counts_malformed_stream_frame_as_malformed() {
        // KILLS mutants at tcp.rs:443 (`+=` -> `*=`/`-=` on malformed_requests
        // in the MalformedStreamFrame arm).
        let mut stats = TcpServeStats::default();
        record_request_failure(
            &mut stats,
            &NetbatError::MalformedStreamFrame {
                reason: "bad stream frame",
            },
        );
        assert_eq!(stats.malformed_requests, 1);
        assert_eq!(stats.limit_failures, 0);
    }

    #[test]
    fn record_request_failure_counts_stream_limit_as_limit_failure() {
        // KILLS mutants at tcp.rs:447 (`+=` -> `*=`/`-=` on limit_failures in
        // the stream-limit arm covering CursorTooLarge and friends).
        let mut stats = TcpServeStats::default();
        record_request_failure(&mut stats, &NetbatError::CursorTooLarge { max: 1 });
        assert_eq!(stats.limit_failures, 1);
        assert_eq!(stats.malformed_requests, 0);
    }

    #[test]
    fn peer_io_failure_does_not_propagate_from_connection() {
        // REGRESSION: previously, a client that sent a valid request and
        // then RST/closed before the server's write_all completed would
        // surface as NetbatError::Io from serve_stream, and
        // serve_tcp_connection escalated that to the whole listener,
        // dropping the accept loop. Now the loop swallows per-connection
        // IO failures and counts them in TcpServeStats.
        let mut stream = WriteFailsAfterRead::new(b"NETBAT/1 CALL ping 6869\n");
        let mut core = core_with_ping();
        let config = TcpServerConfig::default();
        let mut stats = TcpServeStats::default();

        let outcome = serve_connection_loop(&mut stream, &mut core, &config, &mut stats);

        // Per-connection IO is non-fatal: the loop returns Ok and the
        // listener (the caller) is free to accept the next connection.
        assert!(
            outcome.is_ok(),
            "per-connection IO failure must not escalate; got {outcome:?}"
        );
        assert_eq!(stats.connection_io_failures, 1);
        assert_eq!(stats.served_requests, 0);
        assert_eq!(stats.failed_requests, 0);
    }
}
